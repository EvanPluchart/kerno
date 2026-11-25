//! Test data for a table (K72, D-141): rows made on this machine from a seed,
//! the same settings always giving the same rows, and exact about the table's
//! rules: each column's type and its bounds, NULL, the primary and unique keys,
//! the foreign keys (a value picked among the keys the referenced table holds).
//! The rows are written as `INSERT` statements for a query tab ([`Plan::script`])
//! or inserted in one transaction ([`insert`]).
//!
//! Every value is a pure function of the seed, the column and the row number
//! (a counter-based generator, SplitMix64): the preview is the first rows of
//! what is written, and a batch boundary changes nothing. Nothing is ported:
//! DBeaver's mock data generator is not in its Community sources. The word
//! lists below were written for Kerno; the mail domains are the reserved
//! `example.*` ones and the phone numbers the fictional 555-01xx range.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::Engine;
use crate::schema::{ColumnGeneration, ColumnInfo, ConstraintKind, TableRef, TableStructure};
use crate::sql::CellValue;
use crate::{Dialect, Error, Result, Session, TxState, TypeKind, exec};

/// The most rows one run makes: the insert streams them, but each row of a
/// unique key is remembered to keep it unique, and a million is already more
/// than test data needs.
pub const MAX_ROWS: u64 = 1_000_000;
/// The most rows "Open script" writes: more would be tens of megabytes of
/// text in the editor. Insert takes more.
pub const MAX_SCRIPT_ROWS: u64 = 10_000;
/// The most keys read from a referenced table: a foreign key picks among
/// these.
pub const PARENT_KEYS: usize = 10_000;
/// Rows of the preview.
pub const PREVIEW_ROWS: usize = 10;
/// Rows per `INSERT`, and the most text one holds.
const BATCH_ROWS: usize = 500;
const BATCH_BYTES: usize = 1 << 20;
/// Draws of a row before its unique keys are said to be impossible.
const ATTEMPTS: u64 = 100;
/// The share of NULL a nullable column outside every key starts with.
const NULL_PERCENT: u8 = 10;
/// The range dates start with.
pub const DATE_FROM: &str = "2020-01-01";
pub const DATE_TO: &str = "2026-12-31";
/// The savepoint each batch runs under.
const SAVEPOINT: &str = "kerno_test_data";

// The random source -------------------------------------------------------------

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

/// SplitMix64's output function.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The numbers one cell draws: keyed by the seed, a stream (a column, its
/// NULLs, a foreign key), the row and the attempt, so that no other cell's
/// settings change them.
struct Draw(u64);

impl Draw {
    fn new(seed: u64, stream: u64, row: u64, attempt: u64) -> Self {
        let mut state = mix(seed.wrapping_add(GOLDEN));
        for part in [stream, row, attempt] {
            state = mix(state ^ mix(part.wrapping_add(GOLDEN)));
        }
        Self(state)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(GOLDEN);
        mix(self.0)
    }

    /// A number in `0..n` (0 when `n` is 0).
    fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.next()) * u128::from(n)) >> 64) as u64
    }

    /// A number in `min..=max`.
    fn between(&mut self, min: i128, max: i128) -> i128 {
        let wide = (u128::from(self.next()) << 64) | u128::from(self.next());
        match max.abs_diff(min).checked_add(1) {
            Some(span) => min.wrapping_add((wide % span) as i128),
            None => wide as i128,
        }
    }

    fn pick<'a>(&mut self, list: &[&'a str]) -> &'a str {
        list[self.below(list.len() as u64) as usize]
    }
}

/// Streams of a cell's numbers.
fn value_stream(column: usize) -> u64 {
    column as u64 * 2
}
fn null_stream(column: usize) -> u64 {
    column as u64 * 2 + 1
}
fn key_stream(key: usize) -> u64 {
    (1 << 40) + key as u64
}

/// A seed for a new dialog: six digits, easy to note down.
pub fn new_seed() -> u64 {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.subsec_nanos());
        bytes = u64::from(nanos).to_le_bytes();
    }
    u64::from_le_bytes(bytes) % 1_000_000
}

// Word lists, written for Kerno ------------------------------------------------

const FIRST_NAMES: &[&str] = &[
    "Ada", "Alan", "Amara", "Ana", "Arjun", "Ben", "Chen", "Chloe", "Dara", "David", "Elena",
    "Emma", "Farid", "Grace", "Hana", "Ines", "Ivan", "Jack", "Jonas", "Julia", "Kenji", "Lara",
    "Leo", "Lina", "Luca", "Maya", "Mei", "Mila", "Nadia", "Noah", "Omar", "Paula", "Ravi", "Rosa",
    "Sam", "Sara", "Tomas", "Yara", "Yusuf", "Zoe",
];
const LAST_NAMES: &[&str] = &[
    "Adams", "Ahmed", "Alvarez", "Berg", "Brown", "Costa", "Diaz", "Dubois", "Fischer", "Garcia",
    "Hansen", "Ito", "Jensen", "Kaur", "Kim", "Kowalski", "Laurent", "Lee", "Lopez", "Martin",
    "Meyer", "Moreau", "Nakamura", "Novak", "Okafor", "Olsen", "Patel", "Perez", "Rossi", "Santos",
    "Schmidt", "Silva", "Singh", "Smith", "Suzuki", "Tanaka", "Weber", "Wong", "Young", "Zhang",
];
const CITIES: &[&str] = &[
    "Amsterdam",
    "Athens",
    "Austin",
    "Barcelona",
    "Berlin",
    "Bogota",
    "Boston",
    "Cairo",
    "Chicago",
    "Dakar",
    "Denver",
    "Dublin",
    "Hamburg",
    "Helsinki",
    "Lagos",
    "Lima",
    "Lisbon",
    "London",
    "Lyon",
    "Madrid",
    "Manila",
    "Milan",
    "Montreal",
    "Mumbai",
    "Nairobi",
    "Osaka",
    "Oslo",
    "Paris",
    "Porto",
    "Prague",
    "Quito",
    "Rome",
    "Seoul",
    "Seville",
    "Sydney",
    "Tokyo",
    "Toronto",
    "Valencia",
    "Vienna",
    "Warsaw",
];
const COUNTRIES: &[(&str, &str)] = &[
    ("Argentina", "AR"),
    ("Australia", "AU"),
    ("Austria", "AT"),
    ("Belgium", "BE"),
    ("Brazil", "BR"),
    ("Canada", "CA"),
    ("Chile", "CL"),
    ("China", "CN"),
    ("Denmark", "DK"),
    ("Egypt", "EG"),
    ("Finland", "FI"),
    ("France", "FR"),
    ("Germany", "DE"),
    ("Greece", "GR"),
    ("India", "IN"),
    ("Ireland", "IE"),
    ("Italy", "IT"),
    ("Japan", "JP"),
    ("Kenya", "KE"),
    ("Mexico", "MX"),
    ("Morocco", "MA"),
    ("Netherlands", "NL"),
    ("Nigeria", "NG"),
    ("Norway", "NO"),
    ("Peru", "PE"),
    ("Poland", "PL"),
    ("Portugal", "PT"),
    ("Senegal", "SN"),
    ("Spain", "ES"),
    ("Sweden", "SE"),
];
const WORDS: &[&str] = &[
    "amber", "anchor", "apple", "arrow", "basil", "beacon", "birch", "bloom", "breeze", "brick",
    "cedar", "cloud", "comet", "copper", "coral", "crane", "dawn", "delta", "ember", "falcon",
    "fern", "field", "flint", "frost", "garden", "glade", "granite", "harbor", "hazel", "horizon",
    "island", "ivory", "jade", "juniper", "lantern", "lemon", "linen", "maple", "meadow", "mint",
    "moss", "nectar", "oak", "ocean", "olive", "orbit", "pebble", "pine", "prairie", "quartz",
    "river", "saffron", "sage", "shadow", "silver", "slate", "spark", "spruce", "stone", "summit",
    "thistle", "timber", "velvet", "willow",
];
/// Reserved for examples (RFC 2606): never a real mailbox.
const DOMAINS: &[&str] = &["example.com", "example.org", "example.net"];
/// Area codes; the numbers are in 555-0100 to 555-0199, kept for fiction.
const AREAS: &[&str] = &[
    "202", "212", "305", "312", "415", "512", "617", "702", "808", "919",
];

// What a column gets --------------------------------------------------------------

/// A well-known shape of text, guessed from the column's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shape {
    Email,
    FirstName,
    LastName,
    FullName,
    Phone,
    City,
    Country,
    Url,
    Username,
}

impl Shape {
    pub const ALL: [Shape; 9] = [
        Shape::Email,
        Shape::FirstName,
        Shape::LastName,
        Shape::FullName,
        Shape::Phone,
        Shape::City,
        Shape::Country,
        Shape::Url,
        Shape::Username,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Shape::Email => "Email address",
            Shape::FirstName => "First name",
            Shape::LastName => "Last name",
            Shape::FullName => "Full name",
            Shape::Phone => "Phone number",
            Shape::City => "City",
            Shape::Country => "Country",
            Shape::Url => "Web address",
            Shape::Username => "User name",
        }
    }

    /// The shape a column's name asks for.
    fn of_name(name: &str) -> Option<Shape> {
        let name = name.to_ascii_lowercase();
        let is = |names: &[&str]| names.contains(&name.as_str());
        Some(if name.contains("email") || name == "mail" {
            Shape::Email
        } else if is(&["first_name", "firstname", "given_name", "forename"]) {
            Shape::FirstName
        } else if is(&["last_name", "lastname", "surname", "family_name"]) {
            Shape::LastName
        } else if is(&[
            "name",
            "full_name",
            "fullname",
            "display_name",
            "contact_name",
            "customer_name",
            "person_name",
        ]) {
            Shape::FullName
        } else if name.contains("phone") || is(&["mobile", "tel", "telephone"]) {
            Shape::Phone
        } else if is(&["city", "town"]) || name.ends_with("_city") {
            Shape::City
        } else if name.contains("country") {
            Shape::Country
        } else if is(&["url", "website", "homepage", "link"]) || name.ends_with("_url") {
            Shape::Url
        } else if is(&["username", "user_name", "login", "handle", "nickname"]) {
            Shape::Username
        } else {
            return None;
        })
    }

    /// The fewest characters its values need to be worth guessing.
    fn width(self) -> usize {
        match self {
            Shape::Email => 30,
            Shape::FirstName | Shape::LastName => 10,
            Shape::FullName | Shape::Username => 16,
            Shape::Phone => 15,
            Shape::City => 12,
            Shape::Country => 2,
            Shape::Url => 40,
        }
    }

    fn value(self, draw: &mut Draw, limit: Option<usize>, number: Option<u64>) -> String {
        let first = draw.pick(FIRST_NAMES);
        let last = draw.pick(LAST_NAMES);
        let number = number.map_or(String::new(), |n| n.to_string());
        match self {
            Shape::Email => format!(
                "{}.{}{number}@{}",
                first.to_ascii_lowercase(),
                last.to_ascii_lowercase(),
                draw.pick(DOMAINS)
            ),
            Shape::FirstName => format!("{first}{number}"),
            Shape::LastName => format!("{last}{number}"),
            Shape::FullName => format!("{first} {last}{number}"),
            Shape::Phone => format!(
                "+1 {} 555 01{:02}{number}",
                draw.pick(AREAS),
                draw.below(100)
            ),
            Shape::City => format!("{}{number}", draw.pick(CITIES)),
            Shape::Country => {
                let (name, code) = COUNTRIES[draw.below(COUNTRIES.len() as u64) as usize];
                let long = COUNTRIES.iter().map(|(name, _)| name.len()).max();
                if limit.is_some_and(|limit| Some(limit) < long) {
                    format!("{code}{number}")
                } else {
                    format!("{name}{number}")
                }
            }
            Shape::Url => format!(
                "https://{}.example.com/{}{number}",
                draw.pick(WORDS),
                draw.pick(WORDS)
            ),
            Shape::Username => format!(
                "{}_{}{}",
                draw.pick(WORDS),
                draw.pick(WORDS),
                if number.is_empty() {
                    draw.below(100).to_string()
                } else {
                    number
                }
            ),
        }
    }
}

/// What makes a column's values: the entry of its line in the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Left out of the `INSERT`: the column takes its default.
    Default,
    /// Computed or numbered by the server (generated, identity,
    /// auto-increment): never written.
    Server,
    /// Always NULL.
    Null,
    /// `start`, `start + step`, …: never twice the same.
    Sequence,
    /// A whole number in a range.
    Integer,
    /// A number with the column's decimals in a range.
    Decimal,
    Boolean,
    /// Random words, to a length in a range.
    Words,
    Shape(Shape),
    /// A date in a range.
    Date,
    /// A date and a time of day in a range of days (with a zone where the
    /// type keeps one).
    DateTime,
    /// A time of day.
    Time,
    Interval,
    Uuid,
    /// A small JSON object.
    Json,
    /// One of the column's labels (enum, set).
    OneOf,
    /// Random bytes, to a length in a range.
    Bytes,
    /// Random bits, to the column's width.
    Bits,
    /// Up to three elements, each as its type gets.
    Array,
    /// A key that exists in the referenced table.
    Reference,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Default => "Leave to the default",
            Kind::Server => "Made by the server",
            Kind::Null => "NULL",
            Kind::Sequence => "Sequence",
            Kind::Integer => "Random number",
            Kind::Decimal => "Random decimal",
            Kind::Boolean => "True or false",
            Kind::Words => "Random words",
            Kind::Shape(shape) => shape.label(),
            Kind::Date => "Date",
            Kind::DateTime => "Date and time",
            Kind::Time => "Time of day",
            Kind::Interval => "Interval",
            Kind::Uuid => "UUID",
            Kind::Json => "JSON object",
            Kind::OneOf => "One of the labels",
            Kind::Bytes => "Random bytes",
            Kind::Bits => "Random bits",
            Kind::Array => "Array",
            Kind::Reference => "A key of the referenced table",
        }
    }

    /// The names of its two settings, when it has them.
    pub fn params(self) -> Option<[&'static str; 2]> {
        match self {
            Kind::Sequence => Some(["start", "step"]),
            Kind::Integer | Kind::Decimal => Some(["min", "max"]),
            Kind::Words | Kind::Bytes => Some(["min length", "max length"]),
            Kind::Date | Kind::DateTime => Some(["from", "to"]),
            _ => None,
        }
    }
}

