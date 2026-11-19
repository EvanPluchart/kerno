//! Copying tables with their data to another connection (CLAUDE.md §4 J13,
//! DBeaver #17767): the tables are created on the target (or appended to),
//! then the source rows are streamed and written in batches of multi-row
//! `INSERT`s, one transaction per batch.
//!
//! The type mapping follows the principles of DBeaver's data transfer
//! (Apache-2.0): `DBStructUtils.mapTargetDataType`
//! (`plugins/org.jkiss.dbeaver.model/src/org/jkiss/dbeaver/model/struct/DBStructUtils.java`)
//! keeps the full type when source and target are the same kind of server;
//! `JDBCSQLDialect.convertExternalDataType` turns a `varchar` without a
//! length into the target's long text type; `PostgreDialect.convertExternalDataType`
//! maps `json` / `jsonb` to `jsonb` on PostgreSQL; numeric types keep their
//! precision and scale. The table below (PostgreSQL ↔ MySQL / MariaDB) is
//! Kerno's own; what has no counterpart is refused, never guessed.
//!
//! Memory holds one batch: the source is read with back-pressure
//! ([`exec::run`]) and each batch is written before more rows are asked for.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::config::Engine;
use crate::dialect::Dialect;
use crate::driver::{Session, TxState};
use crate::error::{Error, Result};
use crate::exec::{self, ExecEvent, RowLimit};
use crate::result::{RowBatch, TypeKind};
use crate::schema::{ColumnGeneration, ColumnInfo, ConstraintKind, TableRef, TableStructure};
use crate::sql::edits::CellValue;
use crate::table_draft::{AutoIncrement, ColumnDraft, TableDraft};

/// Rows per `INSERT` and per transaction, unless the plan says otherwise.
pub const DEFAULT_BATCH_ROWS: usize = 1_000;

/// Characters of a failing batch's first row kept in the report.
const FIRST_ROW_CHARS: usize = 300;

/// A target type found for a source column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapped {
    /// The target type, as `CREATE TABLE` writes it.
    pub data_type: String,
    /// What the user should know about the conversion.
    pub note: Option<String>,
}

/// Why a source column has no target type: it has no counterpart there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmapped(pub String);

