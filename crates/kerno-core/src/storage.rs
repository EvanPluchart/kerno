//! Kerno's own storage: saved connections (without secrets), query history,
//! saved queries, open tabs and settings, in one SQLite file in the
//! platform data directory.
//!
//! The API is synchronous; callers run it off the UI thread. Nothing secret
//! is ever written here: connection settings have no secret fields, and
//! history text goes through [`redact_secrets`] first.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::ConnectionConfig;
use crate::error::{Error, Result};
use crate::sql::{LexRules, TokenKind, lex};
use crate::tools::TaskSettings;

/// Schema migrations, applied in order. `PRAGMA user_version` records how
/// many have run. Never edit an entry that has shipped: add a new one.
const MIGRATIONS: &[&str] = &[
    // 1: initial schema.
    "CREATE TABLE connections (
         id         TEXT PRIMARY KEY,
         position   INTEGER NOT NULL,
         config     TEXT NOT NULL,
         created_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL
     );
     CREATE TABLE history (
         id            INTEGER PRIMARY KEY AUTOINCREMENT,
         connection_id TEXT NOT NULL,
         database_name TEXT,
         sql           TEXT NOT NULL,
         started_at    INTEGER NOT NULL,
         duration_ms   INTEGER NOT NULL,
         status        TEXT NOT NULL,
         row_count     INTEGER,
         error         TEXT
     );
     CREATE INDEX history_connection_time ON history (connection_id, started_at DESC);
     CREATE TABLE saved_queries (
         id            INTEGER PRIMARY KEY AUTOINCREMENT,
         connection_id TEXT,
         folder        TEXT NOT NULL DEFAULT '',
         name          TEXT NOT NULL,
         sql           TEXT NOT NULL,
         created_at    INTEGER NOT NULL,
         updated_at    INTEGER NOT NULL
     );
     CREATE TABLE tabs (
         position INTEGER PRIMARY KEY,
         state    TEXT NOT NULL
     );
     CREATE TABLE settings (
         key   TEXT PRIMARY KEY,
         value TEXT NOT NULL
     );",
    // 2: the filters last applied to each table's data and query's result.
    "CREATE TABLE filter_history (
         key     TEXT NOT NULL,
         filter  TEXT NOT NULL,
         used_at INTEGER NOT NULL,
         PRIMARY KEY (key, filter)
     );",
    // 3: saved tasks (the tool wizards' settings, run again on demand). No
    // foreign key to `connections`: a task outlives its connection and says
    // it cannot run.
    "CREATE TABLE tasks (
         id            INTEGER PRIMARY KEY AUTOINCREMENT,
         name          TEXT NOT NULL,
         description   TEXT NOT NULL DEFAULT '',
         connection_id TEXT NOT NULL,
         kind          TEXT NOT NULL,
         settings      TEXT NOT NULL,
         created_at    INTEGER NOT NULL,
         last_run_at   INTEGER,
         last_status   TEXT,
         last_log      TEXT NOT NULL DEFAULT ''
     );",
];

/// How many filters [`Storage::filters`] keeps for one key.
pub const FILTERS_KEPT: usize = 20;

/// A connection as stored: its id and its settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedConnection {
    /// Stable identifier, also the key of the connection's secrets.
    pub id: String,
    /// The settings.
    pub config: ConnectionConfig,
}

/// How a statement ended, for the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HistoryStatus {
    /// Ran to completion.
    Ok,
    /// Failed with an error.
    Error,
    /// Cancelled by the user.
    Cancelled,
}

impl HistoryStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "error" => Self::Error,
            "cancelled" => Self::Cancelled,
            _ => Self::Ok,
        }
    }
}

/// One executed statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Row id; 0 for an entry not stored yet.
    pub id: i64,
    /// Connection the statement ran on.
    pub connection_id: String,
    /// Database it ran in.
    pub database: Option<String>,
    /// The statement, with secrets redacted.
    pub sql: String,
    /// When it started, in milliseconds since the Unix epoch.
    pub started_at_ms: i64,
    /// How long it took.
    pub duration_ms: i64,
    /// How it ended.
    pub status: HistoryStatus,
    /// Rows returned or affected, when known.
    pub row_count: Option<i64>,
    /// The error message, for failed statements.
    pub error: Option<String>,
}

/// A query the user saved under a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedQuery {
    /// Row id; 0 for a query not stored yet.
    pub id: i64,
    /// Connection it belongs to, or `None` when usable with any.
    pub connection_id: Option<String>,
    /// Folder path, `/`-separated; empty for the top level.
    pub folder: String,
    /// Name.
    pub name: String,
    /// The SQL text.
    pub sql: String,
}

/// Lines of a task run's log kept: the last ones.
pub const TASK_LOG_LINES: usize = 5_000;

/// A saved task (DBeaver's `TaskImpl`): a tool's settings, run again on
/// demand. No scheduler, as in DBeaver Community.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedTask {
    /// Row id; 0 for a task not stored yet.
    pub id: i64,
    pub name: String,
    pub description: String,
    /// The connection it runs on, looked up at each run.
    pub connection_id: String,
    /// The tool and its settings, with no secret.
    pub settings: TaskSettings,
    pub created_at_ms: i64,
    pub last_run_at_ms: Option<i64>,
    pub last_status: Option<HistoryStatus>,
    /// The last run's command and output, secrets redacted.
    pub last_log: String,
}

impl SavedTask {
    /// A task not stored yet.
    pub fn new(
        name: impl Into<String>,
        connection_id: impl Into<String>,
        settings: TaskSettings,
    ) -> Self {
        Self {
            id: 0,
            name: name.into(),
            description: String::new(),
            connection_id: connection_id.into(),
            settings,
            created_at_ms: 0,
            last_run_at_ms: None,
            last_status: None,
            last_log: String::new(),
        }
    }
}

/// Kerno's SQLite store.
pub struct Storage {
    connection: Mutex<Connection>,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Storage")
    }
}

