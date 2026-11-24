//! Data compare (K60, D-126): the rows of a *source* table (the model)
//! against those of a *target* table (what the script changes), on two
//! sessions of the same engine family, matched by a key, and the script of
//! `INSERT`, `UPDATE` and `DELETE` statements that makes the target like
//! the source.
//!
//! Each side is read by one ordered `SELECT` on a session of its own, and
//! the two streams are merged as they arrive: memory holds the rows of a
//! batch per side and the differences kept, never a whole table. The order
//! is the one thing both servers must agree on, so the key is ordered in a
//! way that depends on no collation (numbers by value, everything else by
//! the bytes of its text), and the merge checks that every key it reads is
//! greater than the one before on its side: a server that orders otherwise
//! stops the compare with an error instead of giving wrong counts, and a
//! key held twice is refused the same way.
//!
//! Values are compared as the engine types them: numbers by value, booleans
//! in any spelling, JSON without the whitespace between its tokens, times
//! without trailing zeros in their fractions, everything else byte for byte,
//! NULL apart from the empty string. Nothing here writes: the sessions are
//! made read-only first, and [`script`] returns text the user reads and runs
//! in a query tab. DBeaver's data compare is in its paid editions; nothing is
//! ported.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as Atomic};
use std::time::{Duration, Instant};

use crate::compare::{Family, commented};
use crate::config::Engine;
use crate::dialect::Dialect;
use crate::driver::Session;
use crate::error::{DbError, Error, Result};
use crate::exec::{self, ExecEvent, RowLimit};
use crate::format::compact_json;
use crate::pending::fetched;
use crate::result::{BatchBuilder, Column, ResultStore, RowBatch, TypeKind};
use crate::schema::{ColumnGeneration, ColumnInfo, TableRef, TableStructure};
use crate::sort::compare_cells;
use crate::sql::edits::{CellValue, RowChange, edit_statements};

/// Why the rows of a `source` table cannot be compared with those of a
/// `target` one, in plain words; `None` when they can.
pub fn refusal(source: Engine, target: Engine) -> Option<String> {
    match (Family::of(source), Family::of(target)) {
        (Some(a), Some(b)) if a == b => None,
        (Some(_), Some(_)) => Some(format!(
            "Comparing the rows of a {} table with those of a {} one is not supported yet: \
             the types do not map one to one.",
            source.label(),
            target.label()
        )),
        _ => Some(format!(
            "Comparing the rows of {} tables is not supported yet.",
            if Family::of(source).is_none() {
                source.label()
            } else {
                target.label()
            }
        )),
    }
}

/// How the values of a column are ordered and compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Integers, decimals and floats: by value (`1.50` is `1.5`).
    Number,
    /// False before true, in any of the servers' spellings.
    Boolean,
    /// Raw bytes (binary columns).
    Bytes,
    /// JSON: without the whitespace between tokens.
    Json,
    /// Dates and times: without trailing zeros in a fraction of a second.
    Temporal,
    /// Everything else: the bytes of the server's text.
    Text,
}

impl Mode {
    pub fn of(kind: TypeKind) -> Mode {
        match kind {
            TypeKind::Integer | TypeKind::Float | TypeKind::Decimal => Mode::Number,
            TypeKind::Boolean => Mode::Boolean,
            TypeKind::Binary => Mode::Bytes,
            TypeKind::Json => Mode::Json,
            TypeKind::Date | TypeKind::Time | TypeKind::DateTime | TypeKind::DateTimeTz => {
                Mode::Temporal
            }
            TypeKind::Text
            | TypeKind::Interval
            | TypeKind::Uuid
            | TypeKind::Array
            | TypeKind::Enum
            | TypeKind::Bit
            | TypeKind::Other => Mode::Text,
        }
    }

    /// How a column typed `source` on one side and `target` on the other
    /// is compared, if it can be: one mode, or JSON against text (MariaDB's
    /// `JSON` is a text column, MySQL's a type of its own).
    pub fn common(source: TypeKind, target: TypeKind) -> Option<Mode> {
        match (Mode::of(source), Mode::of(target)) {
            (a, b) if a == b => Some(a),
            (Mode::Json, Mode::Text) | (Mode::Text, Mode::Json) => Some(Mode::Json),
            _ => None,
        }
    }

    /// The order of two key cells, the one the `ORDER BY` of [`select_sql`]
    /// gives: NULL last.
    fn order(self, a: Option<&[u8]>, b: Option<&[u8]>) -> Ordering {
        match self {
            Mode::Number => compare_cells(a, b, TypeKind::Decimal),
            Mode::Boolean => compare_cells(a, b, TypeKind::Boolean),
            // A time of one precision against another (`DATETIME(3)` and
            // `DATETIME(6)`): without the zeros of a fraction. Within one
            // side every value has the same precision, so the server's byte
            // order is kept.
            Mode::Temporal => {
                let trimmed = |cell: Option<&[u8]>| cell.map(without_trailing_zeros);
                let (a, b) = (trimmed(a), trimmed(b));
                compare_cells(a.as_deref(), b.as_deref(), TypeKind::Text)
            }
            // `compare_cells` of any other kind: bytes, NULL last.
            _ => compare_cells(a, b, TypeKind::Text),
        }
    }

    /// Whether two cells hold the same value.
    fn same(self, a: Option<&[u8]>, b: Option<&[u8]>) -> bool {
        let (a, b) = match (a, b) {
            (None, None) => return true,
            (Some(a), Some(b)) if a == b => return true,
            (Some(a), Some(b)) => (a, b),
            _ => return false,
        };
        match self {
            Mode::Number | Mode::Boolean => self.order(Some(a), Some(b)) == Ordering::Equal,
            Mode::Json => {
                let compact = |bytes| std::str::from_utf8(bytes).ok().and_then(compact_json);
                compact(a).is_some_and(|a| Some(a) == compact(b))
            }
            Mode::Temporal => without_trailing_zeros(a) == without_trailing_zeros(b),
            Mode::Bytes | Mode::Text => false,
        }
    }
}

/// A time's text without the zeros that end its fraction of a second, and
/// without the point when nothing is left after it: `10:00:00.500000+00`
/// is `10:00:00.5+00`, `10:00:00.000` is `10:00:00`.
fn without_trailing_zeros(text: &[u8]) -> Vec<u8> {
    let Some(point) = text.iter().position(|&byte| byte == b'.') else {
        return text.to_vec();
    };
    let digits = text[point + 1..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let fraction = &text[point + 1..point + 1 + digits];
    let kept = fraction.len() - fraction.iter().rev().take_while(|&&b| b == b'0').count();
    let mut out = text[..point].to_vec();
    if kept > 0 {
        out.push(b'.');
        out.extend_from_slice(&fraction[..kept]);
    }
    out.extend_from_slice(&text[point + 1 + digits..]);
    out
}

/// The keys a table's rows can be matched by: its primary key, then each
/// unique index without a predicate whose columns are all NOT NULL (what
/// [`TableStructure::row_identity`] takes the first of).
pub fn keys(structure: &TableStructure) -> Vec<Vec<String>> {
    let mut keys = Vec::new();
    if let Some(primary) = structure.row_identity().filter(|_| {
        structure
            .columns
            .iter()
            .any(|c| c.primary_key_position.is_some())
    }) {
        keys.push(primary);
    }
    let not_null = |name: &String| {
        structure
            .columns
            .iter()
            .any(|column| &column.name == name && !column.nullable)
    };
    for index in &structure.indexes {
        let usable = index.unique
            && !index.primary
            && index.predicate.is_none()
            && !index.columns.is_empty()
            && index.columns.iter().all(not_null);
        if usable && !keys.contains(&index.columns) {
            keys.push(index.columns.clone());
        }
    }
    keys
}

/// Whether the server computes a column's value, so that no statement can
/// write it: a generated column, or an identity it always makes.
fn computed(column: &ColumnInfo) -> bool {
    matches!(
        column.generation,
        Some(
            ColumnGeneration::Stored(_)
                | ColumnGeneration::Virtual(_)
                | ColumnGeneration::IdentityAlways
        )
    )
}

/// A column both tables have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    pub name: String,
    /// Why it cannot be compared, when it cannot.
    pub refused: Option<String>,
    /// Why the script does not write it, when it does not.
    pub unwritten: Option<String>,
}

/// What two tables offer to compare: the keys their rows can be matched by
/// (the target's, its row identity first), the columns both have, and the
/// columns only one has (said, not compared).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    pub keys: Vec<Vec<String>>,
    /// Keys the source holds unique and the target does not: not offered.
    pub source_only_keys: Vec<Vec<String>>,
    /// In the source's order.
    pub shared: Vec<Shared>,
    pub only_in_source: Vec<String>,
    pub only_in_target: Vec<String>,
}

impl Shape {
    /// What can be compared of `source` against `target`, or why nothing
    /// can, in plain words.
    pub fn of(
        source: &TableStructure,
        target: &TableStructure,
    ) -> std::result::Result<Shape, String> {
        let find = |structure: &TableStructure, name: &str| {
            structure.columns.iter().find(|c| c.name == name).cloned()
        };
        let shared: Vec<Shared> = source
            .columns
            .iter()
            .filter_map(|column| {
                let other = find(target, &column.name)?;
                let refused = Mode::common(column.kind, other.kind).is_none().then(|| {
                    format!(
                        "its type is {} in the source and {} in the target",
                        column.data_type, other.data_type
                    )
                });
                let unwritten = computed(&other)
                    .then(|| "the target's server computes it: compared, never written".into());
                Some(Shared {
                    name: column.name.clone(),
                    refused,
                    unwritten,
                })
            })
            .collect();
        let only = |a: &TableStructure, b: &TableStructure| -> Vec<String> {
            a.columns
                .iter()
                .filter(|column| find(b, &column.name).is_none())
                .map(|column| column.name.clone())
                .collect()
        };
        // Only a key the target holds unique: each statement of the script
        // then changes one row at most, whatever the target's collation
        // holds equal. The source's uniqueness is checked as it is read.
        let (source_keys, target_keys) = (keys(source), keys(target));
        if target_keys.is_empty() {
            return Err(
                "The target table has no primary key or unique key on NOT NULL columns, so a \
                 statement of the script could change several of its rows. Add a key to the \
                 target, or swap the tables, to compare their rows."
                    .into(),
            );
        }
        let usable = |key: &Vec<String>| {
            key.iter().all(|name| {
                shared
                    .iter()
                    .any(|column| &column.name == name && column.refused.is_none())
            })
        };
        let usable_keys: Vec<Vec<String>> =
            target_keys.iter().filter(|k| usable(k)).cloned().collect();
        if usable_keys.is_empty() {
            return Err(format!(
                "The target's key ({}) is not in both tables with the same kind of type, so the \
                 rows cannot be matched.",
                target_keys[0].join(", ")
            ));
        }
        let source_only_keys = source_keys
            .into_iter()
            .filter(|key| !target_keys.contains(key))
            .collect();
        Ok(Shape {
            keys: usable_keys,
            source_only_keys,
            shared,
            only_in_source: only(source, target),
            only_in_target: only(target, source),
        })
    }
}