impl std::fmt::Display for Unmapped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The lower-case name of a type without its parenthesised arguments, the
/// numbers among those arguments, and whether it is `unsigned`:
/// `timestamp(3) with time zone` gives `("timestamp with time zone", [3],
/// false)`, `decimal(10,2) unsigned` gives `("decimal", [10, 2], true)`.
fn split_type(data_type: &str) -> (String, Vec<u32>, bool) {
    let lower = data_type.to_ascii_lowercase();
    let (mut rest, mut args) = (lower.clone(), Vec::new());
    if let (Some(open), Some(close)) = (lower.find('('), lower.find(')'))
        && open < close
    {
        args = lower[open + 1..close]
            .split(',')
            .filter_map(|arg| arg.trim().parse().ok())
            .collect();
        rest = format!("{} {}", &lower[..open], &lower[close + 1..]);
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    let unsigned = words.contains(&"unsigned");
    let name = words
        .into_iter()
        .filter(|word| !matches!(*word, "unsigned" | "zerofill"))
        .collect::<Vec<_>>()
        .join(" ");
    (name, args, unsigned)
}

fn mapped(data_type: impl Into<String>) -> Result<Mapped, Unmapped> {
    Ok(Mapped {
        data_type: data_type.into(),
        note: None,
    })
}

fn noted(data_type: impl Into<String>, note: impl Into<String>) -> Result<Mapped, Unmapped> {
    Ok(Mapped {
        data_type: data_type.into(),
        note: Some(note.into()),
    })
}

fn refused(column: &ColumnInfo, to: Engine) -> Result<Mapped, Unmapped> {
    Err(Unmapped(format!(
        "`{} {}` has no {} equivalent: skip the column or type a target type",
        column.name,
        column.data_type,
        to.label()
    )))
}

/// The labels of an enum, for a note.
fn labels(column: &ColumnInfo) -> String {
    let shown: Vec<&str> = column
        .enum_values
        .iter()
        .take(6)
        .map(String::as_str)
        .collect();
    let more = if column.enum_values.len() > shown.len() {
        ", …"
    } else {
        ""
    };
    format!("{}{more}", shown.join(", "))
}

/// The longest label of an enum or set, at least 1.
fn longest_label(column: &ColumnInfo) -> usize {
    column
        .enum_values
        .iter()
        .map(|label| label.chars().count())
        .max()
        .unwrap_or(1)
        .max(1)
}

/// The target type of `column` when copied from a `from` server to a `to`
/// server. The same engine keeps the type as the server prints it; MySQL and
/// MariaDB keep it between them except for MariaDB's own types; PostgreSQL
/// and MySQL / MariaDB map the common types both ways (see the module's
/// documentation) and refuse arrays, ranges, geometric, network, interval
/// and other types that have no counterpart.
pub fn map_type(from: Engine, to: Engine, column: &ColumnInfo) -> Result<Mapped, Unmapped> {
    if from == to {
        return match column.kind {
            TypeKind::Enum if to == Engine::Postgres => noted(
                column.data_type.clone(),
                "a user-defined type: it must exist on the target",
            ),
            _ => mapped(column.data_type.clone()),
        };
    }
    match (from, to) {
        (Engine::Postgres, _) => postgres_to_mysql(to, column),
        (_, Engine::Postgres) => mysql_to_postgres(column),
        _ => between_mysql_and_mariadb(to, column),
    }
}

fn between_mysql_and_mariadb(to: Engine, column: &ColumnInfo) -> Result<Mapped, Unmapped> {
    match column.kind {
        TypeKind::Uuid if to == Engine::MySql => noted("char(36)", "UUID as its text"),
        TypeKind::Other => refused(column, to),
        _ => mapped(column.data_type.clone()),
    }
}

fn postgres_to_mysql(to: Engine, column: &ColumnInfo) -> Result<Mapped, Unmapped> {
    let (name, args, _) = split_type(&column.data_type);
    let key = column.primary_key_position.is_some();
    // MySQL keys no TEXT or BLOB column without a prefix length.
    let long = |text: &str, keyed: &str| {
        if key {
            noted(
                keyed,
                "in the primary key: MySQL cannot key a TEXT or BLOB column, longer values are refused",
            )
        } else {
            mapped(text)
        }
    };
    let precision = |default: u32| args.first().copied().unwrap_or(default).min(6);
    match column.kind {
        TypeKind::Integer => mapped(match name.as_str() {
            "smallint" | "int2" => "smallint",
            "integer" | "int" | "int4" => "int",
            "oid" | "xid" | "cid" => "int unsigned",
            _ => "bigint",
        }),
        TypeKind::Decimal if name == "money" => {
            noted("decimal(19,2)", "money as a number, without its currency")
        }
        TypeKind::Decimal => match args.as_slice() {
            [p, s] if *p <= 65 && *s <= 30 => mapped(format!("decimal({p},{s})")),
            [p] if *p <= 65 => mapped(format!("decimal({p},0)")),
            [] => noted(
                "decimal(65,30)",
                "numeric without a precision: MySQL keeps at most 65 digits, 30 after the point",
            ),
            _ => noted(
                "decimal(65,30)",
                "MySQL keeps at most 65 digits, 30 after the point",
            ),
        },
        TypeKind::Float => mapped(if name == "real" || name == "float4" {
            "float"
        } else {
            "double"
        }),
        TypeKind::Boolean => mapped("tinyint(1)"),
        TypeKind::Text => match name.as_str() {
            "character varying" | "varchar" => match args.first() {
                Some(n) if *n <= 16_383 => mapped(format!("varchar({n})")),
                _ => long("longtext", "varchar(255)"),
            },
            "character" | "char" | "bpchar" => match args.first().copied().unwrap_or(1) {
                n if n <= 255 => mapped(format!("char({n})")),
                n if n <= 16_383 => mapped(format!("varchar({n})")),
                _ => long("longtext", "varchar(255)"),
            },
            "name" => mapped("varchar(63)"),
            "\"char\"" => mapped("char(1)"),
            "xml" => noted("longtext", "XML as text"),
            _ => long("longtext", "varchar(255)"),
        },
        TypeKind::Binary => long("longblob", "varbinary(255)"),
        TypeKind::Date => mapped("date"),
        TypeKind::Time if name.contains("with time zone") => refused(column, to),
        TypeKind::Time => mapped(format!("time({})", precision(6))),
        TypeKind::DateTime => mapped(format!("datetime({})", precision(6))),
        TypeKind::DateTimeTz => noted(
            format!("datetime({})", precision(6)),
            "converted to UTC: MySQL keeps no time zone",
        ),
        TypeKind::Uuid if to == Engine::MariaDb => mapped("uuid"),
        TypeKind::Uuid => noted("char(36)", "UUID as its text"),
        TypeKind::Json => mapped("json"),
        TypeKind::Enum => noted(
            format!("varchar({})", longest_label(column)),
            format!(
                "enum ({}): the target column takes any text",
                labels(column)
            ),
        ),
        TypeKind::Bit if name == "bit" => match args.first().copied().unwrap_or(1) {
            n if n <= 64 => mapped(format!("bit({n})")),
            _ => refused(column, to),
        },
        _ => refused(column, to),
    }
}

/// MySQL types the server stores as binary but that are not bytes to copy.
const SPATIAL: [&str; 10] = [
    "geometry",
    "point",
    "linestring",
    "polygon",
    "multipoint",
    "multilinestring",
    "multipolygon",
    "geometrycollection",
    "geomcollection",
    "vector",
];

fn mysql_to_postgres(column: &ColumnInfo) -> Result<Mapped, Unmapped> {
    let (name, args, unsigned) = split_type(&column.data_type);
    let to = Engine::Postgres;
    if SPATIAL.contains(&name.as_str()) {
        return refused(column, to);
    }
    let precision = |default: u32| args.first().copied().unwrap_or(default).min(6);
    match column.kind {
        TypeKind::Integer => match (name.as_str(), unsigned) {
            ("tinyint" | "year", _) | ("smallint", false) => mapped("smallint"),
            ("smallint", true) | ("mediumint", _) | ("int" | "integer", false) => mapped("integer"),
            ("bigint", true) => noted("numeric(20,0)", "PostgreSQL has no unsigned 64-bit integer"),
            _ => mapped("bigint"),
        },
        TypeKind::Boolean => mapped("boolean"),
        TypeKind::Decimal => match args.as_slice() {
            [p, s] => mapped(format!("numeric({p},{s})")),
            [p] => mapped(format!("numeric({p},0)")),
            _ => mapped("numeric"),
        },
        TypeKind::Float => mapped(if name == "float" {
            "real"
        } else {
            "double precision"
        }),
        TypeKind::Bit => mapped(format!("bit({})", args.first().copied().unwrap_or(1))),
        TypeKind::Text => match (name.as_str(), args.first()) {
            ("char", Some(n)) => mapped(format!("char({n})")),
            ("varchar", Some(n)) => mapped(format!("varchar({n})")),
            _ => mapped("text"),
        },
        TypeKind::Binary => mapped("bytea"),
        TypeKind::Date => mapped("date"),
        TypeKind::Time => mapped(format!("time({})", precision(0))),
        TypeKind::DateTime if name == "timestamp" => noted(
            format!("timestamp({})", precision(0)),
            "the wall-clock time in the source session's time zone",
        ),
        TypeKind::DateTime => mapped(format!("timestamp({})", precision(0))),
        TypeKind::Uuid => mapped("uuid"),
        TypeKind::Json => mapped("jsonb"),
        TypeKind::Enum if name == "set" => noted("text", "set values as comma-separated text"),
        TypeKind::Enum => noted(
            format!("varchar({})", longest_label(column)),
            format!(
                "enum ({}): the target column takes any text",
                labels(column)
            ),
        ),
        _ => refused(column, to),
    }
}

/// One source column and what it becomes on the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMapping {
    /// The source column.
    pub source: ColumnInfo,
    /// Its name on the target.
    pub target_name: String,
    /// Its type on the target; empty when none was found.
    pub target_type: String,
    /// Left out: neither created nor copied.
    pub skip: bool,
    /// What the user should know about the conversion.
    pub note: Option<String>,
    /// Why no target type was found ([`map_type`]).
    pub unmapped: Option<String>,
}

impl ColumnMapping {
    /// The default mapping of `column` from `from` to `to`. A generated
    /// column is skipped: its value is computed, not stored.
    pub fn new(from: Engine, to: Engine, column: &ColumnInfo) -> Self {
        let generated = matches!(
            column.generation,
            Some(ColumnGeneration::Stored(_) | ColumnGeneration::Virtual(_))
        );
        let (target_type, note, unmapped) = match map_type(from, to, column) {
            Ok(found) => (found.data_type, found.note, None),
            Err(why) => (String::new(), None, Some(why.0)),
        };
        Self {
            source: column.clone(),
            target_name: column.name.clone(),
            target_type,
            skip: generated,
            note: if generated {
                Some("generated column: computed, not copied".into())
            } else {
                note
            },
            unmapped,
        }
    }

    /// Why this column keeps the copy from starting: it is copied and has
    /// no target type.
    pub fn problem(&self) -> Option<String> {
        (!self.skip && self.target_type.trim().is_empty()).then(|| {
            self.unmapped
                .clone()
                .unwrap_or_else(|| format!("`{}` needs a target type", self.source.name))
        })
    }

    /// Whether the column numbers its rows on the source.
    fn numbers_rows(&self) -> bool {
        matches!(
            self.source.generation,
            Some(
                ColumnGeneration::IdentityAlways
                    | ColumnGeneration::IdentityByDefault
                    | ColumnGeneration::AutoIncrement
            )
        )
    }
}

