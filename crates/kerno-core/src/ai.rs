//! The AI assistant (CLAUDE.md §4 J17, docs/DECISIONS.md D-068): the second and last exception
//! to "nothing leaves the machine". Requests go only to the endpoint the user configured, only
//! when they ask, never while the assistant is off. That holds by construction:
//!
//! - [`build_payload`] is the only way a request body is made, and [`body`] the only way it
//!   becomes bytes: the "What is sent" preview shows exactly those bytes.
//! - [`request`] is the only function that opens a socket. It takes the settings and refuses
//!   while the assistant is off, when the address is not `https://` (plain `http://` is
//!   allowed to this computer only), and when a host other than this computer was not
//!   confirmed. It never goes through a proxy and never follows a redirect.
//! - Row data enters only through [`Context::sample_rows`], which the app fills only when the
//!   per-request "include sample rows" tick is on.
//!
//! The answer is untrusted text: nothing in it runs or opens anything ([`sql_block`] only
//! picks the SQL the user may insert).

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Secret;
use crate::dialect::Dialect;
use crate::driver::Session;
use crate::schema::{RelationKind, TableRef, TableStructure};
use crate::sql::Catalog;

pub mod chat;
pub mod endpoints;
mod tls;

pub use chat::{
    MAX_QUERY, MAX_TURN_CHARS, MAX_TURNS, Role, Segment, Turn, build_chat, left_out, segments,
    with_query,
};
pub use endpoints::{Model, Reach, SavedEndpoint};

/// How long a chat request may take, from connect to the last byte of the answer: a reasoning
/// model on a server of the user's network can think for minutes before it answers (the answer
/// is not streamed). Stop closes the connection at once whatever this says.
pub const CHAT_TIMEOUT: Duration = Duration::from_secs(300);
/// How long listing the models (List models, Test) may take.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(15);
/// Tables described with their columns; beyond, names only.
pub const MAX_TABLES: usize = 200;
/// Table names listed beyond [`MAX_TABLES`].
pub const MAX_NAMES: usize = 1000;
/// Tables (the first ones described) whose structure is read for a request (K50): each
/// column's nullability, the primary key, the foreign keys and the indexes. Beyond, the
/// catalog's names and types.
pub const MAX_DETAILED: usize = 30;
/// Per table: columns described (beyond, their count), and indexes and foreign keys described
/// from its structure (beyond, their count).
pub const MAX_COLUMNS: usize = 100;
pub const MAX_INDEXES: usize = 12;
pub const MAX_KEYS: usize = 12;
/// Structures read at once, and how long one may take.
const DETAIL_CONCURRENT: usize = 4;
const DETAIL_TIMEOUT: Duration = Duration::from_secs(15);
/// Rows per table when sample rows are included, and the tables sampled.
pub const SAMPLE_ROWS: u64 = 5;
pub const SAMPLE_TABLES: usize = 10;
/// Characters of one sample cell; longer values are cut.
const MAX_CELL: usize = 200;
/// Largest answer read.
const MAX_ANSWER: u64 = 4 * 1024 * 1024;

/// Settings → AI. The API keys are not here: they live in the secret store ([`key_name`]).
///
/// `endpoint` and `model` are the active endpoint's, kept equal to its entry of `endpoints`
/// (K23): they are what every request reads, and what a Kerno older than K23 reads (it ignores
/// `endpoints`, and drops the list if it saves the settings; the active endpoint survives in
/// these two fields and comes back as a saved endpoint, [`AiSettings::adopt_legacy`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    /// "Enable the AI assistant". Off: no request is ever made.
    pub enabled: bool,
    /// The active endpoint's OpenAI-compatible base address, e.g. `http://localhost:11434/v1`.
    pub endpoint: String,
    /// The model named in each request: the active endpoint's.
    pub model: String,
    /// Base addresses of hosts other than this computer that the user allowed requests to.
    pub confirmed: BTreeSet<String>,
    /// The saved endpoints, the active one among them (K23).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<SavedEndpoint>,
}

/// The secret store entry of an endpoint's API key.
pub fn key_name(base: &str) -> String {
    format!("ai/{base}")
}

/// What the user asked the assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Write SQL from a sentence.
    WriteSql { sentence: String },
    /// Explain a query.
    ExplainQuery { sql: String },
    /// Explain a query's plan (the server's EXPLAIN output).
    ExplainPlan { sql: String, plan: String },
    /// Explain an error the server returned, and propose a fix.
    ExplainError { sql: String, error: String },
}

impl Action {
    pub fn label(&self) -> &'static str {
        match self {
            Self::WriteSql { .. } => "Write SQL",
            Self::ExplainQuery { .. } => "Explain query",
            Self::ExplainPlan { .. } => "Explain plan",
            Self::ExplainError { .. } => "Explain error",
        }
    }

    /// The text whose table names choose the tables described.
    pub fn text(&self) -> &str {
        match self {
            Self::WriteSql { sentence } => sentence,
            Self::ExplainQuery { sql }
            | Self::ExplainPlan { sql, .. }
            | Self::ExplainError { sql, .. } => sql,
        }
    }

    /// The user message of this action (K51): its instruction, then what it is about. It goes
    /// in the user's turn, so a conversation may mix actions and follow-ups.
    pub fn message(&self, engine: &str) -> String {
        format!("{}\n\n{}", self.instructions(engine), self.request())
    }

    fn instructions(&self, engine: &str) -> String {
        let engine = if engine.is_empty() { "SQL" } else { engine };
        match self {
            Self::WriteSql { .. } => format!(
                "Write one {engine} SQL statement for this request, using only the tables and \
                 columns described. Give the statement in one ```sql block, then at most two \
                 sentences about it."
            ),
            Self::ExplainQuery { .. } => format!(
                "Explain what this {engine} query does, clause by clause, in plain words. Point \
                 out anything that looks wrong or slow."
            ),
            Self::ExplainPlan { .. } => format!(
                "Explain this {engine} query plan in plain words: what each step does, which \
                 step costs the most and why, and what could make the query faster."
            ),
            Self::ExplainError { .. } => format!(
                "Explain this {engine} error in plain words, and propose a fix: the corrected \
                 statement in one ```sql block."
            ),
        }
    }

    fn request(&self) -> String {
        match self {
            Self::WriteSql { sentence } => sentence.trim().to_string(),
            Self::ExplainQuery { sql } => format!("```sql\n{}\n```", sql.trim()),
            Self::ExplainPlan { sql, plan } => {
                format!(
                    "Query:\n```sql\n{}\n```\n\nPlan:\n```\n{}\n```",
                    sql.trim(),
                    plan.trim()
                )
            }
            Self::ExplainError { sql, error } => {
                format!(
                    "Statement:\n```sql\n{}\n```\n\nError:\n{}",
                    sql.trim(),
                    error.trim()
                )
            }
        }
    }
}

/// A table or view described to the model: names and types only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub view: bool,
    /// Name and type; empty for a table listed by name only.
    pub columns: Vec<(String, String)>,
    /// `customer_id -> shop.customers(id)`.
    pub foreign_keys: Vec<String>,
    /// What its structure says beyond names and types, once read ([`read_details`]); it
    /// replaces `columns` and `foreign_keys` in the text.
    pub detail: Option<Detail>,
}

/// What the model is told of a table beyond names and types (K50): each column's type and
/// whether it can be NULL, the primary key, the foreign keys, the indexes. Nothing else of its
/// structure: no default expression, comment, check, index predicate or definition, which can
/// hold values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    /// Name, type, NULL allowed; the first [`MAX_COLUMNS`].
    pub columns: Vec<(String, String, bool)>,
    /// Columns left out beyond [`MAX_COLUMNS`].
    pub more_columns: usize,
    /// In key order.
    pub primary_key: Vec<String>,
    /// `(customer_id) references shop.customers (id)`; the first [`MAX_KEYS`].
    pub foreign_keys: Vec<String>,
    /// Unique, and the columns or expressions; the first [`MAX_INDEXES`] other than the
    /// primary key's.
    pub indexes: Vec<(bool, Vec<String>)>,
    /// Foreign keys and indexes left out beyond their caps.
    pub more_keys: usize,
    pub more_indexes: usize,
}