fn storage_error(error: rusqlite::Error) -> Error {
    Error::Storage(error.to_string())
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

impl Storage {
    /// Opens (creating it and its directory if needed) the store at `path`
    /// and brings its schema up to date.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| Error::io(format!("creating {}", parent.display()), error))?;
            restrict(parent, 0o700)?;
        }
        // History holds query text: the file is the user's alone. It is
        // created here, before SQLite opens it, so it never exists with
        // wider permissions; SQLite gives its -wal and -shm files the same
        // mode as the main file.
        if !path.exists() {
            create_private(path)?;
        }
        restrict(path, 0o600)?;
        let connection = Connection::open(path).map_err(storage_error)?;
        // WAL: readers never block the writer, and a crash cannot corrupt
        // the file.
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(storage_error)?;
        Self::prepare(connection)
    }

    /// A store that lives in memory, for tests.
    pub fn in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory().map_err(storage_error)?)
    }

    fn prepare(mut connection: Connection) -> Result<Self> {
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(storage_error)?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    fn with<T>(&self, work: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> Result<T> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| Error::Storage("the storage lock was poisoned by a panic".into()))?;
        work(&mut connection).map_err(storage_error)
    }

    // Connections -----------------------------------------------------------

    /// Saved connections, in the order the user arranged them.
    pub fn connections(&self) -> Result<Vec<SavedConnection>> {
        let rows = self.with(|c| {
            c.prepare("SELECT id, config FROM connections ORDER BY position, created_at")?
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })?;
        rows.into_iter()
            .map(|(id, json)| {
                let config = serde_json::from_str(&json).map_err(|error| {
                    Error::Storage(format!("stored connection {id} is unreadable: {error}"))
                })?;
                Ok(SavedConnection { id, config })
            })
            .collect()
    }

    /// Stores a connection. With `id` it replaces that connection; without,
    /// it adds a new one at the end. Returns the id.
    pub fn save_connection(&self, id: Option<&str>, config: &ConnectionConfig) -> Result<String> {
        let json = serde_json::to_string(config)
            .map_err(|error| Error::Storage(format!("could not encode the connection: {error}")))?;
        let now = now_ms();
        match id {
            Some(id) => {
                let updated = self.with(|c| {
                    c.execute(
                        "UPDATE connections SET config = ?2, updated_at = ?3 WHERE id = ?1",
                        params![id, json, now],
                    )
                })?;
                if updated == 0 {
                    return Err(Error::Storage(format!("no saved connection has id {id}")));
                }
                Ok(id.to_string())
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                self.with(|c| {
                    c.execute(
                        "INSERT INTO connections (id, position, config, created_at, updated_at)
                         VALUES (?1, (SELECT COALESCE(MAX(position), 0) + 1 FROM connections), ?2, ?3, ?3)",
                        params![id, json, now],
                    )
                })?;
                Ok(id)
            }
        }
    }

    /// Removes a connection with its history and its saved queries. The
    /// caller removes its secrets from the secret store.
    pub fn delete_connection(&self, id: &str) -> Result<()> {
        self.with(|c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM history WHERE connection_id = ?1", [id])?;
            tx.execute("DELETE FROM saved_queries WHERE connection_id = ?1", [id])?;
            tx.execute("DELETE FROM connections WHERE id = ?1", [id])?;
            tx.commit()
        })
    }

    // History ---------------------------------------------------------------

    /// Records an executed statement. `rules` are the lexical rules of the
    /// engine it ran on, used to redact secrets from its text.
    pub fn add_history(&self, entry: &HistoryEntry, rules: LexRules) -> Result<i64> {
        let sql = redact_secrets(&entry.sql, rules);
        // The server's message may quote the statement: see ERROR_WITHHELD.
        let error = match &entry.error {
            Some(_) if sql != entry.sql => Some(ERROR_WITHHELD),
            other => other.as_deref(),
        };
        self.with(|c| {
            c.execute(
                "INSERT INTO history
                     (connection_id, database_name, sql, started_at, duration_ms, status, row_count, error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    entry.connection_id,
                    entry.database,
                    sql,
                    entry.started_at_ms,
                    entry.duration_ms,
                    entry.status.as_str(),
                    entry.row_count,
                    error,
                ],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    /// The most recent statements, newest first. `connection_id` limits the
    /// list to one connection; `search` (case-insensitive, may be empty)
    /// must appear in the statement text.
    pub fn history(
        &self,
        connection_id: Option<&str>,
        search: &str,
        limit: u32,
    ) -> Result<Vec<HistoryEntry>> {
        // LIKE with the search text as data: escape its wildcards.
        let pattern = format!(
            "%{}%",
            search
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        self.with(|c| {
            c.prepare(
                "SELECT id, connection_id, database_name, sql, started_at, duration_ms, status, row_count, error
                 FROM history
                 WHERE (?1 IS NULL OR connection_id = ?1) AND sql LIKE ?2 ESCAPE '\\'
                 ORDER BY started_at DESC, id DESC
                 LIMIT ?3",
            )?
            .query_map(params![connection_id, pattern, limit], |row| {
                Ok(HistoryEntry {
                    id: row.get(0)?,
                    connection_id: row.get(1)?,
                    database: row.get(2)?,
                    sql: row.get(3)?,
                    started_at_ms: row.get(4)?,
                    duration_ms: row.get(5)?,
                    status: HistoryStatus::parse(&row.get::<_, String>(6)?),
                    row_count: row.get(7)?,
                    error: row.get(8)?,
                })
            })?
            .collect()
        })
    }

    /// Deletes history: of one connection, or all of it.
    pub fn clear_history(&self, connection_id: Option<&str>) -> Result<()> {
        self.with(|c| {
            c.execute(
                "DELETE FROM history WHERE ?1 IS NULL OR connection_id = ?1",
                params![connection_id],
            )
        })
        .map(|_| ())
    }

    // Saved queries -----------------------------------------------------------

    /// Every saved query, by folder then name.
    pub fn saved_queries(&self) -> Result<Vec<SavedQuery>> {
        self.with(|c| {
            c.prepare(
                "SELECT id, connection_id, folder, name, sql FROM saved_queries
                 ORDER BY folder COLLATE NOCASE, name COLLATE NOCASE, id",
            )?
            .query_map([], |row| {
                Ok(SavedQuery {
                    id: row.get(0)?,
                    connection_id: row.get(1)?,
                    folder: row.get(2)?,
                    name: row.get(3)?,
                    sql: row.get(4)?,
                })
            })?
            .collect()
        })
    }

    /// Stores a query: a new one when `query.id` is 0, otherwise an update.
    /// Returns its id.
    pub fn save_query(&self, query: &SavedQuery) -> Result<i64> {
        let now = now_ms();
        if query.id == 0 {
            return self.with(|c| {
                c.execute(
                    "INSERT INTO saved_queries (connection_id, folder, name, sql, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    params![query.connection_id, query.folder, query.name, query.sql, now],
                )?;
                Ok(c.last_insert_rowid())
            });
        }
        let updated = self.with(|c| {
            c.execute(
                "UPDATE saved_queries
                 SET connection_id = ?2, folder = ?3, name = ?4, sql = ?5, updated_at = ?6
                 WHERE id = ?1",
                params![
                    query.id,
                    query.connection_id,
                    query.folder,
                    query.name,
                    query.sql,
                    now
                ],
            )
        })?;
        if updated == 0 {
            return Err(Error::Storage(format!(
                "no saved query has id {}",
                query.id
            )));
        }
        Ok(query.id)
    }

    /// Deletes a saved query.
    pub fn delete_query(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM saved_queries WHERE id = ?1", [id]))
            .map(|_| ())
    }

    // Tabs ------------------------------------------------------------------

    /// The open tabs as they were last saved, in order.
    pub fn tabs<T: DeserializeOwned>(&self) -> Result<Vec<T>> {
        let rows = self.with(|c| {
            c.prepare("SELECT state FROM tabs ORDER BY position")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })?;
        // A tab written by another version that no longer parses is
        // dropped rather than blocking startup.
        Ok(rows
            .iter()
            .filter_map(|json| serde_json::from_str(json).ok())
            .collect())
    }

    /// Replaces the saved tabs with `tabs`.
    pub fn save_tabs<T: Serialize>(&self, tabs: &[T]) -> Result<()> {
        let encoded: Vec<String> = tabs
            .iter()
            .map(serde_json::to_string)
            .collect::<std::result::Result<_, _>>()
            .map_err(|error| Error::Storage(format!("could not encode a tab: {error}")))?;
        self.with(|c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM tabs", [])?;
            for (position, state) in encoded.iter().enumerate() {
                tx.execute(
                    "INSERT INTO tabs (position, state) VALUES (?1, ?2)",
                    params![position as i64, state],
                )?;
            }
            tx.commit()
        })
    }

    // Settings --------------------------------------------------------------

    /// A stored setting, or `None` when it was never set or no longer parses.
    pub fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let json: Option<String> = self.with(|c| {
            c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
        })?;
        Ok(json.and_then(|json| serde_json::from_str(&json).ok()))
    }

    /// Stores a setting.
    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)
            .map_err(|error| Error::Storage(format!("could not encode setting {key}: {error}")))?;
        self.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, json],
            )
        })
        .map(|_| ())
    }

    // Filter history ------------------------------------------------------

    /// Remembers `filter` (the text of a `WHERE`) as just applied to what
    /// `key` names (a table's data, a statement's result), keeping the last
    /// [`FILTERS_KEPT`]. Nothing is kept when the key or the filter holds
    /// what [`redact_secrets`] would mask, nor an empty filter. Returns
    /// whether it was kept.
    pub fn add_filter(&self, key: &str, filter: &str, rules: LexRules) -> Result<bool> {
        let filter = filter.trim();
        if filter.is_empty()
            || redact_secrets(key, rules) != key
            || redact_secrets(filter, rules) != filter
        {
            return Ok(false);
        }
        self.with(|c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO filter_history (key, filter, used_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (key, filter) DO UPDATE SET used_at = excluded.used_at",
                params![key, filter, now_ms()],
            )?;
            tx.execute(
                "DELETE FROM filter_history WHERE key = ?1 AND filter NOT IN (
                     SELECT filter FROM filter_history WHERE key = ?1
                     ORDER BY used_at DESC, rowid DESC LIMIT ?2)",
                params![key, FILTERS_KEPT as i64],
            )?;
            tx.commit()
        })?;
        Ok(true)
    }

    /// The filters applied to what `key` names, the latest first.
    pub fn filters(&self, key: &str) -> Result<Vec<String>> {
        self.with(|c| {
            let mut statement = c.prepare(
                "SELECT filter FROM filter_history WHERE key = ?1
                 ORDER BY used_at DESC, rowid DESC",
            )?;
            statement
                .query_map([key], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()
        })
    }

    // Tasks -----------------------------------------------------------------
    // After DBeaver's `TaskManagerImpl` (Apache-2.0,
    // `org.jkiss.dbeaver.registry.task`): create, update, delete, and the
    // last run kept with the task (DBeaver keeps every run in files; Kerno
    // keeps the last one).

    /// The saved tasks, by name.
    pub fn tasks(&self) -> Result<Vec<SavedTask>> {
        type Row = (
            i64,
            String,
            String,
            String,
            String,
            i64,
            Option<i64>,
            Option<String>,
            String,
        );
        let rows = self.with(|c| {
            c.prepare(
                "SELECT id, name, description, connection_id, settings, created_at, last_run_at,
                        last_status, last_log
                 FROM tasks ORDER BY name COLLATE NOCASE, id",
            )?
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<Row>>>()
        })?;
        rows.into_iter()
            .map(
                |(id, name, description, connection_id, json, created, run, status, log)| {
                    let settings = serde_json::from_str(&json).map_err(|error| {
                        Error::Storage(format!("stored task “{name}” is unreadable: {error}"))
                    })?;
                    Ok(SavedTask {
                        id,
                        name,
                        description,
                        connection_id,
                        settings,
                        created_at_ms: created,
                        last_run_at_ms: run,
                        last_status: status.as_deref().map(HistoryStatus::parse),
                        last_log: log,
                    })
                },
            )
            .collect()
    }

    /// Stores a task: a new one when its id is 0, else its name,
    /// description, connection and settings (its last run is kept).
    /// Returns its id.
    pub fn save_task(&self, task: &SavedTask) -> Result<i64> {
        let json = serde_json::to_string(&task.settings)
            .map_err(|error| Error::Storage(format!("could not encode the task: {error}")))?;
        let kind = task.settings.kind();
        if task.id == 0 {
            return self.with(|c| {
                c.execute(
                    "INSERT INTO tasks (name, description, connection_id, kind, settings, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![task.name, task.description, task.connection_id, kind, json, now_ms()],
                )?;
                Ok(c.last_insert_rowid())
            });
        }
        let updated = self.with(|c| {
            c.execute(
                "UPDATE tasks SET name = ?2, description = ?3, connection_id = ?4, kind = ?5,
                     settings = ?6
                 WHERE id = ?1",
                params![
                    task.id,
                    task.name,
                    task.description,
                    task.connection_id,
                    kind,
                    json
                ],
            )
        })?;
        if updated == 0 {
            return Err(Error::Storage(format!("no saved task has id {}", task.id)));
        }
        Ok(task.id)
    }

    pub fn delete_task(&self, id: i64) -> Result<()> {
        self.with(|c| c.execute("DELETE FROM tasks WHERE id = ?1", [id]))
            .map(drop)
    }

    /// Keeps how the task's last run ended and its log: the last
    /// [`TASK_LOG_LINES`] lines, each through [`redact_secrets`] with the
    /// `rules` of its engine.
    pub fn record_task_run(
        &self,
        id: i64,
        status: HistoryStatus,
        log: &[String],
        rules: LexRules,
    ) -> Result<()> {
        let kept = &log[log.len().saturating_sub(TASK_LOG_LINES)..];
        let log: Vec<String> = kept
            .iter()
            .map(|line| redact_secrets(line, rules))
            .collect();
        self.with(|c| {
            c.execute(
                "UPDATE tasks SET last_run_at = ?2, last_status = ?3, last_log = ?4 WHERE id = ?1",
                params![id, now_ms(), status.as_str(), log.join("\n")],
            )
        })
        .map(drop)
    }
}