/// One column's line: its kind, its two settings as typed, and its share of
/// NULL in percent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnSettings {
    pub kind: Kind,
    pub from: String,
    pub to: String,
    pub nulls: u8,
}

/// What the dialog asks for: how many rows, the seed, one line per column of
/// the table in its order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub rows: u64,
    pub seed: u64,
    pub columns: Vec<ColumnSettings>,
}

// What is read of the table ---------------------------------------------------------

/// The table and what its rules need read: its structure, its row count, the
/// largest value of each integer column that is a unique key alone (where a
/// sequence starts beyond), and the keys of each referenced table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub engine: Engine,
    pub table: TableRef,
    pub structure: TableStructure,
    /// Rows in the table when it was read.
    pub rows: u64,
    /// Per column of the structure.
    pub maxima: Vec<Option<i128>>,
    /// Per foreign key of the structure, the distinct keys of the referenced
    /// table (at most [`PARENT_KEYS`]), each as its columns' values.
    pub parents: Vec<Vec<Vec<CellValue>>>,
    /// SQLite only: the table's SQL holds a CHECK, which SQLite does not list
    /// apart and Kerno does not read.
    pub unread_checks: bool,
}

impl Facts {
    fn column(&self, index: usize) -> &ColumnInfo {
        &self.structure.columns[index]
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.structure.columns.iter().position(|c| c.name == name)
    }

    /// The foreign key a column is part of, and its place in it: the first
    /// one that lists it.
    pub fn reference(&self, column: usize) -> Option<(usize, usize)> {
        let name = &self.column(column).name;
        self.structure
            .foreign_keys
            .iter()
            .enumerate()
            .find_map(|(key, fk)| Some((key, fk.columns.iter().position(|c| c == name)?)))
    }

    /// The sets of columns no two rows may share: the primary key, the
    /// unique constraints, and the unique indexes on plain columns without a
    /// predicate (the others are said in [`notes`]).
    pub fn unique_sets(&self) -> Vec<Vec<usize>> {
        let mut sets: Vec<Vec<usize>> = Vec::new();
        let mut add = |names: &[String]| {
            let set: Option<Vec<usize>> = names.iter().map(|n| self.index_of(n)).collect();
            if let Some(mut set) = set.filter(|set| !set.is_empty()) {
                set.sort_unstable();
                set.dedup();
                if !sets.contains(&set) {
                    sets.push(set);
                }
            }
        };
        if let Some(identity) = self.primary_key() {
            add(&identity);
        }
        for constraint in &self.structure.constraints {
            if matches!(
                constraint.kind,
                ConstraintKind::Unique | ConstraintKind::PrimaryKey
            ) {
                add(&constraint.columns);
            }
        }
        for index in &self.structure.indexes {
            if index.unique && index.predicate.is_none() {
                add(&index.columns);
            }
        }
        sets
    }

    fn primary_key(&self) -> Option<Vec<String>> {
        let mut key: Vec<(u32, String)> = self
            .structure
            .columns
            .iter()
            .filter_map(|c| c.primary_key_position.map(|p| (p, c.name.clone())))
            .collect();
        key.sort();
        (!key.is_empty()).then(|| key.into_iter().map(|(_, name)| name).collect())
    }

    /// Whether the column is a unique key on its own.
    pub fn unique_alone(&self, column: usize) -> bool {
        self.unique_sets().contains(&vec![column])
    }

    /// Whether the column is part of a key: unique, primary or foreign.
    fn in_key(&self, column: usize) -> bool {
        self.reference(column).is_some()
            || self.unique_sets().iter().any(|set| set.contains(&column))
    }

    fn is_mysql(&self) -> bool {
        matches!(self.engine, Engine::MySql | Engine::MariaDb)
    }
}

/// Reads what [`Facts`] holds, on `session` (in the table's database).
pub async fn read(session: Arc<dyn Session>, table: &TableRef) -> Result<Facts> {
    let structure = session.table_structure(table).await?;
    let columns = structure.columns.len();
    let mut facts = Facts {
        engine: session.engine(),
        table: table.clone(),
        structure,
        rows: 0,
        maxima: vec![None; columns],
        parents: Vec::new(),
        unread_checks: false,
    };
    let dialect = session.dialect();
    let counted: Vec<usize> = (0..columns)
        .filter(|&c| facts.column(c).kind == TypeKind::Integer && facts.unique_alone(c))
        .collect();
    let select: Vec<String> = std::iter::once("COUNT(*)".to_string())
        .chain(
            counted
                .iter()
                .map(|&c| format!("MAX({})", dialect.quote_ident(&facts.column(c).name))),
        )
        .collect();
    let sql = format!(
        "SELECT {} FROM {}",
        select.join(", "),
        dialect.table_name(table)
    );
    let row = exec::collect(session.clone(), sql)
        .await?
        .results
        .into_iter()
        .next()
        .and_then(|result| result.rows().into_iter().next())
        .unwrap_or_default();
    let number = |index: usize| -> Option<i128> { row.get(index)?.as_deref()?.trim().parse().ok() };
    facts.rows = number(0).and_then(|n| u64::try_from(n).ok()).unwrap_or(0);
    if facts.engine == Engine::Sqlite {
        let sql = format!(
            "SELECT sql FROM {}.sqlite_master WHERE name = {}",
            dialect.quote_ident(&table.schema),
            dialect.quote_string(&table.name)
        );
        let created = exec::collect(session.clone(), sql)
            .await?
            .results
            .into_iter()
            .next()
            .and_then(|result| result.rows().into_iter().next())
            .and_then(|row| row.into_iter().next().flatten())
            .unwrap_or_default();
        facts.unread_checks = has_check(&created);
    }
    for (position, &column) in counted.iter().enumerate() {
        facts.maxima[column] = number(position + 1);
    }
    for fk in &facts.structure.foreign_keys {
        let names: Vec<String> = fk
            .referenced_columns
            .iter()
            .map(|c| dialect.quote_ident(c))
            .collect();
        let sql = format!(
            "SELECT DISTINCT {} FROM {} WHERE {} ORDER BY {} {}",
            names.join(", "),
            dialect.table_name(&fk.referenced_table),
            names
                .iter()
                .map(|name| format!("{name} IS NOT NULL"))
                .collect::<Vec<_>>()
                .join(" AND "),
            (1..=names.len())
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            dialect.limit_offset(PARENT_KEYS as u64, 0)
        );
        let mut keys = Vec::new();
        for result in exec::collect(session.clone(), sql).await?.results {
            for batch in &result.batches {
                for row in 0..batch.row_count() {
                    keys.push(
                        (0..batch.column_count())
                            .map(|column| match batch.cell(row, column) {
                                None => CellValue::Null,
                                Some(bytes)
                                    if result.columns.get(column).map(|c| c.kind)
                                        == Some(TypeKind::Binary) =>
                                {
                                    CellValue::Bytes(bytes.to_vec())
                                }
                                Some(bytes) => {
                                    CellValue::Text(String::from_utf8_lossy(bytes).into_owned())
                                }
                            })
                            .collect(),
                    );
                }
            }
        }
        facts.parents.push(keys);
    }
    Ok(facts)
}