impl Detail {
    /// The detail of a table from its structure, within the caps.
    pub fn of(structure: &TableStructure) -> Detail {
        let mut primary: Vec<(u32, &str)> = (structure.columns.iter())
            .filter_map(|c| c.primary_key_position.map(|p| (p, c.name.as_str())))
            .collect();
        primary.sort_unstable();
        // Only names go: a key part that is not exactly one of the table's columns (an
        // expression such as `coalesce(code, 'N/A')`, which can hold a value) is never sent.
        let names: HashSet<&str> = (structure.columns.iter())
            .map(|c| c.name.as_str())
            .collect();
        let column = |part: &str| -> Option<String> {
            let part = part.trim();
            // `placed_at DESC`: the column, then its order, kept as the word it is.
            let (name, order) = match part.rsplit_once(' ') {
                Some((name, order))
                    if order.eq_ignore_ascii_case("asc") || order.eq_ignore_ascii_case("desc") =>
                {
                    (name.trim(), Some(order.to_lowercase()))
                }
                _ => (part, None),
            };
            let unquoted = name
                .strip_prefix('"')
                .and_then(|name| name.strip_suffix('"'))
                .or_else(|| name.strip_prefix('`').and_then(|n| n.strip_suffix('`')))
                .unwrap_or(name);
            let found = names.get(unquoted)?;
            Some(match order {
                Some(order) => format!("{found} {order}"),
                None => found.to_string(),
            })
        };
        let keys: Vec<String> = (structure.foreign_keys.iter())
            .filter(|key| key.columns.iter().all(|c| names.contains(c.as_str())))
            .map(|key| {
                format!(
                    "({}) references {}.{} ({})",
                    key.columns.join(", "),
                    key.referenced_table.schema,
                    key.referenced_table.name,
                    key.referenced_columns.join(", ")
                )
            })
            .collect();
        // An expression part is said as `(expression)`; an index of expressions only is left
        // out, and a partial index's predicate is never read.
        let indexes: Vec<(bool, Vec<String>)> = (structure.indexes.iter())
            .filter(|index| !index.primary)
            .filter_map(|index| {
                let parts: Vec<Option<String>> =
                    index.columns.iter().map(|part| column(part)).collect();
                parts.iter().any(Option::is_some).then(|| {
                    let parts = parts
                        .into_iter()
                        .map(|part| part.unwrap_or_else(|| "(expression)".into()))
                        .collect();
                    (index.unique, parts)
                })
            })
            .collect();
        Detail {
            columns: (structure.columns.iter())
                .take(MAX_COLUMNS)
                .map(|c| (c.name.clone(), c.data_type.clone(), c.nullable))
                .collect(),
            more_columns: structure.columns.len().saturating_sub(MAX_COLUMNS),
            primary_key: primary.into_iter().map(|(_, name)| name.into()).collect(),
            more_keys: keys.len().saturating_sub(MAX_KEYS),
            foreign_keys: keys.into_iter().take(MAX_KEYS).collect(),
            more_indexes: indexes.len().saturating_sub(MAX_INDEXES),
            indexes: indexes.into_iter().take(MAX_INDEXES).collect(),
        }
    }

    /// The lines of a table described in full, under its name.
    fn describe(&self, text: &mut String) {
        for (name, data_type, nullable) in &self.columns {
            let null = if *nullable { "" } else { " not null" };
            text.push_str(&format!("\n  {name} {data_type}{null}"));
        }
        if self.more_columns > 0 {
            text.push_str(&format!("\n  … {} more columns", self.more_columns));
        }
        if !self.primary_key.is_empty() {
            text.push_str(&format!(
                "\n  primary key ({})",
                self.primary_key.join(", ")
            ));
        }
        for key in &self.foreign_keys {
            text.push_str(&format!("\n  foreign key {key}"));
        }
        if self.more_keys > 0 {
            text.push_str(&format!("\n  … {} more foreign keys", self.more_keys));
        }
        for (unique, columns) in &self.indexes {
            let kind = if *unique { "unique" } else { "index" };
            text.push_str(&format!("\n  {kind} ({})", columns.join(", ")));
        }
        if self.more_indexes > 0 {
            text.push_str(&format!("\n  … {} more indexes", self.more_indexes));
        }
    }
}

/// Reads the structure of `tables` (at most [`MAX_DETAILED`] of them, in order) for the
/// model, [`DETAIL_CONCURRENT`] at a time, each within [`DETAIL_TIMEOUT`]: run it off the UI
/// thread, on the session the catalog was read from. A table that cannot be read is returned
/// with why; the request then describes it by its catalog names and types.
pub async fn read_details(
    session: Arc<dyn Session>,
    tables: Vec<TableRef>,
) -> Vec<(TableRef, Result<Detail, String>)> {
    use futures_util::StreamExt as _;
    let reading = tables.into_iter().take(MAX_DETAILED).map(|table| {
        let session = session.clone();
        async move {
            let read = tokio::time::timeout(DETAIL_TIMEOUT, session.table_structure(&table)).await;
            let detail = match read {
                Ok(Ok(structure)) => Ok(Detail::of(&structure)),
                Ok(Err(error)) => Err(crate::describe(&error).to_string()),
                Err(_) => Err("the server took too long".to_string()),
            };
            (table, detail)
        }
    });
    futures_util::stream::iter(reading)
        .buffered(DETAIL_CONCURRENT)
        .collect()
        .await
}

/// The first rows of one table, sent only when the user ticked "include sample rows".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub table: TableRef,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// What the model is told of the database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    /// PostgreSQL, MySQL, MariaDB.
    pub engine: String,
    pub server_version: Option<String>,
    pub database: Option<String>,
    /// Every schema (MySQL: database) name.
    pub schemas: Vec<String>,
    pub tables: Vec<Table>,
    /// Row data: `None` unless the user ticked "include sample rows" for this request.
    pub sample_rows: Option<Vec<Sample>>,
}

impl Context {
    /// The context of a request from the catalog a query tab loaded for completion: the
    /// tables `text` (the SQL, the sentence, the conversation) names, in the order it names
    /// them, else the tables of `schema` (the tab's, or the first of the search path); the
    /// first [`MAX_TABLES`] with their columns, then names only. No sample rows, no details:
    /// [`Context::detail_tables`] says which tables [`read_details`] reads.
    pub fn from_catalog(
        catalog: &Catalog,
        engine: &str,
        server_version: Option<String>,
        database: Option<String>,
        schema: Option<&str>,
        text: &str,
    ) -> Context {
        // Each word, at its first place in the text.
        let mut words: std::collections::HashMap<String, usize> = Default::default();
        let split = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .filter(|word| !word.is_empty());
        for (at, word) in split.enumerate() {
            words.entry(word.to_lowercase()).or_insert(at);
        }
        // A word equal to the name, or one `s` apart.
        let named = |name: &str| {
            let name = name.to_lowercase();
            let plural = words.get(&format!("{name}s"));
            let singular = name.strip_suffix('s').and_then(|stem| words.get(stem));
            [words.get(&name), plural, singular]
                .into_iter()
                .flatten()
                .min()
                .copied()
        };
        let all = || {
            catalog
                .schemas
                .iter()
                .flat_map(|schema| schema.relations.iter().map(move |r| (schema, r)))
        };
        let mut placed: Vec<_> = all()
            .filter_map(|(schema, relation)| Some((named(&relation.name)?, schema, relation)))
            .collect();
        // Stable: tables named at the same place keep the catalog's order.
        placed.sort_by_key(|(at, _, _)| *at);
        let mut chosen: Vec<_> = placed
            .into_iter()
            .map(|(_, schema, relation)| (schema, relation))
            .collect();
        if chosen.is_empty() {
            let current = schema
                .map(str::to_string)
                .or_else(|| {
                    catalog
                        .search_path
                        .iter()
                        .find(|name| catalog.schemas.iter().any(|s| s.name == **name))
                        .cloned()
                })
                .unwrap_or_default();
            chosen = all().filter(|(s, _)| s.name == current).collect();
        }
        let tables = chosen
            .into_iter()
            .take(MAX_TABLES + MAX_NAMES)
            .enumerate()
            .map(|(index, (schema, relation))| {
                let full = index < MAX_TABLES;
                Table {
                    schema: schema.name.clone(),
                    name: relation.name.clone(),
                    view: matches!(
                        relation.kind,
                        RelationKind::View | RelationKind::MaterializedView
                    ),
                    columns: if full {
                        (relation.columns.iter())
                            .map(|column| (column.name.clone(), column.data_type.clone()))
                            .collect()
                    } else {
                        Vec::new()
                    },
                    foreign_keys: if full {
                        (relation.foreign_keys.iter())
                            .map(|key| {
                                format!(
                                    "{} -> {}.{}({})",
                                    key.columns.join(", "),
                                    key.referenced_schema,
                                    key.referenced_table,
                                    key.referenced_columns.join(", ")
                                )
                            })
                            .collect()
                    } else {
                        Vec::new()
                    },
                    detail: None,
                }
            })
            .collect();
        Context {
            engine: engine.to_string(),
            server_version,
            database,
            schemas: catalog.schemas.iter().map(|s| s.name.clone()).collect(),
            tables,
            sample_rows: None,
        }
    }