/// Creates an empty file only its owner can read or write.
fn create_private(path: &Path) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map(|_| ())
        .map_err(|error| Error::io(format!("creating {}", path.display()), error))
}

/// Sets Unix permission bits; does nothing on other platforms.
fn restrict(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|error| {
            Error::io(format!("setting permissions of {}", path.display()), error)
        })?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<()> {
    let version: usize = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(storage_error)? as usize;
    if version > MIGRATIONS.len() {
        return Err(Error::Storage(format!(
            "this data was written by a newer version of Kerno (schema {version}, this version knows {})",
            MIGRATIONS.len()
        )));
    }
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(version) {
        let tx = connection.transaction().map_err(storage_error)?;
        tx.execute_batch(migration).map_err(storage_error)?;
        tx.pragma_update(None, "user_version", (index + 1) as i64)
            .map_err(storage_error)?;
        tx.commit().map_err(storage_error)?;
    }
    Ok(())
}

/// What replaces the error text of a statement that contained a secret:
/// servers echo part of the statement in some messages (MySQL's syntax
/// errors quote what follows the error point), possibly cutting a literal
/// in two, so the message cannot be cleaned reliably.
pub const ERROR_WITHHELD: &str = "(message not kept: the statement contained a password)";

/// Removes passwords from a statement, so they never reach the history or
/// a log. Redacted, each becoming `'***'` or `***`:
///
/// - the string after a word ending in `PASSWORD` (`PASSWORD`,
///   `MASTER_PASSWORD`, `SOURCE_PASSWORD`, ...), with optional `=` or `(`;
/// - the string after `IDENTIFIED BY`, `IDENTIFIED WITH plugin BY`,
///   `IDENTIFIED ... AS`, and the old password after `... REPLACE`;
/// - inside any string literal, the value of `password=...` (connection
///   strings) and the password of a `scheme://user:password@host` URL.
///
/// This is a best effort over statements Kerno knows set or carry a
/// password; it cannot recognise a secret used as ordinary data.
pub fn redact_secrets(sql: &str, rules: LexRules) -> String {
    let tokens: Vec<_> = lex(sql, rules)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .collect();
    let word = |index: usize| -> Option<String> {
        tokens
            .get(index)
            .filter(|token| token.kind == TokenKind::Word)
            .map(|token| sql[token.range.clone()].to_ascii_uppercase())
    };
    let identified_within = |index: usize, reach: usize| {
        (1..=reach).any(|back| word(index.wrapping_sub(back)).as_deref() == Some("IDENTIFIED"))
    };

    // Whole literals that are a password.
    let mut whole: Vec<std::ops::Range<usize>> = Vec::new();
    for index in 0..tokens.len() {
        let introduces_secret = match word(index).as_deref() {
            Some(word) if word.ends_with("PASSWORD") => true,
            // IDENTIFIED BY 'password', and MySQL's
            // IDENTIFIED WITH plugin BY 'password' / AS 'hash'.
            Some("BY" | "AS") => identified_within(index, 3),
            // ... IDENTIFIED [WITH plugin] BY 'new' REPLACE 'current'.
            Some("REPLACE") => identified_within(index, 6),
            _ => false,
        };
        if !introduces_secret {
            continue;
        }
        // The secret may follow `=` or sit inside `PASSWORD(...)`.
        let mut next = index + 1;
        while tokens
            .get(next)
            .is_some_and(|token| matches!(&sql[token.range.clone()], "=" | "("))
        {
            next += 1;
        }
        if let Some(token) = tokens.get(next)
            && token.kind == TokenKind::String
        {
            whole.push(token.range.clone());
        }
    }

    // Replacements, in order: whole literals, and secrets inside the others.
    let mut edits: Vec<(std::ops::Range<usize>, &str)> = Vec::new();
    for token in tokens
        .iter()
        .filter(|token| token.kind == TokenKind::String)
    {
        if whole.contains(&token.range) {
            edits.push((token.range.clone(), "'***'"));
            continue;
        }
        for inner in secrets_inside(&sql[token.range.clone()]) {
            edits.push((
                token.range.start + inner.start..token.range.start + inner.end,
                "***",
            ));
        }
    }
    if edits.is_empty() {
        return sql.to_string();
    }
    edits.sort_by_key(|(range, _)| range.start);
    let mut out = String::with_capacity(sql.len());
    let mut position = 0;
    for (range, replacement) in edits {
        out.push_str(&sql[position..range.start]);
        out.push_str(replacement);
        position = range.end;
    }
    out.push_str(&sql[position..]);
    out
}