/// Whether a `CREATE TABLE` holds a `CHECK (…)`.
fn has_check(sql: &str) -> bool {
    let sql = sql.to_ascii_lowercase();
    sql.match_indices("check").any(|(at, word)| {
        let before = sql[..at].chars().next_back();
        let after = sql[at + word.len()..].trim_start();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_') && after.starts_with('(')
    })
}

// The bounds of each type -------------------------------------------------------------

fn lower(column: &ColumnInfo) -> String {
    column.data_type.to_ascii_lowercase()
}

/// The numbers inside the first parentheses of a type: `(10,2)` → `[10, 2]`.
fn type_numbers(data_type: &str) -> Vec<u64> {
    let Some(start) = data_type.find('(') else {
        return Vec::new();
    };
    let end = data_type[start..]
        .find(')')
        .map_or(data_type.len(), |e| start + e);
    data_type[start + 1..end]
        .split(',')
        .filter_map(|n| n.trim().parse().ok())
        .collect()
}

/// The values an integer column holds.
fn integer_bounds(facts: &Facts, column: &ColumnInfo) -> (i128, i128) {
    let t = lower(column);
    if facts.engine == Engine::Sqlite {
        return (i128::from(i64::MIN), i128::from(i64::MAX));
    }
    if facts.is_mysql() && t.starts_with("year") {
        return (1901, 2155);
    }
    let bits = if t.starts_with("tinyint") {
        8
    } else if t.starts_with("smallint") || t == "int2" {
        16
    } else if t.starts_with("mediumint") {
        24
    } else if t.starts_with("bigint") || t == "int8" {
        64
    } else if t.starts_with("int") || t == "integer" {
        32
    } else {
        64
    };
    if t.contains("unsigned") {
        (0, (1i128 << bits) - 1)
    } else {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    }
}

/// The decimals a number column keeps, and the values it holds in units
/// of `10^-scale`.
fn decimal_bounds(column: &ColumnInfo) -> (u32, i128, i128) {
    let t = lower(column);
    let unsigned = t.contains("unsigned");
    let fixed = ["numeric", "decimal", "dec", "number"]
        .iter()
        .any(|name| t.starts_with(name));
    let numbers = type_numbers(&t);
    let (scale, max) = match (fixed, numbers.as_slice()) {
        (true, [precision, scale, ..]) if precision <= &38 => {
            (*scale as u32, 10i128.pow(*precision as u32) - 1)
        }
        (true, [precision]) if precision <= &38 => (0, 10i128.pow(*precision as u32) - 1),
        _ if t.starts_with("money") => (2, i128::from(i64::MAX)),
        _ => (2, 10i128.pow(37)),
    };
    (scale, if unsigned { 0 } else { -max }, max)
}

/// The most characters a text column holds.
fn text_limit(column: &ColumnInfo) -> Option<usize> {
    let t = lower(column);
    if t.starts_with("tinytext") {
        return Some(255);
    }
    let named = [
        "character varying",
        "varchar",
        "character",
        "char",
        "bpchar",
        "nvarchar",
        "nchar",
    ]
    .iter()
    .any(|name| t.starts_with(name));
    named
        .then(|| type_numbers(&t).first().map(|&n| n as usize))
        .flatten()
}

/// The fewest and most bytes a binary column holds: `BINARY(n)` exactly n.
fn bytes_limit(column: &ColumnInfo) -> (Option<usize>, Option<usize>) {
    let t = lower(column);
    let n = type_numbers(&t).first().map(|&n| n as usize);
    if t.starts_with("binary") {
        (n, n)
    } else if t.starts_with("varbinary") {
        (None, n)
    } else if t.starts_with("tinyblob") {
        (None, Some(255))
    } else {
        (None, None)
    }
}

/// The fewest and most bits of a bit column.
fn bits_width(column: &ColumnInfo) -> (usize, usize) {
    let t = lower(column);
    let n = type_numbers(&t).first().map(|&n| n as usize);
    if t.contains("varying") || t.starts_with("varbit") {
        (1, n.unwrap_or(8))
    } else {
        let n = n.unwrap_or(1);
        (n, n)
    }
}

fn has_zone(column: &ColumnInfo) -> bool {
    let t = lower(column);
    column.kind == TypeKind::DateTimeTz || t.contains("with time zone") || t == "timetz"
}

/// A MySQL `TIMESTAMP`: 1970 to 2038 only.
fn is_mysql_timestamp(facts: &Facts, column: &ColumnInfo) -> bool {
    facts.is_mysql() && lower(column).starts_with("timestamp")
}

/// The element of a PostgreSQL array column, as a column of its own.
fn element(column: &ColumnInfo) -> Option<ColumnInfo> {
    let data_type = column.data_type.strip_suffix("[]")?.to_string();
    let t = data_type.to_ascii_lowercase();
    let base = t.split('(').next().unwrap_or_default().trim().to_string();
    let kind = if !column.enum_values.is_empty() {
        TypeKind::Enum
    } else {
        match base.as_str() {
            "smallint" | "integer" | "bigint" => TypeKind::Integer,
            "numeric" | "real" | "double precision" => TypeKind::Decimal,
            "text" | "character varying" | "character" | "citext" => TypeKind::Text,
            "boolean" => TypeKind::Boolean,
            "uuid" => TypeKind::Uuid,
            "date" => TypeKind::Date,
            "timestamp without time zone" => TypeKind::DateTime,
            "timestamp with time zone" => TypeKind::DateTimeTz,
            _ => return None,
        }
    };
    Some(ColumnInfo {
        data_type,
        kind,
        nullable: false,
        default: None,
        generation: None,
        primary_key_position: None,
        ..column.clone()
    })
}

// The kinds offered, and the guess ----------------------------------------------------

/// The kinds a column can get, the guessed one among them.
pub fn kinds(facts: &Facts, column: usize) -> Vec<Kind> {
    let info = facts.column(column);
    if info.generation.is_some() {
        return vec![Kind::Server];
    }
    let mut kinds = Vec::new();
    if facts.reference(column).is_some() {
        kinds.push(Kind::Reference);
    }
    kinds.extend(type_kinds(info));
    // SQLite keeps dates and times as text (K76).
    if facts.engine == Engine::Sqlite && info.kind == TypeKind::Text {
        kinds.extend([Kind::Date, Kind::DateTime, Kind::Time]);
    }
    if info.default.is_some() {
        kinds.push(Kind::Default);
    }
    if info.nullable {
        kinds.push(Kind::Null);
    }
    kinds
}

/// The kinds a column's type can get, whatever its keys.
fn type_kinds(info: &ColumnInfo) -> Vec<Kind> {
    match info.kind {
        TypeKind::Integer => vec![Kind::Sequence, Kind::Integer],
        TypeKind::Float | TypeKind::Decimal => vec![Kind::Decimal],
        TypeKind::Boolean => vec![Kind::Boolean],
        TypeKind::Text => {
            let mut kinds = vec![Kind::Words];
            kinds.extend(Shape::ALL.map(Kind::Shape));
            if text_limit(info).is_none_or(|limit| limit >= 36) {
                kinds.push(Kind::Uuid);
            }
            if text_limit(info).is_none() {
                kinds.push(Kind::Json);
            }
            kinds
        }
        TypeKind::Binary => vec![Kind::Bytes],
        TypeKind::Date => vec![Kind::Date],
        TypeKind::DateTime | TypeKind::DateTimeTz => vec![Kind::DateTime],
        TypeKind::Time => vec![Kind::Time],
        TypeKind::Interval => vec![Kind::Interval],
        TypeKind::Uuid => vec![Kind::Uuid],
        TypeKind::Json => vec![Kind::Json],
        TypeKind::Enum if !info.enum_values.is_empty() => vec![Kind::OneOf],
        TypeKind::Bit => vec![Kind::Bits],
        TypeKind::Array if element(info).is_some_and(|e| !type_kinds(&e).is_empty()) => {
            vec![Kind::Array]
        }
        _ => Vec::new(),
    }
}

/// The lines the dialog starts with: per column, the kind guessed from its
/// type, its keys and its name, with its default settings.
pub fn guess(facts: &Facts) -> Vec<ColumnSettings> {
    (0..facts.structure.columns.len())
        .map(|column| {
            let info = facts.column(column);
            let offered = kinds(facts, column);
            let shape = Shape::of_name(&info.name)
                .filter(|shape| text_limit(info).is_none_or(|limit| limit >= shape.width()));
            let named_uuid = {
                let name = info.name.to_ascii_lowercase();
                name.contains("uuid") || name.contains("guid")
            };
            let kind = if offered.contains(&Kind::Server) {
                Kind::Server
            } else if offered.contains(&Kind::Reference) {
                match facts.reference(column) {
                    Some((key, _)) if facts.parents.get(key).is_some_and(Vec::is_empty) => {
                        if info.nullable {
                            Kind::Null
                        } else {
                            Kind::Reference
                        }
                    }
                    _ => Kind::Reference,
                }
            } else if info.kind == TypeKind::Integer && facts.unique_alone(column) {
                Kind::Sequence
            } else if info.kind == TypeKind::Integer {
                Kind::Integer
            } else if facts.engine == Engine::Sqlite
                && info.kind == TypeKind::Text
                && let Some(kind) = text_time(info)
            {
                // A date the server writes is left to it (K76).
                if info.default.is_some() {
                    Kind::Default
                } else {
                    kind
                }
            } else if info.kind == TypeKind::Text && named_uuid && offered.contains(&Kind::Uuid) {
                Kind::Uuid
            } else if info.kind == TypeKind::Text
                && offered.contains(&Kind::Json)
                && checked_json(facts, &info.name)
            {
                // MariaDB's JSON: a long text checked by json_valid().
                Kind::Json
            } else if info.kind == TypeKind::Text
                && let Some(shape) = shape
            {
                Kind::Shape(shape)
            } else {
                offered
                    .first()
                    .copied()
                    .unwrap_or(if info.default.is_some() {
                        Kind::Default
                    } else {
                        Kind::Null
                    })
            };
            let written = !matches!(kind, Kind::Null | Kind::Server | Kind::Default);
            let nulls = if info.nullable && !facts.in_key(column) && written {
                NULL_PERCENT
            } else {
                0
            };
            ColumnSettings {
                nulls,
                ..defaults(facts, column, kind)
            }
        })
        .collect()
}

/// The kind of time a text column holds, from its default (`CURRENT_TIMESTAMP`,
/// `datetime('now')`, a literal date) or else its name (`created_at`,
/// `birth_date`, `start_time`): SQLite's way of keeping dates (K76, K77).
fn text_time(info: &ColumnInfo) -> Option<Kind> {
    let default = info
        .default
        .as_deref()
        .unwrap_or_default()
        .trim_matches(|c: char| c == '(' || c == ')' || c == '\'' || c == '"' || c.is_whitespace())
        .to_ascii_lowercase();
    let literal_date = default.get(..10).and_then(parse_date).is_some();
    let by_default = if default == "current_timestamp"
        || default.starts_with("datetime(")
        || literal_date && default.len() > 10
    {
        Some(Kind::DateTime)
    } else if default == "current_date" || default.starts_with("date(") || literal_date {
        Some(Kind::Date)
    } else if default == "current_time" || default.starts_with("time(") {
        Some(Kind::Time)
    } else {
        None
    };
    let name = info.name.to_ascii_lowercase();
    // `word` or `*_word`; a time of day `time_*` too (`time_of_day`), not `time_zone`.
    let named = |word: &str| name == word || name.ends_with(&format!("_{word}"));
    let a_time = named("time") || name.starts_with("time_") && !name.starts_with("time_zone");
    by_default.or(
        if name.ends_with("_at")
            || ["timestamp", "datetime", "date_time"]
                .into_iter()
                .any(named)
        {
            Some(Kind::DateTime)
        } else if named("date") {
            Some(Kind::Date)
        } else if a_time {
            Some(Kind::Time)
        } else {
            None
        },
    )
}

/// Whether a CHECK holds `column` to valid JSON (MariaDB's `JSON` type).
fn checked_json(facts: &Facts, column: &str) -> bool {
    facts.structure.constraints.iter().any(|constraint| {
        constraint.kind == ConstraintKind::Check
            && constraint
                .definition
                .to_ascii_lowercase()
                .contains("json_valid")
            && (constraint.columns.iter().any(|c| c == column)
                || constraint.definition.contains(column))
    })
}

/// The settings `kind` starts with on `column`: a range that fits the type,
/// a sequence beyond the largest value the table holds.
pub fn defaults(facts: &Facts, column: usize, kind: Kind) -> ColumnSettings {
    let info = facts.column(column);
    let (from, to) = match kind {
        Kind::Sequence => {
            let start = facts.maxima[column].map_or(1, |max| max.saturating_add(1));
            (start.to_string(), "1".to_string())
        }
        Kind::Integer => {
            let (low, high) = integer_bounds(facts, info);
            if facts.is_mysql() && lower(info).starts_with("year") {
                ("2000".into(), "2030".into())
            } else {
                (low.max(1).to_string(), high.min(1000).to_string())
            }
        }
        Kind::Decimal => {
            let (scale, low, high) = decimal_bounds(info);
            let unit = 10i128.pow(scale);
            (
                decimal_text(low.max(0), scale),
                decimal_text(high.min(1000 * unit), scale),
            )
        }
        Kind::Words => {
            let limit = text_limit(info).unwrap_or(usize::MAX);
            (5.min(limit).to_string(), 40.min(limit).to_string())
        }
        Kind::Bytes => match bytes_limit(info) {
            (Some(min), Some(max)) => (min.to_string(), max.to_string()),
            (_, max) => {
                let max = max.unwrap_or(usize::MAX);
                (8.min(max).to_string(), 32.min(max).to_string())
            }
        },
        Kind::Date | Kind::DateTime => (DATE_FROM.into(), DATE_TO.into()),
        _ => (String::new(), String::new()),
    };
    ColumnSettings {
        kind,
        from,
        to,
        nulls: 0,
    }
}

// The checked plan ------------------------------------------------------------------------

/// What makes one column's values, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Gen {
    /// `DEFAULT`: only when no column is written.
    Default,
    Null,
    Sequence {
        start: i128,
        step: i128,
    },
    Integer {
        min: i128,
        max: i128,
    },
    Decimal {
        min: i128,
        max: i128,
        scale: u32,
    },
    Boolean,
    /// `number`: the first number of a unique column's suffix.
    Words {
        min: usize,
        max: usize,
        number: Option<u64>,
    },
    Shape {
        shape: Shape,
        limit: Option<usize>,
        number: Option<u64>,
    },
    Date {
        from: i64,
        to: i64,
    },
    /// Seconds since 1970.
    DateTime {
        from: i64,
        to: i64,
        zone: bool,
    },
    Time {
        zone: bool,
    },
    Interval,
    Uuid,
    Json,
    OneOf(Vec<String>),
    Bytes {
        min: usize,
        max: usize,
    },
    Bits {
        min: usize,
        max: usize,
    },
    Array(Box<Gen>),
    Reference {
        key: usize,
        part: usize,
    },
}

/// One column the `INSERT` writes.
#[derive(Debug, Clone)]
struct Written {
    /// Its place in the table.
    index: usize,
    info: ColumnInfo,
    make: Gen,
    nulls: u8,
    /// Part of a set no two rows may share (K76): only its values are drawn
    /// again when a row repeats a key, and they are drawn for the row's
    /// number after the rows already in the table, so that a second run with
    /// the same seed does not draw the first run's keys again; every other
    /// value stays the one the same settings gave on an empty table.
    unique: bool,
}

/// The settings checked against the table: what [`Plan::values`] makes.
#[derive(Debug, Clone)]
pub struct Plan {
    table: TableRef,
    seed: u64,
    rows: u64,
    columns: Vec<Written>,
    /// The unique sets, as places in `columns`.
    uniques: Vec<Vec<usize>>,
    parents: Vec<Vec<Vec<CellValue>>>,
    /// Per foreign key, whether its key is part of a unique set.
    unique_keys: Vec<bool>,
    /// The rows already in the table.
    before: u64,
}

fn quoted(name: &str) -> String {
    format!("“{name}”")
}

/// `n` with a comma between thousands.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn decimal_text(units: i128, scale: u32) -> String {
    if scale == 0 {
        return units.to_string();
    }
    let unit = 10i128.pow(scale);
    let sign = if units < 0 { "-" } else { "" };
    let units = units.unsigned_abs();
    let unit = unit.unsigned_abs();
    format!(
        "{sign}{}.{:0width$}",
        units / unit,
        units % unit,
        width = scale as usize
    )
}

/// `text` as units of `10^-scale`, refusing more decimals than `scale`.
fn parse_decimal(text: &str, scale: u32) -> Option<i128> {
    let text = text.trim();
    let (negative, text) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > scale as usize
    {
        return None;
    }
    let whole: i128 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let fraction: i128 = format!("{fraction:0<width$}", width = scale as usize)
        .parse()
        .unwrap_or(0);
    let units = whole
        .checked_mul(10i128.pow(scale))?
        .checked_add(fraction)?;
    Some(if negative { -units } else { units })
}

/// Days since 1970 of `YYYY-MM-DD`.
fn parse_date(text: &str) -> Option<i64> {
    let mut parts = text.trim().split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let days = crate::xlsx::days_from_civil(year, month, day);
    // 31 February is not a day.
    (day >= 1 && crate::format::civil_from_days(days) == (year, month as u32, day as u32))
        .then_some(days)
}