    /// The tables whose first rows "include sample rows" reads: the first [`SAMPLE_TABLES`]
    /// described with their columns.
    pub fn sample_tables(&self) -> Vec<TableRef> {
        self.tables
            .iter()
            .filter(|table| !table.columns.is_empty())
            .take(SAMPLE_TABLES)
            .map(|table| TableRef::new(&table.schema, &table.name))
            .collect()
    }

    /// The tables whose structure [`read_details`] reads: the first [`MAX_DETAILED`]
    /// described with their columns, the ones the text names first.
    pub fn detail_tables(&self) -> Vec<TableRef> {
        self.tables
            .iter()
            .filter(|table| !table.columns.is_empty())
            .take(MAX_DETAILED)
            .map(|table| TableRef::new(&table.schema, &table.name))
            .collect()
    }

    /// Gives the tables their details, where `details` has them.
    pub fn add_details(&mut self, details: &std::collections::HashMap<TableRef, Detail>) {
        for table in &mut self.tables {
            if table.columns.is_empty() {
                continue;
            }
            table.detail = details
                .get(&TableRef::new(&table.schema, &table.name))
                .cloned();
        }
    }

    /// The context as the text of the system message.
    fn describe(&self) -> String {
        let mut text = format!("Database engine: {}", self.engine);
        if let Some(version) = &self.server_version {
            text.push_str(&format!(" (server version: {version})"));
        }
        if let Some(database) = &self.database {
            text.push_str(&format!("\nCurrent database: {database}"));
        }
        text.push_str(&format!("\nSchemas: {}", self.schemas.join(", ")));
        let (full, names): (Vec<&Table>, Vec<&Table>) = self
            .tables
            .iter()
            .partition(|table| !table.columns.is_empty());
        text.push_str(
            "\nTables and columns (name type; `not null`, keys and indexes where they were read):",
        );
        if full.is_empty() {
            text.push_str(" none known");
        }
        for table in full {
            let kind = if table.view { "view" } else { "table" };
            if let Some(detail) = &table.detail {
                text.push_str(&format!("\n- {}.{} ({kind})", table.schema, table.name));
                detail.describe(&mut text);
                continue;
            }
            let mut columns: Vec<String> = (table.columns.iter())
                .take(MAX_COLUMNS)
                .map(|(name, data_type)| format!("{name} {data_type}"))
                .collect();
            if table.columns.len() > MAX_COLUMNS {
                columns.push(format!(
                    "… {} more columns",
                    table.columns.len() - MAX_COLUMNS
                ));
            }
            text.push_str(&format!(
                "\n- {}.{} ({kind}): {}",
                table.schema,
                table.name,
                columns.join(", ")
            ));
            if !table.foreign_keys.is_empty() {
                text.push_str(&format!(
                    "\n  foreign keys: {}",
                    table.foreign_keys.join("; ")
                ));
            }
        }
        if !names.is_empty() {
            let names: Vec<String> = (names.iter())
                .map(|table| format!("{}.{}", table.schema, table.name))
                .collect();
            text.push_str(&format!(
                "\nOther tables (names only): {}",
                names.join(", ")
            ));
        }
        if let Some(samples) = &self.sample_rows {
            text.push_str(&format!(
                "\nSample rows the user chose to include (the first {SAMPLE_ROWS} rows of tables above):"
            ));
            for sample in samples {
                text.push_str(&format!(
                    "\n{}.{}: {}",
                    sample.table.schema,
                    sample.table.name,
                    sample.columns.join(" | ")
                ));
                for row in &sample.rows {
                    let cells: Vec<String> = (row.iter())
                        .map(|cell| match cell {
                            None => "NULL".to_string(),
                            Some(value) if value.chars().count() > MAX_CELL => {
                                format!("{}…", value.chars().take(MAX_CELL).collect::<String>())
                            }
                            Some(value) => value.clone(),
                        })
                        .collect();
                    text.push_str(&format!("\n  {}", cells.join(" | ")));
                }
            }
        }
        text
    }
}

/// The statement that reads the sample rows of `table`.
pub fn sample_sql(dialect: &dyn Dialect, table: &TableRef) -> String {
    format!(
        "SELECT * FROM {} {}",
        dialect.table_name(table),
        dialect.limit_offset(SAMPLE_ROWS, 0)
    )
}

/// The request body of `action` alone, an OpenAI-compatible chat completion: [`build_chat`]
/// with no earlier turn (K52's one answer for a statement goes through it). The model hears its
/// rules and the context, then the action's instruction and subject.
pub fn build_payload(action: &Action, context: &Context, model: &str) -> Value {
    build_chat(context, &[], &action.message(&context.engine), model)
}

/// The bytes sent for `payload`, and shown by the "What is sent" preview: pretty-printed JSON.
pub fn body(payload: &Value) -> Vec<u8> {
    // A `Value` always serialises.
    serde_json::to_vec_pretty(payload).unwrap_or_default()
}

/// [`body`] as text.
pub fn preview(payload: &Value) -> String {
    String::from_utf8_lossy(&body(payload)).into_owned()
}

/// An endpoint address that passed the checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The base address, without a trailing `/`; requests add `/chat/completions`, `/models`.
    pub base: String,
    /// Its host, as the warning names it.
    pub host: String,
    /// The host is this computer (127.0.0.0/8, ::1, `localhost`).
    pub loopback: bool,
    /// Its port, given or the scheme's.
    pub port: u16,
}

/// Reads an endpoint address: `https://`, or `http://` to this computer only; no user name or
/// password in it (the key has its own field), no query.
pub fn endpoint(text: &str) -> Result<Endpoint, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(
            "No endpoint: enter the address of an OpenAI-compatible server, such as \
             http://localhost:11434/v1 for Ollama."
                .into(),
        );
    }
    let url = url::Url::parse(text)
        .map_err(|_| format!("“{text}” is not an address. Write it as http://host:port/v1."))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Put the key in the API key field, not in the address.".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("The address cannot hold a query (?…) or a fragment (#…).".into());
    }
    let (host, loopback) = match url.host() {
        Some(url::Host::Domain(name)) => (name.to_string(), name.eq_ignore_ascii_case("localhost")),
        Some(url::Host::Ipv4(ip)) => (ip.to_string(), IpAddr::V4(ip).is_loopback()),
        Some(url::Host::Ipv6(ip)) => (ip.to_string(), IpAddr::V6(ip).is_loopback()),
        None => return Err(format!("“{text}” names no host.")),
    };
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        "http" => {
            return Err(format!(
                "Plain http:// is allowed only to this computer (localhost, 127.0.0.1, ::1): \
                 use https://{host} so the request and the key are encrypted."
            ));
        }
        _ => return Err("Use an http:// or https:// address.".into()),
    }
    Ok(Endpoint {
        base: url.as_str().trim_end_matches('/').to_string(),
        host,
        loopback,
        port: url.port_or_known_default().unwrap_or(443),
    })
}

/// What [`request`] asks the endpoint.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// `POST {base}/chat/completions` with the [`body`] of this payload.
    Chat(Value),
    /// `GET {base}/models` ("List models").
    Models,
}

/// Why a request was not made, or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    /// The assistant is off: nothing was sent.
    Off,
    /// The address is refused: nothing was sent.
    Endpoint(String),
    /// A host other than this computer, not confirmed yet: nothing was sent.
    NotConfirmed(String),
    /// The server could not be reached, or did not answer in time.
    Network(String),
    /// The server answered with an error status.
    Status(u16, String),
    /// The answer could not be read.
    Answer(String),
    /// Stopped by the user: the connection was closed before the answer.
    Stopped,
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str(
                "The AI assistant is off: turn it on in Settings → AI. Nothing was sent.",
            ),
            Self::Endpoint(why) => write!(f, "{why} Nothing was sent."),
            Self::NotConfirmed(host) => write!(
                f,
                "Requests to {host} leave this computer: allow them in Settings → AI first. \
                 Nothing was sent."
            ),
            Self::Network(why) => f.write_str(why),
            Self::Status(status, text) => {
                match status {
                    401 | 403 => write!(
                        f,
                        "The endpoint refused the request ({status}): check the API key saved \
                         for it in Settings → AI."
                    )?,
                    404 => write!(
                        f,
                        "The endpoint has nothing at that address (404): check the address (most \
                         servers end in /v1). If only the model list is missing, type the \
                         model's name."
                    )?,
                    _ => write!(f, "The endpoint answered with an error ({status}).")?,
                }
                if !text.is_empty() {
                    write!(f, "\n{text}")?;
                }
                Ok(())
            }
            Self::Answer(why) => write!(f, "The answer could not be read: {why}"),
            Self::Stopped => f.write_str("Stopped: the request was closed before its answer."),
        }
    }
}

