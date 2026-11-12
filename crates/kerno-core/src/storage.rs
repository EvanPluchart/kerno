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
];

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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| Error::io(format!("creating {}", parent.display()), error))?;
        }
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
                    entry.error,
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

/// Replaces the password in statements that set one, so it never reaches
/// the history or a log: the string after `PASSWORD` or `IDENTIFIED BY`
/// (with optional `=`, `(`, `WITH plugin`, `AS`) becomes `'***'`.
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
    let mut secret_ranges = Vec::new();
    for index in 0..tokens.len() {
        let introduces_secret = match word(index).as_deref() {
            Some("PASSWORD") => true,
            // IDENTIFIED BY 'password', and MySQL's
            // IDENTIFIED WITH plugin BY 'password' / AS 'hash'.
            Some("BY" | "AS") => {
                (1..=3).any(|back| word(index.wrapping_sub(back)).as_deref() == Some("IDENTIFIED"))
            }
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
            secret_ranges.push(token.range.clone());
        }
    }
    if secret_ranges.is_empty() {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len());
    let mut position = 0;
    for range in secret_ranges {
        out.push_str(&sql[position..range.start]);
        out.push_str("'***'");
        position = range.end;
    }
    out.push_str(&sql[position..]);
    out
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
}