/// Byte ranges, within a string literal's text, of passwords it carries:
/// `password=value` pairs and `scheme://user:password@host` URLs.
fn secrets_inside(literal: &str) -> Vec<std::ops::Range<usize>> {
    let lower = literal.to_ascii_lowercase();
    let bytes = literal.as_bytes();
    let mut ranges = Vec::new();

    // password = value (libpq connection strings and the like). The key may
    // be longer (`sslpassword`), which is a secret just the same.
    let mut from = 0;
    while let Some(found) = lower[from..].find("password") {
        let mut at = from + found + "password".len();
        from = at;
        while bytes.get(at).is_some_and(|b| *b == b' ') {
            at += 1;
        }
        if bytes.get(at) != Some(&b'=') {
            continue;
        }
        at += 1;
        while bytes.get(at).is_some_and(|b| *b == b' ') {
            at += 1;
        }
        // A quoted value: its quotes are doubled, being inside a literal.
        let end = if literal[at..].starts_with("''") {
            literal[at + 2..]
                .find("''")
                .map_or(literal.len(), |close| at + 2 + close + 2)
        } else {
            literal[at..]
                .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ';' | '&'))
                .map_or(literal.len(), |stop| at + stop)
        };
        if end > at {
            ranges.push(at..end);
            from = end;
        }
    }

    // scheme://user:password@host
    let mut from = 0;
    while let Some(found) = literal[from..].find("://") {
        let authority = from + found + 3;
        from = authority;
        let rest = &literal[authority..];
        let stop = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '\'' | '"'))
            .unwrap_or(rest.len());
        let authority_text = &rest[..stop];
        if let Some(at_sign) = authority_text.rfind('@')
            && let Some(colon) = authority_text[..at_sign].find(':')
            && at_sign > colon + 1
        {
            ranges.push(authority + colon + 1..authority + at_sign);
        }
    }

    ranges.sort_by_key(|range| range.start);
    // Overlaps cannot be applied twice; keep the first of any that collide.
    ranges.dedup_by(|later, earlier| later.start < earlier.end);
    ranges
}