impl std::error::Error for AiError {}

/// Whether a request may go, and where: the checks [`request`] makes before any connection
/// (the assistant on, an accepted address, a host other than this computer confirmed).
pub fn gate(settings: &AiSettings) -> Result<Endpoint, AiError> {
    if !settings.enabled {
        return Err(AiError::Off);
    }
    let endpoint = endpoint(&settings.endpoint).map_err(AiError::Endpoint)?;
    if !endpoint.loopback && !settings.confirmed.contains(&endpoint.base) {
        return Err(AiError::NotConfirmed(endpoint.host));
    }
    Ok(endpoint)
}

/// What stops a request in flight (the panel's Stop): the request's connection is closed at
/// once, so its call returns and the endpoint sees the client go, instead of an answer read
/// up to [`CHAT_TIMEOUT`] later (C3). A request sent after the stop is not sent at all.
#[derive(Clone, Default)]
pub struct Stopper(Arc<Mutex<Stopping>>);

#[derive(Default)]
struct Stopping {
    stopped: bool,
    agent: Option<ureq::Agent>,
}

impl Stopper {
    /// Closes the connection of the request in flight, if one is.
    pub fn stop(&self) {
        let agent = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.stopped = true;
            state.agent.take()
        };
        if let Some(agent) = agent {
            agent.shutdown();
        }
    }

    /// Keeps `agent` to stop it; `false` when stopped already.
    fn hold(&self, agent: &ureq::Agent) -> bool {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.stopped {
            state.agent = Some(agent.clone());
        }
        !state.stopped
    }
}

/// Sends `call` to the configured endpoint and returns the answer's body: the one function
/// that opens a socket for the assistant. Refuses, before any connection, while the assistant
/// is off, for a refused address and for an unconfirmed host other than this computer. The key
/// goes in `Authorization: Bearer`, nowhere else. Blocking: run it off the UI thread.
pub fn request(
    settings: &AiSettings,
    key: Option<&Secret>,
    call: &Call,
) -> Result<String, AiError> {
    request_until(settings, key, call, &Stopper::default())
}

/// [`request`], ended early by `stop` (its connection closed: [`AiError::Stopped`]).
pub fn request_until(
    settings: &AiSettings,
    key: Option<&Secret>,
    call: &Call,
    stop: &Stopper,
) -> Result<String, AiError> {
    let timeout = match call {
        Call::Chat(_) => CHAT_TIMEOUT,
        Call::Models => LIST_TIMEOUT,
    };
    send(settings, key, call, stop, timeout)
}

/// [`exchange`] with the answer read whole: the body, or a streamed completion's answer.
fn send(
    settings: &AiSettings,
    key: Option<&Secret>,
    call: &Call,
    stop: &Stopper,
    timeout: Duration,
) -> Result<String, AiError> {
    match exchange(settings, key, call, stop, timeout, &mut |_| {})? {
        Reply::Body(text) => Ok(text),
        Reply::Streamed(answer) => answer,
    }
}

/// The one function that opens a socket for the assistant's requests (Detect goes through it
/// too, with its own timeout), only after [`gate`]. The one other connection is the issuer
/// check of a refused certificate (`tls::issuer`), to the same host, after the same gate. An
/// answer streamed as server-sent events is read as it comes, each piece told to `on_text`.
fn exchange(
    settings: &AiSettings,
    key: Option<&Secret>,
    call: &Call,
    stop: &Stopper,
    timeout: Duration,
    on_text: &mut dyn FnMut(&str),
) -> Result<Reply, AiError> {
    let endpoint = gate(settings)?;
    let mut config = ureq::Agent::config_builder()
        // Only the configured host: no proxy from the environment, no redirect.
        .proxy(None)
        .max_redirects(0)
        .https_only(!endpoint.loopback)
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent(format!("Kerno/{}", env!("CARGO_PKG_VERSION")));
    if endpoint.base.starts_with("https://") {
        config = config.tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(tls::roots())
                .build(),
        );
    }
    let agent = ureq::Agent::new_with_config(config.build());
    if !stop.hold(&agent) {
        return Err(AiError::Stopped);
    }
    let stopped = |error| {
        if stop.0.lock().map(|state| state.stopped).unwrap_or(true) {
            AiError::Stopped
        } else {
            network(&endpoint, error, timeout)
        }
    };
    let bearer = key
        .filter(|key| !key.is_empty())
        .map(|key| format!("Bearer {}", key.expose()));
    let sent = match call {
        Call::Chat(payload) => {
            let mut post = agent
                .post(format!("{}/chat/completions", endpoint.base))
                .header("Content-Type", "application/json");
            if let Some(bearer) = &bearer {
                post = post.header("Authorization", bearer);
            }
            post.send(body(payload))
        }
        Call::Models => {
            let mut get = agent.get(format!("{}/models", endpoint.base));
            if let Some(bearer) = &bearer {
                get = get.header("Authorization", bearer);
            }
            get.call()
        }
    };
    let mut response = sent.map_err(stopped)?;
    let status = response.status().as_u16();
    // An answer written as it comes (K51): read event by event, within the same cap.
    let streamed = (response.headers().get("content-type"))
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_start().starts_with("text/event-stream"));
    if streamed && (200..300).contains(&status) {
        let reader = response.body_mut().with_config().limit(MAX_ANSWER).reader();
        let read = chat::read_stream(reader, on_text);
        // Stopped: the connection closed under the reader, whatever it read since.
        if stop.0.lock().map(|state| state.stopped).unwrap_or(true) {
            return Err(AiError::Stopped);
        }
        return match read {
            Ok(answer) => Ok(Reply::Streamed(answer)),
            Err(error) => Err(AiError::Network(format!(
                "The answer of {} stopped before its end.\n{error}",
                endpoint.host
            ))),
        };
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER)
        .lossy_utf8(true)
        .read_to_string()
        .map_err(stopped)?;
    if !(200..300).contains(&status) {
        let text: String = text.trim().chars().take(300).collect();
        return Err(AiError::Status(status, text));
    }
    Ok(Reply::Body(text))
}

/// What [`send`] read.
enum Reply {
    /// The whole body (a list of models, a completion that is not streamed).
    Body(String),
    /// A streamed completion: its answer, or why it is not one.
    Streamed(Result<String, AiError>),
}

/// A failed connection or read, in plain words first, then the client's own message.
fn network(endpoint: &Endpoint, error: ureq::Error, timeout: Duration) -> AiError {
    let host = &endpoint.host;
    let refused = || {
        format!(
            "{host} refused the connection on port {}: is the server running on that port?",
            endpoint.port
        )
    };
    let plain = match &error {
        ureq::Error::Timeout(_) => format!("{host} gave no answer within {}.", waited(timeout)),
        ureq::Error::HostNotFound => format!(
            "The host “{host}” was not found. Is this computer on the network that serves it, \
             or on its VPN?"
        ),
        ureq::Error::ConnectionFailed => refused(),
        ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused => refused(),
        ureq::Error::BodyExceedsLimit(_) => format!("The answer of {host} is too large."),
        // The issuer's name comes from a certificate nobody verified: it is
        // shown as what the server claims, never as a reason to trust it, and
        // the authority to add is the administrator's, not the one presented
        // (an interceptor writes any name it likes).
        error if tls::unknown_issuer(error) => {
            let claimed = tls::issuer(host, endpoint.port, Duration::from_secs(5))
                .map(|name| {
                    format!(
                        " It names “{name}” as its issuer, a name Kerno could not verify: \
                         whoever answers at this address chose it."
                    )
                })
                .unwrap_or_default();
            format!(
                "{host} presented a certificate this computer does not trust.{claimed} If \
                 {host} belongs to your organisation and has an authority of its own, get that \
                 authority's certificate from its administrator, not from this server, and add \
                 it to the system's trust store (Keychain Access on macOS), then test again. If \
                 you did not expect this, the connection may be intercepted: send nothing."
            )
        }
        error if tls::tls_error(error).is_some() => format!(
            "The secure connection to {host} failed. Kerno trusts the certificates the system \
             trusts."
        ),
        _ => format!("The request to {host} failed."),
    };
    AiError::Network(format!("{plain}\n{error}"))
}