fn date_text(days: i64) -> String {
    let (year, month, day) = crate::format::civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

impl Plan {
    /// Checks `settings` against the table: every refusal in plain words,
    /// before anything is made.
    pub fn new(facts: &Facts, settings: &Settings) -> std::result::Result<Plan, String> {
        if !(1..=MAX_ROWS).contains(&settings.rows) {
            return Err(format!(
                "Ask for 1 to {} rows: more is not test data, and each row of a unique key is \
                 remembered to keep it unique.",
                grouped(MAX_ROWS)
            ));
        }
        if settings.columns.len() != facts.structure.columns.len() {
            return Err("The settings do not match the table's columns: read it again.".into());
        }
        let mut columns = Vec::new();
        for (index, line) in settings.columns.iter().enumerate() {
            let info = facts.column(index);
            let name = quoted(&info.name);
            if !kinds(facts, index).contains(&line.kind) {
                return Err(if kinds(facts, index).is_empty() {
                    format!(
                        "Kerno cannot make values of type {} for {name}, which cannot be NULL and \
                         has no default.",
                        info.data_type
                    )
                } else {
                    format!("{name} cannot get {}.", line.kind.label().to_lowercase())
                });
            }
            if line.nulls > 100 {
                return Err(format!(
                    "The share of NULL of {name} is a percentage: 0 to 100."
                ));
            }
            if line.nulls > 0 && !info.nullable {
                return Err(format!("{name} cannot be NULL."));
            }
            let make = match line.kind {
                Kind::Default | Kind::Server => continue,
                _ => gen_of(facts, index, line, settings.rows)?,
            };
            columns.push(Written {
                index,
                info: info.clone(),
                make,
                nulls: line.nulls,
                unique: facts.unique_sets().iter().any(|set| set.contains(&index)),
            });
        }
        // Every column left to the server: one written as DEFAULT, which
        // every engine takes in a multi-row VALUES.
        if columns.is_empty() {
            columns.push(Written {
                index: 0,
                info: facts.column(0).clone(),
                make: Gen::Default,
                nulls: 0,
                unique: false,
            });
        }
        let unique_keys = (0..facts.parents.len())
            .map(|key| {
                columns.iter().any(|c| {
                    c.unique && matches!(c.make, Gen::Reference { key: k, .. } if k == key)
                })
            })
            .collect();
        let place = |index: usize| columns.iter().position(|c| c.index == index);
        let mut uniques = Vec::new();
        for set in facts.unique_sets() {
            // A column the server makes keeps its own rule.
            let Some(places) = set.iter().map(|&i| place(i)).collect::<Option<Vec<_>>>() else {
                continue;
            };
            if places.iter().any(|&p| columns[p].make == Gen::Default) {
                continue;
            }
            let capacity = capacity(&columns, &places, &facts.parents);
            if capacity < u128::from(settings.rows) {
                let names: Vec<String> =
                    set.iter().map(|&i| facts.column(i).name.clone()).collect();
                return Err(format!(
                    "({}) must be different on every row, and these settings give at most {} \
                     different values for {} rows: widen a range, or ask for fewer rows.",
                    names.join(", "),
                    grouped(u64::try_from(capacity).unwrap_or(u64::MAX)),
                    grouped(settings.rows)
                ));
            }
            uniques.push(places);
        }
        Ok(Plan {
            table: facts.table.clone(),
            seed: settings.seed,
            rows: settings.rows,
            columns,
            uniques,
            parents: facts.parents.clone(),
            unique_keys,
            before: facts.rows,
        })
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    pub fn table(&self) -> &TableRef {
        &self.table
    }

    /// The columns written, in the table's order.
    pub fn names(&self) -> Vec<String> {
        self.columns.iter().map(|c| c.info.name.clone()).collect()
    }

    /// The rows, one at a time.
    pub fn values(&self) -> Values<'_> {
        Values {
            plan: self,
            next: 0,
            seen: vec![HashSet::new(); self.uniques.len()],
        }
    }

    /// The first `n` rows, as they will be written.
    pub fn preview(&self, n: usize) -> std::result::Result<Vec<Vec<CellValue>>, String> {
        self.values().take(n).collect()
    }

    /// The row `row` at attempt `attempt`.
    fn row(&self, row: u64, attempt: u64) -> Vec<CellValue> {
        let mut picks: Vec<Option<usize>> = vec![None; self.parents.len()];
        self.columns
            .iter()
            .map(|column| {
                // A unique value's row and attempt; a sequence's start is its
                // own setting, already beyond the rows there.
                let (at, attempt) = match (column.unique, &column.make) {
                    (true, Gen::Sequence { .. }) => (row, attempt),
                    (true, _) => (row + self.before, attempt),
                    (false, _) => (row, 0),
                };
                if column.nulls > 0
                    && Draw::new(self.seed, null_stream(column.index), at, attempt).below(100)
                        < u64::from(column.nulls)
                {
                    return CellValue::Null;
                }
                if let Gen::Reference { key, part } = column.make {
                    let parents = &self.parents[key];
                    let (at, attempt) = if self.unique_keys[key] {
                        (row + self.before, attempt)
                    } else {
                        (row, 0)
                    };
                    let pick = *picks[key].get_or_insert_with(|| {
                        Draw::new(self.seed, key_stream(key), at, attempt)
                            .below(parents.len() as u64) as usize
                    });
                    return parents[pick][part].clone();
                }
                let mut draw = Draw::new(self.seed, value_stream(column.index), at, attempt);
                column.make.value(&mut draw, at)
            })
            .collect()
    }

    /// The `INSERT` before its rows: `INSERT INTO t (a, b) VALUES `.
    fn head(&self, dialect: &dyn Dialect) -> String {
        let names: Vec<String> = self
            .columns
            .iter()
            .map(|c| dialect.quote_ident(&c.info.name))
            .collect();
        format!(
            "INSERT INTO {} ({}) VALUES",
            dialect.table_name(&self.table),
            names.join(", ")
        )
    }

    /// A row as the `VALUES` list writes it.
    fn tuple(&self, dialect: &dyn Dialect, values: &[CellValue]) -> String {
        let literals: Vec<String> = self
            .columns
            .iter()
            .zip(values)
            .map(|(column, value)| dialect.literal(value, &column.info))
            .collect();
        format!("({})", literals.join(", "))
    }

    /// The rows in batches of `INSERT` lists: at most [`BATCH_ROWS`] rows
    /// and about [`BATCH_BYTES`] of text each.
    fn batches<'a>(&'a self, dialect: &'a dyn Dialect) -> Batches<'a> {
        Batches {
            plan: self,
            dialect,
            values: self.values(),
        }
    }

    /// The `INSERT` statements of every row, for a query tab: at most
    /// [`MAX_SCRIPT_ROWS`] rows. `heading` is written first as comments.
    pub fn script(
        &self,
        dialect: &dyn Dialect,
        heading: &str,
    ) -> std::result::Result<String, String> {
        if self.rows > MAX_SCRIPT_ROWS {
            return Err(format!(
                "A script of more than {} rows is too long for the editor: use Insert, which \
                 takes up to {}.",
                grouped(MAX_SCRIPT_ROWS),
                grouped(MAX_ROWS)
            ));
        }
        let head = self.head(dialect);
        let mut script = crate::compare::commented(heading);
        script.push_str("\n\n");
        for batch in self.batches(dialect) {
            let (_, tuples) = batch?;
            script.push_str(&head);
            script.push_str("\n  ");
            script.push_str(&tuples.join(",\n  "));
            script.push_str(";\n\n");
        }
        script.truncate(script.trim_end().len());
        script.push('\n');
        Ok(script)
    }

    /// The first statement of the insert, with `shown` rows, and the rows
    /// after them counted in a comment: what the confirmation shows.
    pub fn first_statement(
        &self,
        dialect: &dyn Dialect,
        shown: usize,
    ) -> std::result::Result<String, String> {
        let rows: Vec<String> = self
            .values()
            .take(shown)
            .map(|row| row.map(|row| self.tuple(dialect, &row)))
            .collect::<std::result::Result<_, _>>()?;
        let mut sql = format!("{}\n  {};", self.head(dialect), rows.join(",\n  "));
        let more = self.rows.saturating_sub(rows.len() as u64);
        if more > 0 {
            sql.push_str(&format!(
                "\n-- … and {} more {}",
                grouped(more),
                if more == 1 { "row" } else { "rows" }
            ));
        }
        Ok(sql)
    }
}

/// The values no two rows may share, compared as a case-insensitive server
/// would compare them (`a` and `A` are one value under MySQL's `_ci`).
fn unique_key(values: &[&CellValue]) -> Option<Vec<u8>> {
    let mut key = Vec::new();
    for value in values {
        let bytes = match value {
            CellValue::Null => return None,
            CellValue::Text(text) => text.to_lowercase().into_bytes(),
            CellValue::Bytes(bytes) => bytes.clone(),
            CellValue::Default | CellValue::Expression(_) => return None,
        };
        key.extend((bytes.len() as u64).to_le_bytes());
        key.extend(bytes);
    }
    Some(key)
}

/// How many different values `places` can take together, at most.
fn capacity(columns: &[Written], places: &[usize], parents: &[Vec<Vec<CellValue>>]) -> u128 {
    let mut keys_counted = Vec::new();
    let mut total: u128 = 1;
    for &place in places {
        let column = &columns[place];
        let one: u128 = match &column.make {
            Gen::Integer { min, max } | Gen::Decimal { min, max, .. } => max.abs_diff(*min) + 1,
            Gen::Boolean => 2,
            Gen::OneOf(labels) => labels.len() as u128,
            Gen::Date { from, to } => to.abs_diff(*from) as u128 + 1,
            Gen::DateTime { from, to, .. } => to.abs_diff(*from) as u128 + 1,
            Gen::Time { .. } => 86_400,
            Gen::Bits { max, .. } => 1u128.checked_shl(*max as u32).unwrap_or(u128::MAX),
            Gen::Bytes { max, .. } if *max < 16 => 1u128 << (8 * max),
            Gen::Reference { key, .. } if keys_counted.contains(key) => 1,
            Gen::Reference { key, .. } => {
                keys_counted.push(*key);
                parents[*key].len() as u128
            }
            // NULL never collides; the others hardly can.
            _ => u128::MAX,
        };
        // A NULL never collides either.
        let one = if column.nulls > 0 { u128::MAX } else { one };
        total = total.saturating_mul(one);
    }
    total
}

/// The checked generator of one column, or why `line` does not fit it.
fn gen_of(
    facts: &Facts,
    index: usize,
    line: &ColumnSettings,
    rows: u64,
) -> std::result::Result<Gen, String> {
    let info = facts.column(index);
    let name = quoted(&info.name);
    let integer = |text: &str, what: &str| -> std::result::Result<i128, String> {
        text.trim()
            .parse::<i128>()
            .map_err(|_| format!("The {what} of {name} is not a whole number."))
    };
    let length = |text: &str, what: &str| -> std::result::Result<usize, String> {
        text.trim()
            .parse::<usize>()
            .map_err(|_| format!("The {what} of {name} is not a length."))
    };
    let unique = facts.unique_alone(index);
    // The number a unique text column ends with: from 1 at the row drawn,
    // which is shifted beyond the rows there (`Written::shift`).
    let number = unique.then_some(1);
    let make = match line.kind {
        Kind::Default | Kind::Server => Gen::Default,
        Kind::Null => Gen::Null,
        Kind::Sequence => {
            let (start, step) = (integer(&line.from, "start")?, integer(&line.to, "step")?);
            if step == 0 {
                return Err(format!("The step of {name} cannot be 0."));
            }
            let (low, high) = integer_bounds(facts, info);
            let last = step
                .checked_mul(i128::from(rows) - 1)
                .and_then(|n| n.checked_add(start));
            if !(low..=high).contains(&start) || !last.is_some_and(|l| (low..=high).contains(&l)) {
                return Err(format!(
                    "{name} ({}) holds {low} to {high}: a sequence from {start} by {step} leaves \
                     it within {} rows.",
                    info.data_type,
                    grouped(rows)
                ));
            }
            Gen::Sequence { start, step }
        }
        Kind::Integer => {
            let (min, max) = (integer(&line.from, "min")?, integer(&line.to, "max")?);
            let (low, high) = integer_bounds(facts, info);
            if min > max {
                return Err(format!("The min of {name} is above its max."));
            }
            if min < low || max > high {
                return Err(format!(
                    "{name} ({}) holds {low} to {high}.",
                    info.data_type
                ));
            }
            Gen::Integer { min, max }
        }
        Kind::Decimal => {
            let (scale, low, high) = decimal_bounds(info);
            let parse = |text: &str, what: &str| {
                parse_decimal(text, scale).ok_or_else(|| {
                    format!("The {what} of {name} is not a number with at most {scale} decimals.")
                })
            };
            let (min, max) = (parse(&line.from, "min")?, parse(&line.to, "max")?);
            if min > max {
                return Err(format!("The min of {name} is above its max."));
            }
            if min < low || max > high {
                return Err(format!(
                    "{name} ({}) holds {} to {}.",
                    info.data_type,
                    decimal_text(low, scale),
                    decimal_text(high, scale)
                ));
            }
            Gen::Decimal { min, max, scale }
        }
        Kind::Boolean => Gen::Boolean,
        Kind::Words => {
            let (min, max) = (
                length(&line.from, "min length")?,
                length(&line.to, "max length")?,
            );
            if min > max {
                return Err(format!("The min length of {name} is above its max length."));
            }
            if let Some(limit) = text_limit(info)
                && max > limit
            {
                return Err(format!(
                    "{name} holds at most {limit} characters: lower its max length."
                ));
            }
            if let Some(first) = number {
                let digits = (first + rows).to_string().len();
                if max < digits {
                    return Err(format!(
                        "{name} must be different on every row: its values end with the row's \
                         number, which needs {digits} characters."
                    ));
                }
            }
            Gen::Words { min, max, number }
        }
        Kind::Shape(shape) => {
            let limit = text_limit(info);
            if let (Some(limit), Some(first)) = (limit, number) {
                let digits = (first + rows).to_string().len();
                if limit <= digits {
                    return Err(format!(
                        "{name} holds {limit} characters: too few for a value that ends with \
                         the row's number."
                    ));
                }
            }
            Gen::Shape {
                shape,
                limit,
                number,
            }
        }
        Kind::Date | Kind::DateTime => {
            let date = |text: &str, what: &str| {
                parse_date(text)
                    .ok_or_else(|| format!("The {what} date of {name} is not a YYYY-MM-DD date."))
            };
            let (from, to) = (date(&line.from, "from")?, date(&line.to, "to")?);
            if from > to {
                return Err(format!("The from date of {name} is after its to date."));
            }
            if line.kind == Kind::Date {
                Gen::Date { from, to }
            } else {
                if is_mysql_timestamp(facts, info)
                    && (from < 1 || to > parse_date("2038-01-18").unwrap_or(0))
                {
                    return Err(format!(
                        "{name} is a TIMESTAMP, which holds 1970-01-02 to 2038-01-18 here."
                    ));
                }
                Gen::DateTime {
                    from: from * 86_400,
                    to: to * 86_400 + 86_399,
                    zone: has_zone(info),
                }
            }
        }
        Kind::Time => Gen::Time {
            zone: has_zone(info),
        },
        Kind::Interval => Gen::Interval,
        Kind::Uuid => Gen::Uuid,
        Kind::Json => Gen::Json,
        Kind::OneOf => Gen::OneOf(info.enum_values.clone()),
        Kind::Bytes => {
            let (min, max) = (
                length(&line.from, "min length")?,
                length(&line.to, "max length")?,
            );
            if min > max {
                return Err(format!("The min length of {name} is above its max length."));
            }
            if let (_, Some(limit)) = bytes_limit(info)
                && max > limit
            {
                return Err(format!(
                    "{name} holds at most {limit} bytes: lower its max length."
                ));
            }
            Gen::Bytes { min, max }
        }
        Kind::Bits => {
            let (min, max) = bits_width(info);
            Gen::Bits { min, max }
        }
        Kind::Array => {
            let element = element(info).ok_or_else(|| format!("{name} has no element type."))?;
            let element_facts = Facts {
                structure: TableStructure {
                    columns: vec![element],
                    ..TableStructure::default()
                },
                maxima: vec![None],
                parents: Vec::new(),
                ..facts.clone()
            };
            let kind = *type_kinds(&element_facts.structure.columns[0])
                .first()
                .ok_or_else(|| format!("{name} holds elements Kerno cannot make."))?;
            let line = defaults(&element_facts, 0, kind);
            Gen::Array(Box::new(gen_of(&element_facts, 0, &line, rows)?))
        }
        Kind::Reference => {
            let (key, part) = facts
                .reference(index)
                .ok_or_else(|| format!("{name} is not part of a foreign key."))?;
            if facts.parents[key].is_empty() {
                let parent = &facts.structure.foreign_keys[key].referenced_table;
                return Err(format!(
                    "{} has no rows, and {name} must hold one of its keys: add rows there first{}.",
                    quoted(&format!("{}.{}", parent.schema, parent.name)),
                    if info.nullable {
                        ", or make it NULL"
                    } else {
                        ""
                    }
                ));
            }
            Gen::Reference { key, part }
        }
    };
    Ok(make)
}

/// `text` cut to at most `limit` characters.
fn fit(text: String, limit: Option<usize>) -> String {
    match limit {
        Some(limit) if text.chars().count() > limit => text.chars().take(limit).collect(),
        _ => text,
    }
}

/// Random words of exactly `length` characters.
fn words(draw: &mut Draw, length: usize) -> String {
    let mut text = String::new();
    while text.len() < length {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(draw.pick(WORDS));
    }
    text.truncate(length);
    // A value never ends with a space: PAD SPACE would take it for another.
    if text.ends_with(' ') {
        text.pop();
        text.push('s');
    }
    text
}

impl Gen {
    fn value(&self, draw: &mut Draw, row: u64) -> CellValue {
        let text = |text: String| CellValue::Text(text);
        match self {
            Gen::Default => CellValue::Default,
            Gen::Null => CellValue::Null,
            Gen::Reference { .. } => CellValue::Null,
            Gen::Sequence { start, step } => text((start + step * i128::from(row)).to_string()),
            Gen::Integer { min, max } => text(draw.between(*min, *max).to_string()),
            Gen::Decimal { min, max, scale } => {
                text(decimal_text(draw.between(*min, *max), *scale))
            }
            Gen::Boolean => text(if draw.below(2) == 1 { "true" } else { "false" }.into()),
            Gen::Words { min, max, number } => {
                let length = draw.between(*min as i128, *max as i128) as usize;
                match number {
                    Some(first) => {
                        let number = (first + row).to_string();
                        let room = length.max(number.len()) - number.len();
                        text(format!("{}{number}", words(draw, room)))
                    }
                    None => text(words(draw, length)),
                }
            }
            Gen::Shape {
                shape,
                limit,
                number,
            } => {
                let number = number.map(|first| first + row);
                let value = shape.value(draw, *limit, number);
                match (number, limit) {
                    // Too long with its number: the start cut, the number kept.
                    (Some(number), Some(limit)) if value.chars().count() > *limit => {
                        let number = number.to_string();
                        let start: String = value.chars().take(limit - number.len()).collect();
                        text(format!("{start}{number}"))
                    }
                    _ => text(fit(value, *limit)),
                }
            }
            Gen::Date { from, to } => text(date_text(
                draw.between(i128::from(*from), i128::from(*to)) as i64,
            )),
            Gen::DateTime { from, to, zone } => {
                let at = draw.between(i128::from(*from), i128::from(*to)) as i64;
                let seconds = at.rem_euclid(86_400);
                text(format!(
                    "{} {:02}:{:02}:{:02}{}",
                    date_text(at.div_euclid(86_400)),
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60,
                    if *zone { "+00" } else { "" }
                ))
            }
            Gen::Time { zone } => {
                let seconds = draw.below(86_400);
                text(format!(
                    "{:02}:{:02}:{:02}{}",
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60,
                    if *zone { "+00" } else { "" }
                ))
            }
            Gen::Interval => {
                let seconds = draw.below(86_400);
                text(format!(
                    "{} days {:02}:{:02}:{:02}",
                    draw.below(366),
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60
                ))
            }
            Gen::Uuid => {
                let mut bytes = [0u8; 16];
                bytes[..8].copy_from_slice(&draw.next().to_le_bytes());
                bytes[8..].copy_from_slice(&draw.next().to_le_bytes());
                text(
                    uuid::Builder::from_random_bytes(bytes)
                        .into_uuid()
                        .to_string(),
                )
            }
            Gen::Json => text(format!(
                "{{\"id\": {}, \"name\": \"{}\", \"active\": {}, \"score\": {}}}",
                draw.between(1, 10_000),
                draw.pick(WORDS),
                draw.below(2) == 1,
                decimal_text(draw.between(0, 10_000), 2)
            )),
            Gen::OneOf(labels) => text(labels[draw.below(labels.len() as u64) as usize].clone()),
            Gen::Bytes { min, max } => {
                let length = draw.between(*min as i128, *max as i128) as usize;
                CellValue::Bytes((0..length).map(|_| draw.next() as u8).collect())
            }
            Gen::Bits { min, max } => {
                let length = draw.between(*min as i128, *max as i128) as usize;
                text(
                    (0..length)
                        .map(|_| if draw.below(2) == 1 { '1' } else { '0' })
                        .collect(),
                )
            }
            Gen::Array(element) => {
                let count = draw.below(4);
                let items: Vec<String> = (0..count)
                    .map(|_| match element.value(draw, row) {
                        CellValue::Text(item) => {
                            format!("\"{}\"", item.replace('\\', "\\\\").replace('"', "\\\""))
                        }
                        _ => "NULL".into(),
                    })
                    .collect();
                text(format!("{{{}}}", items.join(",")))
            }
        }
    }
}

/// The rows of a plan, made one at a time; each unique set's values are
/// remembered to keep them unique.
pub struct Values<'a> {
    plan: &'a Plan,
    next: u64,
    seen: Vec<HashSet<Vec<u8>>>,
}