/// What the user asked to compare: the two tables, the key their rows are
/// matched by, and the other columns compared.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Request {
    pub source: TableRef,
    pub target: TableRef,
    pub key: Vec<String>,
    pub columns: Vec<String>,
}

/// A column of the compare, as each side has it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Col {
    pub name: String,
    pub mode: Mode,
    pub source: ColumnInfo,
    pub target: ColumnInfo,
}

impl Col {
    /// Whether the script can write it on the target.
    pub fn written(&self) -> bool {
        !computed(&self.target)
    }
}

/// A [`Request`] checked against both tables' structures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub source: TableRef,
    pub target: TableRef,
    /// The key, then the compared columns: the cells of a row, in order.
    pub key: Vec<Col>,
    pub columns: Vec<Col>,
    /// The target's foreign keys to itself: the script orders its rows by
    /// them.
    pub self_keys: Vec<(Vec<String>, Vec<String>)>,
}

impl Plan {
    pub fn new(
        request: &Request,
        source: &TableStructure,
        target: &TableStructure,
    ) -> std::result::Result<Plan, String> {
        if request.key.is_empty() {
            return Err("Pick the key the rows are matched by.".into());
        }
        if !keys(target).contains(&request.key) {
            return Err(format!(
                "The target does not hold ({}) unique, so a statement of the script could \
                 change several of its rows.",
                request.key.join(", ")
            ));
        }
        let col = |name: &String| -> std::result::Result<Col, String> {
            let side = |structure: &TableStructure, which: &str| {
                structure
                    .columns
                    .iter()
                    .find(|column| &column.name == name)
                    .cloned()
                    .ok_or_else(|| format!("The {which} table has no column “{name}”."))
            };
            let (s, t) = (side(source, "source")?, side(target, "target")?);
            let Some(mode) = Mode::common(s.kind, t.kind) else {
                return Err(format!(
                    "The column “{name}” is {} in the source and {} in the target: they cannot \
                     be compared.",
                    s.data_type, t.data_type
                ));
            };
            Ok(Col {
                name: name.clone(),
                mode,
                source: s,
                target: t,
            })
        };
        let key = request
            .key
            .iter()
            .map(col)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let columns = request
            .columns
            .iter()
            .filter(|name| !request.key.contains(name))
            .map(col)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let self_keys = target
            .foreign_keys
            .iter()
            .filter(|fk| fk.referenced_table == request.target)
            .map(|fk| (fk.columns.clone(), fk.referenced_columns.clone()))
            .collect();
        Ok(Plan {
            source: request.source.clone(),
            target: request.target.clone(),
            key,
            columns,
            self_keys,
        })
    }

    /// The cells of a row: the key's, then the compared columns'.
    pub fn width(&self) -> usize {
        self.key.len() + self.columns.len()
    }

    /// Every column of a row, in order.
    pub fn cols(&self) -> impl Iterator<Item = &Col> {
        self.key.iter().chain(&self.columns)
    }

    /// The columns of the rows kept of side `side` (0: source).
    pub fn result_columns(&self, side: usize) -> Arc<[Column]> {
        self.cols()
            .map(|col| {
                let info = if side == 0 { &col.source } else { &col.target };
                Column::new(col.name.clone(), info.data_type.clone(), info.kind)
            })
            .collect()
    }
}

/// What a side's session is told before it reads: nothing it runs may
/// write, and values print the same way on both servers (times in UTC, ISO
/// dates, the shortest exact floats).
pub fn settings(engine: Engine) -> &'static [&'static str] {
    match Family::of(engine) {
        Some(Family::Postgres) => &[
            "SET default_transaction_read_only TO on",
            "SET TIME ZONE 'UTC'",
            "SET DateStyle TO ISO",
            "SET IntervalStyle TO postgres",
            // Exact on every release: 1 is exact only from PostgreSQL 12.
            "SET extra_float_digits TO 3",
        ],
        Some(Family::MySql) => &[
            "SET SESSION TRANSACTION READ ONLY",
            "SET time_zone = '+00:00'",
        ],
        None => &[],
    }
}

/// The `ORDER BY` term of a key column: numbers, booleans and bytes in
/// their own order (no collation applies to them), anything else by the
/// bytes of its text, whatever the column's collation: `COLLATE "C"` on
/// PostgreSQL, the binary string of its UTF-8 on MySQL and MariaDB (a
/// `_bin` collation would ignore trailing spaces). NULL last.
fn order_term(dialect: &dyn Dialect, col: &Col, info: &ColumnInfo) -> String {
    let name = dialect.quote_ident(&col.name);
    let mysql = Family::of(dialect.engine()) == Some(Family::MySql);
    let term = match col.mode {
        Mode::Number | Mode::Boolean | Mode::Bytes => name.clone(),
        _ if mysql => format!("CAST(CONVERT({name} USING utf8mb4) AS BINARY)"),
        _ => format!("CAST({name} AS text) COLLATE \"C\""),
    };
    // PostgreSQL puts NULL last in an ascending order; MySQL first.
    if mysql && info.nullable {
        format!("{name} IS NULL, {term}")
    } else {
        term
    }
}

/// The `SELECT` that reads side `side` (0: source) of `plan`.
pub fn select_sql(plan: &Plan, side: usize, dialect: &dyn Dialect) -> String {
    let (table, info): (&TableRef, fn(&Col) -> &ColumnInfo) = if side == 0 {
        (&plan.source, |col| &col.source)
    } else {
        (&plan.target, |col| &col.target)
    };
    // MySQL and MariaDB print a FLOAT with six digits (1.2345678 and
    // 1.2345679 both read `1.23457`): as a DOUBLE it prints the value it
    // holds, which a FLOAT column takes back as it was.
    let mysql = Family::of(dialect.engine()) == Some(Family::MySql);
    let columns: Vec<String> = plan
        .cols()
        .map(|col| {
            let name = dialect.quote_ident(&col.name);
            if mysql
                && info(col)
                    .data_type
                    .to_ascii_lowercase()
                    .starts_with("float")
            {
                format!("({name} + 0e0)")
            } else {
                name
            }
        })
        .collect();
    let order: Vec<String> = plan
        .key
        .iter()
        .map(|col| order_term(dialect, col, info(col)))
        .collect();
    format!(
        "SELECT {} FROM {} ORDER BY {}",
        columns.join(", "),
        dialect.table_name(table),
        order.join(", ")
    )
}

/// How a key row differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Difference {
    OnlyInSource,
    OnlyInTarget,
    Different,
}

impl Difference {
    /// A mark and words, as the schema compare marks its objects.
    pub fn mark(self) -> (&'static str, &'static str) {
        match self {
            Difference::OnlyInSource => ("+", "only in source"),
            Difference::OnlyInTarget => ("−", "only in target"),
            Difference::Different => ("≠", "different"),
        }
    }
}

/// A difference kept: where its rows are in [`Outcome::kept`], and which
/// compared columns differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub difference: Difference,
    pub source: Option<usize>,
    pub target: Option<usize>,
    /// Indexes into [`Plan::columns`].
    pub changed: Box<[u32]>,
    /// A row both sides hold under keys the target's server holds equal and
    /// whose bytes differ (`a` and `A` under a `_ci` collation, `a` and
    /// `a ` with PAD SPACE): its UPDATE also writes the source's spelling.
    pub respelled: bool,
    /// A row of one side whose key the other side's server holds equal to a
    /// key of a row this compare did not keep beside it: no statement is
    /// written for it, a comment says why.
    pub held: bool,
}

impl Row {
    /// Words for the row, its mark's or more precise ones.
    pub fn words(&self) -> &'static str {
        match (self.difference, self.respelled, self.held) {
            (Difference::Different, true, _) => "key spelled otherwise",
            (Difference::OnlyInSource, _, true) => "only in source, target holds its key",
            (Difference::OnlyInTarget, _, true) => "only in target, source holds its key",
            (difference, ..) => difference.mark().1,
        }
    }
}

/// How far a compare is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Rows read of the source and of the target.
    pub read: [u64; 2],
    pub only_in_source: u64,
    pub only_in_target: u64,
    pub different: u64,
    pub same: u64,
}

impl Counts {
    pub fn differences(&self) -> u64 {
        self.only_in_source + self.only_in_target + self.different
    }
}

/// How many differences are kept to show and to write; beyond either cap
/// the counts go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub rows: usize,
    pub bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            rows: 10_000,
            bytes: 64 * 1024 * 1024,
        }
    }
}

/// What a compare found.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub counts: Counts,
    /// The differences kept, in key order.
    pub rows: Vec<Row>,
    /// The rows of the differences kept, source then target, as
    /// [`Plan::result_columns`] names them.
    pub kept: [ResultStore; 2],
    /// Every difference was kept.
    pub complete: bool,
    /// Per compared column, how many rows differ in it.
    pub changed: Vec<u64>,
}

/// The rows of a difference worth keeping, copied out of their batches:
/// a batch is never held past its rows.
struct Keep {
    limits: Limits,
    builders: [BatchBuilder; 2],
    stores: [ResultStore; 2],
    bytes: usize,
    rows: Vec<Row>,
    complete: bool,
}

/// Rows a kept batch holds before it is finished.
const KEPT_BATCH: usize = 1024;

impl Keep {
    fn row_bytes(batch: &RowBatch, row: usize) -> usize {
        (0..batch.column_count())
            .map(|column| batch.cell(row, column).map_or(0, <[u8]>::len))
            .sum()
    }

    /// Room for one more difference of these rows. Once one is turned down
    /// nothing more is kept: the differences kept are the first ones, so a
    /// kept child never misses a parent that came before it.
    fn room(&mut self, rows: [Option<(&RowBatch, usize)>; 2]) -> bool {
        if !self.complete {
            return false;
        }
        let bytes: usize = rows
            .iter()
            .flatten()
            .map(|(batch, row)| Self::row_bytes(batch, *row))
            .sum();
        let room = self.rows.len() < self.limits.rows && self.bytes + bytes <= self.limits.bytes;
        if room {
            self.bytes += bytes;
        } else {
            self.complete = false;
        }
        room
    }

    fn copy(&mut self, side: usize, batch: &RowBatch, row: usize) -> usize {
        let builder = &mut self.builders[side];
        let at = self.stores[side].row_count() + builder.row_count();
        for column in 0..batch.column_count() {
            builder.push_opt(batch.cell(row, column));
        }
        if builder.row_count() >= KEPT_BATCH || builder.is_full() {
            self.stores[side].push(builder.finish());
        }
        at
    }

    fn keep(
        &mut self,
        difference: Difference,
        rows: [Option<(&RowBatch, usize)>; 2],
        changed: Box<[u32]>,
    ) {
        if !self.room(rows) {
            return;
        }
        let [source, target] = rows;
        let source = source.map(|(batch, row)| self.copy(0, batch, row));
        let target = target.map(|(batch, row)| self.copy(1, batch, row));
        self.rows.push(Row {
            difference,
            source,
            target,
            changed,
            respelled: false,
            held: false,
        });
    }
}