/// Where Kerno keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Directory of the store, the known-hosts file and the logs.
    pub data_dir: PathBuf,
    /// Directory of the user's keymap override.
    pub config_dir: PathBuf,
}

impl AppPaths {
    /// The platform's directories for Kerno, or the directory named by
    /// `KERNO_DATA_DIR` (used by tests and by checks of the real app, so
    /// they never touch the user's data).
    pub fn locate() -> Result<Self> {
        if let Some(dir) = std::env::var_os("KERNO_DATA_DIR").filter(|dir| !dir.is_empty()) {
            return Ok(Self::under(PathBuf::from(dir)));
        }
        let dirs = directories::ProjectDirs::from("", "", "Kerno").ok_or_else(|| {
            Error::Storage("could not determine the home directory to store Kerno's data in".into())
        })?;
        Ok(Self {
            data_dir: dirs.data_dir().to_path_buf(),
            config_dir: dirs.config_dir().to_path_buf(),
        })
    }

    /// Everything under one directory.
    pub fn under(dir: PathBuf) -> Self {
        Self {
            config_dir: dir.clone(),
            data_dir: dir,
        }
    }

    /// The SQLite store.
    pub fn database(&self) -> PathBuf {
        self.data_dir.join("kerno.db")
    }

    /// SSH host keys the user accepted in Kerno.
    pub fn known_hosts(&self) -> PathBuf {
        self.data_dir.join("known_hosts")
    }