impl Iterator for Values<'_> {
    type Item = std::result::Result<Vec<CellValue>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.plan.rows {
            return None;
        }
        let row = self.next;
        self.next += 1;
        for attempt in 0..ATTEMPTS {
            let values = self.plan.row(row, attempt);
            let keys: Vec<Option<Vec<u8>>> = self
                .plan
                .uniques
                .iter()
                .map(|places| unique_key(&places.iter().map(|&p| &values[p]).collect::<Vec<_>>()))
                .collect();
            let taken = keys
                .iter()
                .zip(&self.seen)
                .any(|(key, seen)| key.as_ref().is_some_and(|key| seen.contains(key)));
            if !taken {
                for (key, seen) in keys.into_iter().zip(&mut self.seen) {
                    if let Some(key) = key {
                        seen.insert(key);
                    }
                }
                return Some(Ok(values));
            }
        }
        self.next = self.plan.rows;
        Some(Err(format!(
            "Kerno found no new values for a unique key at row {} after {ATTEMPTS} tries: widen \
             its ranges, or ask for fewer rows.",
            grouped(row + 1)
        )))
    }
}

/// The rows in `INSERT` lists.
struct Batches<'a> {
    plan: &'a Plan,
    dialect: &'a dyn Dialect,
    values: Values<'a>,
}

impl Iterator for Batches<'_> {
    /// The number of the batch's first row (from 1), and its rows written.
    type Item = std::result::Result<(u64, Vec<String>), String>;

    fn next(&mut self) -> Option<Self::Item> {
        let first = self.values.next + 1;
        let mut tuples = Vec::new();
        let mut bytes = 0;
        while tuples.len() < BATCH_ROWS && bytes < BATCH_BYTES {
            match self.values.next() {
                None => break,
                Some(Ok(row)) => {
                    let tuple = self.plan.tuple(self.dialect, &row);
                    bytes += tuple.len();
                    tuples.push(tuple);
                }
                Some(Err(problem)) => return Some(Err(problem)),
            }
        }
        (!tuples.is_empty()).then_some(Ok((first, tuples)))
    }
}

/// The value as the preview shows it.
pub fn shown(value: &CellValue) -> String {
    match value {
        CellValue::Null => "NULL".into(),
        CellValue::Default => "DEFAULT".into(),
        CellValue::Text(text) | CellValue::Expression(text) => text.clone(),
        CellValue::Bytes(bytes) => format!("\\x{}", crate::dialect::hex(bytes)),
    }
}

// The insert ----------------------------------------------------------------------------

/// How an insert ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Rows inserted; 0 when it failed.
    pub inserted: u64,
    /// The row the server refused; nothing was kept.
    pub failed: Option<Failure>,
}

/// The row that stopped an insert, from 1, and the server's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub row: u64,
    pub message: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "row {}: {}", grouped(self.row), self.message)
    }
}

async fn run(session: &Arc<dyn Session>, sql: &str) -> Result<()> {
    exec::collect(session.clone(), sql).await.map(|_| ())
}