/// One table to copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableTransfer {
    /// The source table.
    pub source: TableRef,
    /// Its name on the target, in the plan's target schema.
    pub target_name: String,
    /// Its columns, in table order.
    pub columns: Vec<ColumnMapping>,
    /// What the copy does not bring (indexes, foreign keys, checks,
    /// triggers, defaults), for the report.
    pub not_copied: Vec<String>,
    /// The tables of the source it references, for the order of creation.
    pub references: Vec<TableRef>,
}

impl TableTransfer {
    /// The default transfer of `source`, whose structure is `structure`,
    /// from a `from` server to a `to` server.
    pub fn new(from: Engine, to: Engine, source: TableRef, structure: &TableStructure) -> Self {
        let mut not_copied: Vec<String> = structure
            .indexes
            .iter()
            .filter(|index| !index.primary)
            .map(|index| format!("index {}", index.name))
            .collect();
        not_copied.extend(
            structure
                .foreign_keys
                .iter()
                .map(|key| format!("foreign key {}", key.name)),
        );
        not_copied.extend(
            structure
                .constraints
                .iter()
                .filter(|c| matches!(c.kind, ConstraintKind::Check | ConstraintKind::Exclusion))
                .map(|c| format!("constraint {}", c.name)),
        );
        not_copied.extend(
            structure
                .triggers
                .iter()
                .map(|trigger| format!("trigger {}", trigger.name)),
        );
        if structure
            .columns
            .iter()
            .any(|column| column.default.is_some() && column.generation.is_none())
        {
            not_copied.push("column defaults".into());
        }
        Self {
            target_name: source.name.clone(),
            source,
            columns: structure
                .columns
                .iter()
                .map(|column| ColumnMapping::new(from, to, column))
                .collect(),
            not_copied,
            references: structure
                .foreign_keys
                .iter()
                .map(|key| key.referenced_table.clone())
                .collect(),
        }
    }

    /// The columns copied.
    pub fn copied(&self) -> impl Iterator<Item = &ColumnMapping> {
        self.columns.iter().filter(|column| !column.skip)
    }

    /// The target table, in `schema`.
    pub fn target(&self, schema: &str) -> TableRef {
        TableRef::new(schema, self.target_name.trim())
    }

    /// The primary key on the target: the source's, when every column of
    /// it is copied; none otherwise.
    fn primary_key(&self) -> Vec<String> {
        let mut key: Vec<(u32, &ColumnMapping)> = self
            .columns
            .iter()
            .filter_map(|c| c.source.primary_key_position.map(|p| (p, c)))
            .collect();
        if key.iter().any(|(_, column)| column.skip) {
            return Vec::new();
        }
        key.sort_by_key(|(position, _)| *position);
        key.into_iter()
            .map(|(_, column)| column.target_name.trim().to_string())
            .collect()
    }
}

/// What to do with the target tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransferMode {
    /// Create each table, then fill it.
    #[default]
    Create,
    /// Add the rows to tables that exist.
    Append,
}

/// What a copy in [`TransferMode::Create`] does with a table that exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IfExists {
    /// Stop the copy there.
    #[default]
    Stop,
    /// Add the rows to it.
    Append,
    /// Leave it, and copy the next table.
    Skip,
}

/// What a table that fails does to the copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnError {
    /// The tables after it are not copied.
    #[default]
    Stop,
    /// The copy goes on with the next table.
    SkipTable,
}

/// A whole copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferPlan {
    /// The tables, in the order they are copied.
    pub tables: Vec<TableTransfer>,
    /// The target's schema (its database, on MySQL and MariaDB).
    pub target_schema: String,
    pub mode: TransferMode,
    pub if_exists: IfExists,
    /// Rows per `INSERT` and per transaction.
    pub batch_rows: usize,
    pub on_error: OnError,
}

impl TransferPlan {
    /// A plan with the defaults, the tables ordered by their foreign keys.
    pub fn new(tables: Vec<TableTransfer>, target_schema: impl Into<String>) -> Self {
        Self {
            tables: order_by_foreign_keys(tables),
            target_schema: target_schema.into(),
            mode: TransferMode::default(),
            if_exists: IfExists::default(),
            batch_rows: DEFAULT_BATCH_ROWS,
            on_error: OnError::default(),
        }
    }

    /// What keeps the copy from starting, in words; empty when it can.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.tables.is_empty() {
            problems.push("Tick at least one table.".into());
        }
        if self.target_schema.trim().is_empty() {
            problems.push("Choose the target schema.".into());
        }
        if self.batch_rows == 0 {
            problems.push("A batch holds at least one row.".into());
        }
        let mut targets = HashSet::new();
        for table in &self.tables {
            let name = table.target_name.trim();
            if name.is_empty() {
                problems.push(format!("{} needs a target name.", table.source.name));
            } else if !targets.insert(name.to_lowercase()) {
                problems.push(format!("Two tables go into {name}."));
            }
            if table.copied().next().is_none() {
                problems.push(format!("{}: every column is skipped.", table.source.name));
            }
            let mut columns = HashSet::new();
            for column in table.copied() {
                let target = column.target_name.trim();
                if let Some(problem) = column.problem() {
                    problems.push(format!("{}: {problem}.", table.source.name));
                } else if target.is_empty() {
                    problems.push(format!(
                        "{}: `{}` needs a target name.",
                        table.source.name, column.source.name
                    ));
                } else if !columns.insert(target.to_lowercase()) {
                    problems.push(format!(
                        "{}: two columns go into `{target}`.",
                        table.source.name
                    ));
                }
            }
        }
        problems
    }
}

/// `tables` in an order where a table comes after the tables it references
/// among them; the order is kept otherwise, and tables in a cycle keep
/// theirs at the end.
pub fn order_by_foreign_keys(tables: Vec<TableTransfer>) -> Vec<TableTransfer> {
    let picked: HashSet<&TableRef> = tables.iter().map(|table| &table.source).collect();
    let mut pending: Vec<usize> = (0..tables.len()).collect();
    let mut placed: HashSet<TableRef> = HashSet::new();
    let mut order = Vec::with_capacity(tables.len());
    loop {
        let ready = pending.iter().position(|&index| {
            let table = &tables[index];
            table.references.iter().all(|referenced| {
                referenced == &table.source
                    || !picked.contains(referenced)
                    || placed.contains(referenced)
            })
        });
        let Some(position) = ready else { break };
        let index = pending.remove(position);
        placed.insert(tables[index].source.clone());
        order.push(index);
    }
    order.extend(pending);
    let mut slots: Vec<Option<TableTransfer>> = tables.into_iter().map(Some).collect();
    order
        .into_iter()
        .filter_map(|index| slots[index].take())
        .collect()
}

/// The statements that create `table` in `schema` on the target: its copied
/// columns with their target types and NULL rules, and the primary key when
/// every column of it is copied. A column that numbers its rows keeps doing
/// so (`GENERATED BY DEFAULT AS IDENTITY`, `AUTO_INCREMENT`), so the copied
/// values can be written; on MySQL and MariaDB only the first column of the
/// key can. Nothing else is created (see [`TableTransfer::not_copied`]).
pub fn create_statements(
    table: &TableTransfer,
    schema: &str,
    dialect: &dyn Dialect,
) -> Result<Vec<String>> {
    dialect.create_table_sql(&draft(table, schema, dialect.engine()))
}