    /// Directory of the rotating log files.
    pub fn logs(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// The pictures folders are drawn with, as Kerno copied them.
    pub fn folder_icons(&self) -> PathBuf {
        self.data_dir.join("folder-icons")
    }

    /// The user's keymap override.
    pub fn keymap(&self) -> PathBuf {
        self.config_dir.join("keymap.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Engine, SshAuth, SshConfig};

    fn config(name: &str) -> ConnectionConfig {
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.name = name.into();
        config
    }

    fn entry(connection: &str, sql: &str, started: i64) -> HistoryEntry {
        HistoryEntry {
            id: 0,
            connection_id: connection.into(),
            database: Some("kerno".into()),
            sql: sql.into(),
            started_at_ms: started,
            duration_ms: 12,
            status: HistoryStatus::Ok,
            row_count: Some(3),
            error: None,
        }
    }

    #[test]
    fn connections_are_saved_updated_ordered_and_deleted() {
        let storage = Storage::in_memory().unwrap();
        assert!(storage.connections().unwrap().is_empty());

        let a = storage.save_connection(None, &config("a")).unwrap();
        let b = storage.save_connection(None, &config("b")).unwrap();
        assert_ne!(a, b);
        let names = |storage: &Storage| -> Vec<String> {
            storage
                .connections()
                .unwrap()
                .into_iter()
                .map(|c| c.config.name)
                .collect()
        };
        assert_eq!(names(&storage), ["a", "b"]);

        let mut changed = config("a renamed");
        changed.ssh = Some(SshConfig {
            host: "bastion".into(),
            port: 22,
            user: "ops".into(),
            auth: SshAuth::Password,
            keepalive_secs: 30,
        });
        assert_eq!(storage.save_connection(Some(&a), &changed).unwrap(), a);
        let stored = storage.connections().unwrap();
        assert_eq!(stored[0].id, a);
        assert_eq!(stored[0].config, changed);

        assert!(
            storage
                .save_connection(Some("no-such-id"), &config("x"))
                .is_err()
        );

        storage
            .add_history(&entry(&a, "select 1", 1), LexRules::POSTGRES)
            .unwrap();
        storage
            .save_query(&SavedQuery {
                id: 0,
                connection_id: Some(a.clone()),
                folder: String::new(),
                name: "q".into(),
                sql: "select 1".into(),
            })
            .unwrap();
        storage.delete_connection(&a).unwrap();
        assert_eq!(names(&storage), ["b"]);
        assert!(storage.history(None, "", 10).unwrap().is_empty());
        assert!(storage.saved_queries().unwrap().is_empty());
    }

    #[test]
    fn history_is_newest_first_filtered_and_searchable() {
        let storage = Storage::in_memory().unwrap();
        storage
            .add_history(
                &entry("c1", "SELECT * FROM orders", 100),
                LexRules::POSTGRES,
            )
            .unwrap();
        storage
            .add_history(
                &entry("c2", "select * from customers", 200),
                LexRules::POSTGRES,
            )
            .unwrap();
        let mut failed = entry("c1", "DELETE FROM orders_100%", 300);
        failed.status = HistoryStatus::Error;
        failed.error = Some("boom".into());
        failed.row_count = None;
        storage.add_history(&failed, LexRules::POSTGRES).unwrap();

        let sql = |entries: Vec<HistoryEntry>| -> Vec<String> {
            entries.into_iter().map(|e| e.sql).collect()
        };
        assert_eq!(
            sql(storage.history(None, "", 10).unwrap()),
            [
                "DELETE FROM orders_100%",
                "select * from customers",
                "SELECT * FROM orders"
            ]
        );
        assert_eq!(
            sql(storage.history(Some("c1"), "", 10).unwrap()),
            ["DELETE FROM orders_100%", "SELECT * FROM orders"]
        );
        // Case-insensitive, and the search text is data, not a pattern.
        assert_eq!(sql(storage.history(None, "ORDERS", 10).unwrap()).len(), 2);
        assert_eq!(
            sql(storage.history(None, "100%", 10).unwrap()),
            ["DELETE FROM orders_100%"]
        );
        assert!(storage.history(None, "s_l_c_", 10).unwrap().is_empty());
        assert_eq!(storage.history(None, "", 1).unwrap().len(), 1);

        let stored = &storage.history(Some("c1"), "DELETE", 10).unwrap()[0];
        assert_eq!(stored.status, HistoryStatus::Error);
        assert_eq!(stored.error.as_deref(), Some("boom"));
        assert_eq!(stored.row_count, None);
        assert_eq!(stored.duration_ms, 12);
        assert!(stored.id > 0);

        storage.clear_history(Some("c1")).unwrap();
        assert_eq!(storage.history(None, "", 10).unwrap().len(), 1);
        storage.clear_history(None).unwrap();
        assert!(storage.history(None, "", 10).unwrap().is_empty());
    }

    #[test]
    fn saved_queries_are_created_updated_sorted_and_deleted() {
        let storage = Storage::in_memory().unwrap();
        let query = |folder: &str, name: &str| SavedQuery {
            id: 0,
            connection_id: None,
            folder: folder.into(),
            name: name.into(),
            sql: format!("-- {name}"),
        };
        let b = storage.save_query(&query("reports", "b")).unwrap();
        storage.save_query(&query("reports", "A")).unwrap();
        storage.save_query(&query("", "top")).unwrap();
        let names: Vec<_> = storage
            .saved_queries()
            .unwrap()
            .into_iter()
            .map(|q| q.name)
            .collect();
        assert_eq!(names, ["top", "A", "b"]);

        let mut renamed = query("archive", "b2");
        renamed.id = b;
        assert_eq!(storage.save_query(&renamed).unwrap(), b);
        let stored = storage.saved_queries().unwrap();
        assert_eq!(stored.iter().find(|q| q.id == b).unwrap().folder, "archive");

        renamed.id = 9_999;
        assert!(storage.save_query(&renamed).is_err());
        storage.delete_query(b).unwrap();
        assert_eq!(storage.saved_queries().unwrap().len(), 2);
    }

    #[test]
    fn tabs_and_settings_round_trip() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct Tab {
            title: String,
            sql: String,
        }
        let storage = Storage::in_memory().unwrap();
        assert!(storage.tabs::<Tab>().unwrap().is_empty());
        let tabs = vec![
            Tab {
                title: "one".into(),
                sql: "select 'unsaved'\n".into(),
            },
            Tab {
                title: "two".into(),
                sql: String::new(),
            },
        ];
        storage.save_tabs(&tabs).unwrap();
        assert_eq!(storage.tabs::<Tab>().unwrap(), tabs);
        storage.save_tabs(&tabs[1..]).unwrap();
        assert_eq!(storage.tabs::<Tab>().unwrap().len(), 1);
        // A tab that no longer parses is dropped, not fatal.
        assert!(storage.tabs::<Vec<u8>>().unwrap().is_empty());

        assert_eq!(storage.setting::<u32>("grid.page_size").unwrap(), None);
        storage.set_setting("grid.page_size", &1000u32).unwrap();
        storage.set_setting("grid.page_size", &500u32).unwrap();
        assert_eq!(storage.setting::<u32>("grid.page_size").unwrap(), Some(500));
        assert_eq!(storage.setting::<String>("grid.page_size").unwrap(), None);
    }

    #[test]
    fn filters_are_kept_per_key_latest_first_without_secrets() {
        let storage = Storage::in_memory().unwrap();
        let rules = LexRules::POSTGRES;
        assert!(storage.filters("t").unwrap().is_empty());
        assert!(storage.add_filter("t", " id > 1 ", rules).unwrap());
        assert!(storage.add_filter("t", "status = 'paid'", rules).unwrap());
        assert!(storage.add_filter("other", "id = 2", rules).unwrap());
        assert_eq!(storage.filters("t").unwrap(), ["status = 'paid'", "id > 1"]);
        // Applied again, it comes first and is not listed twice.
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(storage.add_filter("t", "id > 1", rules).unwrap());
        assert_eq!(storage.filters("t").unwrap(), ["id > 1", "status = 'paid'"]);
        // Only the latest are kept.
        for n in 0..FILTERS_KEPT {
            storage
                .add_filter("t", &format!("id = {n}"), rules)
                .unwrap();
        }
        let kept = storage.filters("t").unwrap();
        assert_eq!(kept.len(), FILTERS_KEPT);
        assert!(!kept.contains(&"status = 'paid'".to_string()));
        // Nothing the history would mask is kept in clear, nor nothing.
        assert!(!storage.add_filter("t", "  ", rules).unwrap());
        assert!(
            !storage
                .add_filter("t", "dsn = 'postgres://u:hunter2@h/db'", rules)
                .unwrap()
        );
        assert!(
            !storage
                .add_filter("q:select password 'hunter2'", "id = 1", rules)
                .unwrap()
        );
        assert_eq!(
            storage
                .filters("q:select password 'hunter2'")
                .unwrap()
                .len(),
            0
        );
        assert_eq!(storage.filters("other").unwrap(), ["id = 2"]);
    }

    #[test]
    fn schema_survives_reopening_and_refuses_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/dir/kerno.db");
        let id = {
            let storage = Storage::open(&path).unwrap();
            storage.save_connection(None, &config("kept")).unwrap()
        };
        let storage = Storage::open(&path).unwrap();
        assert_eq!(storage.connections().unwrap()[0].id, id);
        drop(storage);

        let raw = Connection::open(&path).unwrap();
        raw.pragma_update(None, "user_version", 999).unwrap();
        drop(raw);
        let error = Storage::open(&path).unwrap_err();
        assert!(error.to_string().contains("newer version"), "{error}");
    }

    #[test]
    fn passwords_in_statements_are_redacted() {
        let pg = |sql: &str| redact_secrets(sql, LexRules::POSTGRES);
        let my = |sql: &str| redact_secrets(sql, LexRules::MYSQL);
        assert_eq!(
            pg("ALTER USER ada PASSWORD 'hunter2'"),
            "ALTER USER ada PASSWORD '***'"
        );
        assert_eq!(
            pg("create role r login\n  password 'it''s secret' valid until 'tomorrow'"),
            "create role r login\n  password '***' valid until 'tomorrow'"
        );
        assert_eq!(
            pg("ALTER ROLE r WITH ENCRYPTED PASSWORD $$s3cret$$"),
            "ALTER ROLE r WITH ENCRYPTED PASSWORD '***'"
        );
        assert_eq!(
            my("CREATE USER 'u'@'%' IDENTIFIED BY 'hunter2'"),
            "CREATE USER 'u'@'%' IDENTIFIED BY '***'"
        );
        assert_eq!(
            my("ALTER USER u IDENTIFIED WITH caching_sha2_password BY \"hunter2\""),
            "ALTER USER u IDENTIFIED WITH caching_sha2_password BY '***'"
        );
        assert_eq!(
            my("ALTER USER u IDENTIFIED WITH mysql_native_password AS '*HASH'"),
            "ALTER USER u IDENTIFIED WITH mysql_native_password AS '***'"
        );
        assert_eq!(my("SET PASSWORD = 'hunter2'"), "SET PASSWORD = '***'");
        assert_eq!(
            my("SET PASSWORD FOR u = PASSWORD('hunter2')"),
            "SET PASSWORD FOR u = PASSWORD('***')"
        );
        // Replication and option forms: any word ending in PASSWORD.
        assert_eq!(
            my("CHANGE REPLICATION SOURCE TO SOURCE_USER='r', SOURCE_PASSWORD='hunter2'"),
            "CHANGE REPLICATION SOURCE TO SOURCE_USER='r', SOURCE_PASSWORD='***'"
        );
        assert_eq!(
            my("CHANGE MASTER TO MASTER_PASSWORD = 'hunter2', MASTER_PORT = 3306"),
            "CHANGE MASTER TO MASTER_PASSWORD = '***', MASTER_PORT = 3306"
        );
        assert_eq!(
            pg("CREATE USER MAPPING FOR u SERVER s OPTIONS (user 'r', password 'hunter2')"),
            "CREATE USER MAPPING FOR u SERVER s OPTIONS (user 'r', password '***')"
        );
        // The current password given along with the new one.
        assert_eq!(
            my("ALTER USER u IDENTIFIED BY 'new-pw' REPLACE 'old-pw'"),
            "ALTER USER u IDENTIFIED BY '***' REPLACE '***'"
        );
        assert_eq!(
            my(
                "ALTER USER u IDENTIFIED WITH caching_sha2_password BY 'new-pw' REPLACE 'old-pw' RETAIN CURRENT PASSWORD"
            ),
            "ALTER USER u IDENTIFIED WITH caching_sha2_password BY '***' REPLACE '***' RETAIN CURRENT PASSWORD"
        );
        // Connection strings and URLs inside literals.
        assert_eq!(
            pg(
                "CREATE SUBSCRIPTION s CONNECTION 'host=db user=r password=hunter2 dbname=x' PUBLICATION p"
            ),
            "CREATE SUBSCRIPTION s CONNECTION 'host=db user=r password=*** dbname=x' PUBLICATION p"
        );
        assert_eq!(
            pg("select dblink_connect('host=db PASSWORD = ''hunter 2'' sslpassword=k3y')"),
            "select dblink_connect('host=db PASSWORD = *** sslpassword=***')"
        );
        assert_eq!(
            my(
                "CREATE TABLE f (id int) ENGINE=FEDERATED CONNECTION='mysql://fed:hunter2@remote:3306/db/t'"
            ),
            "CREATE TABLE f (id int) ENGINE=FEDERATED CONNECTION='mysql://fed:***@remote:3306/db/t'"
        );
        for redacted in [
            pg("ALTER USER ada PASSWORD 'hunter2'"),
            my("CHANGE MASTER TO MASTER_PASSWORD = 'hunter2'"),
            pg("select dblink_connect('password=hunter2')"),
            my("select 'mysql://u:hunter2@h/db'"),
        ] {
            assert!(!redacted.contains("hunter2"), "{redacted}");
        }
        // A URL without a password, and `password` not followed by `=`.
        for sql in [
            "select 'https://example.com/a:b@c'",
            "select 'postgres://user@host/db'",
            "select 'the password is unknown'",
        ] {
            assert_eq!(pg(sql), sql);
        }
        // Ordinary statements are untouched, including a column called password.
        for sql in [
            "select 'password' from t",
            "select password from users where name = 'ada'",
            "select * from t -- password 'x'",
            "update users set name = 'by' where id = 1",
        ] {
            assert_eq!(pg(sql), sql);
        }
    }

    #[test]
    fn history_never_stores_a_password() {
        let storage = Storage::in_memory().unwrap();
        storage
            .add_history(
                &entry("c", "ALTER USER ada PASSWORD 'hunter2'", 1),
                LexRules::POSTGRES,
            )
            .unwrap();
        let stored = storage.history(None, "", 10).unwrap();
        assert_eq!(stored[0].sql, "ALTER USER ada PASSWORD '***'");
        assert!(storage.history(None, "hunter2", 10).unwrap().is_empty());

        // A server error that quotes the statement, cut mid-literal as
        // MySQL does: the message is not kept at all.
        let mut failed = entry("c", "ALTER USER ada IDENTIFIED BY 'hunter2' REQUIRE", 2);
        failed.status = HistoryStatus::Error;
        failed.error = Some(
            "You have an error in your SQL syntax near 'IDENTIFIED BY 'hunt' at line 1".into(),
        );
        storage.add_history(&failed, LexRules::MYSQL).unwrap();
        let stored = &storage.history(None, "REQUIRE", 10).unwrap()[0];
        assert_eq!(stored.error.as_deref(), Some(ERROR_WITHHELD));
        assert_eq!(stored.status, HistoryStatus::Error);

        // Errors of statements without a secret are kept as they are.
        let mut failed = entry("c", "select nope", 3);
        failed.error = Some("column \"nope\" does not exist".into());
        storage.add_history(&failed, LexRules::POSTGRES).unwrap();
        let stored = &storage.history(None, "nope", 10).unwrap()[0];
        assert_eq!(
            stored.error.as_deref(),
            Some("column \"nope\" does not exist")
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_store_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;

        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("Kerno");
        let path = data_dir.join("kerno.db");
        let storage = Storage::open(&path).unwrap();
        storage
            .add_history(&entry("c", "select 1", 1), LexRules::POSTGRES)
            .unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&data_dir), 0o700);
        // The write-ahead log holds the same data.
        let wal = data_dir.join("kerno.db-wal");
        assert!(wal.exists(), "WAL mode is on");
        assert_eq!(mode(&wal), 0o600);
        drop(storage);

        // A file that was left wide open is tightened on the next start.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _storage = Storage::open(&path).unwrap();
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn paths_follow_the_override_directory() {
        let paths = AppPaths::under("/tmp/kerno-x".into());
        assert_eq!(paths.database(), PathBuf::from("/tmp/kerno-x/kerno.db"));
        assert_eq!(
            paths.known_hosts(),
            PathBuf::from("/tmp/kerno-x/known_hosts")
        );
        assert_eq!(paths.logs(), PathBuf::from("/tmp/kerno-x/logs"));
        assert_eq!(paths.keymap(), PathBuf::from("/tmp/kerno-x/keymap.json"));
    }

    fn backup_task(name: &str, connection: &str) -> SavedTask {
        let options = crate::tools::BackupOptions::new(Engine::Postgres, "shop");
        SavedTask::new(name, connection, TaskSettings::Backup(options))
    }

    #[test]
    fn tasks_are_saved_listed_updated_run_and_deleted() {
        let storage = Storage::in_memory().unwrap();
        let first = storage.save_task(&backup_task("nightly", "c1")).unwrap();
        let mut script = SavedTask::new(
            "Apply fixes",
            "c2",
            TaskSettings::ExecuteScript(crate::tools::ScriptOptions::new(
                Engine::MySql,
                "shop",
                "/fixes.sql",
            )),
        );
        script.description = "the hotfix script".into();
        let second = storage.save_task(&script).unwrap();
        let tasks = storage.tasks().unwrap();
        // By name, case aside.
        assert_eq!(
            tasks
                .iter()
                .map(|task| task.name.as_str())
                .collect::<Vec<_>>(),
            ["Apply fixes", "nightly"]
        );
        assert_eq!(tasks[0].id, second);
        assert_eq!(tasks[0].settings, script.settings);
        assert_eq!(tasks[0].description, "the hotfix script");
        assert!(tasks[1].created_at_ms > 0 && tasks[1].last_run_at_ms.is_none());

        // Renamed and edited; the id stays.
        let mut renamed = tasks[1].clone();
        renamed.name = "Nightly backup".into();
        let crate::tools::TaskSettings::Backup(options) = &mut renamed.settings else {
            unreachable!()
        };
        options.file_pattern = "shop-${date}".into();
        assert_eq!(storage.save_task(&renamed).unwrap(), first);
        let stored = storage
            .tasks()
            .unwrap()
            .into_iter()
            .find(|t| t.id == first)
            .unwrap();
        assert_eq!(stored.name, "Nightly backup");
        assert_eq!(stored.settings, renamed.settings);
        let mut missing = renamed.clone();
        missing.id = 999;
        assert!(storage.save_task(&missing).is_err());

        // A run keeps its status and the last lines of its log, without
        // a password.
        let mut log: Vec<String> = (0..TASK_LOG_LINES + 10)
            .map(|n| format!("line {n}"))
            .collect();
        log.push("psql:fix.sql:3: ALTER ROLE app PASSWORD 'hunter2';".into());
        storage
            .record_task_run(first, HistoryStatus::Error, &log, LexRules::POSTGRES)
            .unwrap();
        let run = storage
            .tasks()
            .unwrap()
            .into_iter()
            .find(|t| t.id == first)
            .unwrap();
        assert_eq!(run.last_status, Some(HistoryStatus::Error));
        assert!(run.last_run_at_ms.is_some());
        let lines: Vec<&str> = run.last_log.lines().collect();
        assert_eq!(lines.len(), TASK_LOG_LINES);
        assert_eq!(lines[0], "line 11");
        assert!(
            !run.last_log.contains("hunter2"),
            "{}",
            lines.last().unwrap()
        );
        assert!(run.last_log.ends_with("PASSWORD '***';"));
        // Saving the task again keeps its last run.
        storage.save_task(&renamed).unwrap();
        let kept = storage
            .tasks()
            .unwrap()
            .into_iter()
            .find(|t| t.id == first)
            .unwrap();
        assert_eq!(kept.last_status, Some(HistoryStatus::Error));

        // Deleting a connection keeps its tasks: they say they cannot run.
        let connection = storage.save_connection(None, &config("c")).unwrap();
        storage
            .save_task(&backup_task("orphan", &connection))
            .unwrap();
        storage.delete_connection(&connection).unwrap();
        assert!(
            storage
                .tasks()
                .unwrap()
                .iter()
                .any(|t| t.connection_id == connection)
        );

        storage.delete_task(first).unwrap();
        assert!(storage.tasks().unwrap().iter().all(|t| t.id != first));
    }

    #[test]
    fn a_store_at_version_2_gains_the_tasks_table_and_keeps_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kerno.db");
        {
            let mut raw = Connection::open(&path).unwrap();
            let tx = raw.transaction().unwrap();
            for migration in &MIGRATIONS[..2] {
                tx.execute_batch(migration).unwrap();
            }
            tx.pragma_update(None, "user_version", 2).unwrap();
            tx.execute(
                "INSERT INTO connections (id, position, config, created_at, updated_at)
                 VALUES ('kept', 1, ?1, 1, 1)",
                [serde_json::to_string(&config("kept")).unwrap()],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO filter_history (key, filter, used_at) VALUES ('t', 'id = 1', 1)",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let storage = Storage::open(&path).unwrap();
        assert_eq!(storage.connections().unwrap()[0].config.name, "kept");
        assert_eq!(storage.filters("t").unwrap(), ["id = 1"]);
        assert!(storage.tasks().unwrap().is_empty());
        let version: i64 = storage
            .with(|c| c.query_row("PRAGMA user_version", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        assert!(storage.save_task(&backup_task("first", "kept")).unwrap() > 0);
    }

    #[test]
    fn a_user_mapping_password_is_redacted() {
        let sql = "CREATE USER MAPPING FOR CURRENT_USER SERVER kerno_fdw \
                   OPTIONS (user 'kerno', password 'hunter2')";
        assert_eq!(
            redact_secrets(sql, LexRules::POSTGRES),
            "CREATE USER MAPPING FOR CURRENT_USER SERVER kerno_fdw \
             OPTIONS (user 'kerno', password '***')"
        );
    }
}