/// Inserts the rows of `plan` in one transaction: every row, or none.
///
/// The transaction starts, each batch is one multi-row `INSERT` under a
/// savepoint, and it commits. A batch the server refuses is tried again row by
/// row to name the row, then everything is rolled back and the row reported
/// in [`Report::failed`]. `progress` gets the rows inserted after each batch;
/// setting `cancel` rolls back between two batches and returns
/// [`Error::Cancelled`] (the caller also cancels the running statement).
/// Refused before anything runs when the session is in a transaction. Must
/// be called from within the Tokio runtime.
pub async fn insert(
    session: Arc<dyn Session>,
    plan: Plan,
    progress: impl Fn(u64) + Send + 'static,
    cancel: Arc<AtomicBool>,
) -> Result<Report> {
    if session.tx_state() != TxState::Idle {
        return Err(Error::Config(
            "a transaction is already open in this session; commit or roll it back first".into(),
        ));
    }
    let dialect = session.dialect();
    let head = plan.head(dialect);
    let mut inserted = 0;
    let outcome: Result<Option<Failure>> = async {
        run(&session, dialect.begin_sql()).await?;
        // The batches are made here, between two statements: each is a few
        // hundred rows.
        let mut batches = plan.batches(dialect);
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            let (first, tuples) = match batches.next() {
                None => break,
                Some(Ok(batch)) => batch,
                Some(Err(problem)) => return Err(Error::Config(problem)),
            };
            run(&session, &format!("SAVEPOINT {SAVEPOINT}")).await?;
            match run(&session, &format!("{head} {}", tuples.join(", "))).await {
                Ok(()) => {
                    run(&session, &format!("RELEASE SAVEPOINT {SAVEPOINT}")).await?;
                    inserted += tuples.len() as u64;
                    progress(inserted);
                }
                Err(_) if cancel.load(Ordering::Acquire) => return Err(Error::Cancelled),
                Err(error) if error.db().is_some() => {
                    run(&session, &format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}")).await?;
                    for (offset, tuple) in tuples.iter().enumerate() {
                        match run(&session, &format!("{head} {tuple}")).await {
                            Ok(()) => {}
                            Err(_) if cancel.load(Ordering::Acquire) => {
                                return Err(Error::Cancelled);
                            }
                            Err(error) if error.db().is_some() => {
                                return Ok(Some(refused(first + offset as u64, &error)));
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    return Ok(Some(refused(first, &error)));
                }
                Err(error) => return Err(error),
            }
        }
        run(&session, "COMMIT").await?;
        Ok(None)
    }
    .await;
    if let Ok(None) = outcome {
        return Ok(Report {
            inserted,
            failed: None,
        });
    }
    if let Err(error) = run(&session, "ROLLBACK").await {
        tracing::warn!(%error, "rollback after a failed insert of test data did not succeed");
    }
    outcome.map(|failed| Report {
        inserted: 0,
        failed,
    })
}

fn refused(row: u64, error: &Error) -> Failure {
    Failure {
        row,
        message: crate::error::describe(error).to_string(),
    }
}

/// What the dialog says beside the columns: the rules Kerno does not read,
/// and where it picks keys among a part of a referenced table.
pub fn notes(facts: &Facts) -> Vec<String> {
    let mut notes = Vec::new();
    if facts.unread_checks {
        notes.push(
            "The table has CHECK rules Kerno does not read on SQLite: a row that breaks one \
             stops the insert, and nothing is kept."
                .into(),
        );
    }
    for constraint in &facts.structure.constraints {
        let what = match constraint.kind {
            ConstraintKind::Check => "A rule Kerno does not check",
            ConstraintKind::Exclusion => "An exclusion rule Kerno does not check",
            _ => continue,
        };
        notes.push(format!(
            "{what}: {}. A row that breaks it stops the insert, and nothing is kept.",
            constraint.definition
        ));
    }
    let plain = |names: &[String]| names.iter().all(|n| facts.index_of(n).is_some());
    for index in &facts.structure.indexes {
        if index.unique && (index.predicate.is_some() || !plain(&index.columns)) {
            notes.push(format!(
                "A unique index Kerno does not check: {}. A row that breaks it stops the insert, \
                 and nothing is kept.",
                index.name
            ));
        }
    }
    for (fk, keys) in facts.structure.foreign_keys.iter().zip(&facts.parents) {
        let parent = format!(
            "{}.{}",
            fk.referenced_table.schema, fk.referenced_table.name
        );
        if fk.referenced_table == facts.table {
            notes.push(format!(
                "({}) points at rows already in the table.",
                fk.columns.join(", ")
            ));
        }
        if keys.len() >= PARENT_KEYS {
            notes.push(format!(
                "({}) picks among the first {} keys of {}.",
                fk.columns.join(", "),
                grouped(PARENT_KEYS as u64),
                quoted(&parent)
            ));
        }
    }
    for (index, column) in facts.structure.columns.iter().enumerate() {
        if matches!(
            column.generation,
            Some(
                ColumnGeneration::IdentityAlways
                    | ColumnGeneration::IdentityByDefault
                    | ColumnGeneration::AutoIncrement
            )
        ) && facts.unique_alone(index)
        {
            notes.push(format!(
                "{} is numbered by the server.",
                quoted(&column.name)
            ));
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::test_support::AnsiDialect;
    use crate::schema::{ConstraintInfo, ForeignKeyInfo, IndexInfo};

    fn column(name: &str, data_type: &str, kind: TypeKind, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            kind,
            nullable,
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

    fn key(mut info: ColumnInfo, position: u32) -> ColumnInfo {
        info.primary_key_position = Some(position);
        info
    }

    fn unique_index(name: &str, columns: &[&str]) -> IndexInfo {
        IndexInfo {
            name: name.into(),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            unique: true,
            primary: false,
            backs_constraint: true,
            method: "btree".into(),
            predicate: None,
            definition: None,
        }
    }

    fn foreign_key(columns: &[&str], parent: &str, referenced: &[&str]) -> ForeignKeyInfo {
        ForeignKeyInfo {
            name: format!("{parent}_fk"),
            table: TableRef::new("s", "t"),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            referenced_table: TableRef::new("s", parent),
            referenced_columns: referenced.iter().map(|c| c.to_string()).collect(),
            on_update: "NO ACTION".into(),
            on_delete: "NO ACTION".into(),
        }
    }

    fn facts(engine: Engine, columns: Vec<ColumnInfo>) -> Facts {
        let maxima = vec![None; columns.len()];
        Facts {
            engine,
            table: TableRef::new("s", "t"),
            structure: TableStructure {
                columns,
                ..TableStructure::default()
            },
            rows: 0,
            maxima,
            parents: Vec::new(),
            unread_checks: false,
        }
    }

    fn facts_with(info: ColumnInfo) -> Facts {
        facts(Engine::Postgres, vec![info])
    }

    fn text(value: &str) -> CellValue {
        CellValue::Text(value.into())
    }

    fn line(kind: Kind, from: &str, to: &str) -> ColumnSettings {
        ColumnSettings {
            kind,
            from: from.into(),
            to: to.into(),
            nulls: 0,
        }
    }

    fn settings(facts: &Facts, rows: u64, seed: u64) -> Settings {
        Settings {
            rows,
            seed,
            columns: guess(facts),
        }
    }

    /// Every value of the first column over `rows` rows, with `line`.
    fn values_of(facts: &Facts, line: ColumnSettings, rows: u64) -> Vec<CellValue> {
        let mut settings = settings(facts, rows, 7);
        settings.columns[0] = line;
        let plan = Plan::new(facts, &settings).unwrap();
        plan.values().map(|row| row.unwrap().remove(0)).collect()
    }

    fn texts(values: &[CellValue]) -> Vec<String> {
        values.iter().map(shown).collect()
    }

    fn refusal(facts: &Facts, columns: Vec<ColumnSettings>, rows: u64) -> String {
        Plan::new(
            facts,
            &Settings {
                rows,
                seed: 1,
                columns,
            },
        )
        .unwrap_err()
    }

    fn range(line: &ColumnSettings) -> (&str, &str) {
        (line.from.as_str(), line.to.as_str())
    }

    /// A small table of orders: a key, a unique email, a name, a price, a
    /// date, a reference to a customer, a computed total.
    fn orders() -> Facts {
        let mut facts = facts(
            Engine::Postgres,
            vec![
                key(column("id", "integer", TypeKind::Integer, false), 1),
                column("email", "character varying(120)", TypeKind::Text, false),
                column("name", "text", TypeKind::Text, true),
                column("price", "numeric(8,2)", TypeKind::Decimal, true),
                column("placed", "date", TypeKind::Date, true),
                column("customer_id", "integer", TypeKind::Integer, false),
                ColumnInfo {
                    generation: Some(ColumnGeneration::Stored("price * 2".into())),
                    ..column("total", "numeric", TypeKind::Decimal, true)
                },
            ],
        );
        facts.structure.indexes = vec![unique_index("orders_email", &["email"])];
        facts.structure.foreign_keys = vec![foreign_key(&["customer_id"], "customers", &["id"])];
        facts.parents = vec![vec![vec![text("10")], vec![text("20")], vec![text("30")]]];
        facts.maxima[0] = Some(41);
        facts.rows = 3;
        facts
    }

    #[test]
    fn the_guess_follows_types_keys_and_names() {
        let facts = orders();
        let guessed = guess(&facts);
        let kinds_guessed: Vec<Kind> = guessed.iter().map(|c| c.kind).collect();
        assert_eq!(
            kinds_guessed,
            [
                Kind::Sequence,
                Kind::Shape(Shape::Email),
                Kind::Shape(Shape::FullName),
                Kind::Decimal,
                Kind::Date,
                Kind::Reference,
                Kind::Server,
            ]
        );
        // The sequence starts beyond the largest key there.
        assert_eq!(range(&guessed[0]), ("42", "1"));
        // A key is never NULL; a nullable column outside every key 10 %.
        let nulls: Vec<u8> = guessed.iter().map(|c| c.nulls).collect();
        assert_eq!(nulls, [0, 0, 10, 10, 10, 0, 0]);
        assert_eq!(range(&guessed[3]), ("0.00", "1000.00"));
        assert_eq!(range(&guessed[4]), (DATE_FROM, DATE_TO));
        // What each column can get instead.
        assert_eq!(kinds(&facts, 6), [Kind::Server]);
        assert_eq!(
            kinds(&facts, 5),
            [Kind::Reference, Kind::Sequence, Kind::Integer]
        );
        assert!(kinds(&facts, 2).ends_with(&[Kind::Uuid, Kind::Json, Kind::Null]));
        // A short text column is not given a shape its values do not fit.
        let short = facts_with(column("email", "varchar(12)", TypeKind::Text, false));
        assert_eq!(guess(&short)[0].kind, Kind::Words);
        assert_eq!(range(&guess(&short)[0]), ("5", "12"));
        for (name, shape) in [
            ("first_name", Shape::FirstName),
            ("surname", Shape::LastName),
            ("mobile_phone", Shape::Phone),
            ("city", Shape::City),
            ("country_code", Shape::Country),
            ("website", Shape::Url),
            ("username", Shape::Username),
        ] {
            let facts = facts_with(column(name, "text", TypeKind::Text, false));
            assert_eq!(guess(&facts)[0].kind, Kind::Shape(shape), "{name}");
        }
        let facts = facts_with(column("public_uuid", "char(36)", TypeKind::Text, false));
        assert_eq!(guess(&facts)[0].kind, Kind::Uuid);
        // A column with a default can be left to it.
        let facts = facts_with(ColumnInfo {
            default: Some("now()".into()),
            ..column(
                "at",
                "timestamp with time zone",
                TypeKind::DateTimeTz,
                false,
            )
        });
        assert_eq!(kinds(&facts, 0), [Kind::DateTime, Kind::Default]);
    }

    #[test]
    fn the_same_seed_gives_the_same_rows_and_another_seed_others() {
        let facts = orders();
        let rows = |seed| {
            Plan::new(&facts, &settings(&facts, 50, seed))
                .unwrap()
                .values()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
        };
        assert_eq!(rows(5), rows(5));
        assert_ne!(rows(5), rows(6));
        // The preview is the first rows of the run.
        let plan = Plan::new(&facts, &settings(&facts, 50, 5)).unwrap();
        assert_eq!(plan.preview(PREVIEW_ROWS).unwrap(), rows(5)[..PREVIEW_ROWS]);
        // A column's values do not move when another column's settings do.
        let mut changed = settings(&facts, 50, 5);
        changed.columns[3] = line(Kind::Decimal, "5", "6");
        let other = Plan::new(&facts, &changed).unwrap();
        let column = |plan: &Plan, index: usize| -> Vec<CellValue> {
            plan.values()
                .map(|row| row.unwrap()[index].clone())
                .collect()
        };
        assert_eq!(column(&plan, 2), column(&other, 2));
        assert_ne!(column(&plan, 3), column(&other, 3));
        // The computed total is not written.
        assert_eq!(
            plan.names(),
            ["id", "email", "name", "price", "placed", "customer_id"]
        );
    }

    #[test]
    fn numbers_stay_in_their_range_and_their_type() {
        let int = |data_type: &str| facts_with(column("n", data_type, TypeKind::Integer, false));
        let values = values_of(&int("integer"), line(Kind::Integer, "-5", "5"), 2000);
        let numbers: Vec<i128> = texts(&values).iter().map(|v| v.parse().unwrap()).collect();
        assert!(numbers.iter().all(|n| (-5..=5).contains(n)));
        assert!(numbers.contains(&-5) && numbers.contains(&5));
        let values = values_of(&int("bigint"), line(Kind::Sequence, "100", "-3"), 4);
        assert_eq!(texts(&values), ["100", "97", "94", "91"]);
        // The type's own bounds, and the range guessed within them.
        for (data_type, engine, (low, high)) in [
            ("smallint", Engine::Postgres, (-32768, 32767)),
            ("integer", Engine::Postgres, (-2147483648, 2147483647)),
            ("tinyint(4)", Engine::MySql, (-128, 127)),
            ("tinyint unsigned", Engine::MySql, (0, 255)),
            ("mediumint", Engine::MariaDb, (-8388608, 8388607)),
            ("bigint unsigned", Engine::MySql, (0, 18446744073709551615)),
            ("year", Engine::MySql, (1901, 2155)),
            (
                "INTEGER",
                Engine::Sqlite,
                (i128::from(i64::MIN), i128::from(i64::MAX)),
            ),
        ] {
            let mut facts = int(data_type);
            facts.engine = engine;
            assert_eq!(
                integer_bounds(&facts, facts.column(0)),
                (low, high),
                "{data_type}"
            );
            let too_far = line(Kind::Integer, &low.to_string(), &(high + 1).to_string());
            assert!(
                refusal(&facts, vec![too_far], 1).contains(&format!("holds {low} to {high}")),
                "{data_type}"
            );
            Plan::new(&facts, &settings(&facts, 10, 1)).unwrap();
        }
        let mut year = int("year");
        year.engine = Engine::MySql;
        assert_eq!(range(&guess(&year)[0]), ("2000", "2030"));
        // A sequence that would leave a smallint is refused before a row is made.
        assert_eq!(
            refusal(
                &int("smallint"),
                vec![line(Kind::Sequence, "32000", "1")],
                1000
            ),
            "“n” (smallint) holds -32768 to 32767: a sequence from 32000 by 1 leaves it within \
             1,000 rows."
        );
        let fits = Settings {
            rows: 768,
            seed: 1,
            columns: vec![line(Kind::Sequence, "32000", "1")],
        };
        Plan::new(&int("smallint"), &fits).unwrap();
        assert!(
            refusal(&int("integer"), vec![line(Kind::Sequence, "1", "0")], 3)
                .contains("cannot be 0")
        );
        // Decimals: the column's scale and precision.
        let price = facts_with(column("p", "numeric(5,2)", TypeKind::Decimal, false));
        let values = texts(&values_of(
            &price,
            line(Kind::Decimal, "-1.5", "999.99"),
            3000,
        ));
        assert!(values.iter().all(|v| {
            let n: f64 = v.parse().unwrap();
            v.split_once('.').unwrap().1.len() == 2 && (-1.5..=999.99).contains(&n)
        }));
        assert_eq!(
            refusal(&price, vec![line(Kind::Decimal, "0", "1000")], 1),
            "“p” (numeric(5,2)) holds -999.99 to 999.99."
        );
        assert!(
            refusal(&price, vec![line(Kind::Decimal, "0", "1.234")], 1)
                .contains("at most 2 decimals")
        );
        let unsigned = column("p", "decimal(4,1) unsigned", TypeKind::Decimal, false);
        assert_eq!(decimal_bounds(&unsigned), (1, 0, 9999));
        let real = facts_with(column("r", "double precision", TypeKind::Float, false));
        assert_eq!(guess(&real)[0].to, "1000.00");
    }

    #[test]
    fn text_has_its_length_and_its_shape() {
        let note = facts_with(column("note", "varchar(20)", TypeKind::Text, false));
        let values = texts(&values_of(&note, line(Kind::Words, "3", "20"), 2000));
        assert!(
            values
                .iter()
                .all(|v| (3..=20).contains(&v.len()) && !v.ends_with(' '))
        );
        assert!(values.iter().any(|v| v.len() == 3) && values.iter().any(|v| v.len() == 20));
        assert_eq!(
            refusal(&note, vec![line(Kind::Words, "3", "30")], 1),
            "“note” holds at most 20 characters: lower its max length."
        );
        let shape = |shape: Shape, data_type: &str| {
            texts(&values_of(
                &facts_with(column("x", data_type, TypeKind::Text, false)),
                line(Kind::Shape(shape), "", ""),
                300,
            ))
        };
        for email in shape(Shape::Email, "text") {
            let (local, domain) = email.split_once('@').unwrap();
            assert!(local.contains('.') && DOMAINS.contains(&domain), "{email}");
        }
        for phone in shape(Shape::Phone, "text") {
            assert!(
                phone.starts_with("+1 ") && phone.contains(" 555 01"),
                "{phone}"
            );
            assert_eq!(phone.len(), 15, "{phone}");
        }
        assert!(
            shape(Shape::Country, "char(2)")
                .iter()
                .all(|c| c.len() == 2)
        );
        assert!(shape(Shape::Country, "text").iter().any(|c| c.len() > 2));
        assert!(
            shape(Shape::Url, "text")
                .iter()
                .all(|u| u.starts_with("https://") && u.contains(".example.com/"))
        );
        assert!(
            shape(Shape::FullName, "varchar(8)")
                .iter()
                .all(|n| n.chars().count() <= 8)
        );
        for (one, list) in [
            (Shape::City, CITIES),
            (Shape::FirstName, FIRST_NAMES),
            (Shape::LastName, LAST_NAMES),
        ] {
            assert!(
                shape(one, "text")
                    .iter()
                    .all(|v| list.contains(&v.as_str())),
                "{one:?}"
            );
        }
        assert!(
            shape(Shape::Username, "text")
                .iter()
                .all(|u| u.contains('_'))
        );
    }

    #[test]
    fn dates_times_and_the_other_types() {
        let one = |data_type: &str, kind: TypeKind, line: ColumnSettings| {
            texts(&values_of(
                &facts_with(column("x", data_type, kind, false)),
                line,
                2000,
            ))
        };
        let dates = one(
            "date",
            TypeKind::Date,
            line(Kind::Date, "2024-02-28", "2024-03-01"),
        );
        assert!(
            dates
                .iter()
                .all(|d| ["2024-02-28", "2024-02-29", "2024-03-01"].contains(&d.as_str()))
        );
        assert!(dates.contains(&"2024-02-29".to_string()));
        let stamps = one(
            "timestamp with time zone",
            TypeKind::DateTimeTz,
            line(Kind::DateTime, "2021-05-01", "2021-05-01"),
        );
        assert!(
            stamps
                .iter()
                .all(|s| s.starts_with("2021-05-01 ") && s.ends_with("+00") && s.len() == 22),
            "{:?}",
            &stamps[..3]
        );
        let naive = one(
            "timestamp without time zone",
            TypeKind::DateTime,
            line(Kind::DateTime, "2021-05-01", "2021-05-02"),
        );
        assert!(naive.iter().all(|s| s.len() == 19));
        assert!(naive.iter().any(|s| s.starts_with("2021-05-02")));
        let times = one(
            "time with time zone",
            TypeKind::Time,
            line(Kind::Time, "", ""),
        );
        assert!(times.iter().all(|t| t.len() == 11 && t.ends_with("+00")));
        let day = facts_with(column("d", "date", TypeKind::Date, false));
        assert!(
            refusal(&day, vec![line(Kind::Date, "2024-02-30", "2024-03-01")], 1)
                .contains("not a YYYY-MM-DD date")
        );
        assert!(
            refusal(&day, vec![line(Kind::Date, "2024-03-01", "2024-02-01")], 1)
                .contains("after its to date")
        );
        // MySQL's TIMESTAMP: 1970 to 2038.
        let stamp = facts(
            Engine::MySql,
            vec![column("t", "timestamp", TypeKind::DateTime, false)],
        );
        assert_eq!(
            refusal(
                &stamp,
                vec![line(Kind::DateTime, "2020-01-01", "2040-01-01")],
                1
            ),
            "“t” is a TIMESTAMP, which holds 1970-01-02 to 2038-01-18 here."
        );
        for uuid in one("uuid", TypeKind::Uuid, line(Kind::Uuid, "", "")) {
            assert_eq!(uuid::Uuid::parse_str(&uuid).unwrap().get_version_num(), 4);
        }
        for json in one("jsonb", TypeKind::Json, line(Kind::Json, "", "")) {
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(
                value["name"].is_string() && value["active"].is_boolean(),
                "{json}"
            );
        }
        let mood = ColumnInfo {
            enum_values: vec!["sad".into(), "ok".into(), "happy".into()],
            ..column("m", "mood", TypeKind::Enum, false)
        };
        let labels = texts(&values_of(
            &facts_with(mood.clone()),
            line(Kind::OneOf, "", ""),
            300,
        ));
        assert!(labels.iter().all(|l| mood.enum_values.contains(l)));
        assert_eq!(labels.iter().collect::<HashSet<_>>().len(), 3);
        let bytes = values_of(
            &facts(
                Engine::MySql,
                vec![column("b", "varbinary(16)", TypeKind::Binary, false)],
            ),
            line(Kind::Bytes, "4", "16"),
            500,
        );
        assert!(
            bytes
                .iter()
                .all(|b| matches!(b, CellValue::Bytes(b) if (4..=16).contains(&b.len())))
        );
        let binary = facts(
            Engine::MySql,
            vec![column("b", "binary(16)", TypeKind::Binary, false)],
        );
        assert_eq!(range(&guess(&binary)[0]), ("16", "16"));
        assert!(
            refusal(&binary, vec![line(Kind::Bytes, "4", "17")], 1).contains("at most 16 bytes")
        );
        let bits = one("bit(5)", TypeKind::Bit, line(Kind::Bits, "", ""));
        assert!(
            bits.iter()
                .all(|b| b.len() == 5 && b.chars().all(|c| c == '0' || c == '1'))
        );
        let varbits = one("bit varying(3)", TypeKind::Bit, line(Kind::Bits, "", ""));
        assert!(varbits.iter().all(|b| (1..=3).contains(&b.len())));
        let intervals = one("interval", TypeKind::Interval, line(Kind::Interval, "", ""));
        assert!(intervals.iter().all(|i| i.contains(" days ")));
        let booleans = one("boolean", TypeKind::Boolean, line(Kind::Boolean, "", ""));
        assert_eq!(booleans.iter().collect::<HashSet<_>>().len(), 2);
        // Arrays of what their element gets, elements quoted as arrays write them.
        let arrays = one("integer[]", TypeKind::Array, line(Kind::Array, "", ""));
        assert!(
            arrays
                .iter()
                .all(|a| a.starts_with('{') && a.ends_with('}'))
        );
        assert!(arrays.iter().any(|a| a == "{}"));
        assert!(arrays.iter().any(|a| a.matches(',').count() == 2));
        let words = one("text[]", TypeKind::Array, line(Kind::Array, "", ""));
        assert!(
            words
                .iter()
                .any(|a| a.starts_with("{\"") && a.contains(' '))
        );
        // A type Kerno cannot make, in a column that cannot be NULL: said.
        let point = facts_with(column("p", "point", TypeKind::Other, false));
        assert_eq!(guess(&point)[0].kind, Kind::Null);
        assert_eq!(
            refusal(&point, guess(&point), 1),
            "Kerno cannot make values of type point for “p”, which cannot be NULL and has no \
             default."
        );
        let nullable = facts_with(column("p", "point", TypeKind::Other, true));
        Plan::new(&nullable, &settings(&nullable, 3, 1)).unwrap();
    }

    #[test]
    fn nulls_come_in_their_share_and_only_where_allowed() {
        let facts = facts_with(column("n", "integer", TypeKind::Integer, true));
        let mut share = line(Kind::Integer, "1", "9");
        share.nulls = 10;
        let values = values_of(&facts, share.clone(), 10_000);
        let nulls = values.iter().filter(|v| **v == CellValue::Null).count();
        assert!((800..1200).contains(&nulls), "{nulls}");
        share.nulls = 0;
        assert!(!values_of(&facts, share.clone(), 1000).contains(&CellValue::Null));
        share.nulls = 100;
        assert!(
            values_of(&facts, share.clone(), 100)
                .iter()
                .all(|v| *v == CellValue::Null)
        );
        share.nulls = 101;
        assert!(refusal(&facts, vec![share.clone()], 1).contains("0 to 100"));
        let required = facts_with(column("n", "integer", TypeKind::Integer, false));
        share.nulls = 5;
        assert_eq!(refusal(&required, vec![share], 1), "“n” cannot be NULL.");
    }

    #[test]
    fn unique_keys_never_repeat_and_impossible_ones_are_refused() {
        // A unique integer in a range: every value once, then refused.
        let mut facts = facts_with(column("n", "integer", TypeKind::Integer, false));
        facts.structure.indexes = vec![unique_index("n_key", &["n"])];
        let values = texts(&values_of(&facts, line(Kind::Integer, "1", "50"), 50));
        assert_eq!(values.iter().collect::<HashSet<_>>().len(), 50);
        assert_eq!(
            refusal(&facts, vec![line(Kind::Integer, "1", "50")], 51),
            "(n) must be different on every row, and these settings give at most 50 different \
             values for 51 rows: widen a range, or ask for fewer rows."
        );
        // A unique text ends with a number beyond the rows there, and so does
        // a unique shape; compared without case, as a `_ci` server does.
        let mut code = facts_with(column("code", "varchar(12)", TypeKind::Text, false));
        code.structure.indexes = vec![unique_index("code_key", &["code"])];
        code.rows = 41;
        let values = texts(&values_of(&code, line(Kind::Words, "1", "4"), 200));
        assert!(
            values[0].ends_with("42") && values[199].ends_with("241"),
            "{:?}",
            &values[..3]
        );
        assert!(values.iter().all(|v| v.len() <= 4));
        let folded: HashSet<String> = values.iter().map(|v| v.to_lowercase()).collect();
        assert_eq!(folded.len(), 200);
        assert!(
            refusal(&code, vec![line(Kind::Words, "1", "2")], 200).contains("needs 3 characters")
        );
        let mut email = facts_with(column("email", "varchar(26)", TypeKind::Text, false));
        email.structure.indexes = vec![unique_index("email_key", &["email"])];
        let emails = texts(&values_of(
            &email,
            line(Kind::Shape(Shape::Email), "", ""),
            2000,
        ));
        assert_eq!(emails.iter().collect::<HashSet<_>>().len(), 2000);
        assert!(emails.iter().all(|e| e.chars().count() <= 26));
        assert!(
            emails.iter().any(|e| e.contains("1000@")),
            "{:?}",
            &emails[..3]
        );
        // A unique flag: two rows at most.
        let mut flag = facts_with(column("f", "boolean", TypeKind::Boolean, false));
        flag.structure.indexes = vec![unique_index("f_key", &["f"])];
        assert!(refusal(&flag, guess(&flag), 3).contains("at most 2 different values"));
        assert_eq!(unique_key(&[&text("Ab")]), unique_key(&[&text("aB")]));
        assert_eq!(unique_key(&[&CellValue::Null]), None);
    }

    /// A junction table: its key is two foreign keys, each pair once; more
    /// rows than pairs is refused; a parent without rows is said first.
    #[test]
    fn a_junction_table_takes_each_pair_of_keys_once() {
        let mut facts = facts(
            Engine::Postgres,
            vec![
                key(column("a", "integer", TypeKind::Integer, false), 1),
                key(column("b", "integer", TypeKind::Integer, false), 2),
            ],
        );
        facts.structure.foreign_keys = vec![
            foreign_key(&["a"], "pa", &["id"]),
            foreign_key(&["b"], "pb", &["id"]),
        ];
        facts.parents = vec![
            vec![vec![text("1")], vec![text("2")], vec![text("3")]],
            vec![vec![text("7")], vec![text("8")]],
        ];
        assert_eq!(guess(&facts)[0].kind, Kind::Reference);
        let plan = Plan::new(&facts, &settings(&facts, 6, 3)).unwrap();
        let pairs: HashSet<Vec<String>> = plan.values().map(|row| texts(&row.unwrap())).collect();
        assert_eq!(pairs.len(), 6);
        assert!(
            refusal(&facts, guess(&facts), 7).starts_with("(a, b) must be different on every row")
        );
        facts.parents[1].clear();
        assert_eq!(
            refusal(&facts, guess(&facts), 1),
            "“s.pb” has no rows, and “b” must hold one of its keys: add rows there first."
        );
    }

    /// A composite foreign key is picked as a whole; a nullable one whose
    /// parent is empty is left NULL.
    #[test]
    fn a_composite_reference_is_one_key_of_the_parent() {
        let mut facts = facts(
            Engine::Postgres,
            vec![
                key(column("id", "integer", TypeKind::Integer, false), 1),
                column("x", "integer", TypeKind::Integer, false),
                column("y", "text", TypeKind::Text, false),
                column("z", "integer", TypeKind::Integer, true),
            ],
        );
        facts.structure.foreign_keys = vec![
            foreign_key(&["x", "y"], "pair", &["px", "py"]),
            foreign_key(&["z"], "empty", &["id"]),
        ];
        facts.parents = vec![
            (0..5)
                .map(|n| vec![text(&n.to_string()), text(&format!("k{n}"))])
                .collect(),
            Vec::new(),
        ];
        let guessed = guess(&facts);
        assert_eq!(guessed[3].kind, Kind::Null);
        let plan = Plan::new(&facts, &settings(&facts, 300, 9)).unwrap();
        for row in plan.values().map(Result::unwrap) {
            assert_eq!(format!("k{}", shown(&row[1])), shown(&row[2]));
            assert_eq!(row[3], CellValue::Null);
        }
        let mut reference = guessed.clone();
        reference[3] = line(Kind::Reference, "", "");
        assert!(refusal(&facts, reference, 1).ends_with("add rows there first, or make it NULL."));
    }

    #[test]
    fn the_script_writes_batches_with_the_dialect_s_literals() {
        let facts = orders();
        let plan = Plan::new(&facts, &settings(&facts, 1201, 4)).unwrap();
        // The heading is commented line by line: a name holding a line
        // break never leaves SQL out of its comment.
        let script = plan
            .script(&AnsiDialect, "Test data\nDROP TABLE x;")
            .unwrap();
        assert!(
            script.starts_with(
                "-- Test data\n-- DROP TABLE x;\n\nINSERT INTO \"s\".\"t\" (\"id\", \"email\", \
                 \"name\", \"price\", \"placed\", \"customer_id\") VALUES\n  (42, '"
            ),
            "{}",
            &script[..300]
        );
        // 500, 500 and 201 rows: three statements.
        assert_eq!(script.matches("INSERT INTO").count(), 3);
        assert_eq!(script.matches("\n  (").count(), 1201);
        assert!(script.ends_with(");\n"));
        // The rows are those of the run, whatever the batches.
        let first: Vec<String> = plan
            .values()
            .take(2)
            .map(|row| plan.tuple(&AnsiDialect, &row.unwrap()))
            .collect();
        assert!(script.contains(&format!("{},\n  {}", first[0], first[1])));
        let batches: Vec<(u64, usize)> = plan
            .batches(&AnsiDialect)
            .map(|batch| {
                let (start, tuples) = batch.unwrap();
                (start, tuples.len())
            })
            .collect();
        assert_eq!(batches, [(1, 500), (501, 500), (1001, 201)]);
        // What the confirmation shows.
        let statement = plan.first_statement(&AnsiDialect, 2).unwrap();
        assert!(
            statement.ends_with(");\n-- … and 1,199 more rows"),
            "{statement}"
        );
        // Past the cap the script is refused, Insert offered.
        let big = Plan::new(&facts, &settings(&facts, MAX_SCRIPT_ROWS + 1, 4)).unwrap();
        assert_eq!(
            big.script(&AnsiDialect, "x").unwrap_err(),
            "A script of more than 10,000 rows is too long for the editor: use Insert, which \
             takes up to 1,000,000."
        );
        assert!(
            refusal(&facts, guess(&facts), MAX_ROWS + 1).starts_with("Ask for 1 to 1,000,000 rows")
        );
        assert!(refusal(&facts, guess(&facts), 0).starts_with("Ask for 1"));
        // Bytes are written by the dialect's byte literal.
        let blob = facts_with(column("b", "BLOB", TypeKind::Binary, false));
        let plan = Plan::new(&blob, &settings(&blob, 1, 1)).unwrap();
        assert!(
            plan.script(&AnsiDialect, "x")
                .unwrap()
                .contains("VALUES\n  (X'")
        );
    }

    /// Every column left to the server: the rows are still made, each as
    /// `DEFAULT` for the first column.
    #[test]
    fn a_table_the_server_fills_alone_gets_rows_of_defaults() {
        let facts = facts_with(ColumnInfo {
            generation: Some(ColumnGeneration::IdentityAlways),
            ..key(column("id", "integer", TypeKind::Integer, false), 1)
        });
        let plan = Plan::new(&facts, &settings(&facts, 3, 1)).unwrap();
        assert_eq!(
            plan.script(&AnsiDialect, "x").unwrap(),
            "-- x\n\nINSERT INTO \"s\".\"t\" (\"id\") VALUES\n  (DEFAULT),\n  (DEFAULT),\n  \
             (DEFAULT);\n"
        );
        assert_eq!(notes(&facts), ["“id” is numbered by the server."]);
    }

    #[test]
    fn rules_kerno_does_not_check_are_said() {
        let mut facts = orders();
        facts.structure.constraints = vec![ConstraintInfo {
            name: "positive".into(),
            kind: ConstraintKind::Check,
            columns: vec!["price".into()],
            definition: "CHECK ((price >= 0))".into(),
        }];
        facts.structure.indexes.push(IndexInfo {
            predicate: Some("name IS NOT NULL".into()),
            ..unique_index("partial", &["name"])
        });
        facts.parents[0] = (0..PARENT_KEYS)
            .map(|n| vec![text(&n.to_string())])
            .collect();
        assert_eq!(
            notes(&facts),
            [
                "A rule Kerno does not check: CHECK ((price >= 0)). A row that breaks it stops \
                 the insert, and nothing is kept.",
                "A unique index Kerno does not check: partial. A row that breaks it stops the \
                 insert, and nothing is kept.",
                "(customer_id) picks among the first 10,000 keys of “s.customers”.",
            ]
        );
        // MariaDB's JSON is a text checked by json_valid(): it gets JSON.
        let mut maria = facts_with(column("doc", "longtext", TypeKind::Text, true));
        maria.engine = Engine::MariaDb;
        maria.structure.constraints = vec![ConstraintInfo {
            name: "doc".into(),
            kind: ConstraintKind::Check,
            columns: Vec::new(),
            definition: "CHECK (json_valid(`doc`))".into(),
        }];
        assert_eq!(guess(&maria)[0].kind, Kind::Json);
        // SQLite lists no CHECK apart: one in the table's SQL is said.
        assert!(has_check("CREATE TABLE t (m TEXT CHECK (m IN ('a')))"));
        assert!(has_check("create table t (n int, check(n > 0))"));
        assert!(!has_check("CREATE TABLE t (checked INT, recheck (x))"));
        let mut lite = facts_with(column("m", "TEXT", TypeKind::Text, false));
        lite.engine = Engine::Sqlite;
        lite.unread_checks = true;
        assert_eq!(
            notes(&lite),
            [
                "The table has CHECK rules Kerno does not read on SQLite: a row that breaks one \
              stops the insert, and nothing is kept."
            ]
        );
    }

    #[test]
    fn numbers_dates_and_seeds_are_read_and_written_exactly() {
        assert_eq!(parse_decimal("12.5", 2), Some(1250));
        assert_eq!(parse_decimal("-0.05", 2), Some(-5));
        assert_eq!(parse_decimal(".5", 1), Some(5));
        assert_eq!(parse_decimal("1e3", 2), None);
        assert_eq!(parse_decimal("", 2), None);
        assert_eq!(decimal_text(-5, 2), "-0.05");
        assert_eq!(decimal_text(1250, 2), "12.50");
        assert_eq!(decimal_text(7, 0), "7");
        assert_eq!(parse_date("1970-01-02"), Some(1));
        assert_eq!(date_text(parse_date("2024-02-29").unwrap()), "2024-02-29");
        assert_eq!(parse_date("2023-02-29"), None);
        assert_eq!(grouped(1_000_000), "1,000,000");
        assert_eq!(grouped(999), "999");
        assert!(new_seed() < 1_000_000);
        // The same keys draw the same numbers; a range's ends are reached.
        assert_eq!(Draw::new(1, 2, 3, 0).next(), Draw::new(1, 2, 3, 0).next());
        assert_ne!(Draw::new(1, 2, 3, 0).next(), Draw::new(1, 2, 4, 0).next());
        let mut draw = Draw::new(9, 9, 9, 9);
        let drawn: HashSet<i128> = (0..200).map(|_| draw.between(-1, 1)).collect();
        assert_eq!(drawn, HashSet::from([-1, 0, 1]));
        let wide = draw.between(i128::from(i64::MIN), i128::from(u64::MAX));
        assert!(wide >= i128::from(i64::MIN) && wide <= i128::from(u64::MAX));
    }

    /// K76: the same settings give the same rows, except the values that must be unique, which
    /// continue after the rows already in the table: a second run with the same seed draws none
    /// of the first run's keys again, and keeps every other value.
    #[test]
    fn unique_values_continue_after_the_rows_there_and_the_rest_repeats() {
        let mut facts = facts(
            Engine::Postgres,
            vec![
                key(column("id", "uuid", TypeKind::Uuid, false), 1),
                column("code", "integer", TypeKind::Integer, false),
                column("label", "text", TypeKind::Text, false),
                column("pa", "integer", TypeKind::Integer, false),
                column("pb", "integer", TypeKind::Integer, false),
            ],
        );
        facts.structure.indexes = vec![
            unique_index("code_key", &["code"]),
            unique_index("pair_key", &["pa", "pb"]),
        ];
        facts.structure.foreign_keys = vec![
            foreign_key(&["pa"], "a", &["id"]),
            foreign_key(&["pb"], "b", &["id"]),
        ];
        let keys = |n: u32| (1..=n).map(|k| vec![text(&k.to_string())]).collect();
        facts.parents = vec![keys(30), keys(30)];
        let run = |facts: &Facts| -> Vec<Vec<CellValue>> {
            let mut settings = settings(facts, 50, 11);
            settings.columns[1] = line(Kind::Integer, "1", "1000000");
            Plan::new(facts, &settings)
                .unwrap()
                .values()
                .map(Result::unwrap)
                .collect()
        };
        // An empty table twice: the same rows.
        let first = run(&facts);
        assert_eq!(first, run(&facts));
        // The same seed on the table those rows went into.
        facts.rows = 50;
        let second = run(&facts);
        let column = |rows: &[Vec<CellValue>], i: usize| -> HashSet<String> {
            rows.iter().map(|row| shown(&row[i])).collect()
        };
        for (i, name) in [(0, "id"), (1, "code")] {
            assert!(
                column(&first, i).is_disjoint(&column(&second, i)),
                "{name} drew the first run's values again"
            );
        }
        let pairs = |rows: &[Vec<CellValue>]| -> Vec<(String, String)> {
            rows.iter()
                .map(|row| (shown(&row[3]), shown(&row[4])))
                .collect()
        };
        assert!(
            pairs(&first) != pairs(&second),
            "the pairs drew the same keys again"
        );
        // What need not be unique is drawn as before.
        let labels = |rows: &[Vec<CellValue>]| -> Vec<CellValue> {
            rows.iter().map(|row| row[2].clone()).collect()
        };
        assert_eq!(labels(&first), labels(&second));
    }

    /// K76: a SQLite text column that holds a date or a time is guessed from its default, left
    /// to the default when it has one, and otherwise from its name.
    #[test]
    fn a_sqlite_text_column_of_dates_is_guessed_from_its_default_and_its_name() {
        let text_column = |name: &str, default: Option<&str>| ColumnInfo {
            default: default.map(Into::into),
            ..column(name, "TEXT", TypeKind::Text, true)
        };
        let sqlite = |info| facts(Engine::Sqlite, vec![info]);
        for (name, default, time) in [
            ("created_at", Some("CURRENT_TIMESTAMP"), Kind::DateTime),
            ("made", Some("datetime('now')"), Kind::DateTime),
            ("day", Some("CURRENT_DATE"), Kind::Date),
            ("day", Some("'2024-05-01'"), Kind::Date),
            ("clock", Some("current_time"), Kind::Time),
        ] {
            let facts = sqlite(text_column(name, default));
            assert_eq!(text_time(facts.column(0)), Some(time), "{default:?}");
            assert_eq!(guess(&facts)[0].kind, Kind::Default, "{default:?}");
            assert!(kinds(&facts, 0).contains(&time));
        }
        for (name, time) in [
            ("updated_at", Kind::DateTime),
            ("timestamp", Kind::DateTime),
            ("birth_date", Kind::Date),
            ("date", Kind::Date),
            ("time", Kind::Time),
            // K77: a time of day by its name; `*_at`, `timestamp`, `datetime` stay a date
            // and time, `*_date` a date.
            ("at_time", Kind::Time),
            ("start_time", Kind::Time),
            ("time_of_day", Kind::Time),
            ("opens_at_time", Kind::Time),
            ("due_date", Kind::Date),
            ("starts_at", Kind::DateTime),
            ("time_at", Kind::DateTime),
            ("datetime", Kind::DateTime),
            ("created_datetime", Kind::DateTime),
            ("event_date_time", Kind::DateTime),
            ("sent_timestamp", Kind::DateTime),
        ] {
            let facts = sqlite(text_column(name, None));
            let guessed = &guess(&facts)[0];
            assert_eq!(guessed.kind, time, "{name}");
            // Rows written: the time drawn as SQLite's CURRENT_TIMESTAMP writes it.
            let line = ColumnSettings {
                nulls: 0,
                ..guessed.clone()
            };
            let values = texts(&values_of(&facts, line, 5));
            let width = match time {
                Kind::DateTime => 19,
                Kind::Date => 10,
                _ => 8,
            };
            assert!(values.iter().all(|v| v.len() == width), "{values:?}");
        }
        // A plain text keeps its words; another engine's text is not a date.
        for name in ["note", "time_zone", "time_zone_name", "timezone"] {
            assert_eq!(
                guess(&sqlite(text_column(name, None)))[0].kind,
                Kind::Words,
                "{name}"
            );
        }
        let elsewhere = facts_with(text_column("created_at", Some("'2024-05-01'")));
        assert!(!kinds(&elsewhere, 0).contains(&Kind::Date));
        assert_eq!(guess(&elsewhere)[0].kind, Kind::Words);
    }
}