/// One side's stream as the merge reads it.
#[derive(Default)]
struct Cursor {
    batch: Option<RowBatch>,
    row: usize,
    ended: bool,
    /// The key of the row read last, to check the next one is greater.
    last: Option<Vec<Option<Vec<u8>>>>,
}

impl Cursor {
    fn current(&self) -> Option<(&RowBatch, usize)> {
        self.batch.as_ref().map(|batch| (batch, self.row))
    }
}

/// The merge of the two sorted streams, fed a batch at a time: pure, so
/// that every interleaving can be played without a server.
pub struct Merge {
    key: Vec<Mode>,
    values: Vec<Mode>,
    sides: [Cursor; 2],
    keep: Keep,
    counts: Counts,
    changed: Vec<u64>,
}

/// Why the compare stops, said as the server's own refusals are.
fn refused(message: String) -> Error {
    Error::Db(Box::new(DbError::message(message)))
}

/// `source` or `target`, for messages.
fn side_name(side: usize) -> &'static str {
    if side == 0 { "source" } else { "target" }
}

/// A key's cells as text, for messages.
fn key_text(cells: &[Option<&[u8]>]) -> String {
    let text: Vec<String> = cells
        .iter()
        .map(|cell| {
            cell.map_or("NULL".into(), |bytes| {
                String::from_utf8_lossy(bytes).into_owned()
            })
        })
        .collect();
    text.join(", ")
}