fn draft(table: &TableTransfer, schema: &str, to: Engine) -> TableDraft {
    TableDraft {
        table: table.target(schema),
        comment: None,
        columns: table
            .copied()
            .map(|column| ColumnDraft {
                original: None,
                name: column.target_name.trim().to_string(),
                data_type: column.target_type.trim().to_string(),
                nullable: column.source.nullable,
                default: None,
                comment: None,
                auto_increment: if numbers_rows_on(column, to) {
                    AutoIncrement::On
                } else {
                    AutoIncrement::Off
                },
                on_update: None,
                invisible: false,
            })
            .collect(),
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        primary_key: table.primary_key(),
    }
}

/// Whether the created column numbers its rows on a `to` server.
fn numbers_rows_on(column: &ColumnMapping, to: Engine) -> bool {
    column.numbers_rows()
        && (to == Engine::Postgres || column.source.primary_key_position == Some(1))
}

/// `INSERT INTO schema.table (columns) VALUES `: what every batch starts
/// with.
pub fn insert_head(table: &TableTransfer, schema: &str, dialect: &dyn Dialect) -> String {
    let columns: Vec<String> = table
        .copied()
        .map(|column| dialect.quote_ident(column.target_name.trim()))
        .collect();
    format!(
        "INSERT INTO {} ({}) VALUES ",
        dialect.table_name(&table.target(schema)),
        columns.join(", ")
    )
}

/// The statements that move a PostgreSQL identity past the copied values,
/// so the next row the target numbers does not collide with them.
pub fn sequence_statements(
    table: &TableTransfer,
    schema: &str,
    dialect: &dyn Dialect,
) -> Vec<String> {
    if dialect.engine() != Engine::Postgres {
        return Vec::new();
    }
    let target = dialect.table_name(&table.target(schema));
    table
        .copied()
        .filter(|column| numbers_rows_on(column, Engine::Postgres))
        .map(|column| {
            let name = column.target_name.trim();
            format!(
                "SELECT setval(pg_get_serial_sequence({}, {}), COALESCE(MAX({}), 0) + 1, false) \
                 FROM {target}",
                dialect.quote_string(&target),
                dialect.quote_string(name),
                dialect.quote_ident(name)
            )
        })
        .collect()
}

/// Every statement the copy runs on the target, as the confirmation shows
/// it: the `CREATE TABLE`s of the tables that do not exist (`existing`
/// holds the target's table names), one `INSERT` with its rows elided, and
/// the identity updates.
pub fn statements(
    plan: &TransferPlan,
    dialect: &dyn Dialect,
    existing: &[String],
) -> Result<Vec<String>> {
    let exists = |table: &TableTransfer| {
        existing
            .iter()
            .any(|name| name.eq_ignore_ascii_case(table.target_name.trim()))
    };
    let mut sql = Vec::new();
    let mut identities = Vec::new();
    let mut sample = None;
    for table in &plan.tables {
        let creates = plan.mode == TransferMode::Create && !exists(table);
        let written =
            creates || plan.mode == TransferMode::Append || plan.if_exists == IfExists::Append;
        if creates {
            sql.extend(create_statements(table, &plan.target_schema, dialect)?);
            identities.extend(sequence_statements(table, &plan.target_schema, dialect));
        }
        if written && sample.is_none() {
            // The rows elided; the confirmation says how many per batch.
            sample = Some(format!(
                "{}(…), (…)",
                insert_head(table, &plan.target_schema, dialect)
            ));
        }
    }
    sql.extend(sample);
    sql.extend(identities);
    Ok(sql)
}

/// What the copy reports while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferEvent {
    /// A table starts; `estimate` is the source's row estimate, if any.
    Table { index: usize, estimate: Option<u64> },
    /// Rows read from the source and written to the target so far.
    Rows {
        index: usize,
        read: u64,
        written: u64,
    },
    /// A line for the log.
    Log(String),
}

/// How a table ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TableStatus {
    /// Its turn did not come (an earlier table failed, or the copy ended).
    #[default]
    NotStarted,
    /// Every row is on the target.
    Copied,
    /// Left as it is, and why.
    Skipped(String),
    /// Stopped by an error. The batches written before the failing one
    /// stay: each batch is its own transaction.
    Failed {
        error: String,
        /// The first row of the failing batch, as its `INSERT` writes it.
        first_row: Option<String>,
    },
    /// The user stopped the copy during this table.
    Cancelled,
}

/// One table of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableOutcome {
    pub source: TableRef,
    pub target: TableRef,
    pub read: u64,
    pub written: u64,
    pub elapsed: Duration,
    pub status: TableStatus,
    /// What was not copied with it.
    pub not_copied: Vec<String>,
}

/// How a copy ended.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransferReport {
    pub tables: Vec<TableOutcome>,
}

impl TransferReport {
    /// Whether every table was copied.
    pub fn succeeded(&self) -> bool {
        self.tables
            .iter()
            .all(|table| matches!(table.status, TableStatus::Copied | TableStatus::Skipped(_)))
    }

    /// Rows written, every table together.
    pub fn written(&self) -> u64 {
        self.tables.iter().map(|table| table.written).sum()
    }

    /// The report as text, one paragraph per table, to copy.
    pub fn text(&self) -> String {
        let mut lines = Vec::new();
        for table in &self.tables {
            let seconds = table.elapsed.as_secs_f64();
            let rate = if seconds > 0.0 && table.written > 0 {
                format!(", {} rows/s", (table.written as f64 / seconds).round())
            } else {
                String::new()
            };
            let status = match &table.status {
                TableStatus::NotStarted => "not started".to_string(),
                TableStatus::Copied => "copied".to_string(),
                TableStatus::Skipped(why) => format!("skipped: {why}"),
                // The server's detail and hint go on lines of their own (C14).
                TableStatus::Failed { error, .. } => {
                    format!("failed: {}", error.lines().next().unwrap_or_default())
                }
                TableStatus::Cancelled => "cancelled".to_string(),
            };
            lines.push(format!(
                "{}.{} → {}.{}: {status}. {} rows read, {} written in {seconds:.1} s{rate}.",
                table.source.schema,
                table.source.name,
                table.target.schema,
                table.target.name,
                table.read,
                table.written,
            ));
            if let TableStatus::Failed { error, first_row } = &table.status {
                lines.extend(error.lines().skip(1).map(|line| format!("  {line}")));
                if let Some(row) = first_row {
                    lines.push(format!("  First row of the failing batch: {row}"));
                }
            }
            if !table.not_copied.is_empty() {
                lines.push(format!("  Not copied: {}.", table.not_copied.join(", ")));
            }
        }
        lines.join("\n")
    }
}