/// A timeout in words: `1500 ms`, `15 seconds`, `5 minutes`.
fn waited(timeout: Duration) -> String {
    match timeout.as_secs() {
        0 => format!("{} ms", timeout.as_millis()),
        seconds if seconds < 120 => format!("{seconds} seconds"),
        seconds => format!("{} minutes", seconds / 60),
    }
}

/// Asks the endpoint for a completion of `payload` and returns the model's answer.
pub fn chat(
    settings: &AiSettings,
    key: Option<&Secret>,
    payload: &Value,
) -> Result<String, AiError> {
    chat_until(settings, key, payload, &Stopper::default())
}

/// [`chat`], ended early by `stop`.
pub fn chat_until(
    settings: &AiSettings,
    key: Option<&Secret>,
    payload: &Value,
    stop: &Stopper,
) -> Result<String, AiError> {
    chat_stream(settings, key, payload, stop, &mut |_| {})
}

/// [`chat_until`], telling `on_text` the answer's pieces as they arrive (K51): each piece of a
/// streamed answer (`"stream": true`, server-sent events), or the whole answer at once from a
/// server that does not stream. The pieces are raw: a model's `<think>…</think>` is in them;
/// the answer returned is without it ([`strip_reasoning`]). Stop closes the connection at once
/// ([`AiError::Stopped`]); the answer is capped as a whole one is. Blocking: run it off the UI
/// thread.
pub fn chat_stream(
    settings: &AiSettings,
    key: Option<&Secret>,
    payload: &Value,
    stop: &Stopper,
    on_text: &mut dyn FnMut(&str),
) -> Result<String, AiError> {
    let call = Call::Chat(payload.clone());
    match exchange(settings, key, &call, stop, CHAT_TIMEOUT, on_text)? {
        Reply::Body(text) => {
            let answer = answer_text(&text)?;
            on_text(&answer);
            Ok(answer)
        }
        Reply::Streamed(answer) => answer,
    }
}

/// The answer of a chat completion's body, without the model's reasoning: a reasoning field
/// (`reasoning_content`, `reasoning`) is not read, and `<think>…</think>` is taken out of the
/// text ([`strip_reasoning`]), so neither is shown, copied or inserted.
pub fn answer_text(body: &str) -> Result<String, AiError> {
    let answer: Value =
        serde_json::from_str(body).map_err(|error| AiError::Answer(error.to_string()))?;
    let message = answer
        .pointer("/choices/0/message")
        .ok_or_else(|| AiError::Answer("it holds no choices[0].message".into()))?;
    let text = strip_reasoning(message.get("content").and_then(Value::as_str).unwrap_or(""));
    if text.is_empty() {
        let reasoned = ["reasoning_content", "reasoning"].iter().any(|field| {
            (message.get(*field).and_then(Value::as_str)).is_some_and(|text| !text.is_empty())
        });
        return Err(AiError::Answer(if reasoned {
            "the model gave its reasoning but no answer (it may have run out of tokens): ask \
             again, or choose a model that thinks less."
                .into()
        } else {
            "the model's answer is empty.".into()
        }));
    }
    Ok(text)
}

/// `text` without the reasoning some models write in it: each `<think>…</think>`, the text up
/// to a `</think>` whose `<think>` the server's template wrote (Qwen3, DeepSeek-R1), and what
/// follows a `<think>` never closed (an answer cut short).
pub fn strip_reasoning(text: &str) -> String {
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    let mut text = text.to_string();
    loop {
        match (text.find(OPEN), text.find(CLOSE)) {
            (Some(open), Some(close)) if open < close => {
                text.replace_range(open..close + CLOSE.len(), "")
            }
            (_, Some(close)) => text.replace_range(..close + CLOSE.len(), ""),
            (Some(open), None) => {
                text.truncate(open);
                break;
            }
            (None, None) => break,
        }
    }
    text.trim().to_string()
}

/// The model names the endpoint offers (`GET /models`), sorted.
pub fn list_models(settings: &AiSettings, key: Option<&Secret>) -> Result<Vec<String>, AiError> {
    parse_models(&request(settings, key, &Call::Models)?)
}

/// The model ids of a `GET /models` body, sorted.
fn parse_models(text: &str) -> Result<Vec<String>, AiError> {
    let answer: Value =
        serde_json::from_str(text).map_err(|error| AiError::Answer(error.to_string()))?;
    let mut models: Vec<String> = answer
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| AiError::Answer("it holds no data list".into()))?
        .iter()
        .filter_map(|model| model.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    models.sort();
    Ok(models)
}

/// The SQL of an answer: its first fenced block (```sql or plain ```), else the whole answer.
pub fn sql_block(answer: &str) -> String {
    let mut rest = answer;
    while let Some(start) = rest.find("```") {
        let after = &rest[start + 3..];
        let Some(line_end) = after.find('\n') else {
            break;
        };
        let language = after[..line_end].trim().to_lowercase();
        let code = &after[line_end + 1..];
        let Some(end) = code.find("```") else {
            break;
        };
        if language.is_empty() || language == "sql" {
            return code[..end].trim().to_string();
        }
        rest = &code[end + 3..];
    }
    answer.trim().to_string()
}

/// An answer as the panel shows it: the lines of its code fences (```sql, ```) taken out, the
/// code and the prose around it kept.
pub fn unfenced(answer: &str) -> String {
    let lines: Vec<&str> = (answer.lines())
        .filter(|line| !line.trim_start().starts_with("```"))
        .collect();
    lines.join("\n").trim().to_string()
}

/// What Write SQL puts in the editor's `text` over `range`, and where the SQL starts in it: a
/// selection is replaced by the SQL; at a caret the SQL goes on its own line, after a line
/// break when text comes before the caret on its line and before one when text follows it, so
/// it is never glued to a neighbouring word.
pub fn placed(text: &str, range: std::ops::Range<usize>, sql: &str) -> (String, usize) {
    if !range.is_empty() {
        return (sql.to_string(), 0);
    }
    let at = range.start;
    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let before = if text[line_start..at].trim().is_empty() {
        ""
    } else {
        "\n"
    };
    let after = if text[at..line_end].trim().is_empty() {
        ""
    } else {
        "\n"
    };
    (format!("{before}{sql}{after}"), before.len())
}