impl Merge {
    pub fn new(plan: &Plan, limits: Limits) -> Merge {
        Merge {
            key: plan.key.iter().map(|col| col.mode).collect(),
            values: plan.columns.iter().map(|col| col.mode).collect(),
            sides: Default::default(),
            keep: Keep {
                limits,
                builders: [0, 1].map(|_| BatchBuilder::new(plan.width())),
                stores: [0, 1].map(|side| ResultStore::new(plan.result_columns(side))),
                bytes: 0,
                rows: Vec::new(),
                complete: true,
            },
            counts: Counts::default(),
            changed: vec![0; plan.columns.len()],
        }
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// The side whose next batch the merge needs, or `None` once both
    /// sides ended.
    pub fn wants(&self) -> Option<usize> {
        (0..2).find(|&side| !self.sides[side].ended && self.sides[side].batch.is_none())
    }

    /// Gives side `side` (0: source) its next batch, `None` when it has no
    /// more rows, and merges what can be merged.
    pub fn feed(&mut self, side: usize, batch: Option<RowBatch>) -> Result<()> {
        let width = self.key.len() + self.values.len();
        match batch {
            Some(batch) if batch.row_count() == 0 => {}
            Some(batch) => {
                if batch.column_count() != width {
                    return Err(Error::Protocol(format!(
                        "the {} sent {} columns instead of {width}",
                        side_name(side),
                        batch.column_count()
                    )));
                }
                self.counts.read[side] += batch.row_count() as u64;
                let cursor = &mut self.sides[side];
                cursor.batch = Some(batch);
                cursor.row = 0;
            }
            None => self.sides[side].ended = true,
        }
        self.advance()
    }

    fn key_order(&self, a: (&RowBatch, usize), b: (&RowBatch, usize)) -> Ordering {
        for (column, mode) in self.key.iter().enumerate() {
            let order = mode.order(a.0.cell(a.1, column), b.0.cell(b.1, column));
            if order != Ordering::Equal {
                return order;
            }
        }
        Ordering::Equal
    }

    /// Checks the current row of `side` comes after the one before it, and
    /// remembers its key (in buffers kept from row to row).
    fn check_order(&mut self, side: usize) -> Result<()> {
        let modes = &self.key;
        let Cursor {
            batch, row, last, ..
        } = &mut self.sides[side];
        let Some(batch) = batch.as_ref() else {
            return Ok(());
        };
        let key: Vec<Option<&[u8]>> = (0..modes.len()).map(|c| batch.cell(*row, c)).collect();
        if let Some(last) = last.as_ref() {
            let order = modes
                .iter()
                .zip(last.iter().zip(&key))
                .map(|(mode, (last, key))| mode.order(last.as_deref(), *key))
                .find(|order| *order != Ordering::Equal)
                .unwrap_or(Ordering::Equal);
            let last: Vec<Option<&[u8]>> = last.iter().map(Option::as_deref).collect();
            match order {
                Ordering::Less => {}
                Ordering::Equal => {
                    return Err(refused(format!(
                        "The {} holds more than one row with the key ({}): that key does not \
                         tell its rows apart, so they cannot be matched. Compare by another \
                         key.",
                        side_name(side),
                        key_text(&key)
                    )));
                }
                Ordering::Greater => {
                    return Err(refused(format!(
                        "The {} sent its rows in another order than the compare reads them \
                         (({}) came after ({})), so they cannot be matched and nothing is \
                         shown.",
                        side_name(side),
                        key_text(&key),
                        key_text(&last)
                    )));
                }
            }
        }
        let last = last.get_or_insert_with(|| vec![None; key.len()]);
        for (slot, cell) in last.iter_mut().zip(&key) {
            match cell {
                Some(bytes) => {
                    let buffer = slot.get_or_insert_with(Vec::new);
                    buffer.clear();
                    buffer.extend_from_slice(bytes);
                }
                None => *slot = None,
            }
        }
        Ok(())
    }

    /// Moves `side` to its next row; its batch is dropped once read.
    fn step(&mut self, side: usize) {
        let cursor = &mut self.sides[side];
        cursor.row += 1;
        if cursor
            .batch
            .as_ref()
            .is_some_and(|batch| cursor.row >= batch.row_count())
        {
            cursor.batch = None;
        }
    }

    fn advance(&mut self) -> Result<()> {
        loop {
            let needs = |cursor: &Cursor| !cursor.ended && cursor.batch.is_none();
            if needs(&self.sides[0]) || needs(&self.sides[1]) {
                return Ok(());
            }
            let order = match (self.sides[0].current(), self.sides[1].current()) {
                (None, None) => return Ok(()),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => self.key_order(a, b),
            };
            match order {
                Ordering::Less => self.check_order(0)?,
                Ordering::Greater => self.check_order(1)?,
                Ordering::Equal => {
                    self.check_order(0)?;
                    self.check_order(1)?;
                }
            }
            let [source, target] = &self.sides;
            match order {
                Ordering::Less => {
                    self.counts.only_in_source += 1;
                    self.keep.keep(
                        Difference::OnlyInSource,
                        [source.current(), None],
                        Box::new([]),
                    );
                    self.step(0);
                }
                Ordering::Greater => {
                    self.counts.only_in_target += 1;
                    self.keep.keep(
                        Difference::OnlyInTarget,
                        [None, target.current()],
                        Box::new([]),
                    );
                    self.step(1);
                }
                Ordering::Equal => {
                    let (a, b) = (
                        source.current().expect("a row"),
                        target.current().expect("a row"),
                    );
                    let first = self.key.len();
                    let changed: Box<[u32]> = self
                        .values
                        .iter()
                        .enumerate()
                        .filter(|(index, mode)| {
                            let column = first + index;
                            !mode.same(a.0.cell(a.1, column), b.0.cell(b.1, column))
                        })
                        .map(|(index, _)| index as u32)
                        .collect();
                    if changed.is_empty() {
                        self.counts.same += 1;
                    } else {
                        self.counts.different += 1;
                        for &index in &changed {
                            self.changed[index as usize] += 1;
                        }
                        self.keep
                            .keep(Difference::Different, [Some(a), Some(b)], changed);
                    }
                    self.step(0);
                    self.step(1);
                }
            }
        }
    }

    /// What was found, once both sides ended.
    pub fn finish(mut self) -> Outcome {
        for side in 0..2 {
            let batch = self.keep.builders[side].finish();
            self.keep.stores[side].push(batch);
        }
        Outcome {
            counts: self.counts,
            rows: self.keep.rows,
            kept: self.keep.stores,
            complete: self.keep.complete,
            changed: self.changed,
        }
    }
}

/// How often [`run`] tells its progress: enough for a counter to move, few
/// enough that the UI redraws ten times a second, not once per batch.
pub const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Compares the rows of `plan`'s two tables: `sessions` are the source's
/// and the target's, each of its own (they are made read-only and their
/// settings changed). Both reads run at once; `progress` hears the counts
/// as they grow, at most every [`PROGRESS_EVERY`], and when a side ends.
/// Stops with [`Error::Cancelled`] once `cancel` is set; dropping the
/// future stops both statements on their servers.
///
/// Must be called from within the Tokio runtime.
pub async fn run(
    sessions: [Arc<dyn Session>; 2],
    plan: &Plan,
    limits: Limits,
    progress: impl Fn(Counts) + Send,
    cancel: &AtomicBool,
) -> Result<Outcome> {
    for session in &sessions {
        for sql in settings(session.engine()) {
            exec::collect(session.clone(), *sql).await?;
        }
    }
    let mut reads = [0, 1].map(|side| {
        let session = sessions[side].clone();
        let sql = select_sql(plan, side, session.dialect());
        exec::run(session, sql, RowLimit::All)
    });
    let mut merge = Merge::new(plan, limits);
    let mut told: Option<Instant> = None;
    while let Some(side) = merge.wants() {
        if cancel.load(Atomic::Acquire) {
            return Err(Error::Cancelled);
        }
        if told.is_none_or(|told| told.elapsed() >= PROGRESS_EVERY) {
            progress(merge.counts());
            told = Some(Instant::now());
        }
        match reads[side].next().await {
            Some(ExecEvent::Columns(columns)) if columns.len() != plan.width() => {
                return Err(Error::Protocol(format!(
                    "the {} sent {} columns instead of {}",
                    side_name(side),
                    columns.len(),
                    plan.width()
                )));
            }
            Some(ExecEvent::Columns(_) | ExecEvent::Notice(_)) => {}
            Some(ExecEvent::Rows(batch)) => merge.feed(side, Some(batch))?,
            Some(ExecEvent::Complete { .. } | ExecEvent::Finished(Ok(()))) | None => {
                merge.feed(side, None)?;
                progress(merge.counts());
            }
            Some(ExecEvent::Finished(Err(error))) => return Err(error),
        }
    }
    let mut outcome = merge.finish();
    reconcile(&sessions, plan, &mut outcome, cancel).await?;
    Ok(outcome)
}

/// Kept rows asked of the other side at a time, one `SELECT` each joined by
/// `UNION ALL`.
const TWINS_BATCH: usize = 100;

/// The `SELECT` that asks side `other` (0: source) which of `rows` (rows of
/// the other side, `(n, key values)`) it holds, by its own equality: the
/// one the script's `WHERE` uses. Each row found comes back as `n` and its
/// key as that side prints it.
fn twins_sql(
    plan: &Plan,
    other: usize,
    rows: &[(usize, Vec<CellValue>)],
    dialect: &dyn Dialect,
) -> String {
    let table = if other == 0 {
        &plan.source
    } else {
        &plan.target
    };
    let names: Vec<String> = plan
        .key
        .iter()
        .map(|col| dialect.quote_ident(&col.name))
        .collect();
    rows.iter()
        .map(|(n, key)| {
            let conditions: Vec<String> = plan
                .key
                .iter()
                .zip(key)
                .map(|(col, value)| {
                    let name = dialect.quote_ident(&col.name);
                    let info = if other == 0 { &col.source } else { &col.target };
                    match value {
                        CellValue::Null => format!("{name} IS NULL"),
                        value => format!("{name} = {}", dialect.literal(value, info)),
                    }
                })
                .collect();
            format!(
                "SELECT {n} AS {}, {} FROM {} WHERE {}",
                dialect.quote_ident("kerno_row"),
                names.join(", "),
                dialect.table_name(table),
                conditions.join(" AND ")
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// Asks `session` (side `other`) which of the kept rows `rows` of the other
/// side it holds under a key its server takes for theirs: each found as the
/// row's place in [`Outcome::rows`] and the key as that side prints it.
async fn twins(
    session: &Arc<dyn Session>,
    plan: &Plan,
    outcome: &Outcome,
    other: usize,
    rows: &[usize],
    cancel: &AtomicBool,
) -> Result<Vec<(usize, Vec<Option<Vec<u8>>>)>> {
    let side = 1 - other;
    let mut found = Vec::new();
    for chunk in rows.chunks(TWINS_BATCH) {
        if cancel.load(Atomic::Acquire) {
            return Err(Error::Cancelled);
        }
        let asked: Vec<(usize, Vec<CellValue>)> = chunk
            .iter()
            .filter_map(|&index| {
                let row = &outcome.rows[index];
                let at = if side == 0 { row.source } else { row.target }?;
                let key = (0..plan.key.len())
                    .map(|column| kept_value(outcome, side, at, column))
                    .collect();
                Some((index, key))
            })
            .collect();
        if asked.is_empty() {
            continue;
        }
        let sql = twins_sql(plan, other, &asked, session.dialect());
        let collected = exec::collect(session.clone(), sql).await.map_err(|error| {
            refused(format!(
                "The {} could not be asked which rows it holds under another spelling of their \
                 key: {error}",
                side_name(other)
            ))
        })?;
        for result in collected.results {
            for batch in &result.batches {
                for row in 0..batch.row_count() {
                    let index = batch.text(row, 0).and_then(|n| n.parse::<usize>().ok());
                    let key = (1..batch.column_count())
                        .map(|column| batch.cell(row, column).map(<[u8]>::to_vec))
                        .collect();
                    if let Some(index) = index {
                        found.push((index, key));
                    }
                }
            }
        }
    }
    Ok(found)
}

/// After the merge, which matched keys by their bytes: the kept rows of one
/// side are asked of the other side, by that side's own equality (`a` and
/// `A` under a `_ci` collation, PAD SPACE, `citext`, a uuid against text).
/// A row only in the source the target holds is put together with its kept
/// twin only in the target ([`pair`]); without a kept twin, and a row only
/// in the target the source holds, it is `held`: no statement is written.
async fn reconcile(
    sessions: &[Arc<dyn Session>; 2],
    plan: &Plan,
    outcome: &mut Outcome,
    cancel: &AtomicBool,
) -> Result<()> {
    let of = |outcome: &Outcome, difference| -> Vec<usize> {
        (0..outcome.rows.len())
            .filter(|&index| outcome.rows[index].difference == difference)
            .collect()
    };
    let sources = of(outcome, Difference::OnlyInSource);
    let found = twins(&sessions[1], plan, outcome, 1, &sources, cancel).await?;
    pair(plan, outcome, found);
    let targets = of(outcome, Difference::OnlyInTarget);
    let found = twins(&sessions[0], plan, outcome, 0, &targets, cancel).await?;
    for (index, _) in found {
        outcome.rows[index].held = true;
    }
    Ok(())
}

/// Puts together the rows only in the source that the target holds
/// (`found`: their place in [`Outcome::rows`] and the target's spelling of
/// the key) with their kept twin only in the target: one row, different
/// when a compared column differs or when the key's spelling can be written
/// (text of one type on both sides), the same otherwise (a time of another
/// precision or type, a uuid against text). A row whose twin was not kept
/// is `held`.
pub fn pair(plan: &Plan, outcome: &mut Outcome, found: Vec<(usize, Vec<Option<Vec<u8>>>)>) {
    let width = plan.key.len();
    let mut twins: HashMap<Vec<Option<Vec<u8>>>, usize> = HashMap::new();
    for (index, row) in outcome.rows.iter().enumerate() {
        if let (Difference::OnlyInTarget, Some(at)) = (row.difference, row.target) {
            let key = (0..width)
                .map(|column| outcome.kept[1].cell(at, column).map(<[u8]>::to_vec))
                .collect();
            twins.insert(key, index);
        }
    }
    let respellable = plan.key.iter().all(|col| {
        col.mode == Mode::Text
            && col
                .source
                .data_type
                .eq_ignore_ascii_case(&col.target.data_type)
            && col.written()
    });
    let mut gone = vec![false; outcome.rows.len()];
    for (index, key) in found {
        let row = &outcome.rows[index];
        let (Difference::OnlyInSource, Some(source)) = (row.difference, row.source) else {
            continue;
        };
        let Some(twin) = twins.remove(&key) else {
            outcome.rows[index].held = true;
            continue;
        };
        let Some(target) = outcome.rows[twin].target else {
            continue;
        };
        let changed: Box<[u32]> = plan
            .columns
            .iter()
            .enumerate()
            .filter(|(column, col)| {
                let cell = |side: usize, at: usize| outcome.kept[side].cell(at, width + column);
                !col.mode.same(cell(0, source), cell(1, target))
            })
            .map(|(column, _)| column as u32)
            .collect();
        outcome.counts.only_in_source -= 1;
        outcome.counts.only_in_target -= 1;
        gone[twin] = true;
        if changed.is_empty() && !respellable {
            outcome.counts.same += 1;
            gone[index] = true;
            continue;
        }
        outcome.counts.different += 1;
        for &column in &changed {
            outcome.changed[column as usize] += 1;
        }
        outcome.rows[index] = Row {
            difference: Difference::Different,
            source: Some(source),
            target: Some(target),
            changed,
            respelled: respellable,
            held: false,
        };
    }
    let mut at = 0;
    outcome.rows.retain(|_| {
        at += 1;
        !gone[at - 1]
    });
}

/// A kept cell as a value to write: as [`fetched`] reads it.
fn kept_value(outcome: &Outcome, side: usize, row: usize, column: usize) -> CellValue {
    fetched(&outcome.kept[side], row, column)
}

/// `rows` (kept rows of side `side`) ordered so that a row comes after the
/// rows it references through the table's own foreign keys, or before when
/// `children_first`; rows in a cycle keep their order after the others.
/// Gives the order and whether a cycle was met.
fn by_references(
    plan: &Plan,
    outcome: &Outcome,
    side: usize,
    rows: &[usize],
    children_first: bool,
) -> (Vec<usize>, bool) {
    let store = &outcome.kept[side];
    let position = |name: &String| plan.cols().position(|col| &col.name == name);
    let mut parents: Vec<Vec<usize>> = vec![Vec::new(); rows.len()];
    for (columns, referenced) in &plan.self_keys {
        let (Some(columns), Some(referenced)) = (
            columns.iter().map(position).collect::<Option<Vec<_>>>(),
            referenced.iter().map(position).collect::<Option<Vec<_>>>(),
        ) else {
            continue;
        };
        let values = |row: usize, at: &[usize]| -> Option<Vec<&[u8]>> {
            at.iter().map(|&column| store.cell(row, column)).collect()
        };
        let mut by_value: HashMap<Vec<&[u8]>, usize> = HashMap::new();
        for (node, &row) in rows.iter().enumerate() {
            if let Some(value) = values(row, &referenced) {
                by_value.insert(value, node);
            }
        }
        for (node, &row) in rows.iter().enumerate() {
            if let Some(parent) = values(row, &columns).and_then(|value| by_value.get(&value))
                && *parent != node
            {
                parents[node].push(*parent);
            }
        }
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); rows.len()];
    let mut waiting: Vec<usize> = vec![0; rows.len()];
    for (node, of) in parents.iter().enumerate() {
        for &parent in of {
            children[parent].push(node);
            waiting[node] += 1;
        }
    }
    let mut ready: BinaryHeap<Reverse<usize>> = (0..rows.len())
        .filter(|&node| waiting[node] == 0)
        .map(Reverse)
        .collect();
    let mut order = Vec::with_capacity(rows.len());
    let mut placed = vec![false; rows.len()];
    while let Some(Reverse(node)) = ready.pop() {
        order.push(node);
        placed[node] = true;
        for &child in &children[node] {
            waiting[child] -= 1;
            if waiting[child] == 0 {
                ready.push(Reverse(child));
            }
        }
    }
    let cycle = order.len() < rows.len();
    order.extend((0..rows.len()).filter(|&node| !placed[node]));
    if children_first {
        order.reverse();
    }
    (order.into_iter().map(|node| rows[node]).collect(), cycle)
}

/// The comment written in place of the statement of each `held` row of
/// `rows`, rows of side `side` (0: source, inserted; 1: target, deleted).
fn held_notes(out: &mut Vec<String>, plan: &Plan, outcome: &Outcome, rows: &[&Row], side: usize) {
    for row in rows.iter().filter(|row| row.held) {
        let Some(at) = (if side == 0 { row.source } else { row.target }) else {
            continue;
        };
        let key: Vec<Option<&[u8]>> = (0..plan.key.len())
            .map(|column| outcome.kept[side].cell(at, column))
            .collect();
        let (verb, other) = if side == 0 {
            ("not inserted", "target")
        } else {
            ("not deleted", "source")
        };
        out.push(commented(&format!(
            "({}): {verb}: the {other} holds a row whose key its server takes for this one, \
             spelled otherwise. Compare again once this script has run.",
            key_text(&key)
        )));
    }
}

/// Whether a kept row of side `side` holds text that is not UTF-8 (a
/// `SQL_ASCII` database, say): it would be written with U+FFFD in place of
/// its bytes, so no statement is written for it, and `out` says why.
fn not_utf8(out: &mut Vec<String>, plan: &Plan, outcome: &Outcome, side: usize, at: usize) -> bool {
    let store = &outcome.kept[side];
    let bad = (0..plan.width()).any(|column| {
        store.columns()[column].kind != TypeKind::Binary
            && store
                .cell(at, column)
                .is_some_and(|bytes| std::str::from_utf8(bytes).is_err())
    });
    if bad {
        let key: Vec<Option<&[u8]>> = (0..plan.key.len()).map(|c| store.cell(at, c)).collect();
        out.push(commented(&format!(
            "({}): not written: a value of the {} is not UTF-8 text, and the script would change \
             its bytes.",
            key_text(&key),
            side_name(side)
        )));
    }
    bad
}

/// `statement` with a semicolon.
fn ended(statement: String) -> String {
    format!("{statement};")
}

/// The script that makes the target like the source for the differences
/// kept in `outcome`: `INSERT` for rows only in the source, `UPDATE` of the
/// changed columns for rows that differ, and, when `deletes`, `DELETE` for
/// rows only in the target (said in a comment otherwise). Written by the
/// pending-edit generator ([`edit_statements`]) in the target's `dialect`:
/// new rows parents first and deleted rows children first along the
/// table's own foreign keys, then updates, then deletes. Columns the
/// target's server computes are never written. Nothing runs.
pub fn script(
    plan: &Plan,
    outcome: &Outcome,
    dialect: &dyn Dialect,
    deletes: bool,
) -> Result<String> {
    let columns: Vec<ColumnInfo> = plan.cols().map(|col| col.target.clone()).collect();
    let mut out: Vec<String> = Vec::new();
    let note = |out: &mut Vec<String>, text: &str| out.push(commented(text));
    let of = |difference: Difference| -> Vec<&Row> {
        outcome
            .rows
            .iter()
            .filter(|row| row.difference == difference)
            .collect()
    };
    let (inserts, updates, removals) = (
        of(Difference::OnlyInSource),
        of(Difference::Different),
        of(Difference::OnlyInTarget),
    );
    if inserts.is_empty() && updates.is_empty() && removals.is_empty() {
        note(
            &mut out,
            "Nothing to change: the rows compared are the same on both sides.",
        );
        return Ok(out.join("\n") + "\n");
    }
    if !outcome.complete {
        note(
            &mut out,
            &format!(
                "Only the first {} of the {} differences are in this script: compare again \
                 once it has run to get the others.",
                outcome.rows.len(),
                outcome.counts.differences()
            ),
        );
        out.push(String::new());
    }
    let mysql = Family::of(dialect.engine()) == Some(Family::MySql);
    let timestamps = mysql
        && plan
            .cols()
            .any(|col| col.target.data_type.to_lowercase().starts_with("timestamp"));
    if timestamps {
        note(
            &mut out,
            "The values were read in UTC: this session writes TIMESTAMP values in UTC too.",
        );
        out.push("SET time_zone = '+00:00';".into());
        out.push(String::new());
    }
    let key_of = |side: usize, row: usize| -> Vec<(String, CellValue)> {
        plan.key
            .iter()
            .enumerate()
            .map(|(column, col)| (col.name.clone(), kept_value(outcome, side, row, column)))
            .collect()
    };
    let first = plan.key.len();
    // New rows carry the key even when the server makes it (an identity it
    // always makes takes OVERRIDING SYSTEM VALUE), so that they match.
    let overriding = !mysql
        && plan
            .key
            .iter()
            .any(|col| col.target.generation == Some(ColumnGeneration::IdentityAlways));
    if !inserts.is_empty() {
        out.push(commented(&format!(
            "Rows only in the source, inserted: {}",
            inserts.iter().filter(|row| !row.held).count()
        )));
        held_notes(&mut out, plan, outcome, &inserts, 0);
        let rows: Vec<usize> = inserts
            .iter()
            .filter(|row| !row.held)
            .filter_map(|row| row.source)
            .collect();
        let (rows, cycle) = by_references(plan, outcome, 0, &rows, false);
        if cycle {
            note(
                &mut out,
                "Some of these rows reference each other in a cycle: they come last, and the \
                 server may refuse them until the cycle is broken by hand.",
            );
        }
        let names: Vec<String> = plan
            .key
            .iter()
            .map(|col| col.name.clone())
            .chain(
                plan.columns
                    .iter()
                    .filter(|col| col.written())
                    .map(|col| col.name.clone()),
            )
            .collect();
        for row in rows {
            if not_utf8(&mut out, plan, outcome, 0, row) {
                continue;
            }
            let values: Vec<(String, CellValue)> = plan
                .cols()
                .enumerate()
                .filter(|(column, col)| *column < first || col.written())
                .map(|(column, col)| (col.name.clone(), kept_value(outcome, 0, row, column)))
                .collect();
            let statement = edit_statements(
                dialect,
                &plan.target,
                &columns,
                &[RowChange::Insert { values }],
            )?
            .remove(0);
            let statement = if overriding {
                let head = format!(
                    "INSERT INTO {} ({})",
                    dialect.table_name(&plan.target),
                    names
                        .iter()
                        .map(|name| dialect.quote_ident(name))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                match statement.strip_prefix(&head) {
                    Some(rest) => format!("{head} OVERRIDING SYSTEM VALUE{rest}"),
                    None => statement,
                }
            } else {
                statement
            };
            out.push(ended(statement));
        }
        out.push(String::new());
    }
    if !updates.is_empty() {
        out.push(commented(&format!(
            "Rows that differ, their changed columns updated: {}",
            updates.len()
        )));
        for row in &updates {
            let (Some(source), Some(target)) = (row.source, row.target) else {
                continue;
            };
            if not_utf8(&mut out, plan, outcome, 0, source)
                || not_utf8(&mut out, plan, outcome, 1, target)
            {
                continue;
            }
            // A key spelled otherwise takes the source's spelling; the
            // `WHERE` stays on the target's, which its key holds unique.
            let respelled = plan
                .key
                .iter()
                .enumerate()
                .filter(|_| row.respelled)
                .map(|(column, col)| (col.name.clone(), kept_value(outcome, 0, source, column)));
            let set: Vec<(String, CellValue)> = respelled
                .chain(
                    row.changed
                        .iter()
                        .map(|&index| index as usize)
                        .filter(|&index| plan.columns[index].written())
                        .map(|index| {
                            let col = &plan.columns[index];
                            (
                                col.name.clone(),
                                kept_value(outcome, 0, source, first + index),
                            )
                        }),
                )
                .collect();
            if set.is_empty() {
                let key: Vec<Option<&[u8]>> = (0..first)
                    .map(|column| outcome.kept[1].cell(target, column))
                    .collect();
                note(
                    &mut out,
                    &format!(
                        "({}): only columns the server computes differ, nothing to write.",
                        key_text(&key)
                    ),
                );
                continue;
            }
            let change = RowChange::Update {
                key: key_of(1, target),
                set,
            };
            for statement in edit_statements(dialect, &plan.target, &columns, &[change])? {
                out.push(ended(statement));
            }
        }
        out.push(String::new());
    }
    if !removals.is_empty() {
        if deletes {
            out.push(commented(&format!(
                "Rows only in the target, deleted: {}",
                removals.iter().filter(|row| !row.held).count()
            )));
            held_notes(&mut out, plan, outcome, &removals, 1);
            let rows: Vec<usize> = removals
                .iter()
                .filter(|row| !row.held)
                .filter_map(|row| row.target)
                .collect();
            let (rows, _) = by_references(plan, outcome, 1, &rows, true);
            for row in rows {
                if not_utf8(&mut out, plan, outcome, 1, row) {
                    continue;
                }
                let change = RowChange::Delete {
                    key: key_of(1, row),
                };
                for statement in edit_statements(dialect, &plan.target, &columns, &[change])? {
                    out.push(ended(statement));
                }
            }
        } else {
            note(
                &mut out,
                &format!(
                    "Rows only in the target, kept: {}. Tick “Also delete what only the target \
                     has” to write their DELETE statements.",
                    removals.len()
                ),
            );
        }
        out.push(String::new());
    }
    let sequenced: Vec<&Col> = plan
        .key
        .iter()
        .filter(|col| {
            matches!(
                col.target.generation,
                Some(
                    ColumnGeneration::AutoIncrement
                        | ColumnGeneration::IdentityByDefault
                        | ColumnGeneration::IdentityAlways
                )
            )
        })
        .collect();
    if !mysql && !inserts.is_empty() && !sequenced.is_empty() {
        let names: Vec<&str> = sequenced.iter().map(|col| col.name.as_str()).collect();
        note(
            &mut out,
            &format!(
                "The new rows carry their own {}: the sequence behind it does not move. Set it \
                 past the largest value before the application inserts rows, with setval().",
                names.join(", ")
            ),
        );
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    Ok(out.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    //! K60: the merge of the two sorted streams, the order both servers are
    //! read in, the values compared as the engine types them, and the script.

    use super::*;
    use crate::dialect::test_support::AnsiDialect;
    use crate::schema::{ForeignKeyInfo, IndexInfo};

    fn info(name: &str, kind: TypeKind, key: Option<u32>) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: format!("{kind:?}").to_lowercase(),
            kind,
            nullable: key.is_none(),
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: key,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn table(columns: Vec<ColumnInfo>) -> TableStructure {
        TableStructure {
            columns,
            ..TableStructure::default()
        }
    }

    fn t() -> TableRef {
        TableRef::new("s", "t")
    }

    /// A plan over `columns` of a table that both sides have as it is,
    /// keyed by the columns with a key position.
    fn plan_of(columns: Vec<ColumnInfo>) -> Plan {
        let structure = table(columns);
        let key = structure.row_identity().expect("a key");
        let others = structure
            .columns
            .iter()
            .map(|column| column.name.clone())
            .filter(|name| !key.contains(name))
            .collect();
        let request = Request {
            source: t(),
            target: t(),
            key,
            columns: others,
        };
        Plan::new(&request, &structure, &structure).unwrap()
    }

    /// `id` int key, `name` text, `price` decimal.
    fn simple() -> Plan {
        plan_of(vec![
            info("id", TypeKind::Integer, Some(1)),
            info("name", TypeKind::Text, None),
            info("price", TypeKind::Decimal, None),
        ])
    }

    type Cells<'a> = Vec<Option<&'a str>>;

    fn row<'a>(cells: &[Option<&'a str>]) -> Cells<'a> {
        cells.to_vec()
    }

    /// `rows` in batches of `size`.
    fn batches(rows: &[Cells], width: usize, size: usize) -> Vec<RowBatch> {
        rows.chunks(size.max(1))
            .map(|chunk| {
                let mut builder = BatchBuilder::new(width);
                for row in chunk {
                    for cell in row {
                        builder.push_opt(cell.map(str::as_bytes));
                    }
                }
                builder.finish()
            })
            .collect()
    }

    /// Plays the merge with these batches of each side, the source's first.
    fn play(plan: &Plan, feeds: [Vec<RowBatch>; 2], limits: Limits) -> Result<Outcome> {
        let mut feeds = feeds.map(Vec::into_iter);
        let mut merge = Merge::new(plan, limits);
        while let Some(side) = merge.wants() {
            let batch = feeds[side].next();
            merge.feed(side, batch)?;
        }
        Ok(merge.finish())
    }

    /// Merges the two sides fed in batches of `sizes`, the source's and
    /// the target's.
    fn merge_sized(
        plan: &Plan,
        source: &[Cells],
        target: &[Cells],
        sizes: (usize, usize),
    ) -> Result<Outcome> {
        let feeds = [
            batches(source, plan.width(), sizes.0),
            batches(target, plan.width(), sizes.1),
        ];
        play(plan, feeds, Limits::default())
    }

    /// Each difference as text: its mark, its key, and the changed columns.
    fn summary(plan: &Plan, outcome: &Outcome) -> Vec<String> {
        outcome
            .rows
            .iter()
            .map(|row| {
                let (side, at) = match (row.source, row.target) {
                    (Some(at), _) => (0, at),
                    (None, Some(at)) => (1, at),
                    (None, None) => unreachable!(),
                };
                let key: Vec<&str> = (0..plan.key.len())
                    .map(|column| outcome.kept[side].text(at, column).unwrap_or("NULL"))
                    .collect();
                let changed: Vec<&str> = row
                    .changed
                    .iter()
                    .map(|&index| plan.columns[index as usize].name.as_str())
                    .collect();
                format!(
                    "{} {} {}",
                    row.difference.mark().0,
                    key.join("/"),
                    changed.join(",")
                )
                .trim_end()
                .to_string()
            })
            .collect()
    }

    /// Merges with batches of every size, which must all agree; gives the
    /// summary and the counts.
    fn merged(plan: &Plan, source: &[Cells], target: &[Cells]) -> (Vec<String>, Counts) {
        let whole = merge_sized(plan, source, target, (usize::MAX, usize::MAX)).unwrap();
        let expected = (summary(plan, &whole), whole.counts);
        for sizes in [
            (1, 1),
            (1, 3),
            (2, 1),
            (3, 2),
            (1, usize::MAX),
            (usize::MAX, 1),
        ] {
            let outcome = merge_sized(plan, source, target, sizes).unwrap();
            assert_eq!(
                (summary(plan, &outcome), outcome.counts),
                expected,
                "{sizes:?}"
            );
        }
        expected
    }

    #[test]
    fn runs_only_on_one_side_and_rows_on_both_interleave() {
        let plan = simple();
        let s = |id: &'static str| row(&[Some(id), Some("a"), Some("1.50")]);
        let source = vec![s("1"), s("2"), s("3"), s("5"), s("6"), s("9"), s("10")];
        let target = vec![
            row(&[Some("3"), Some("a"), Some("1.5")]),
            row(&[Some("4"), Some("a"), Some("1.50")]),
            row(&[Some("5"), Some("b"), Some("1.50")]),
            row(&[Some("6"), Some("a"), Some("2")]),
            row(&[Some("7"), Some("a"), Some("1")]),
            row(&[Some("8"), Some("a"), Some("1")]),
            row(&[Some("10"), Some("b"), Some("9")]),
        ];
        let (summary, counts) = merged(&plan, &source, &target);
        assert_eq!(
            summary,
            [
                "+ 1",
                "+ 2",
                "− 4",
                "≠ 5 name",
                "≠ 6 price",
                "− 7",
                "− 8",
                "+ 9",
                "≠ 10 name,price"
            ]
        );
        assert_eq!(
            counts,
            Counts {
                read: [7, 7],
                only_in_source: 3,
                only_in_target: 3,
                different: 3,
                // 3: 1.5 is 1.50.
                same: 1,
            }
        );
    }

    #[test]
    fn an_empty_side_and_two_empty_sides() {
        let plan = simple();
        let rows = vec![
            row(&[Some("1"), None, None]),
            row(&[Some("2"), Some(""), None]),
        ];
        assert_eq!(merged(&plan, &rows, &[]).0, ["+ 1", "+ 2"]);
        assert_eq!(merged(&plan, &[], &rows).0, ["− 1", "− 2"]);
        let (summary, counts) = merged(&plan, &[], &[]);
        assert!(summary.is_empty());
        assert_eq!(counts, Counts::default());
    }

    #[test]
    fn null_is_not_the_empty_string_and_matches_null() {
        let plan = simple();
        let source = vec![
            row(&[Some("1"), None, None]),
            row(&[Some("2"), Some(""), Some("0")]),
            row(&[Some("3"), None, Some("1")]),
        ];
        let target = vec![
            row(&[Some("1"), None, None]),
            row(&[Some("2"), None, Some("0.0")]),
            row(&[Some("3"), Some(""), None]),
        ];
        assert_eq!(
            merged(&plan, &source, &target).0,
            ["≠ 2 name", "≠ 3 name,price"]
        );
    }

    #[test]
    fn composite_keys_order_by_each_column_in_turn() {
        let plan = plan_of(vec![
            info("order_id", TypeKind::Integer, Some(1)),
            info("line", TypeKind::Text, Some(2)),
            info("qty", TypeKind::Integer, None),
        ]);
        let source = vec![
            row(&[Some("1"), Some("B"), Some("1")]),
            row(&[Some("1"), Some("a"), Some("1")]),
            row(&[Some("2"), Some("A"), Some("1")]),
            row(&[Some("10"), Some("A"), Some("1")]),
        ];
        let target = vec![
            row(&[Some("1"), Some("a"), Some("2")]),
            row(&[Some("1"), Some("b"), Some("1")]),
            row(&[Some("10"), Some("A"), Some("1")]),
        ];
        assert_eq!(
            merged(&plan, &source, &target).0,
            ["+ 1/B", "≠ 1/a qty", "− 1/b", "+ 2/A"]
        );
    }

    /// A MariaDB-like dialect: backquotes, no schemas.
    struct My;

    impl Dialect for My {
        fn engine(&self) -> Engine {
            Engine::MariaDb
        }
        fn has_schemas(&self) -> bool {
            false
        }
        fn lex_rules(&self) -> crate::sql::LexRules {
            crate::sql::LexRules::MYSQL
        }
        fn quote_ident(&self, ident: &str) -> String {
            format!("`{ident}`")
        }
        fn quote_string(&self, text: &str) -> String {
            format!("'{text}'")
        }
        fn quote_bytes(&self, _: &[u8]) -> String {
            String::new()
        }
        fn explain(&self, sql: &str, _: bool) -> String {
            sql.into()
        }
        fn type_for(&self, _: TypeKind) -> &'static str {
            "text"
        }
        fn estimated_rows_sql(&self, _: &TableRef) -> String {
            String::new()
        }
    }

    /// The trap of text keys: no collation, only bytes, on both servers;
    /// numbers by value, not as text.
    #[test]
    fn keys_order_by_bytes_or_by_value_whatever_the_collation() {
        let plan = plan_of(vec![info("k", TypeKind::Text, Some(1))]);
        // In a locale (en_US) these sort a, B, é, Z; in bytes B, Z, a, é.
        let bytes = ["B", "Z", "a", "é"].map(|k| row(&[Some(k)]));
        assert_eq!(merged(&plan, &bytes, &bytes).1.same, 4);
        let locale = ["a", "B", "é", "Z"].map(|k| row(&[Some(k)]));
        let error = merge_sized(&plan, &locale, &bytes, (9, 9)).unwrap_err();
        assert!(
            error.to_string().contains(
                "The source sent its rows in another order than the compare reads them \
                 ((B) came after (a))"
            ),
            "{error}"
        );
        // Numbers by value: 9 before 10, which as text is out of order.
        let numbers = plan_of(vec![info("k", TypeKind::Integer, Some(1))]);
        let by_value = ["9", "10", "100"].map(|k| row(&[Some(k)]));
        assert_eq!(merged(&numbers, &by_value, &by_value).1.same, 3);
        assert!(merge_sized(&plan, &by_value, &[], (9, 9)).is_err());
        // What the servers are asked: no collation applies.
        assert_eq!(
            select_sql(&plan, 1, &AnsiDialect),
            "SELECT \"k\" FROM \"s\".\"t\" ORDER BY CAST(\"k\" AS text) COLLATE \"C\""
        );
        assert_eq!(
            select_sql(&numbers, 0, &AnsiDialect),
            "SELECT \"k\" FROM \"s\".\"t\" ORDER BY \"k\""
        );
        assert_eq!(
            select_sql(&plan, 0, &My),
            "SELECT `k` FROM `s`.`t` ORDER BY CAST(CONVERT(`k` USING utf8mb4) AS BINARY)"
        );
        // A key column that may hold NULL puts it last on MySQL, as
        // PostgreSQL does.
        let mut nullable = plan.clone();
        nullable.key[0].target.nullable = true;
        assert!(
            select_sql(&nullable, 1, &My)
                .ends_with("ORDER BY `k` IS NULL, CAST(CONVERT(`k` USING utf8mb4) AS BINARY)")
        );
    }

    #[test]
    fn duplicate_keys_are_refused_on_either_side() {
        let plan = simple();
        let rows = vec![
            row(&[Some("1"), None, None]),
            row(&[Some("2"), None, None]),
            row(&[Some("2"), Some("x"), None]),
        ];
        for (source, target, side) in [(&rows, &vec![], "source"), (&vec![], &rows, "target")] {
            for sizes in [(1, 1), (9, 9)] {
                let error = merge_sized(&plan, source, target, sizes).unwrap_err();
                assert_eq!(
                    error.to_string(),
                    format!(
                        "The {side} holds more than one row with the key (2): that key does not \
                         tell its rows apart, so they cannot be matched. Compare by another key."
                    )
                );
            }
        }
        // 2 and 2.0 are one number.
        let decimal = plan_of(vec![info("k", TypeKind::Decimal, Some(1))]);
        let rows = ["1", "2", "2.0"].map(|k| row(&[Some(k)]));
        assert!(merge_sized(&decimal, &rows, &[], (9, 9)).is_err());
    }

    #[test]
    fn values_compare_as_the_engine_types_them() {
        use Mode::*;
        let same = |mode: Mode, a: &str, b: &str| mode.same(Some(a.as_bytes()), Some(b.as_bytes()));
        assert!(same(Number, "1.50", "1.5") && same(Number, "-0", "0"));
        assert!(same(Number, "1e2", "100"));
        assert!(!same(Number, "1.5", "1.51") && !same(Text, "1.50", "1.5"));
        assert!(same(Boolean, "t", "true") && same(Boolean, "0", "f"));
        assert!(!same(Boolean, "t", "f"));
        assert!(same(
            Json,
            "{\"a\": 1, \"b\": [1, 2]}",
            "{\"a\":1,\"b\":[1,2]}"
        ));
        // Key order is what the server printed: jsonb and MySQL's JSON sort
        // their keys themselves.
        assert!(!same(Json, "{\"a\":1,\"b\":2}", "{\"b\":2,\"a\":1}"));
        assert!(!same(Json, "{\"a\":\"x y\"}", "{\"a\":\"xy\"}"));
        assert!(same(
            Temporal,
            "2024-01-01 10:00:00.500000+00",
            "2024-01-01 10:00:00.5+00"
        ));
        assert!(same(Temporal, "10:00:00.000", "10:00:00"));
        assert!(!same(Temporal, "10:00:00.5", "10:00:00.05"));
        assert!(!same(Text, "a", "A") && !same(Bytes, "a", "a "));
        assert!(Text.same(None, None) && !Text.same(None, Some(b"")));
        // MariaDB's JSON is a text column: compared as JSON with MySQL's.
        assert_eq!(Mode::common(TypeKind::Json, TypeKind::Text), Some(Json));
        assert_eq!(Mode::common(TypeKind::Integer, TypeKind::Text), None);
    }

    /// Three source rows of 13 bytes each, the target empty.
    fn three_rows(plan: &Plan, limits: Limits) -> Outcome {
        let source = vec![
            row(&[Some("1"), Some("abcdefghij"), Some("12")]),
            row(&[Some("2"), Some("abcdefghij"), Some("12")]),
            row(&[Some("3"), Some("abcdefghij"), Some("12")]),
        ];
        play(plan, [batches(&source, 3, 9), Vec::new()], limits).unwrap()
    }

    #[test]
    fn differences_beyond_the_cap_are_counted_not_kept() {
        let plan = simple();
        let side = |odd: u32| -> Vec<RowBatch> {
            let keys: Vec<String> = (0..100_000u32).map(|n| (n * 2 + odd).to_string()).collect();
            keys.chunks(1024)
                .map(|chunk| {
                    let mut builder = BatchBuilder::new(3);
                    for key in chunk {
                        builder.push_text(key);
                        builder.push_text("some text");
                        builder.push_text("1");
                    }
                    builder.finish()
                })
                .collect()
        };
        let limits = Limits {
            rows: 10,
            bytes: usize::MAX,
        };
        let outcome = play(&plan, [side(0), side(1)], limits).unwrap();
        assert_eq!(outcome.rows.len(), 10);
        assert_eq!(
            outcome.kept[0].row_count() + outcome.kept[1].row_count(),
            10
        );
        assert!(!outcome.complete);
        assert_eq!(
            (outcome.counts.only_in_source, outcome.counts.only_in_target),
            (100_000, 100_000)
        );
        // The bytes cap too: room for two rows of 13 bytes.
        let limits = Limits {
            rows: 100,
            bytes: 27,
        };
        let small = three_rows(&plan, limits);
        assert_eq!((small.rows.len(), small.complete), (2, false));
        assert!(three_rows(&plan, Limits::default()).complete);
        // F6: a large row turned down, a smaller one after it is not kept:
        // the differences kept are the first ones.
        let source = vec![
            row(&[Some("1"), Some("a"), Some("1")]),
            row(&[Some("2"), Some("a much longer value"), Some("1")]),
            row(&[Some("3"), Some("b"), Some("1")]),
        ];
        let limits = Limits {
            rows: 100,
            bytes: 10,
        };
        let outcome = play(&plan, [batches(&source, 3, 9), Vec::new()], limits).unwrap();
        assert_eq!(summary(&plan, &outcome), ["+ 1"]);
    }

    fn unique(name: &str, columns: &[&str], predicate: Option<&str>) -> IndexInfo {
        IndexInfo {
            name: name.into(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            unique: true,
            primary: false,
            backs_constraint: false,
            method: "btree".into(),
            predicate: predicate.map(str::to_string),
            definition: None,
        }
    }

    #[test]
    fn the_keys_columns_and_refusals_of_two_tables() {
        let mut email = info("email", TypeKind::Text, None);
        email.nullable = false;
        let mut total = info("total", TypeKind::Decimal, None);
        total.generation = Some(ColumnGeneration::Stored("price * 2".into()));
        let source = TableStructure {
            columns: vec![
                info("id", TypeKind::Integer, Some(1)),
                email.clone(),
                info("nick", TypeKind::Text, None),
                info("note", TypeKind::Text, None),
                info("total", TypeKind::Decimal, None),
                info("old", TypeKind::Text, None),
            ],
            indexes: vec![
                unique("u_email", &["email"], None),
                unique("u_nick", &["nick"], None),
                unique("u_part", &["email"], Some("id > 0")),
            ],
            ..TableStructure::default()
        };
        let target = TableStructure {
            columns: vec![
                info("id", TypeKind::Integer, Some(1)),
                email,
                info("nick", TypeKind::Text, None),
                info("note", TypeKind::Integer, None),
                total,
                info("new", TypeKind::Text, None),
            ],
            ..TableStructure::default()
        };
        let shape = Shape::of(&source, &target).unwrap();
        // Only the target's keys: `email` is unique in the source alone, so
        // a statement on it could hit two rows of the target. The nullable
        // `nick` and the partial index are no key.
        assert_eq!(shape.keys, [vec!["id".to_string()]]);
        assert_eq!(shape.source_only_keys, [vec!["email".to_string()]]);
        let request = Request {
            source: t(),
            target: t(),
            key: vec!["email".into()],
            columns: vec![],
        };
        assert!(
            Plan::new(&request, &source, &target)
                .unwrap_err()
                .starts_with("The target does not hold (email) unique")
        );
        assert_eq!(
            (&shape.only_in_source[..], &shape.only_in_target[..]),
            (&["old".to_string()][..], &["new".to_string()][..])
        );
        let column = |name: &str| shape.shared.iter().find(|c| c.name == name).unwrap();
        assert_eq!(
            column("note").refused.as_deref(),
            Some("its type is text in the source and integer in the target")
        );
        assert!(column("total").unwritten.is_some() && column("total").refused.is_none());
        // A target without a key: refused in words, whatever the source has.
        let keyless = table(vec![info("a", TypeKind::Text, None)]);
        let keyed = table(vec![info("a", TypeKind::Text, Some(1))]);
        for source in [&keyless, &keyed] {
            assert!(
                Shape::of(source, &keyless)
                    .unwrap_err()
                    .starts_with("The target table has no primary key")
            );
        }
        // The target's key serves when the source has none.
        assert_eq!(
            Shape::of(&keyless, &keyed).unwrap().keys,
            [vec!["a".to_string()]]
        );
        // A key the other side lacks matches nothing.
        let other = table(vec![info("b", TypeKind::Text, Some(1))]);
        assert_eq!(
            Shape::of(&keyed, &other).unwrap_err(),
            "The target's key (b) is not in both tables with the same kind of type, so the rows \
             cannot be matched."
        );
        // A plan refuses what the shape refused.
        let request = Request {
            source: t(),
            target: t(),
            key: vec!["id".into()],
            columns: vec!["note".into()],
        };
        assert!(
            Plan::new(&request, &source, &target)
                .unwrap_err()
                .contains("cannot be compared")
        );
        let request = Request {
            columns: vec!["old".into()],
            ..request
        };
        assert_eq!(
            Plan::new(&request, &source, &target).unwrap_err(),
            "The target table has no column “old”."
        );
    }

    /// The script of `plan` for these rows.
    fn script_of(plan: &Plan, source: &[Cells], target: &[Cells], deletes: bool) -> String {
        let outcome = merge_sized(plan, source, target, (9, 9)).unwrap();
        script(plan, &outcome, &AnsiDialect, deletes).unwrap()
    }

    #[test]
    fn the_script_inserts_updates_and_deletes_only_when_asked() {
        let plan = simple();
        let source = vec![
            row(&[Some("1"), Some("it's"), Some("2.50")]),
            row(&[Some("2"), Some("b"), Some("1")]),
        ];
        let target = vec![
            row(&[Some("2"), Some("b"), Some("3")]),
            row(&[Some("3"), None, None]),
        ];
        assert_eq!(
            script_of(&plan, &source, &target, false),
            "-- Rows only in the source, inserted: 1\n\
             INSERT INTO \"s\".\"t\" (\"id\", \"name\", \"price\") VALUES (1, 'it''s', 2.50);\n\
             \n\
             -- Rows that differ, their changed columns updated: 1\n\
             UPDATE \"s\".\"t\" SET \"price\" = 1 WHERE \"id\" = 2;\n\
             \n\
             -- Rows only in the target, kept: 1. Tick “Also delete what only the target has” \
             to write their DELETE statements.\n"
        );
        let deleted = script_of(&plan, &source, &target, true);
        assert!(
            deleted.ends_with(
                "-- Rows only in the target, deleted: 1\nDELETE FROM \"s\".\"t\" WHERE \"id\" = 3;\n"
            ),
            "{deleted}"
        );
        assert_eq!(
            script_of(&plan, &source, &source, true),
            "-- Nothing to change: the rows compared are the same on both sides.\n"
        );
    }

    #[test]
    fn composite_keys_binary_json_and_computed_columns() {
        let mut total = info("total", TypeKind::Decimal, None);
        total.generation = Some(ColumnGeneration::Stored("2".into()));
        let plan = plan_of(vec![
            info("a b", TypeKind::Integer, Some(1)),
            info("k\"q", TypeKind::Text, Some(2)),
            info("blob", TypeKind::Binary, None),
            info("doc", TypeKind::Json, None),
            total,
        ]);
        let source = vec![
            row(&[
                Some("1"),
                Some("x"),
                Some("\u{1}\u{2}"),
                Some("{\"a\": 1}"),
                Some("2"),
            ]),
            row(&[Some("2"), Some("y"), None, Some("[1, 2]"), Some("4")]),
        ];
        let target = vec![row(&[
            Some("2"),
            Some("y"),
            Some("\u{3}"),
            Some("[1,2]"),
            Some("5"),
        ])];
        assert_eq!(
            script_of(&plan, &source, &target, false),
            "-- Rows only in the source, inserted: 1\n\
             INSERT INTO \"s\".\"t\" (\"a b\", \"k\"\"q\", \"blob\", \"doc\") \
             VALUES (1, 'x', X'0102', '{\"a\": 1}');\n\
             \n\
             -- Rows that differ, their changed columns updated: 1\n\
             UPDATE \"s\".\"t\" SET \"blob\" = NULL WHERE \"a b\" = 2 AND \"k\"\"q\" = 'y';\n"
        );
        // Only the computed column differs: nothing to write, said.
        let target = vec![row(&[Some("2"), Some("y"), None, Some("[1,2]"), Some("5")])];
        let script = script_of(&plan, &source[1..], &target, false);
        assert!(
            script
                .contains("-- (2, y): only columns the server computes differ, nothing to write."),
            "{script}"
        );
        assert!(!script.contains("UPDATE"));
    }

    /// A key read from the server reaches a comment: a line break in it
    /// never ends the comment (the K30 finding).
    #[test]
    fn a_hostile_key_never_leaves_sql_out_of_a_comment() {
        let mut total = info("total", TypeKind::Decimal, None);
        total.generation = Some(ColumnGeneration::Stored("2".into()));
        let plan = plan_of(vec![info("k", TypeKind::Text, Some(1)), total]);
        for hostile in [
            "x\nDROP TABLE kerno_test_victim;",
            "x\rDROP TABLE kerno_test_victim;",
            "x\r\nDROP TABLE kerno_test_victim;",
            "x\u{2028}DROP TABLE kerno_test_victim;",
            "x\u{85}DROP TABLE kerno_test_victim;",
        ] {
            let source = vec![row(&[Some(hostile), Some("1")])];
            let target = vec![row(&[Some(hostile), Some("2")])];
            let script = script_of(&plan, &source, &target, true);
            for line in script.split(['\n', '\r', '\u{85}', '\u{2028}', '\u{2029}']) {
                assert!(
                    line.is_empty() || line.starts_with("--"),
                    "{line:?} in {script:?}"
                );
            }
        }
    }

    #[test]
    fn rows_of_a_table_that_references_itself_come_parents_first() {
        let mut structure = table(vec![
            info("id", TypeKind::Integer, Some(1)),
            info("parent", TypeKind::Integer, None),
        ]);
        structure.foreign_keys.push(ForeignKeyInfo {
            name: "fk".into(),
            table: t(),
            columns: vec!["parent".into()],
            referenced_table: t(),
            referenced_columns: vec!["id".into()],
            on_update: String::new(),
            on_delete: String::new(),
        });
        let request = Request {
            source: t(),
            target: t(),
            key: vec!["id".into()],
            columns: vec!["parent".into()],
        };
        let plan = Plan::new(&request, &structure, &structure).unwrap();
        // 3 ← 1 ← 4 ← 2, and 5 refers to itself; read in key order.
        let tree = vec![
            row(&[Some("1"), Some("3")]),
            row(&[Some("2"), Some("4")]),
            row(&[Some("3"), None]),
            row(&[Some("4"), Some("1")]),
            row(&[Some("5"), Some("5")]),
        ];
        // The id of each statement of a kind, in order.
        let ids = |script: &str, verb: &str| -> Vec<String> {
            script
                .lines()
                .filter(|line| line.starts_with(verb))
                .map(|line| {
                    let id = match verb {
                        "INSERT" => line.split("VALUES (").nth(1).unwrap(),
                        _ => line.split("\"id\" = ").nth(1).unwrap(),
                    };
                    id.split([',', ';']).next().unwrap().to_string()
                })
                .collect()
        };
        let inserted = script_of(&plan, &tree, &[], false);
        assert_eq!(
            ids(&inserted, "INSERT"),
            ["3", "1", "4", "2", "5"],
            "{inserted}"
        );
        let deleted = script_of(&plan, &[], &tree, true);
        assert_eq!(
            ids(&deleted, "DELETE"),
            ["5", "2", "4", "1", "3"],
            "{deleted}"
        );
        // A cycle: said, its rows last.
        let cycle = vec![
            row(&[Some("1"), Some("2")]),
            row(&[Some("2"), Some("1")]),
            row(&[Some("3"), None]),
        ];
        let script = script_of(&plan, &cycle, &[], false);
        assert!(
            script.contains("-- Some of these rows reference each other in a cycle"),
            "{script}"
        );
        assert_eq!(ids(&script, "INSERT"), ["3", "1", "2"]);
    }

    #[test]
    fn a_key_the_server_always_makes_is_written_over_its_identity() {
        let mut id = info("id", TypeKind::Integer, Some(1));
        id.generation = Some(ColumnGeneration::IdentityAlways);
        let plan = plan_of(vec![id, info("name", TypeKind::Text, None)]);
        let script = script_of(&plan, &[row(&[Some("7"), Some("a")])], &[], false);
        assert!(
            script.contains(
                "INSERT INTO \"s\".\"t\" (\"id\", \"name\") OVERRIDING SYSTEM VALUE VALUES (7, 'a');"
            ),
            "{script}"
        );
        assert!(
            script.contains(
                "-- The new rows carry their own id: the sequence behind it does not move."
            ),
            "{script}"
        );
    }

    #[test]
    fn a_script_of_the_kept_differences_says_it_is_partial() {
        let plan = simple();
        let limits = Limits {
            rows: 1,
            bytes: usize::MAX,
        };
        let outcome = three_rows(&plan, limits);
        let text = script(&plan, &outcome, &AnsiDialect, false).unwrap();
        assert!(
            text.starts_with("-- Only the first 1 of the 3 differences are in this script"),
            "{text}"
        );
        assert_eq!(text.matches("INSERT").count(), 1);
    }

    /// F2: keys the target's server holds equal and whose bytes differ are
    /// one row, never an INSERT and a DELETE.
    #[test]
    fn a_key_spelled_otherwise_is_one_row_never_an_insert_and_a_delete() {
        let plan = plan_of(vec![
            info("k", TypeKind::Text, Some(1)),
            info("n", TypeKind::Integer, None),
        ]);
        // `a` and `A` for a `_ci` target, `b` and `b ` with PAD SPACE.
        let source = vec![
            row(&[Some("a"), Some("1")]),
            row(&[Some("b"), Some("2")]),
            row(&[Some("c"), Some("3")]),
            row(&[Some("x"), Some("4")]),
        ];
        let target = vec![
            row(&[Some("A"), Some("1")]),
            row(&[Some("Y"), Some("5")]),
            row(&[Some("b "), Some("9")]),
        ];
        let mut outcome = merge_sized(&plan, &source, &target, (9, 9)).unwrap();
        assert_eq!(outcome.counts.differences(), 7);
        // What the target answers for the rows only in the source: `a` is its
        // `A`, `b` its `b `, `c` a row this compare did not keep (`C`).
        let place = |outcome: &Outcome, key: &str| {
            outcome
                .rows
                .iter()
                .position(|row| {
                    row.difference == Difference::OnlyInSource
                        && outcome.kept[0].text(row.source.unwrap(), 0) == Some(key)
                })
                .unwrap()
        };
        let found = vec![
            (place(&outcome, "a"), vec![Some(b"A".to_vec())]),
            (place(&outcome, "b"), vec![Some(b"b ".to_vec())]),
            (place(&outcome, "c"), vec![Some(b"C".to_vec())]),
        ];
        pair(&plan, &mut outcome, found);
        let words: Vec<&str> = outcome.rows.iter().map(Row::words).collect();
        assert_eq!(
            words,
            [
                "only in target",
                "key spelled otherwise",
                "key spelled otherwise",
                "only in source, target holds its key",
                "only in source"
            ]
        );
        assert_eq!(
            (
                outcome.counts.only_in_source,
                outcome.counts.only_in_target,
                outcome.counts.different
            ),
            (2, 1, 2)
        );
        let text = script(&plan, &outcome, &AnsiDialect, true).unwrap();
        for line in [
            "UPDATE \"s\".\"t\" SET \"k\" = 'a' WHERE \"k\" = 'A';",
            "UPDATE \"s\".\"t\" SET \"k\" = 'b', \"n\" = 2 WHERE \"k\" = 'b ';",
            "-- (c): not inserted: the target holds a row whose key its server takes for this one",
            "INSERT INTO \"s\".\"t\" (\"k\", \"n\") VALUES ('x', 4);",
            "DELETE FROM \"s\".\"t\" WHERE \"k\" = 'Y';",
        ] {
            assert!(text.contains(line), "{line} in\n{text}");
        }
        assert!(!text.contains("VALUES ('a'") && !text.contains("WHERE \"k\" = 'A';\nDELETE"));
        assert_eq!(text.matches("INSERT").count(), 1);
        assert_eq!(text.matches("DELETE").count(), 1);
        // A uuid against text: the target cannot keep another spelling, so a
        // twin with the same values is the same row, not a difference.
        let mut other = plan.clone();
        other.key[0].target.data_type = "uuid".into();
        let mut outcome = merge_sized(&other, &source[..1], &target[..1], (9, 9)).unwrap();
        let a = place(&outcome, "a");
        pair(&other, &mut outcome, vec![(a, vec![Some(b"A".to_vec())])]);
        assert!(outcome.rows.is_empty());
        assert_eq!((outcome.counts.same, outcome.counts.differences()), (1, 0));
    }

    /// F2: a time of one precision is matched with the same time of
    /// another (`DATETIME(3)` against `DATETIME(6)`).
    #[test]
    fn times_of_two_precisions_are_one_key() {
        let plan = plan_of(vec![info("at", TypeKind::DateTime, Some(1))]);
        let source =
            ["2024-01-01 10:00:00.500", "2024-01-01 10:00:01.000"].map(|k| row(&[Some(k)]));
        let target =
            ["2024-01-01 10:00:00.500000", "2024-01-01 10:00:01.000000"].map(|k| row(&[Some(k)]));
        assert_eq!(merged(&plan, &source, &target).1.same, 2);
    }

    /// K60 review, F5: text that is not UTF-8 (a `SQL_ASCII` database) is
    /// never written with U+FFFD in place of its bytes.
    #[test]
    fn text_that_is_not_utf8_is_never_written() {
        let plan = simple();
        let side = |rows: &[(&str, &[u8])]| -> Vec<RowBatch> {
            let mut builder = BatchBuilder::new(3);
            for (key, name) in rows {
                builder.push_text(key);
                builder.push_bytes(name);
                builder.push_text("1");
            }
            vec![builder.finish()]
        };
        let source = side(&[("1", b"caf\xe9"), ("2", b"ok"), ("3", b"new")]);
        let target = side(&[("2", b"caf\xe9"), ("4", b"old\xff")]);
        let outcome = play(&plan, [source, target], Limits::default()).unwrap();
        let text = script(&plan, &outcome, &AnsiDialect, true).unwrap();
        for line in [
            "-- (1): not written: a value of the source is not UTF-8 text",
            "-- (2): not written: a value of the target is not UTF-8 text",
            "-- (4): not written: a value of the target is not UTF-8 text",
            "INSERT INTO \"s\".\"t\" (\"id\", \"name\", \"price\") VALUES (3, 'new', 1);",
        ] {
            assert!(text.contains(line), "{line} in\n{text}");
        }
        assert!(!text.contains('\u{fffd}'), "{text}");
        assert_eq!(text.matches("INSERT").count(), 1);
        assert!(
            !text.contains("UPDATE") && !text.contains("DELETE"),
            "{text}"
        );
    }

    /// K60 review, F3: a MySQL FLOAT is read as a DOUBLE, which prints it
    /// whole.
    #[test]
    fn a_mysql_float_is_read_whole() {
        let mut plan = plan_of(vec![
            info("k", TypeKind::Integer, Some(1)),
            info("f", TypeKind::Float, None),
        ]);
        for col in &mut plan.columns {
            col.source.data_type = "float".into();
        }
        assert_eq!(
            select_sql(&plan, 0, &My),
            "SELECT `k`, (`f` + 0e0) FROM `s`.`t` ORDER BY `k`"
        );
        assert_eq!(
            select_sql(&plan, 0, &AnsiDialect),
            "SELECT \"k\", \"f\" FROM \"s\".\"t\" ORDER BY \"k\""
        );
    }

    #[test]
    fn only_engines_of_one_family_compare() {
        assert_eq!(refusal(Engine::MySql, Engine::MariaDb), None);
        assert_eq!(refusal(Engine::Postgres, Engine::Postgres), None);
        assert_eq!(
            refusal(Engine::Postgres, Engine::MySql).as_deref(),
            Some(
                "Comparing the rows of a PostgreSQL table with those of a MySQL one is not \
                 supported yet: the types do not map one to one."
            )
        );
        assert!(refusal(Engine::Sqlite, Engine::Sqlite).is_some());
        assert!(settings(Engine::Postgres).contains(&"SET TIME ZONE 'UTC'"));
        // F4: exact floats on every PostgreSQL release, not from 12 only.
        assert!(settings(Engine::Postgres).contains(&"SET extra_float_digits TO 3"));
        assert!(settings(Engine::MariaDb).contains(&"SET SESSION TRANSACTION READ ONLY"));
    }
}