/// Copies the tables of `plan` from `source` to `target`, table after
/// table, and says how each one ended.
///
/// Per table: in [`TransferMode::Create`] the table is created (or, when it
/// exists, the plan's [`IfExists`] applies); in [`TransferMode::Append`] it
/// must exist and have every target column. The source rows are then read
/// with back-pressure and written `batch_rows` at a time, each batch one
/// `INSERT` in its own transaction, rolled back when it fails: the report
/// gives the error and the batch's first row, and the batches written
/// before it stay. A created PostgreSQL identity is moved past the copied
/// values at the end. `cancel` stops between two batches (and the source's
/// statement with it).
///
/// `source` and `target` must be two sessions (one runs a statement at a
/// time). Fails without writing when the plan has a problem
/// ([`TransferPlan::problems`]), when the target session is in a
/// transaction, or when the target schema cannot be read. Must be called
/// from within the Tokio runtime.
pub async fn run(
    source: Arc<dyn Session>,
    target: Arc<dyn Session>,
    plan: TransferPlan,
    progress: impl Fn(TransferEvent) + Send + Sync + 'static,
    cancel: Arc<AtomicBool>,
) -> Result<TransferReport> {
    if let Some(problem) = plan.problems().into_iter().next() {
        return Err(Error::Config(problem));
    }
    if target.tx_state() != TxState::Idle {
        return Err(Error::Config(
            "a transaction is already open in the target session".into(),
        ));
    }
    let existing: Vec<String> = target
        .objects(&plan.target_schema)
        .await?
        .relations
        .into_iter()
        .map(|relation| relation.name)
        .collect();
    let mut report = TransferReport {
        tables: plan
            .tables
            .iter()
            .map(|table| TableOutcome {
                source: table.source.clone(),
                target: table.target(&plan.target_schema),
                read: 0,
                written: 0,
                elapsed: Duration::ZERO,
                status: TableStatus::NotStarted,
                not_copied: table.not_copied.clone(),
            })
            .collect(),
    };
    let job = Job {
        source,
        target,
        plan: &plan,
        existing,
        progress: &progress,
        cancel: &cancel,
    };
    for (index, table) in plan.tables.iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let started = Instant::now();
        let outcome = &mut report.tables[index];
        let (status, stop) = job.copy(index, table, outcome).await;
        outcome.status = status;
        outcome.elapsed = started.elapsed();
        let name = &table.source.name;
        progress(TransferEvent::Log(match &outcome.status {
            TableStatus::Copied => format!(
                "{name}: {} rows copied in {:.1} s.",
                outcome.written,
                outcome.elapsed.as_secs_f64()
            ),
            TableStatus::Skipped(why) => format!("{name}: skipped, {why}."),
            TableStatus::Failed { error, .. } => {
                let (first, more) = error.split_once('\n').unwrap_or((error, ""));
                let more = more.lines().map(|line| format!("\n  {line}"));
                format!("{name}: failed, {first}.{}", more.collect::<String>())
            }
            TableStatus::Cancelled => format!("{name}: cancelled."),
            TableStatus::NotStarted => format!("{name}: not started."),
        }));
        let failed = matches!(outcome.status, TableStatus::Failed { .. });
        if stop || (failed && plan.on_error == OnError::Stop) {
            break;
        }
    }
    Ok(report)
}

/// What every table of a run shares.
struct Job<'a, P> {
    source: Arc<dyn Session>,
    target: Arc<dyn Session>,
    plan: &'a TransferPlan,
    /// The target schema's table names when the copy started.
    existing: Vec<String>,
    progress: &'a P,
    cancel: &'a Arc<AtomicBool>,
}

fn failed(error: impl Into<String>) -> TableStatus {
    TableStatus::Failed {
        error: error.into(),
        first_row: None,
    }
}

/// What went wrong: the server's message and code, then its detail and
/// hint on lines of their own (C14).
fn message(error: &Error) -> String {
    crate::error::describe(error).to_string()
}

async fn run_sql(session: &Arc<dyn Session>, sql: &str) -> Result<()> {
    exec::collect(session.clone(), sql).await.map(|_| ())
}

impl<P: Fn(TransferEvent)> Job<'_, P> {
    /// Copies one table: how it ended, and whether the copy stops there
    /// whatever [`OnError`] says (a table that exists under
    /// [`IfExists::Stop`], a cancel).
    async fn copy(
        &self,
        index: usize,
        table: &TableTransfer,
        outcome: &mut TableOutcome,
    ) -> (TableStatus, bool) {
        let plan = self.plan;
        let dialect = self.target.dialect();
        let target = table.target(&plan.target_schema);
        let exists = self
            .existing
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&target.name));
        let creates = match (plan.mode, exists, plan.if_exists) {
            (TransferMode::Append, false, _) => {
                return (failed("the table does not exist on the target"), false);
            }
            (TransferMode::Create, true, IfExists::Stop) => {
                return (failed("the table exists on the target"), true);
            }
            (TransferMode::Create, true, IfExists::Skip) => {
                return (
                    TableStatus::Skipped("the table exists on the target".into()),
                    false,
                );
            }
            (TransferMode::Create, false, _) => true,
            (TransferMode::Append, true, _) | (TransferMode::Create, true, IfExists::Append) => {
                false
            }
        };
        let estimate = self.estimate(&table.source).await;
        (self.progress)(TransferEvent::Table { index, estimate });
        if creates {
            (self.progress)(TransferEvent::Log(format!(
                "Creating {}…",
                dialect.table_name(&target)
            )));
            let created = match create_statements(table, &plan.target_schema, dialect) {
                Ok(statements) => statements,
                Err(error) => return (failed(message(&error)), false),
            };
            for sql in &created {
                if let Err(error) = run_sql(&self.target, sql).await {
                    return (failed(message(&error)), false);
                }
            }
        }
        // The target's own columns decide how each value is written.
        let columns = match self.target.table_structure(&target).await {
            Ok(structure) => structure.columns,
            Err(error) => return (failed(message(&error)), false),
        };
        let mut targets = Vec::new();
        for column in table.copied() {
            let name = column.target_name.trim();
            let found = columns
                .iter()
                .find(|c| c.name == name)
                .or_else(|| columns.iter().find(|c| c.name.eq_ignore_ascii_case(name)));
            match found {
                Some(found) => targets.push(found.clone()),
                None => {
                    return (
                        failed(format!("the target table has no column `{name}`")),
                        false,
                    );
                }
            }
        }
        let head = insert_head(table, &plan.target_schema, dialect);
        let select = select_sql(table, self.source.dialect(), dialect.engine());
        // Dropping the execution (a return below) stops the source's
        // statement on the server.
        let mut execution = exec::run(
            self.source.clone(),
            select,
            RowLimit::Rows(plan.batch_rows as u64),
        );
        let mut binary: Vec<bool> = Vec::new();
        let mut rows: Vec<String> = Vec::with_capacity(plan.batch_rows);
        while let Some(event) = execution.next().await {
            if self.cancel.load(Ordering::Acquire) {
                return (TableStatus::Cancelled, true);
            }
            match event {
                ExecEvent::Columns(read) => {
                    binary = read
                        .iter()
                        .map(|column| column.kind == TypeKind::Binary)
                        .collect();
                }
                ExecEvent::Rows(batch) => {
                    for row in 0..batch.row_count() {
                        rows.push(tuple(&batch, row, &binary, &targets, dialect));
                        outcome.read += 1;
                        if rows.len() >= plan.batch_rows
                            && let Some(ended) = self.write(&head, &mut rows, index, outcome).await
                        {
                            return ended;
                        }
                    }
                    execution.request_rows(batch.row_count() as u64);
                }
                ExecEvent::Finished(Err(error)) => {
                    return (
                        failed(format!("reading the source: {}", message(&error))),
                        false,
                    );
                }
                _ => {}
            }
        }
        if !rows.is_empty()
            && let Some(ended) = self.write(&head, &mut rows, index, outcome).await
        {
            return ended;
        }
        if creates {
            for sql in sequence_statements(table, &plan.target_schema, dialect) {
                if let Err(error) = run_sql(&self.target, &sql).await {
                    return (failed(message(&error)), false);
                }
            }
        }
        (TableStatus::Copied, false)
    }

    /// Writes `rows` in one transaction and empties them; how the table
    /// ended when it cannot go on.
    async fn write(
        &self,
        head: &str,
        rows: &mut Vec<String>,
        index: usize,
        outcome: &mut TableOutcome,
    ) -> Option<(TableStatus, bool)> {
        if self.cancel.load(Ordering::Acquire) {
            return Some((TableStatus::Cancelled, true));
        }
        let insert = format!("{head}{}", rows.join(", "));
        let written = async {
            run_sql(&self.target, "START TRANSACTION").await?;
            run_sql(&self.target, &insert).await?;
            run_sql(&self.target, "COMMIT").await
        }
        .await;
        match written {
            Ok(()) => {
                outcome.written += rows.len() as u64;
                rows.clear();
                (self.progress)(TransferEvent::Rows {
                    index,
                    read: outcome.read,
                    written: outcome.written,
                });
                None
            }
            Err(error) => {
                if let Err(error) = run_sql(&self.target, "ROLLBACK").await {
                    tracing::warn!(%error, "rollback after a failed batch did not succeed");
                }
                if self.cancel.load(Ordering::Acquire) {
                    return Some((TableStatus::Cancelled, true));
                }
                let first_row = rows
                    .first()
                    .map(|row| row.chars().take(FIRST_ROW_CHARS).collect());
                Some((
                    TableStatus::Failed {
                        error: message(&error),
                        first_row,
                    },
                    false,
                ))
            }
        }
    }

    /// The source's row estimate of `table`, when it has one.
    async fn estimate(&self, table: &TableRef) -> Option<u64> {
        let sql = self.source.dialect().estimated_rows_sql(table);
        let collected = exec::collect(self.source.clone(), sql).await.ok()?;
        let text = collected.results.last()?.rows().first()?.first()?.clone()?;
        text.parse::<f64>()
            .ok()
            .filter(|rows| *rows > 0.0)
            .map(|rows| rows as u64)
    }
}