/// What Insert puts in the editor's `text` at `range` so that the SQL runs as statements of its
/// own (the real-app pass: `… limit 50` with no `;` and `SELECT …` on the next line read as
/// one): `(the range replaced, its new text, where the SQL starts in that text)`, one edit, one
/// undo step. A selection is replaced as [`placed`] does. At a caret, the statement before it,
/// when it is not ended, is ended with `;` right after its last word; the SQL is ended with `;`
/// when a statement follows it. Checked with the statement splitter of `rules`.
pub fn insertion(
    text: &str,
    range: std::ops::Range<usize>,
    sql: &str,
    rules: crate::sql::LexRules,
) -> (std::ops::Range<usize>, String, usize) {
    use crate::sql::split_statements;
    if !range.is_empty() {
        let (placed, start) = placed(text, range.clone(), sql);
        return (range, placed, start);
    }
    let at = range.start;
    let mut sql = sql.trim().to_string();
    // A statement after the caret: the SQL is ended so that it does not run on into it.
    if !split_statements(&text[at..], rules).is_empty() && !sql.ends_with(';') {
        sql.push(';');
    }
    let (placed, start) = placed(text, range.clone(), &sql);
    // The SQL starts a statement of its own once in place: nothing more to do.
    let candidate = format!("{}{placed}{}", &text[..at], &text[at..]);
    let starts = |source: &str, offset: usize| {
        split_statements(source, rules)
            .iter()
            .any(|span| span.range.start == offset)
    };
    if starts(&candidate, at + start) {
        return (range, placed, start);
    }
    // The statement before it is not ended: `;` right after it, in the same edit.
    let Some(end) = split_statements(&text[..at], rules)
        .last()
        .map(|span| span.range.end)
    else {
        return (range, placed, start);
    };
    let between = &text[end..at];
    (
        end..at,
        format!(";{between}{placed}"),
        1 + between.len() + start,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ColumnInfo, ConstraintInfo, ForeignKeyInfo, IndexInfo};
    use crate::sql::{CatalogColumn, CatalogForeignKey, CatalogRelation, CatalogSchema};
    use serde_json::json;

    fn relation(name: &str, columns: &[(&str, &str)]) -> CatalogRelation {
        CatalogRelation {
            name: name.into(),
            kind: RelationKind::Table,
            columns: columns
                .iter()
                .map(|(name, data_type)| CatalogColumn {
                    name: (*name).into(),
                    data_type: (*data_type).into(),
                })
                .collect(),
            foreign_keys: Vec::new(),
        }
    }

    fn catalog() -> Catalog {
        let mut orders = relation("orders", &[("id", "bigint"), ("customer_id", "bigint")]);
        orders.foreign_keys.push(CatalogForeignKey {
            columns: vec!["customer_id".into()],
            referenced_schema: "shop".into(),
            referenced_table: "customers".into(),
            referenced_columns: vec!["id".into()],
        });
        Catalog {
            schemas: vec![
                CatalogSchema {
                    name: "public".into(),
                    relations: vec![relation("all_types", &[("c_int", "integer")])],
                    routines: Vec::new(),
                },
                CatalogSchema {
                    name: "shop".into(),
                    relations: vec![
                        relation("customers", &[("id", "bigint"), ("email", "text")]),
                        orders,
                        relation("products", &[("sku", "text")]),
                    ],
                    routines: Vec::new(),
                },
            ],
            search_path: vec!["public".into()],
        }
    }

    fn write(sentence: &str) -> Action {
        Action::WriteSql {
            sentence: sentence.into(),
        }
    }

    fn on() -> AiSettings {
        AiSettings {
            enabled: true,
            endpoint: "http://127.0.0.1:9/v1".into(),
            model: "m".into(),
            confirmed: BTreeSet::new(),
            endpoints: Vec::new(),
        }
    }

    #[test]
    fn the_context_holds_the_tables_named_else_the_current_schema() {
        let action = write("Total of the orders of each customer");
        let context =
            Context::from_catalog(&catalog(), "PostgreSQL", None, None, None, action.text());
        // In the order the text names them (K50).
        let names: Vec<&str> = context.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["orders", "customers"]);
        assert_eq!(context.schemas, ["public", "shop"]);
        assert_eq!(
            context.tables[0].foreign_keys,
            ["customer_id -> shop.customers(id)"]
        );
        // Nothing named: the tab's schema, else the first of the search path.
        let action = write("how many rows?");
        let context = Context::from_catalog(
            &catalog(),
            "PostgreSQL",
            None,
            None,
            Some("shop"),
            action.text(),
        );
        assert_eq!(context.tables.len(), 3);
        let context =
            Context::from_catalog(&catalog(), "PostgreSQL", None, None, None, action.text());
        assert_eq!(context.tables[0].name, "all_types");
        // From SQL too, qualified or not.
        let action = Action::ExplainQuery {
            sql: "select * from shop.products p".into(),
        };
        let context =
            Context::from_catalog(&catalog(), "PostgreSQL", None, None, None, action.text());
        assert_eq!(context.tables.len(), 1);
        assert!(context.sample_rows.is_none());
    }

    fn column(name: &str, data_type: &str, nullable: bool, pk: Option<u32>) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            kind: crate::TypeKind::Text,
            nullable,
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: pk,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn index(name: &str, columns: &[&str], unique: bool, primary: bool) -> IndexInfo {
        IndexInfo {
            name: name.into(),
            columns: columns.iter().map(|c| (*c).into()).collect(),
            unique,
            primary,
            backs_constraint: primary,
            method: "btree".into(),
            predicate: None,
            definition: None,
        }
    }

    /// `shop.order_items` as a server describes it, with what must never be sent: a default
    /// holding a value, comments, a check, a partial index's predicate, index definitions.
    fn order_items() -> TableStructure {
        let mut quantity = column("quantity", "integer", false, None);
        quantity.default = Some("'kerno-default-value'::text".into());
        quantity.comment = Some("kerno-column-comment".into());
        let mut partial = index("items_big_idx", &["quantity"], false, false);
        partial.predicate = Some("quantity > 4242".into());
        partial.definition = Some("CREATE INDEX items_big_idx … WHERE quantity > 4242".into());
        let key = |columns: &[&str], table: &str, referenced: &[&str]| ForeignKeyInfo {
            name: format!("{table}_fk"),
            table: TableRef::new("shop", "order_items"),
            columns: columns.iter().map(|c| (*c).into()).collect(),
            referenced_table: TableRef::new("shop", table),
            referenced_columns: referenced.iter().map(|c| (*c).into()).collect(),
            on_update: "NO ACTION".into(),
            on_delete: "CASCADE".into(),
        };
        TableStructure {
            columns: vec![
                column("order_id", "bigint", false, Some(1)),
                column("line_no", "smallint", false, Some(2)),
                column("product_id", "bigint", false, None),
                quantity,
                column("note", "text", true, None),
            ],
            indexes: vec![
                index("order_items_pkey", &["order_id", "line_no"], true, true),
                index("order_items_product_idx", &["product_id"], false, false),
                index("order_items_note_key", &["note"], true, false),
                partial,
                // Expressions can hold values: never sent, said as `(expression)`.
                index(
                    "items_mixed_idx",
                    &["\"line_no\" DESC", "lower(note || 'kerno-literal-1')"],
                    false,
                    false,
                ),
                index(
                    "items_expression_idx",
                    &["coalesce(note, 'kerno-literal-2')"],
                    true,
                    false,
                ),
            ],
            constraints: vec![ConstraintInfo {
                name: "quantity_check".into(),
                kind: crate::schema::ConstraintKind::Check,
                columns: vec!["quantity".into()],
                definition: "CHECK ((quantity > 4243))".into(),
            }],
            foreign_keys: vec![
                key(&["order_id"], "orders", &["id"]),
                key(&["product_id"], "products", &["id"]),
            ],
            comment: Some("kerno-table-comment".into()),
            ..TableStructure::default()
        }
    }

    /// K50: a table read in full says each column's type and whether it can be NULL, the
    /// primary key, the foreign keys and the indexes, as a short text a person reads; nothing
    /// that can hold a value (defaults, comments, checks, predicates, definitions).
    #[test]
    fn a_table_read_in_full_says_nulls_keys_and_indexes_and_no_value() {
        let mut catalog = catalog();
        catalog.schemas[1].relations.push(relation(
            "order_items",
            &[("order_id", "bigint"), ("line_no", "smallint")],
        ));
        let action = Action::ExplainPlan {
            sql: "select * from shop.order_items i join shop.customers c on true".into(),
            plan: "Nested Loop".into(),
        };
        let mut context =
            Context::from_catalog(&catalog, "PostgreSQL", None, None, None, action.text());
        // The tables the statement names, in its order, are the ones read in full.
        let items = TableRef::new("shop", "order_items");
        assert_eq!(
            context.detail_tables(),
            [items.clone(), TableRef::new("shop", "customers")]
        );
        let details = [(items, Detail::of(&order_items()))].into_iter().collect();
        context.add_details(&details);
        let system = build_payload(&action, &context, "m")["messages"][0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            system.contains(
                "- shop.order_items (table)\n  order_id bigint not null\n  line_no smallint not \
                 null\n  product_id bigint not null\n  quantity integer not null\n  note text\n  \
                 primary key (order_id, line_no)\n  foreign key (order_id) references \
                 shop.orders (id)\n  foreign key (product_id) references shop.products (id)\n  \
                 index (product_id)\n  unique (note)\n  index (quantity)\n  index (line_no \
                 desc, (expression))\n- shop.customers \
                 (table): id bigint, email text"
            ),
            "{system}"
        );
        for never in [
            "kerno-default-value",
            "kerno-column-comment",
            "kerno-table-comment",
            "kerno-literal",
            "coalesce",
            "lower(",
            "4242",
            "4243",
            "CREATE INDEX",
            "pkey",
        ] {
            assert!(!system.contains(never), "{never}:\n{system}");
        }
    }

    /// K50: a table with more columns, keys or indexes than its caps says how many are left
    /// out; at most [`MAX_DETAILED`] tables are read.
    #[test]
    fn a_table_read_in_full_stays_within_its_caps() {
        let mut structure = order_items();
        structure.columns = (0..MAX_COLUMNS + 7)
            .map(|n| column(&format!("c{n}"), "int", true, None))
            .collect();
        // The foreign keys' column.
        structure.columns[0].name = "order_id".into();
        structure.indexes = (0..MAX_INDEXES + 3)
            .map(|n| index(&format!("i{n}"), &["c1"], false, false))
            .collect();
        let key = structure.foreign_keys[0].clone();
        structure.foreign_keys = vec![key; MAX_KEYS + 2];
        let detail = Detail::of(&structure);
        assert_eq!(detail.columns.len(), MAX_COLUMNS);
        assert_eq!(
            (detail.more_columns, detail.more_indexes, detail.more_keys),
            (7, 3, 2)
        );
        let mut text = String::new();
        detail.describe(&mut text);
        assert!(
            text.contains("\n  c99 int\n  … 7 more columns\n  foreign key"),
            "{text}"
        );
        assert!(text.contains("\n  … 2 more foreign keys"), "{text}");
        assert!(
            text.ends_with("\n  index (c1)\n  … 3 more indexes"),
            "{text}"
        );
        // A table of the catalog alone is capped the same way.
        let wide = relation(
            "wide",
            &(0..MAX_COLUMNS + 1)
                .map(|n| (format!("c{n}"), "int".to_string()))
                .collect::<Vec<_>>()
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect::<Vec<_>>(),
        );
        let relations = std::iter::once(wide)
            .chain((0..MAX_DETAILED + 3).map(|n| relation(&format!("t{n}"), &[("id", "int")])))
            .collect();
        let catalog = Catalog {
            schemas: vec![CatalogSchema {
                name: "big".into(),
                relations,
                routines: Vec::new(),
            }],
            search_path: vec!["big".into()],
        };
        let context = Context::from_catalog(&catalog, "MySQL", None, None, None, "x");
        assert_eq!(context.detail_tables().len(), MAX_DETAILED);
        let system = build_payload(&write("x"), &context, "m")["messages"][0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(system.contains(", c99 int, … 1 more columns\n"), "{system}");
    }

    #[test]
    fn beyond_the_cap_tables_are_named_only() {
        let relations = (0..MAX_TABLES + 5)
            .map(|n| relation(&format!("t{n}"), &[("id", "int")]))
            .collect();
        let catalog = Catalog {
            schemas: vec![CatalogSchema {
                name: "big".into(),
                relations,
                routines: Vec::new(),
            }],
            search_path: vec!["big".into()],
        };
        let context = Context::from_catalog(&catalog, "MySQL", None, None, None, "x");
        assert_eq!(context.tables.len(), MAX_TABLES + 5);
        assert!(context.tables[MAX_TABLES - 1].columns.len() == 1);
        assert!(context.tables[MAX_TABLES].columns.is_empty());
        assert_eq!(context.sample_tables().len(), SAMPLE_TABLES);
        let payload = build_payload(&write("x"), &context, "m");
        let system = payload["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("Other tables (names only): big.t200, big.t201"));
    }

    #[test]
    fn the_payload_holds_names_and_types_and_rows_only_when_included() {
        let action = write("list the customers");
        let mut context = Context::from_catalog(
            &catalog(),
            "PostgreSQL",
            Some("17.2".into()),
            Some("kerno".into()),
            None,
            action.text(),
        );
        let payload = build_payload(&action, &context, "llama3");
        assert_eq!(payload["model"], "llama3");
        // K51: the answer as it is written; the instruction in the user's turn, before it.
        assert_eq!(payload["stream"], true);
        let user = payload["messages"][1]["content"].as_str().unwrap();
        assert!(
            user.starts_with("Write one PostgreSQL SQL statement"),
            "{user}"
        );
        assert!(user.ends_with("\n\nlist the customers"), "{user}");
        let system = payload["messages"][0]["content"].as_str().unwrap();
        assert!(
            system.contains("- shop.customers (table): id bigint, email text"),
            "{system}"
        );
        assert!(system.contains("PostgreSQL (server version: 17.2)"));
        assert!(!system.contains("Sample rows"));
        context.sample_rows = Some(vec![Sample {
            table: TableRef::new("shop", "customers"),
            columns: vec!["id".into(), "email".into()],
            rows: vec![
                vec![Some("1".into()), Some("ada@example.com".into())],
                vec![Some("2".into()), None],
            ],
        }]);
        let system = build_payload(&action, &context, "llama3")["messages"][0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(system.contains("shop.customers: id | email\n  1 | ada@example.com\n  2 | NULL"));
        // The preview is the body, byte for byte.
        let payload = build_payload(&action, &context, "llama3");
        assert_eq!(preview(&payload).into_bytes(), body(&payload));
        assert_eq!(
            serde_json::from_slice::<Value>(&body(&payload)).unwrap(),
            payload
        );
    }

    #[test]
    fn each_action_says_what_it_asks() {
        let context = Context::default();
        let plan = Action::ExplainPlan {
            sql: "select 1".into(),
            plan: "Seq Scan".into(),
        };
        let user = build_payload(&plan, &context, "m")["messages"][1]["content"].clone();
        assert_eq!(
            user,
            "Explain this SQL query plan in plain words: what each step does, which step costs \
             the most and why, and what could make the query faster.\n\nQuery:\n```sql\nselect \
             1\n```\n\nPlan:\n```\nSeq Scan\n```"
        );
        let error = Action::ExplainError {
            sql: "selec 1".into(),
            error: "syntax error at or near \"selec\"".into(),
        };
        let payload = build_payload(&error, &context, "m");
        assert!(
            payload["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("Error:\nsyntax error")
        );
        assert!(
            payload["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("propose a fix")
        );
    }

    #[test]
    fn only_https_or_this_computer_and_no_key_in_the_address() {
        for local in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:1234/v1/",
            "http://127.8.9.10/v1",
            "http://[::1]:8080/v1",
        ] {
            let endpoint = endpoint(local).unwrap();
            assert!(endpoint.loopback, "{local}");
            assert!(!endpoint.base.ends_with('/'));
        }
        let remote = endpoint("https://api.example.com/v1").unwrap();
        assert!(!remote.loopback);
        assert_eq!(remote.host, "api.example.com");
        for refused in [
            "",
            "http://api.example.com/v1",
            "http://192.168.1.2:11434/v1",
            "http://localhost.example.com/v1",
            "ftp://localhost/v1",
            "https://user:sk-123@api.example.com/v1",
            "https://api.example.com/v1?key=sk-123",
            "localhost:11434",
        ] {
            assert!(endpoint(refused).is_err(), "{refused}");
        }
    }

    /// Nothing is sent while off, to a refused address or to an unconfirmed remote host: the
    /// checks come before any connection (the fake server tests count connections, see
    /// `tests/ai.rs`).
    #[test]
    fn the_one_sender_refuses_before_connecting() {
        let payload = json!({});
        let off = AiSettings {
            enabled: false,
            ..on()
        };
        assert_eq!(
            request(&off, None, &Call::Chat(payload.clone())),
            Err(AiError::Off)
        );
        assert_eq!(list_models(&off, None), Err(AiError::Off));
        let plain = AiSettings {
            endpoint: "http://api.example.com/v1".into(),
            ..on()
        };
        assert!(matches!(
            request(&plain, None, &Call::Models),
            Err(AiError::Endpoint(_))
        ));
        let remote = AiSettings {
            endpoint: "https://api.example.invalid/v1".into(),
            ..on()
        };
        assert_eq!(
            request(&remote, None, &Call::Models),
            Err(AiError::NotConfirmed("api.example.invalid".into()))
        );
        assert!(AiError::Off.to_string().contains("Settings → AI"));
    }

    #[test]
    fn the_sql_of_an_answer_is_its_first_sql_block() {
        assert_eq!(
            sql_block("Here:\n```python\nx\n```\n```sql\nSELECT 1;\n```\nDone."),
            "SELECT 1;"
        );
        assert_eq!(sql_block("```\nSELECT 2\n```"), "SELECT 2");
        assert_eq!(sql_block("  SELECT 3  "), "SELECT 3");
        assert_eq!(sql_block("```sql\nunclosed"), "```sql\nunclosed");
    }

    /// The panel shows an answer without its fences, the prose kept.
    #[test]
    fn an_answer_is_shown_without_its_fences() {
        assert_eq!(
            unfenced("Here:\n```sql\nSELECT 1;\n```\nDone."),
            "Here:\nSELECT 1;\nDone."
        );
        assert_eq!(unfenced("```\nSELECT 2\n```"), "SELECT 2");
        assert_eq!(unfenced("plain"), "plain");
    }

    /// Write SQL's answer never joins a neighbouring word: on its own line at a caret, in
    /// place of a selection.
    #[test]
    fn inserted_sql_goes_on_its_own_line() {
        let sql = "SELECT 2;";
        let text = "select 1 as one";
        let end = text.len();
        assert_eq!(placed(text, end..end, sql), ("\nSELECT 2;".into(), 1));
        assert_eq!(placed(text, 0..0, sql), ("SELECT 2;\n".into(), 0));
        assert_eq!(placed(text, 6..6, sql), ("\nSELECT 2;\n".into(), 1));
        assert_eq!(placed("", 0..0, sql), ("SELECT 2;".into(), 0));
        assert_eq!(placed("a\n  \nb", 3..3, sql), ("SELECT 2;".into(), 0));
        assert_eq!(placed(text, 0..6, sql), ("SELECT 2;".into(), 0));
    }

    /// The real-app pass after K51: Insert after `… limit 50` (no `;`) made one broken
    /// statement. After Insert the statement splitter finds the SQL as statements of its own.
    #[test]
    fn inserted_sql_runs_as_statements_of_its_own() {
        use crate::sql::{LexRules, split_statements};
        let insert = |text: &str, range: std::ops::Range<usize>, sql: &str| {
            let (replaced, new, start) = insertion(text, range.clone(), sql, LexRules::POSTGRES);
            let result = format!("{}{new}{}", &text[..replaced.start], &text[replaced.end..]);
            assert!(
                new[start..].starts_with(sql.trim().trim_end_matches(';')),
                "{new:?}"
            );
            let statements: Vec<String> = split_statements(&result, LexRules::POSTGRES)
                .iter()
                .map(|span| span.text(&result).to_string())
                .collect();
            (result, statements)
        };
        // Not ended: ended with `;`, in the same edit.
        let text = "select * from shop.orders limit 50";
        let (result, statements) = insert(text, text.len()..text.len(), "SELECT 1;");
        assert_eq!(result, "select * from shop.orders limit 50;\nSELECT 1;");
        assert_eq!(
            statements,
            ["select * from shop.orders limit 50", "SELECT 1"]
        );
        // A line comment after it: the `;` goes before the comment, not in it.
        let text = "select 1 -- the first";
        let (_, statements) = insert(text, text.len()..text.len(), "SELECT 2;");
        assert_eq!(statements.len(), 2, "{statements:?}");
        assert_eq!(statements[1], "SELECT 2");
        // Ended: nothing added.
        let text = "select 1;";
        let (result, statements) = insert(text, text.len()..text.len(), "SELECT 2;");
        assert_eq!(result, "select 1;\nSELECT 2;");
        assert_eq!(statements, ["select 1", "SELECT 2"]);
        // Empty editor.
        let (result, statements) = insert("", 0..0, "SELECT 3");
        assert_eq!((result.as_str(), statements.len()), ("SELECT 3", 1));
        // Between two statements, the SQL without `;`: ended, so the next one stays its own.
        let text = "select 1;\n\nselect 3;";
        let (result, statements) = insert(text, 10..10, "SELECT 2");
        assert_eq!(result, "select 1;\nSELECT 2;\nselect 3;");
        assert_eq!(statements, ["select 1", "SELECT 2", "select 3"]);
        // A selection is replaced, as before.
        let text = "select 1; select 2";
        let (result, statements) = insert(text, 10..18, "SELECT 9");
        assert_eq!(result, "select 1; SELECT 9");
        assert_eq!(statements, ["select 1", "SELECT 9"]);
    }

    #[test]
    fn settings_hold_no_key_and_load_from_older_stores() {
        let settings: AiSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, AiSettings::default());
        assert!(!settings.enabled);
        assert_eq!(
            key_name("http://localhost:11434/v1"),
            "ai/http://localhost:11434/v1"
        );
    }

    /// K23: the reasoning of Qwen, DeepSeek and GLM variants never reaches the panel, Copy or
    /// Insert: a separate field is not read, `<think>` is taken out in every shape.
    #[test]
    fn the_reasoning_is_left_out_of_the_answer() {
        assert_eq!(
            strip_reasoning("<think>```sql\nDROP TABLE t;\n```</think>\n```sql\nSELECT 1;\n```"),
            "```sql\nSELECT 1;\n```"
        );
        // The template wrote `<think>`: the text starts inside it.
        assert_eq!(
            strip_reasoning("weighing it…</think>The answer."),
            "The answer."
        );
        // Cut short while thinking: nothing of it.
        assert_eq!(strip_reasoning("Here.<think>and then"), "Here.");
        assert_eq!(strip_reasoning("<think>a</think>x<think>b</think>y"), "xy");
        assert_eq!(strip_reasoning("plain"), "plain");
        let body = |message: Value| json!({"choices": [{"message": message}]}).to_string();
        let separate = body(json!({"content": "```sql\nSELECT 2;\n```",
            "reasoning_content": "```sql\nDELETE FROM t;\n```"}));
        assert_eq!(answer_text(&separate).unwrap(), "```sql\nSELECT 2;\n```");
        assert_eq!(sql_block(&answer_text(&separate).unwrap()), "SELECT 2;");
        let inline = body(
            json!({"content": "<think>```sql\nDELETE FROM t;\n```</think>```sql\nSELECT 3;\n```"}),
        );
        assert_eq!(sql_block(&answer_text(&inline).unwrap()), "SELECT 3;");
        // Only reasoning, the budget spent: said in plain words.
        let only = body(json!({"content": null, "reasoning_content": "long thoughts"}));
        let why = answer_text(&only).unwrap_err().to_string();
        assert!(why.contains("reasoning but no answer"), "{why}");
    }

    /// K23: the client trusts what the system trusts and no less; a certificate whose
    /// authority the system does not trust is refused, said with the authority's name. A
    /// local TLS server with the certificate `scripts/test-env.sh` generates for the test
    /// servers (no private key is kept in the repository); nothing leaves the machine.
    #[test]
    fn a_certificate_the_system_does_not_trust_is_said_with_its_issuer() {
        use std::net::TcpListener;
        let (pem, key) = (
            crate::testing::generated("tls/server.crt"),
            crate::testing::generated("tls/server.key"),
        );
        let (Ok(pem), Ok(key)) = (std::fs::read(&pem), std::fs::read(&key)) else {
            let message = format!(
                "{} is missing; generate the test certificates with `scripts/test-env.sh up`",
                pem.display()
            );
            if std::env::var_os("KERNO_REQUIRE_CONTAINERS").is_some() {
                panic!("{message}");
            }
            eprintln!("SKIPPED: {message}");
            return;
        };
        let cert = ureq::tls::Certificate::from_pem(&pem).unwrap();
        assert_eq!(
            tls::issuer_name(cert.der()).as_deref(),
            Some("Kerno Test CA")
        );
        let key = ureq::tls::PrivateKey::from_pem(&key).unwrap();
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().to_vec().into()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.der().to_vec().into()),
            )
            .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut server = rustls::ServerConnection::new(config.clone()).unwrap();
                while server.is_handshaking() {
                    if server.complete_io(&mut stream).is_err() {
                        break;
                    }
                }
            }
        });
        let settings = AiSettings {
            enabled: true,
            endpoint: format!("https://127.0.0.1:{port}/v1"),
            ..AiSettings::default()
        };
        let error = list_models(&settings, Some(&Secret::new("sk-tls-0005"))).unwrap_err();
        let text = error.to_string();
        assert!(matches!(error, AiError::Network(_)), "{error:?}");
        assert!(
            text.starts_with(
                "127.0.0.1 presented a certificate this computer does not trust. It names \
                 “Kerno Test CA” as its issuer, a name Kerno could not verify"
            ),
            "{text}"
        );
        // The name a server claims is never a reason to trust it: the user is sent to the
        // administrator, and told the connection may be intercepted.
        assert!(
            text.contains("from its administrator, not from this server"),
            "{text}"
        );
        assert!(text.contains("may be intercepted"), "{text}");
        assert!(!text.contains("add that authority"), "{text}");
        assert!(!text.contains("sk-tls"), "{text}");
    }
}

// K53 (D-122): what a right-click asks the assistant.
pub mod ask;

// K52 (D-121): the assistant asked in the editor, and its proposal as a diff.
pub mod inline;
