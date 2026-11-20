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

use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Secret;
use crate::dialect::Dialect;
use crate::schema::{RelationKind, TableRef};
use crate::sql::Catalog;

pub mod endpoints;
mod tls;

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
    fn text(&self) -> &str {
        match self {
            Self::WriteSql { sentence } => sentence,
            Self::ExplainQuery { sql }
            | Self::ExplainPlan { sql, .. }
            | Self::ExplainError { sql, .. } => sql,
        }
    }

    fn instructions(&self, engine: &str) -> String {
        match self {
            Self::WriteSql { .. } => format!(
                "Write one {engine} SQL statement for the user's request, using only the tables \
                 and columns below. Give the statement in one ```sql block, then at most two \
                 sentences about it."
            ),
            Self::ExplainQuery { .. } => format!(
                "Explain what the user's {engine} query does, clause by clause, in plain words. \
                 Point out anything that looks wrong or slow."
            ),
            Self::ExplainPlan { .. } => format!(
                "Explain the user's {engine} query plan in plain words: what each step does, \
                 which step costs the most and why, and what could make the query faster."
            ),
            Self::ExplainError { .. } => format!(
                "Explain the {engine} error the user got, in plain words, and propose a fix: \
                 the corrected statement in one ```sql block."
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
    /// The context of `action` from the catalog a query tab loaded for completion: the tables
    /// the SQL or the sentence names, else the tables of `schema` (the tab's, or the first of
    /// the search path); the first [`MAX_TABLES`] with their columns, then names only. No
    /// sample rows.
    pub fn from_catalog(
        catalog: &Catalog,
        engine: &str,
        server_version: Option<String>,
        database: Option<String>,
        schema: Option<&str>,
        action: &Action,
    ) -> Context {
        let words: Vec<String> = action
            .text()
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect();
        let named = |name: &str| {
            let name = name.to_lowercase();
            words.iter().any(|word| {
                *word == name || format!("{word}s") == name || format!("{name}s") == *word
            })
        };
        let all = || {
            catalog
                .schemas
                .iter()
                .flat_map(|schema| schema.relations.iter().map(move |r| (schema, r)))
        };
        let mut chosen: Vec<_> = all()
            .filter(|(_, relation)| named(&relation.name))
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
        text.push_str("\nTables and columns (name type):");
        if full.is_empty() {
            text.push_str(" none known");
        }
        for table in full {
            let kind = if table.view { "view" } else { "table" };
            let columns: Vec<String> = (table.columns.iter())
                .map(|(name, data_type)| format!("{name} {data_type}"))
                .collect();
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

/// The request body of `action`, an OpenAI-compatible chat completion: the only way one is
/// made. The model hears the instructions and the context, then the user's request.
pub fn build_payload(action: &Action, context: &Context, model: &str) -> Value {
    let system = format!(
        "You are the assistant of Kerno, a database client. {}\nAnswer in plain text. Kerno \
         never runs what you write: the user reads it and decides.\n\n{}",
        action.instructions(&context.engine),
        context.describe()
    );
    json!({
        "model": model,
        "stream": false,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": action.request()},
        ],
    })
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

/// The one function that opens a socket for the assistant's requests (Detect goes through it
/// too, with its own timeout), only after [`gate`]. The one other connection is the issuer
/// check of a refused certificate (`tls::issuer`), to the same host, after the same gate.
fn send(
    settings: &AiSettings,
    key: Option<&Secret>,
    call: &Call,
    stop: &Stopper,
    timeout: Duration,
) -> Result<String, AiError> {
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
    Ok(text)
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
        error if tls::unknown_issuer(error) => {
            let issuer = tls::issuer(host, endpoint.port, Duration::from_secs(5))
                .map(|name| format!("“{name}”"))
                .unwrap_or_else(|| "an authority".into());
            format!(
                "The certificate of {host} was issued by {issuer}, which this computer does not \
                 trust. Kerno trusts the certificates the system trusts: add that authority to \
                 the system's trust store (Keychain Access on macOS), then test again."
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
    let text = request_until(settings, key, &Call::Chat(payload.clone()), stop)?;
    answer_text(&text)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::{CatalogColumn, CatalogForeignKey, CatalogRelation, CatalogSchema};

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
        let context = Context::from_catalog(&catalog(), "PostgreSQL", None, None, None, &action);
        let names: Vec<&str> = context.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["customers", "orders"]);
        assert_eq!(context.schemas, ["public", "shop"]);
        assert_eq!(
            context.tables[1].foreign_keys,
            ["customer_id -> shop.customers(id)"]
        );
        // Nothing named: the tab's schema, else the first of the search path.
        let action = write("how many rows?");
        let context =
            Context::from_catalog(&catalog(), "PostgreSQL", None, None, Some("shop"), &action);
        assert_eq!(context.tables.len(), 3);
        let context = Context::from_catalog(&catalog(), "PostgreSQL", None, None, None, &action);
        assert_eq!(context.tables[0].name, "all_types");
        // From SQL too, qualified or not.
        let action = Action::ExplainQuery {
            sql: "select * from shop.products p".into(),
        };
        let context = Context::from_catalog(&catalog(), "PostgreSQL", None, None, None, &action);
        assert_eq!(context.tables.len(), 1);
        assert!(context.sample_rows.is_none());
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
        let context = Context::from_catalog(&catalog, "MySQL", None, None, None, &write("x"));
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
            &action,
        );
        let payload = build_payload(&action, &context, "llama3");
        assert_eq!(payload["model"], "llama3");
        assert_eq!(payload["stream"], false);
        assert_eq!(payload["messages"][1]["content"], "list the customers");
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
            "Query:\n```sql\nselect 1\n```\n\nPlan:\n```\nSeq Scan\n```"
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
            payload["messages"][0]["content"]
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
                "The certificate of 127.0.0.1 was issued by “Kerno Test CA”, which this \
                 computer does not trust. Kerno trusts the certificates the system trusts"
            ),
            "{text}"
        );
        assert!(!text.contains("sk-tls"), "{text}");
    }
}