/// The `SELECT` that reads the copied columns of `table`, each in a form the
/// `to` server reads: a PostgreSQL timestamp with zone in UTC, money as a
/// number.
fn select_sql(table: &TableTransfer, source: &dyn Dialect, to: Engine) -> String {
    let crossing = source.engine() == Engine::Postgres && to != Engine::Postgres;
    let columns: Vec<String> = table
        .copied()
        .map(|column| {
            let name = source.quote_ident(&column.source.name);
            match column.source.kind {
                TypeKind::DateTimeTz if crossing => format!("({name} AT TIME ZONE 'UTC')"),
                TypeKind::Decimal
                    if crossing && split_type(&column.source.data_type).0 == "money" =>
                {
                    format!("({name})::numeric")
                }
                _ => name,
            }
        })
        .collect();
    format!(
        "SELECT {} FROM {}",
        columns.join(", "),
        source.table_name(&table.source)
    )
}

/// `(v1, v2, …)`: one row of `batch` as the target writes it, each value a
/// literal of the target's column.
fn tuple(
    batch: &RowBatch,
    row: usize,
    binary: &[bool],
    targets: &[ColumnInfo],
    dialect: &dyn Dialect,
) -> String {
    let mut tuple = String::from("(");
    for (column, target) in targets.iter().enumerate() {
        if column > 0 {
            tuple.push_str(", ");
        }
        let value = match batch.cell(row, column) {
            None => CellValue::Null,
            Some(bytes) if binary.get(column) == Some(&true) => CellValue::Bytes(bytes.to_vec()),
            Some(bytes) => CellValue::Text(String::from_utf8_lossy(bytes).into_owned()),
        };
        tuple.push_str(&dialect.literal(&value, target));
    }
    tuple.push(')');
    tuple
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ForeignKeyInfo, IndexInfo};

    fn column(name: &str, data_type: &str, kind: TypeKind) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            kind,
            nullable: true,
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: None,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn map(from: Engine, to: Engine, data_type: &str, kind: TypeKind) -> Result<String, String> {
        map_type(from, to, &column("c", data_type, kind))
            .map(|found| found.data_type)
            .map_err(|why| why.0)
    }

    #[test]
    fn types_are_split_into_name_arguments_and_sign() {
        assert_eq!(
            split_type("timestamp(3) with time zone"),
            ("timestamp with time zone".into(), vec![3], false)
        );
        assert_eq!(
            split_type("decimal(10,2) unsigned zerofill"),
            ("decimal".into(), vec![10, 2], true)
        );
        assert_eq!(
            split_type("character varying"),
            ("character varying".into(), vec![], false)
        );
        assert_eq!(
            split_type("BIGINT UNSIGNED"),
            ("bigint".into(), vec![], true)
        );
    }

    /// DBeaver #17767: the common types, PostgreSQL to MySQL and MariaDB.
    #[test]
    fn postgres_types_map_to_mysql_and_mariadb() {
        use Engine::{MariaDb, MySql, Postgres};
        let cases: [(&str, TypeKind, &str); 24] = [
            ("smallint", TypeKind::Integer, "smallint"),
            ("integer", TypeKind::Integer, "int"),
            ("bigint", TypeKind::Integer, "bigint"),
            ("oid", TypeKind::Integer, "int unsigned"),
            ("numeric(20,6)", TypeKind::Decimal, "decimal(20,6)"),
            ("numeric(12)", TypeKind::Decimal, "decimal(12,0)"),
            ("numeric", TypeKind::Decimal, "decimal(65,30)"),
            ("money", TypeKind::Decimal, "decimal(19,2)"),
            ("real", TypeKind::Float, "float"),
            ("double precision", TypeKind::Float, "double"),
            ("boolean", TypeKind::Boolean, "tinyint(1)"),
            ("character varying(50)", TypeKind::Text, "varchar(50)"),
            ("character varying", TypeKind::Text, "longtext"),
            ("character varying(100000)", TypeKind::Text, "longtext"),
            ("character(5)", TypeKind::Text, "char(5)"),
            ("text", TypeKind::Text, "longtext"),
            ("bytea", TypeKind::Binary, "longblob"),
            ("date", TypeKind::Date, "date"),
            ("time without time zone", TypeKind::Time, "time(6)"),
            ("time(3) without time zone", TypeKind::Time, "time(3)"),
            (
                "timestamp without time zone",
                TypeKind::DateTime,
                "datetime(6)",
            ),
            (
                "timestamp(0) with time zone",
                TypeKind::DateTimeTz,
                "datetime(0)",
            ),
            ("jsonb", TypeKind::Json, "json"),
            ("bit(8)", TypeKind::Bit, "bit(8)"),
        ];
        for (data_type, kind, expected) in cases {
            for to in [MySql, MariaDb] {
                assert_eq!(
                    map(Postgres, to, data_type, kind),
                    Ok(expected.into()),
                    "{data_type}"
                );
            }
        }
        assert_eq!(
            map(Postgres, MySql, "uuid", TypeKind::Uuid),
            Ok("char(36)".into())
        );
        assert_eq!(
            map(Postgres, MariaDb, "uuid", TypeKind::Uuid),
            Ok("uuid".into())
        );
        // An enum becomes text as long as its longest label, with a note.
        let mut mood = column("mood", "mood", TypeKind::Enum);
        mood.enum_values = vec!["sad".into(), "ok".into(), "happy".into()];
        let found = map_type(Postgres, MySql, &mood).unwrap();
        assert_eq!(found.data_type, "varchar(5)");
        assert!(found.note.unwrap().contains("sad, ok, happy"));
        // A timestamp with zone is converted to UTC, and says so.
        let tz = column("at", "timestamp with time zone", TypeKind::DateTimeTz);
        assert!(
            map_type(Postgres, MySql, &tz)
                .unwrap()
                .note
                .unwrap()
                .contains("UTC")
        );
    }

    #[test]
    fn mysql_and_mariadb_types_map_to_postgres() {
        use Engine::{MariaDb, MySql, Postgres};
        let cases: [(&str, TypeKind, &str); 22] = [
            ("tinyint", TypeKind::Integer, "smallint"),
            ("tinyint(1)", TypeKind::Boolean, "boolean"),
            ("smallint", TypeKind::Integer, "smallint"),
            ("smallint unsigned", TypeKind::Integer, "integer"),
            ("mediumint", TypeKind::Integer, "integer"),
            ("int", TypeKind::Integer, "integer"),
            ("int unsigned", TypeKind::Integer, "bigint"),
            ("bigint", TypeKind::Integer, "bigint"),
            ("bigint unsigned", TypeKind::Integer, "numeric(20,0)"),
            ("year", TypeKind::Integer, "smallint"),
            ("decimal(10,2)", TypeKind::Decimal, "numeric(10,2)"),
            ("float", TypeKind::Float, "real"),
            ("double", TypeKind::Float, "double precision"),
            ("bit(4)", TypeKind::Bit, "bit(4)"),
            ("char(3)", TypeKind::Text, "char(3)"),
            ("varchar(190)", TypeKind::Text, "varchar(190)"),
            ("mediumtext", TypeKind::Text, "text"),
            ("varbinary(16)", TypeKind::Binary, "bytea"),
            ("time(3)", TypeKind::Time, "time(3)"),
            ("datetime(6)", TypeKind::DateTime, "timestamp(6)"),
            ("timestamp", TypeKind::DateTime, "timestamp(0)"),
            ("json", TypeKind::Json, "jsonb"),
        ];
        for (data_type, kind, expected) in cases {
            for from in [MySql, MariaDb] {
                assert_eq!(
                    map(from, Postgres, data_type, kind),
                    Ok(expected.into()),
                    "{data_type}"
                );
            }
        }
        assert_eq!(
            map(MariaDb, Postgres, "uuid", TypeKind::Uuid),
            Ok("uuid".into())
        );
        assert_eq!(
            map(MySql, Postgres, "set('a','b')", TypeKind::Enum),
            Ok("text".into())
        );
        // Between MySQL and MariaDB the type is kept, MariaDB's own aside.
        assert_eq!(
            map(MySql, MariaDb, "int unsigned", TypeKind::Integer),
            Ok("int unsigned".into())
        );
        assert_eq!(
            map(MariaDb, MySql, "uuid", TypeKind::Uuid),
            Ok("char(36)".into())
        );
    }

    #[test]
    fn the_same_engine_keeps_the_type() {
        for engine in Engine::ALL {
            assert_eq!(
                map(engine, engine, "text[]", TypeKind::Array),
                Ok("text[]".into())
            );
            assert_eq!(
                map(engine, engine, "numeric(20,6)", TypeKind::Decimal),
                Ok("numeric(20,6)".into())
            );
        }
    }

    /// What has no counterpart is refused, with the column named, never
    /// guessed.
    #[test]
    fn types_without_a_counterpart_are_refused_clearly() {
        use Engine::{MySql, Postgres};
        for (data_type, kind) in [
            ("text[]", TypeKind::Array),
            ("int4range", TypeKind::Other),
            ("point", TypeKind::Other),
            ("inet", TypeKind::Other),
            ("interval", TypeKind::Interval),
            ("time with time zone", TypeKind::Time),
            ("shop.address", TypeKind::Other),
            ("bit varying(16)", TypeKind::Bit),
        ] {
            let why = map_type(Postgres, MySql, &column("tags", data_type, kind)).unwrap_err();
            assert_eq!(
                why.0,
                format!(
                    "`tags {data_type}` has no MySQL equivalent: skip the column or type a target type"
                )
            );
        }
        for (data_type, kind) in [("geometry", TypeKind::Binary), ("inet6", TypeKind::Other)] {
            assert!(
                map(MySql, Postgres, data_type, kind).is_err(),
                "{data_type}"
            );
        }
    }

    /// A text or binary column of the primary key gets a length on MySQL.
    #[test]
    fn a_long_key_column_gets_a_length_on_mysql() {
        let mut key = column("code", "text", TypeKind::Text);
        key.primary_key_position = Some(1);
        let found = map_type(Engine::Postgres, Engine::MySql, &key).unwrap();
        assert_eq!(found.data_type, "varchar(255)");
        assert!(found.note.is_some());
    }

    #[test]
    fn an_unmapped_column_blocks_until_skipped_or_typed() {
        let table = TableTransfer::new(
            Engine::Postgres,
            Engine::MySql,
            TableRef::new("s", "t"),
            &TableStructure {
                columns: vec![
                    column("id", "integer", TypeKind::Integer),
                    column("tags", "text[]", TypeKind::Array),
                ],
                ..TableStructure::default()
            },
        );
        let mut plan = TransferPlan::new(vec![table], "target");
        assert_eq!(
            plan.problems(),
            ["t: `tags text[]` has no MySQL equivalent: skip the column or type a target type."]
        );
        plan.tables[0].columns[1].skip = true;
        assert!(plan.problems().is_empty());
        plan.tables[0].columns[1].skip = false;
        plan.tables[0].columns[1].target_type = "json".into();
        assert!(plan.problems().is_empty());
        // Names that collide and a table without columns are problems too.
        plan.tables[0].columns[1].target_name = "ID".into();
        assert_eq!(plan.problems(), ["t: two columns go into `ID`."]);
        plan.tables[0]
            .columns
            .iter_mut()
            .for_each(|c| c.skip = true);
        assert_eq!(plan.problems(), ["t: every column is skipped."]);
    }

    fn referencing(name: &str, references: &[&str]) -> TableTransfer {
        TableTransfer {
            source: TableRef::new("s", name),
            target_name: name.into(),
            columns: vec![ColumnMapping::new(
                Engine::Postgres,
                Engine::Postgres,
                &column("id", "integer", TypeKind::Integer),
            )],
            not_copied: Vec::new(),
            references: references.iter().map(|r| TableRef::new("s", *r)).collect(),
        }
    }

    #[test]
    fn tables_are_ordered_by_their_foreign_keys() {
        let names = |tables: Vec<TableTransfer>| -> Vec<String> {
            order_by_foreign_keys(tables)
                .into_iter()
                .map(|t| t.target_name)
                .collect()
        };
        // lines → orders → customers; products alone; a self reference and a
        // table outside the selection do not hold anything back.
        assert_eq!(
            names(vec![
                referencing("lines", &["orders", "products"]),
                referencing("orders", &["customers", "orders"]),
                referencing("customers", &["regions"]),
                referencing("products", &[]),
            ]),
            ["customers", "orders", "products", "lines"]
        );
        // A cycle keeps its order, at the end.
        assert_eq!(
            names(vec![
                referencing("a", &["b"]),
                referencing("b", &["a"]),
                referencing("c", &[]),
            ]),
            ["c", "a", "b"]
        );
    }

    fn orders() -> TableStructure {
        let mut id = column("id", "integer", TypeKind::Integer);
        id.primary_key_position = Some(1);
        id.nullable = false;
        id.generation = Some(ColumnGeneration::IdentityAlways);
        let mut total = column("total", "numeric(10,2)", TypeKind::Decimal);
        total.default = Some("0".into());
        let mut doubled = column("doubled", "numeric", TypeKind::Decimal);
        doubled.generation = Some(ColumnGeneration::Stored("total * 2".into()));
        TableStructure {
            columns: vec![id, total, doubled],
            indexes: vec![
                IndexInfo {
                    name: "orders_pkey".into(),
                    columns: vec!["id".into()],
                    unique: true,
                    primary: true,
                    backs_constraint: true,
                    method: "btree".into(),
                    predicate: None,
                    definition: None,
                },
                IndexInfo {
                    name: "orders_total".into(),
                    columns: vec!["total".into()],
                    unique: false,
                    primary: false,
                    backs_constraint: false,
                    method: "btree".into(),
                    predicate: None,
                    definition: None,
                },
            ],
            foreign_keys: vec![ForeignKeyInfo {
                name: "orders_customer".into(),
                table: TableRef::new("shop", "orders"),
                columns: vec!["customer".into()],
                referenced_table: TableRef::new("shop", "customers"),
                referenced_columns: vec!["id".into()],
                on_update: "NO ACTION".into(),
                on_delete: "NO ACTION".into(),
            }],
            ..TableStructure::default()
        }
    }

    #[test]
    fn a_created_table_keeps_its_key_and_numbering_and_lists_what_is_left() {
        let table = TableTransfer::new(
            Engine::Postgres,
            Engine::Postgres,
            TableRef::new("shop", "orders"),
            &orders(),
        );
        // The generated column is skipped; the rest says what is not copied.
        assert!(table.columns[2].skip);
        assert_eq!(
            table.not_copied,
            [
                "index orders_total",
                "foreign key orders_customer",
                "column defaults"
            ]
        );
        assert_eq!(table.references, [TableRef::new("shop", "customers")]);
        let dialect = crate::dialect::test_support::AnsiDialect;
        assert_eq!(
            create_statements(&table, "copy", &dialect).unwrap(),
            [
                "CREATE TABLE \"copy\".\"orders\" (\n    \"id\" integer GENERATED BY DEFAULT AS \
              IDENTITY NOT NULL,\n    \"total\" numeric(10,2),\n    PRIMARY KEY (\"id\")\n)"
            ]
        );
        assert_eq!(
            insert_head(&table, "copy", &dialect),
            "INSERT INTO \"copy\".\"orders\" (\"id\", \"total\") VALUES "
        );
        // A key column skipped: no key.
        let mut keyless = table.clone();
        keyless.columns[0].skip = true;
        assert!(keyless.primary_key().is_empty());
    }

    /// What the confirmation shows: the `CREATE`s of the tables that do not
    /// exist, one `INSERT` with its rows elided, the identity updates.
    #[test]
    fn the_statements_shown_are_those_the_copy_runs() {
        let dialect = crate::dialect::test_support::AnsiDialect;
        let orders = TableTransfer::new(
            Engine::Postgres,
            Engine::Postgres,
            TableRef::new("shop", "orders"),
            &orders(),
        );
        let mut plan = TransferPlan::new(vec![orders], "copy");
        let sql = statements(&plan, &dialect, &[]).unwrap();
        assert_eq!(sql.len(), 3, "{sql:?}");
        assert!(sql[0].starts_with("CREATE TABLE \"copy\".\"orders\""));
        assert_eq!(
            sql[1],
            "INSERT INTO \"copy\".\"orders\" (\"id\", \"total\") VALUES (…), (…)"
        );
        assert_eq!(
            sql[2],
            "SELECT setval(pg_get_serial_sequence('\"copy\".\"orders\"', 'id'), \
             COALESCE(MAX(\"id\"), 0) + 1, false) FROM \"copy\".\"orders\""
        );
        // A table that exists is not created; skipped, it is not written.
        let existing = ["ORDERS".to_string()];
        plan.if_exists = IfExists::Append;
        assert_eq!(
            statements(&plan, &dialect, &existing).unwrap(),
            [sql[1].clone()]
        );
        plan.if_exists = IfExists::Skip;
        assert!(statements(&plan, &dialect, &existing).unwrap().is_empty());
    }

    #[test]
    fn the_report_reads_as_text() {
        let report = TransferReport {
            tables: vec![
                TableOutcome {
                    source: TableRef::new("shop", "orders"),
                    target: TableRef::new("copy", "orders"),
                    read: 5000,
                    written: 4000,
                    elapsed: Duration::from_secs(2),
                    status: TableStatus::Failed {
                        error: "duplicate key".into(),
                        first_row: Some("(4001, 'x')".into()),
                    },
                    not_copied: vec!["index orders_total".into()],
                },
                TableOutcome {
                    source: TableRef::new("shop", "lines"),
                    target: TableRef::new("copy", "lines"),
                    read: 0,
                    written: 0,
                    elapsed: Duration::ZERO,
                    status: TableStatus::NotStarted,
                    not_copied: Vec::new(),
                },
            ],
        };
        assert!(!report.succeeded());
        assert_eq!(report.written(), 4000);
        assert_eq!(
            report.text(),
            "shop.orders → copy.orders: failed: duplicate key. 5000 rows read, 4000 written in \
             2.0 s, 2000 rows/s.\n  First row of the failing batch: (4001, 'x')\n  Not copied: \
             index orders_total.\nshop.lines → copy.lines: not started. 0 rows read, 0 written \
             in 0.0 s."
        );
    }
}
