//! A MySQL / MariaDB backup into a folder, written by several `mysqldump` at
//! once, and its restore by several `mysql` clients (K63, D-135). mysqldump
//! reads with one connection and has no parallel mode that writes a script
//! (mysqlpump is gone from MySQL 8.4, `mariadb-dump --parallel` needs
//! `--tab`), so the parallelism is Kerno's own, with the stock tools, from
//! MySQL 5.1 on.
//!
//! The folder holds one file per table, each written by its own mysqldump
//! of that one table; a table of at least [`RANGE_THRESHOLD`] bytes with a
//! single-column integer primary key is cut into ranges of that key
//! ([`ranges`]), one `mysqldump --where` per range, the first carrying the
//! table's definition. What is not a table's rows comes once, after the
//! tables, in mysqldump's own order: the routines, triggers and events,
//! then the views (which may call a routine and read each other). The
//! manifest ([`MANIFEST`]) names the files and their order; a folder
//! without it is not a Kerno backup.
//!
//! Each mysqldump reads at its own moment. With the default "single
//! transaction" an InnoDB table (or one range of it) is read in one
//! snapshot and any other table under `LOCK TABLES … READ` while it is
//! read; two tables, or two ranges, are not read at the same instant. With
//! "lock all tables", Kerno's own session holds `FLUSH TABLES WITH READ
//! LOCK` from before the first mysqldump until the last has ended, so that
//! everything is read at one moment; any end of the run releases it.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::{
    BackupContent, BackupFormat, BackupOptions, CANCEL_POLL, DumpMethod, EngineBackup, MySqlBackup,
    RestoreOptions, ToolCommand, ToolVersion, backup_command, check_script, restore_command, run,
};
use crate::Session;
use crate::config::{ConnectParams, Engine};
use crate::error::{Error, Result};
use crate::exec;
use crate::schema::TableRef;

/// The manifest's file name, in the backup's folder.
pub const MANIFEST: &str = "kerno-backup.json";

/// The manifest's own version: a restore refuses another.
const MANIFEST_VERSION: u32 = 1;

/// The data size (`information_schema.TABLES.DATA_LENGTH`) from which a
/// table with a single-column integer primary key is cut into ranges of it.
/// Measured (BENCH.md, K63, MySQL 8.4 on this Mac): a mysqldump of an
/// empty table takes 20 ms; a table of 30 MB backs up in 0.3 s and
/// restores in 2 s on one client, in 0.15 s and 1 s cut in 8; one of 238 MB
/// in 1.7 s and 14.5 s, against 0.7 s and 7 s. Below about 32 MB a cut
/// saves under a second, while the other tables keep the processes busy in
/// a database of many: smaller tables are left whole.
pub const RANGE_THRESHOLD: u64 = 32 << 20;

/// A key range: rows whose key is at least the first bound and below the
/// second; `None` is open on that side.
pub type KeyRange = (Option<i128>, Option<i128>);

/// The ranges that cut the keys `min..=max` into `count` pieces of about
/// equal span. They are open below the first and above the last, so that
/// together they hold every integer once: a row inserted during the backup
/// below `min` or above `max` falls in one of them too. Fewer pieces when
/// the span is smaller than `count`; one, unbounded, for `count` ≤ 1.
pub fn ranges(min: i128, max: i128, count: u32) -> Vec<KeyRange> {
    let span = (max - min).saturating_add(1).max(1);
    let count = i128::from(count.max(1)).min(span);
    let bounds: Vec<i128> = (1..count).map(|i| min + span * i / count).collect();
    let mut pieces = Vec::with_capacity(bounds.len() + 1);
    let mut low = None;
    for bound in bounds {
        pieces.push((low, Some(bound)));
        low = Some(bound);
    }
    pieces.push((low, None));
    pieces
}

/// The `--where` condition of a range of `column` (quoted for MySQL).
fn range_where(column: &str, range: KeyRange) -> Option<String> {
    let column = format!("`{}`", column.replace('`', "``"));
    match range {
        (None, None) => None,
        (Some(low), None) => Some(format!("{column} >= {low}")),
        (None, Some(high)) => Some(format!("{column} < {high}")),
        (Some(low), Some(high)) => Some(format!("{column} >= {low} AND {column} < {high}")),
    }
}

/// A table as the catalog describes it, for [`plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    /// Its storage engine (`InnoDB`, `MyISAM`…); `None` when unknown.
    pub engine: Option<String>,
    /// `DATA_LENGTH`.
    pub bytes: u64,
    /// Its single-column integer primary key and the key's bounds, read
    /// only for a table large enough to be cut; `None` without one, or for
    /// an empty table.
    pub key: Option<IntKey>,
}

/// A single-column integer primary key and its smallest and largest value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntKey {
    pub column: String,
    pub min: i128,
    pub max: i128,
}

/// A database's tables and views, as [`read_catalog`] reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    pub tables: Vec<TableInfo>,
    pub views: Vec<String>,
}

/// The files of one table, in the order they load: the first creates the
/// table, the others add rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableFiles {
    pub table: String,
    /// Its storage engine at the backup: the restore loads the ranges of
    /// an InnoDB table side by side, of any other one after the other.
    pub engine: Option<String>,
    pub files: Vec<Piece>,
}

impl TableFiles {
    /// Whether the table takes several writers at once (row locks).
    pub fn concurrent(&self) -> bool {
        self.engine
            .as_deref()
            .is_some_and(|engine| engine.eq_ignore_ascii_case("innodb"))
    }
}

/// One file of a table: all its rows, or a range of its key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Piece {
    pub file: String,
    /// mysqldump's `--where` for a range.
    #[serde(default, rename = "where")]
    pub condition: Option<String>,
}

/// What a file after the tables holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterKind {
    /// The procedures and functions, the triggers of the tables and the
    /// events (as the options ask for them).
    Routines,
    Views,
}

/// A file loaded once every table is in, in the manifest's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AfterFile {
    pub file: String,
    pub kind: AfterKind,
    /// The views, or the tables whose triggers it holds when only some
    /// tables are backed up (empty: the whole database).
    #[serde(default)]
    pub objects: Vec<String>,
}

impl AfterFile {
    /// What it holds, in words.
    pub fn label(&self) -> &'static str {
        match self.kind {
            AfterKind::Routines => "routines, triggers and events",
            AfterKind::Views => "views",
        }
    }
}

/// The backup's manifest, written last: the files, their order, the server,
/// the client and the options used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// [`MANIFEST_VERSION`].
    pub kerno_backup: u32,
    pub engine: Engine,
    pub database: String,
    pub server_version: String,
    /// The mysqldump that wrote it, as `--version` says.
    pub client: String,
    pub content: BackupContent,
    pub options: MySqlBackup,
    /// Loaded first, N at a time.
    pub tables: Vec<TableFiles>,
    /// Loaded then, one after the other.
    pub after: Vec<AfterFile>,
}

impl Manifest {
    /// Every file, tables first.
    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.tables
            .iter()
            .flat_map(|table| table.files.iter().map(|piece| piece.file.as_str()))
            .chain(self.after.iter().map(|after| after.file.as_str()))
    }
}

/// A file name safe on every file system: ASCII letters, digits, `_` and
/// `-`, the rest `_`, at most 48 characters. The table's number before it
/// keeps two names that clean to the same one apart (and `A` from `a` on a
/// case-insensitive disk).
fn file_stem(number: usize, name: &str) -> String {
    let clean: String = name
        .chars()
        .take(48)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{number:04}-{clean}")
}

/// The files of a backup of `catalog` with `options` on `jobs` processes:
/// what [`Manifest::tables`] and [`Manifest::after`] list. Pure. Only the
/// tables and views named in `options.tables` when it names some.
pub fn plan(
    catalog: &Catalog,
    options: &BackupOptions,
    jobs: u32,
    threshold: u64,
) -> (Vec<TableFiles>, Vec<AfterFile>) {
    let EngineBackup::MySql(my) = &options.engine else {
        return (Vec::new(), Vec::new());
    };
    let named = |name: &str| {
        options.tables.is_empty() || options.tables.iter().any(|table| table.name == name)
    };
    let data = options.content != BackupContent::SchemaOnly;
    let definitions = options.content != BackupContent::DataOnly;
    let tables: Vec<TableFiles> = catalog
        .tables
        .iter()
        .filter(|table| named(&table.name))
        .enumerate()
        .map(|(index, table)| {
            let stem = file_stem(index + 1, &table.name);
            let cut = table
                .key
                .as_ref()
                .filter(|_| data && table.bytes >= threshold)
                .map(|key| (key, ranges(key.min, key.max, jobs)))
                .filter(|(_, pieces)| pieces.len() > 1);
            let files = match cut {
                None => vec![Piece {
                    file: format!("{stem}.sql"),
                    condition: None,
                }],
                Some((key, pieces)) => pieces
                    .into_iter()
                    .enumerate()
                    .map(|(at, range)| Piece {
                        file: format!("{stem}.part{:02}.sql", at + 1),
                        condition: range_where(&key.column, range),
                    })
                    .collect(),
            };
            TableFiles {
                table: table.name.clone(),
                engine: table.engine.clone(),
                files,
            }
        })
        .collect();
    let mut after = Vec::new();
    let some_table = options.tables.is_empty() || !tables.is_empty();
    if definitions && (my.routines || my.events || (my.triggers && some_table)) {
        after.push(AfterFile {
            file: "routines.sql".into(),
            kind: AfterKind::Routines,
            objects: if options.tables.is_empty() {
                Vec::new()
            } else {
                tables.iter().map(|table| table.table.clone()).collect()
            },
        });
    }
    let views: Vec<String> = catalog
        .views
        .iter()
        .filter(|view| named(view))
        .cloned()
        .collect();
    if definitions && !views.is_empty() {
        after.push(AfterFile {
            file: "views.sql".into(),
            kind: AfterKind::Views,
            objects: views,
        });
    }
    (tables, after)
}

/// How one mysqldump keeps what it reads consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PieceLock {
    /// `--single-transaction`: one snapshot (InnoDB).
    Snapshot,
    /// `--lock-tables`: the table is locked for reading while it is read.
    LockTable,
    /// `--skip-lock-tables`: Kerno's read lock is held, or no lock asked.
    None,
}

/// `--set-gtid-purged=OFF` for a MySQL mysqldump that has it (5.6 on): on a
/// server with GTIDs each file would otherwise set `GTID_PURGED`, which a
/// second file of the same restore cannot set again.
fn gtid_off(client: Option<&ToolVersion>) -> Option<String> {
    client
        .filter(|version| !version.mariadb && version.number >= (5, 6, 0))
        .map(|_| "--set-gtid-purged=OFF".into())
}

/// Everything a folder backup runs, for [`back_up`] and to show.
#[derive(Debug, Clone)]
pub struct FolderBackup {
    pub tool_path: PathBuf,
    pub params: ConnectParams,
    /// [`BackupFormat::Directory`], [`MySqlBackup::jobs`] the processes.
    pub options: BackupOptions,
    /// The folder, which the run makes.
    pub folder: PathBuf,
    /// The mysqldump's version, as [`super::version`] read it.
    pub client: Option<ToolVersion>,
    /// [`RANGE_THRESHOLD`], lower in the tests.
    pub range_threshold: u64,
}

impl FolderBackup {
    fn my(&self) -> Result<&MySqlBackup> {
        match &self.options.engine {
            EngineBackup::MySql(my) => Ok(my),
            EngineBackup::Postgres(_) => Err(Error::Unsupported(
                "the folder of one file per table is MySQL's and MariaDB's backup".into(),
            )),
        }
    }

    /// How many mysqldump run at once.
    pub fn jobs(&self) -> u32 {
        self.my()
            .ok()
            .and_then(|my| my.jobs)
            .unwrap_or(1)
            .clamp(1, 64)
    }

    /// Whether Kerno holds a read lock for the whole run.
    pub fn locks(&self) -> bool {
        self.my()
            .is_ok_and(|my| my.method == DumpMethod::LockAllTables)
    }

    /// A mysqldump of `objects` (the whole database when empty) into the
    /// folder's `file`, with `my`'s options and `content`, then `more`
    /// arguments: the plain backup's command line ([`backup_command`]), so
    /// the connection, the TLS and the user's extra arguments are the same.
    fn dump(
        &self,
        file: &str,
        objects: &[String],
        content: BackupContent,
        mut my: MySqlBackup,
        lock: PieceLock,
        more: &[String],
    ) -> Result<ToolCommand> {
        my.jobs = None;
        my.method = match lock {
            PieceLock::Snapshot => DumpMethod::SingleTransaction,
            PieceLock::LockTable | PieceLock::None => DumpMethod::NoLocks,
        };
        let options = BackupOptions {
            format: BackupFormat::Sql,
            content,
            schemas: Vec::new(),
            tables: objects
                .iter()
                .map(|name| TableRef::new(self.options.database.clone(), name.clone()))
                .collect(),
            engine: EngineBackup::MySql(my),
            ..self.options.clone()
        };
        let output = self.folder.join(file);
        let mut command = backup_command(&self.tool_path, &self.params, &options, &output)?;
        let args = &mut command.args;
        if lock == PieceLock::LockTable
            && let Some(arg) = args.iter_mut().find(|arg| *arg == "--skip-lock-tables")
        {
            *arg = "--lock-tables".into();
        }
        // Right after the output: before the user's extra arguments and the
        // positional database and tables.
        let at = args
            .iter()
            .position(|arg| arg.starts_with("--result-file="))
            .map_or(args.len(), |at| at + 1);
        // mysqldump keeps the last `--where`: the user's would replace the
        // range's, and every range would hold the same rows.
        let ranged = more.iter().any(|arg| arg.starts_with("--where="));
        if ranged && args[at..].iter().any(|arg| arg.starts_with("--where")) {
            return Err(Error::Unsupported(
                "a --where of your own would replace the ranges a large table is cut into: \
                 leave it out of the folder backup"
                    .into(),
            ));
        }
        args.splice(at..at, more.iter().cloned());
        Ok(command)
    }

    /// The command of one file of `table`.
    pub fn piece_command(&self, table: &TableFiles, piece: &Piece) -> Result<ToolCommand> {
        let my = self.my()?.clone();
        let first = table.files.first() == Some(piece);
        let content = match self.options.content {
            // The ranges after the first add rows to the table it created.
            BackupContent::All if !first => BackupContent::DataOnly,
            content => content,
        };
        let lock = match my.method {
            DumpMethod::LockAllTables | DumpMethod::NoLocks => PieceLock::None,
            DumpMethod::SingleTransaction if table.concurrent() => PieceLock::Snapshot,
            DumpMethod::SingleTransaction => PieceLock::LockTable,
        };
        let mut more: Vec<String> = gtid_off(self.client.as_ref()).into_iter().collect();
        if let Some(condition) = &piece.condition {
            more.push(format!("--where={condition}"));
            // mysqldump wraps the rows in LOCK TABLES … WRITE, which would
            // make the ranges of one table wait for each other at the
            // restore.
            more.push("--skip-add-locks".into());
        }
        let my = MySqlBackup {
            routines: false,
            events: false,
            triggers: false,
            ..my
        };
        self.dump(
            &piece.file,
            std::slice::from_ref(&table.table),
            content,
            my,
            lock,
            &more,
        )
    }

    /// The command of a file loaded after the tables.
    pub fn after_command(&self, after: &AfterFile) -> Result<ToolCommand> {
        let my = self.my()?.clone();
        let mut more: Vec<String> = gtid_off(self.client.as_ref()).into_iter().collect();
        let my = match after.kind {
            AfterKind::Routines => {
                // Neither the tables' definitions nor their rows: what
                // mysqldump writes besides them. Only views picked: no
                // table, so no trigger (an empty list is the whole database).
                more.push("--no-create-info".into());
                let triggers =
                    my.triggers && (self.options.tables.is_empty() || !after.objects.is_empty());
                MySqlBackup { triggers, ..my }
            }
            AfterKind::Views => MySqlBackup {
                routines: false,
                events: false,
                triggers: false,
                ..my
            },
        };
        self.dump(
            &after.file,
            &after.objects,
            BackupContent::SchemaOnly,
            my,
            PieceLock::None,
            &more,
        )
    }

    /// A command that stands for the run, to show before it: the first
    /// table's, its file named `<table>.sql`.
    pub fn sample_command(&self) -> Result<ToolCommand> {
        let table = TableFiles {
            table: "<table>".into(),
            engine: Some("InnoDB".into()),
            files: vec![Piece {
                file: "<table>.sql".into(),
                condition: None,
            }],
        };
        self.piece_command(&table, &table.files[0])
    }
}

/// How a folder backup or restore ended.
#[derive(Debug)]
pub struct FolderEnd {
    /// `Ok` when every file was written (loaded); [`Error::Cancelled`]
    /// when stopped; [`Error::Config`] saying what failed and where.
    pub outcome: Result<()>,
    /// A backup's folder this run made was removed.
    pub removed: bool,
}

/// The rows of a query, as text.
async fn rows(session: &Arc<dyn Session>, sql: String) -> Result<Vec<Vec<Option<String>>>> {
    Ok(exec::collect(session.clone(), sql)
        .await?
        .results
        .first()
        .map(|result| result.rows())
        .unwrap_or_default())
}

/// The integer types a key can be cut on.
fn is_integer(data_type: &str) -> bool {
    [
        "tinyint",
        "smallint",
        "mediumint",
        "int",
        "integer",
        "bigint",
    ]
    .iter()
    .any(|name| data_type.eq_ignore_ascii_case(name))
}

/// Reads the tables of `database` (their engine and size), its views, and
/// the integer key and bounds of each table of at least
/// [`RANGE_THRESHOLD`] bytes, from `information_schema` (MySQL 5.1 on).
pub async fn read_catalog(
    session: &Arc<dyn Session>,
    database: &str,
    threshold: u64,
) -> Result<Catalog> {
    let schema = session.dialect().quote_string(database);
    // MySQL 8.0.3 on keeps the sizes of information_schema for a day by
    // default: a table that grew since would be read as small.
    if session.engine() == Engine::MySql && session.server().version_number >= (8, 0, 3) {
        let _ = exec::collect(
            session.clone(),
            "SET SESSION information_schema_stats_expiry = 0",
        )
        .await;
    }
    let mut catalog = Catalog::default();
    let listed = rows(
        session,
        format!(
            "SELECT TABLE_NAME, TABLE_TYPE, ENGINE, DATA_LENGTH FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = {schema} ORDER BY TABLE_NAME"
        ),
    )
    .await?;
    for row in listed {
        let text = |at: usize| row.get(at).cloned().flatten();
        let Some(name) = text(0) else { continue };
        if text(1).as_deref() == Some("VIEW") {
            catalog.views.push(name);
        } else {
            catalog.tables.push(TableInfo {
                name,
                engine: text(2),
                bytes: text(3).and_then(|n| n.parse().ok()).unwrap_or(0),
                key: None,
            });
        }
    }
    if catalog.tables.iter().all(|table| table.bytes < threshold) {
        return Ok(catalog);
    }
    let keys = rows(
        session,
        format!(
            "SELECT k.TABLE_NAME, MIN(k.COLUMN_NAME), MIN(c.DATA_TYPE), COUNT(*) \
             FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.COLUMNS c \
             ON c.TABLE_SCHEMA = k.TABLE_SCHEMA AND c.TABLE_NAME = k.TABLE_NAME \
             AND c.COLUMN_NAME = k.COLUMN_NAME \
             WHERE k.TABLE_SCHEMA = {schema} AND k.CONSTRAINT_NAME = 'PRIMARY' \
             GROUP BY k.TABLE_NAME"
        ),
    )
    .await?;
    let keys: BTreeMap<String, String> = keys
        .into_iter()
        .filter_map(|row| {
            let text = |at: usize| row.get(at).cloned().flatten();
            let single = text(3).as_deref() == Some("1");
            let integer = text(2).is_some_and(|data_type| is_integer(&data_type));
            (single && integer).then(|| Some((text(0)?, text(1)?)))?
        })
        .collect();
    let quote = |name: &str| session.dialect().quote_ident(name);
    for table in &mut catalog.tables {
        let Some(column) = keys.get(&table.name).filter(|_| table.bytes >= threshold) else {
            continue;
        };
        let bounds = rows(
            session,
            format!(
                "SELECT MIN({c}), MAX({c}) FROM {}.{}",
                quote(database),
                quote(&table.name),
                c = quote(column)
            ),
        )
        .await?;
        let bound = |at: usize| {
            bounds
                .first()
                .and_then(|row| row.get(at).cloned().flatten())
                .and_then(|n| n.parse::<i128>().ok())
        };
        // An empty table has no bounds: dumped whole.
        if let (Some(min), Some(max)) = (bound(0), bound(1)) {
            table.key = Some(IntKey {
                column: column.clone(),
                min,
                max,
            });
        }
    }
    Ok(catalog)
}

/// Whether the account may hold `FLUSH TABLES WITH READ LOCK`: it has the
/// global RELOAD privilege (a privilege that only an active role of MySQL
/// 8 gives is not seen, and the choice stays off).
pub async fn can_lock(session: &Arc<dyn Session>) -> Result<bool> {
    let found = rows(
        session,
        "SELECT COUNT(*) FROM information_schema.USER_PRIVILEGES WHERE PRIVILEGE_TYPE = 'RELOAD' \
         AND GRANTEE = CONCAT('''', SUBSTRING_INDEX(CURRENT_USER(), '@', 1), '''@''', \
         SUBSTRING_INDEX(CURRENT_USER(), '@', -1), '''')"
            .into(),
    )
    .await?;
    Ok(found
        .first()
        .and_then(|row| row.first().cloned().flatten())
        .is_some_and(|count| count != "0"))
}

/// One tool run of a pool.
struct Job {
    /// Which file, as the manifest names it.
    file: String,
    /// The log's prefix.
    label: String,
    command: ToolCommand,
}

/// How one file of a pool ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileState {
    Done,
    Failed,
    /// Stopped by the cancel or by another file's failure.
    Stopped,
}

/// A pool's end: each file run and how it ended (a file never started is
/// absent), and the first failure (its label and what the tool said).
#[derive(Debug, Default)]
struct PoolEnd {
    states: BTreeMap<String, FileState>,
    failure: Option<(String, String)>,
    cancelled: bool,
}

/// What a tool line says of a failure: the last line that is not one of
/// mysqldump's `--verbose` notes nor a warning.
fn failure_line(line: &str) -> bool {
    !line.starts_with("-- ") && !line.contains("Warning") && !line.trim().is_empty()
}

/// mysqldump's note when it reads no column of a table: a client that does
/// not understand this server (8.4 and later on MySQL 5.1); it still exits
/// 0 when the triggers are left out, with no table in the file.
const NO_FIELDS: &str = "it has no fields";

/// Runs `groups` with at most `jobs` tools at once. A group is a list of
/// stages: a stage's files run side by side, the next stage once they all
/// succeeded. The first failure, or `cancel`, stops every tool still
/// running and starts no other. Every tool has ended when it returns.
async fn run_pool(
    groups: Vec<VecDeque<Vec<Job>>>,
    jobs: usize,
    on_line: Arc<dyn Fn(String) + Send + Sync>,
    cancel: &AtomicBool,
) -> PoolEnd {
    let stop = Arc::new(AtomicBool::new(false));
    let mut groups = groups;
    let mut ready: VecDeque<(usize, Job)> = VecDeque::new();
    let mut left: Vec<usize> = vec![0; groups.len()];
    for (at, group) in groups.iter_mut().enumerate() {
        if let Some(stage) = group.pop_front() {
            left[at] = stage.len();
            ready.extend(stage.into_iter().map(|job| (at, job)));
        }
    }
    let mut end = PoolEnd::default();
    let mut running = tokio::task::JoinSet::new();
    let mut tick = tokio::time::interval(CANCEL_POLL);
    loop {
        while running.len() < jobs.max(1) && !stop.load(Ordering::Relaxed) {
            let Some((group, job)) = ready.pop_front() else {
                break;
            };
            let said: Arc<Mutex<(Option<String>, bool)>> = Arc::default();
            let (sink, heard, label) = (on_line.clone(), said.clone(), job.label.clone());
            let stop = stop.clone();
            running.spawn(async move {
                let outcome = run(
                    job.command,
                    move |line| {
                        if let Ok(mut said) = heard.lock() {
                            said.1 |= line.contains(NO_FIELDS);
                            if failure_line(&line) {
                                said.0 = Some(line.clone());
                            }
                        }
                        sink(format!("[{label}] {line}"));
                    },
                    stop,
                )
                .await;
                let (last, no_fields) = said.lock().map(|said| said.clone()).unwrap_or_default();
                (group, job.file, job.label, outcome, last, no_fields)
            });
        }
        if running.is_empty() {
            break;
        }
        tokio::select! {
            joined = running.join_next() => {
                let Some(Ok((group, file, label, outcome, last, no_fields))) = joined else {
                    continue;
                };
                let failed = match &outcome {
                    Ok(_) if no_fields => Some(
                        "the client found no columns in the table: it cannot read this \
                         server's tables (choose another client with “Local client…”)"
                            .to_string(),
                    ),
                    Ok(status) if status.success() => None,
                    Ok(status) => Some(last.unwrap_or_else(|| format!("{status}"))),
                    Err(Error::Cancelled) => {
                        end.states.insert(file, FileState::Stopped);
                        continue;
                    }
                    Err(error) => Some(error.to_string()),
                };
                match failed {
                    None => {
                        end.states.insert(file, FileState::Done);
                        left[group] -= 1;
                        if left[group] == 0
                            && let Some(stage) = groups[group].pop_front()
                        {
                            left[group] = stage.len();
                            ready.extend(stage.into_iter().map(|job| (group, job)));
                        }
                    }
                    Some(reason) => {
                        end.states.insert(file, FileState::Failed);
                        end.failure.get_or_insert((label, reason));
                        stop.store(true, Ordering::Relaxed);
                    }
                }
            }
            _ = tick.tick() => {
                if cancel.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
                    end.cancelled = true;
                    stop.store(true, Ordering::Relaxed);
                }
            }
        }
    }
    // A cancel that came while the last tool ended.
    end.cancelled |= end.failure.is_none() && cancel.load(Ordering::Relaxed);
    end
}

/// Runs `sql` on `session`, given up (and cancelled on the server) when
/// `cancel` is set: `FLUSH TABLES WITH READ LOCK` waits for every running
/// statement and transaction that writes.
async fn statement(session: &Arc<dyn Session>, sql: &str, cancel: &AtomicBool) -> Result<()> {
    let running = exec::collect(session.clone(), sql);
    tokio::pin!(running);
    let mut tick = tokio::time::interval(CANCEL_POLL);
    loop {
        tokio::select! {
            done = &mut running => return done.map(drop),
            _ = tick.tick() => {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
            }
        }
    }
}

/// Backs `job.options.database` up into the new folder `job.folder`, on
/// [`FolderBackup::jobs`] mysqldump at once, reading the catalog and
/// holding the read lock on `session` (a session of its own, in the
/// database). The manifest is written last. A failure or `cancel` stops
/// every mysqldump, releases the lock and removes the folder this run made;
/// a folder already there is neither written into nor removed.
pub async fn back_up(
    job: FolderBackup,
    session: Arc<dyn Session>,
    on_line: Arc<dyn Fn(String) + Send + Sync>,
    cancel: Arc<AtomicBool>,
) -> FolderEnd {
    let mut made = false;
    let outcome = back_up_in(&job, &session, &on_line, &cancel, &mut made).await;
    let removed = outcome.is_err() && made && std::fs::remove_dir_all(&job.folder).is_ok();
    FolderEnd { outcome, removed }
}

async fn back_up_in(
    job: &FolderBackup,
    session: &Arc<dyn Session>,
    on_line: &Arc<dyn Fn(String) + Send + Sync>,
    cancel: &AtomicBool,
    made: &mut bool,
) -> Result<()> {
    let database = &job.options.database;
    let catalog = read_catalog(session, database, job.range_threshold).await?;
    let (tables, after) = plan(&catalog, &job.options, job.jobs(), job.range_threshold);
    // Every command is built before anything runs: a refused option stops
    // the backup before the folder exists.
    let mut groups = Vec::new();
    for table in &tables {
        for (at, piece) in table.files.iter().enumerate() {
            let label = match table.files.len() {
                1 => table.table.clone(),
                n => format!("{} {}/{n}", table.table, at + 1),
            };
            groups.push(VecDeque::from([vec![Job {
                file: piece.file.clone(),
                label,
                command: job.piece_command(table, piece)?,
            }]]));
        }
    }
    let mut finishing = VecDeque::new();
    for file in &after {
        finishing.push_back(vec![Job {
            file: file.file.clone(),
            label: file.label().into(),
            command: job.after_command(file)?,
        }]);
    }
    std::fs::create_dir(&job.folder).map_err(|error| {
        Error::Config(if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!(
                "{} already exists: the backup is written into a new folder. Nothing was run.",
                job.folder.display()
            )
        } else {
            format!(
                "Could not make {}: {error}. Nothing was run.",
                job.folder.display()
            )
        })
    })?;
    *made = true;
    let cut = tables.iter().filter(|table| table.files.len() > 1).count();
    on_line(format!(
        "Kerno: {} tables into {} files, {} mysqldump at once{}.",
        tables.len(),
        tables.iter().map(|table| table.files.len()).sum::<usize>() + after.len(),
        job.jobs(),
        match cut {
            0 => String::new(),
            1 => ", one table cut into ranges of its key".into(),
            n => format!(", {n} tables cut into ranges of their key"),
        }
    ));
    let locked = job.locks();
    if locked {
        on_line("Kerno: FLUSH TABLES WITH READ LOCK (writes wait until the backup ends)".into());
        if let Err(error) = statement(session, "FLUSH TABLES WITH READ LOCK", cancel).await {
            // A cancel that came as the lock was granted: released too.
            let _ = exec::collect(session.clone(), "UNLOCK TABLES").await;
            return Err(error);
        }
    }
    let outcome = async {
        let jobs = job.jobs() as usize;
        let end = run_pool(groups, jobs, on_line.clone(), cancel).await;
        settle(end, "mysqldump")?;
        let end = run_pool(vec![finishing], 1, on_line.clone(), cancel).await;
        settle(end, "mysqldump")
    }
    .await;
    if locked {
        // Released before anything else, whatever happened: a failed
        // UNLOCK still ends with the session, which closes.
        let unlocked = exec::collect(session.clone(), "UNLOCK TABLES").await;
        if unlocked.is_err() {
            session.close().await;
        }
        on_line("Kerno: UNLOCK TABLES".into());
    }
    outcome?;
    let manifest = Manifest {
        kerno_backup: MANIFEST_VERSION,
        engine: job.params.engine,
        database: database.clone(),
        server_version: session.server().version.clone(),
        client: job
            .client
            .as_ref()
            .map(|version| version.text.clone())
            .unwrap_or_default(),
        content: job.options.content,
        options: job.my()?.clone(),
        tables,
        after,
    };
    let text = serde_json::to_string_pretty(&manifest)
        .map_err(|error| Error::Config(format!("the manifest: {error}")))?;
    std::fs::write(job.folder.join(MANIFEST), text)
        .map_err(|error| Error::io("writing the backup's manifest", error))
}

/// A backup pool's end as an outcome.
fn settle(end: PoolEnd, tool: &str) -> Result<()> {
    if let Some((label, reason)) = end.failure {
        return Err(Error::Config(format!("{tool} failed on {label}: {reason}")));
    }
    if end.cancelled {
        return Err(Error::Cancelled);
    }
    Ok(())
}

/// Whether `name` is a plain file name, which the manifest may name: no
/// folder, no way out of the backup's folder.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.starts_with('.')
}

/// Reads the manifest of a backup folder and checks it names only plain
/// files that are there. Blocking.
pub fn read_manifest(folder: &Path) -> std::result::Result<Manifest, String> {
    let path = folder.join(MANIFEST);
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|error| {
        format!(
            "{} is not a manifest Kerno can read: {error}",
            path.display()
        )
    })?;
    if manifest.kerno_backup != MANIFEST_VERSION {
        return Err(format!(
            "{} was written by another version of Kerno (format {}): this one reads format \
             {MANIFEST_VERSION}.",
            path.display(),
            manifest.kerno_backup
        ));
    }
    for file in manifest.files() {
        if !plain_name(file) {
            return Err(format!(
                "{} names “{file}”, which is not a file of the backup's folder.",
                path.display()
            ));
        }
        if !folder.join(file).is_file() {
            return Err(format!(
                "{} is missing from {}: the backup is incomplete.",
                file,
                folder.display()
            ));
        }
    }
    Ok(manifest)
}

/// The restore of a backup folder into `options.database`.
#[derive(Debug, Clone)]
pub struct FolderRestore {
    pub tool_path: PathBuf,
    pub params: ConnectParams,
    /// `input` the folder; `jobs` the clients at once; the rest as a
    /// plain SQL restore's (the client commands, foreign keys, extra
    /// arguments, another user).
    pub options: RestoreOptions,
    pub manifest: Manifest,
    /// Bytes sent to the clients so far, all files together.
    pub sent: Arc<AtomicU64>,
}

impl FolderRestore {
    /// The `mysql` command that loads one file of the folder.
    pub fn file_command(&self, file: &str) -> Result<ToolCommand> {
        let options = RestoreOptions {
            input: self.options.input.join(file),
            format: BackupFormat::Sql,
            jobs: None,
            ..self.options.clone()
        };
        let mut command = restore_command(&self.tool_path, &self.params, &options)?;
        command.sent = self.sent.clone();
        Ok(command)
    }

    /// The size of every file, against [`FolderRestore::sent`]. Blocking.
    pub fn total(&self) -> u64 {
        self.manifest
            .files()
            .filter_map(|file| std::fs::metadata(self.options.input.join(file)).ok())
            .map(|meta| meta.len())
            .sum()
    }

    fn jobs(&self) -> usize {
        self.options.jobs.unwrap_or(1).clamp(1, 64) as usize
    }
}

/// Loads a backup folder: the tables first, [`FolderRestore::options`]'s
/// jobs clients at once (the first file of a table alone, then its ranges
/// side by side on InnoDB, one after the other otherwise), then the files
/// after the tables, in order, by one client. Every file is checked for
/// client commands before any client starts. A failure stops the others
/// and says which table, where, and what is in and what is not; what was
/// loaded stays, as with a plain SQL file.
pub async fn restore(
    job: FolderRestore,
    on_line: Arc<dyn Fn(String) + Send + Sync>,
    cancel: Arc<AtomicBool>,
) -> FolderEnd {
    FolderEnd {
        outcome: restore_in(&job, &on_line, &cancel).await,
        removed: false,
    }
}

async fn restore_in(
    job: &FolderRestore,
    on_line: &Arc<dyn Fn(String) + Send + Sync>,
    cancel: &Arc<AtomicBool>,
) -> Result<()> {
    let manifest = &job.manifest;
    let mut groups = Vec::new();
    let mut checks = Vec::new();
    for table in &manifest.tables {
        let mut stages = VecDeque::new();
        let mut rest = Vec::new();
        for (at, piece) in table.files.iter().enumerate() {
            let mut command = job.file_command(&piece.file)?;
            checks.extend(command.script_check.take());
            let file = Job {
                file: piece.file.clone(),
                label: match table.files.len() {
                    1 => table.table.clone(),
                    n => format!("{} {}/{n}", table.table, at + 1),
                },
                command,
            };
            if at == 0 {
                stages.push_back(vec![file]);
            } else if table.concurrent() {
                rest.push(file);
            } else {
                stages.push_back(vec![file]);
            }
        }
        if !rest.is_empty() {
            stages.push_back(rest);
        }
        groups.push(stages);
    }
    let mut finishing = VecDeque::new();
    for after in &manifest.after {
        let mut command = job.file_command(&after.file)?;
        checks.extend(command.script_check.take());
        finishing.push_back(vec![Job {
            file: after.file.clone(),
            label: after.label().into(),
            command,
        }]);
    }
    // All the files before the first client: a refusal loads nothing.
    let checking = cancel.clone();
    tokio::task::spawn_blocking(move || {
        checks
            .iter()
            .try_for_each(|check| check_script(check, &checking))
    })
    .await
    .map_err(|error| Error::io("checking the backup's files", std::io::Error::other(error)))??;
    on_line(format!(
        "Kerno: {} tables from {} files, {} mysql clients at once.",
        manifest.tables.len(),
        manifest.files().count(),
        job.jobs()
    ));
    let tables = run_pool(groups, job.jobs(), on_line.clone(), cancel).await;
    let finished = if tables.failure.is_none() && !tables.cancelled {
        Some(run_pool(vec![finishing], 1, on_line.clone(), cancel).await)
    } else {
        None
    };
    let failure = tables
        .failure
        .clone()
        .or_else(|| finished.as_ref().and_then(|end| end.failure.clone()));
    let cancelled = tables.cancelled || finished.as_ref().is_some_and(|end| end.cancelled);
    if failure.is_none() && !cancelled {
        return Ok(());
    }
    let summary = restore_summary(manifest, &tables, finished.as_ref());
    match failure {
        Some((label, reason)) => Err(Error::Config(format!(
            "mysql failed on {label}: {reason}. {summary}"
        ))),
        None => {
            on_line(format!("Kerno: {summary}"));
            Err(Error::Cancelled)
        }
    }
}

/// What a stopped restore loaded: the tables in, partly in, not started,
/// and the files after the tables.
fn restore_summary(manifest: &Manifest, tables: &PoolEnd, after: Option<&PoolEnd>) -> String {
    let (mut whole, mut partly, mut none) = (Vec::new(), Vec::new(), Vec::new());
    for table in &manifest.tables {
        let done = table
            .files
            .iter()
            .filter(|piece| tables.states.get(&piece.file) == Some(&FileState::Done))
            .count();
        let touched = table
            .files
            .iter()
            .any(|piece| tables.states.contains_key(&piece.file));
        let name = format!("“{}”", table.table);
        if done == table.files.len() {
            whole.push(name);
        } else if touched {
            partly.push(name);
        } else {
            none.push(name);
        }
    }
    let list = |names: &[String]| {
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    };
    let after_done: Vec<&str> = manifest
        .after
        .iter()
        .filter(|file| {
            after.is_some_and(|end| end.states.get(&file.file) == Some(&FileState::Done))
        })
        .map(AfterFile::label)
        .collect();
    let after_left: Vec<&str> = manifest
        .after
        .iter()
        .map(AfterFile::label)
        .filter(|label| !after_done.contains(label))
        .collect();
    let mut text = format!(
        "Loaded: {}. Partly loaded (some rows, or a table stopped midway): {}. Not loaded: {}.",
        list(&whole),
        list(&partly),
        list(&none)
    );
    if !after_left.is_empty() {
        text.push_str(&format!(
            " Not loaded either: the {}.",
            after_left.join(" and the ")
        ));
    }
    text.push_str(" What was loaded stays.");
    text
}

/// What a backup of a MySQL server of `server` version with this mysqldump
/// is known to get wrong, said before the run, or `None`. Reproduced on the
/// legacy test servers (K63): MySQL's mysqldump 8.4.13 and 26.10 read no
/// column of a MySQL 5.1 table (each table is skipped, "it has no fields",
/// and the file holds no table; exit 0 when the triggers are left out);
/// MariaDB's mariadb-dump 11.8 does the same on 5.1, and fails on 5.5 and
/// 5.6 (it asks `information_schema.COLUMNS` for `GENERATION_EXPRESSION`)
/// after writing a table's definition without its rows. `others` are the
/// clients found, to name one that works.
pub fn dump_client_problem(
    client: Option<&ToolVersion>,
    engine: Engine,
    server: (u32, u32, u32),
    others: &[(PathBuf, ToolVersion)],
) -> Option<String> {
    let client = client?;
    if engine != Engine::MySql {
        return None;
    }
    let broken = if client.mariadb {
        client.number >= (11, 8, 0) && server < (5, 7, 0)
    } else {
        client.number >= (8, 4, 0) && server < (5, 5, 0)
    };
    if !broken {
        return None;
    }
    let (client_name, what) = if client.mariadb {
        ("mariadb-dump", "reads the tables of this server wrongly")
    } else {
        ("mysqldump", "reads no column of this server's tables")
    };
    let (a, b, c) = client.number;
    let (x, y, z) = server;
    let mut text = format!(
        "This {client_name} ({a}.{b}.{c}) {what}: a backup of MySQL {x}.{y}.{z} with it would \
         hold no table, or tables without their rows. Choose a MySQL 5.7 client (5.7.11 or \
         later) with “Local client…”"
    );
    let works = others.iter().find(|(_, version)| {
        !version.mariadb && version.number >= (5, 7, 11) && version.number < (8, 0, 0)
    });
    match works {
        Some((dir, version)) => text.push_str(&format!(
            ": the one in {} ({}.{}.{}) works.",
            dir.display(),
            version.number.0,
            version.number.1,
            version.number.2
        )),
        None => {
            text.push_str("; none was found on this computer (Homebrew installs one as mysql@5.7).")
        }
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How many of `pieces` hold `key`.
    fn holding(pieces: &[KeyRange], key: i128) -> usize {
        pieces
            .iter()
            .filter(|(low, high)| {
                low.is_none_or(|low| key >= low) && high.is_none_or(|high| key < high)
            })
            .count()
    }

    #[test]
    fn the_ranges_hold_every_key_once() {
        let cases: [(i128, i128, u32); 9] = [
            (1, 1_000_000, 8),
            (1, 1, 8),
            (5, 6, 8),
            (-50, 50, 3),
            (0, 7, 8),
            (0, 7, 1),
            (i128::from(i64::MIN), i128::from(i64::MAX), 8),
            (0, i128::from(u64::MAX), 64),
            (1, 1_000_003, 7),
        ];
        for (min, max, count) in cases {
            let pieces = ranges(min, max, count);
            let span = max - min + 1;
            assert_eq!(
                pieces.len() as i128,
                i128::from(count).min(span),
                "{min}..={max} in {count}: {pieces:?}"
            );
            assert_eq!(pieces.first().unwrap().0, None);
            assert_eq!(pieces.last().unwrap().1, None);
            let bounds: Vec<i128> = pieces.iter().filter_map(|piece| piece.1).collect();
            assert!(bounds.windows(2).all(|w| w[0] < w[1]), "{bounds:?}");
            let mut keys = vec![min - 1, min, max, max + 1, i128::MIN, i128::MAX];
            for bound in &bounds {
                keys.extend([bound - 1, *bound, bound + 1]);
            }
            // A sparse key: gaps between the rows change nothing.
            keys.extend((0..200).map(|i| min + (max - min) / 200 * i));
            for key in keys {
                assert_eq!(holding(&pieces, key), 1, "{key} in {pieces:?}");
            }
        }
    }

    #[test]
    fn a_range_is_written_as_mysqldump_where() {
        assert_eq!(range_where("id", (None, None)), None);
        assert_eq!(range_where("id", (None, Some(10))).unwrap(), "`id` < 10");
        assert_eq!(
            range_where("a`b", (Some(10), Some(20))).unwrap(),
            "`a``b` >= 10 AND `a``b` < 20"
        );
        assert_eq!(range_where("id", (Some(-3), None)).unwrap(), "`id` >= -3");
    }

    fn table(name: &str, engine: &str, bytes: u64, key: Option<(i128, i128)>) -> TableInfo {
        TableInfo {
            name: name.into(),
            engine: Some(engine.into()),
            bytes,
            key: key.map(|(min, max)| IntKey {
                column: "id".into(),
                min,
                max,
            }),
        }
    }

    fn catalog() -> Catalog {
        Catalog {
            tables: vec![
                table("big", "InnoDB", 2 * RANGE_THRESHOLD, Some((1, 1000))),
                table("big no key", "InnoDB", 3 * RANGE_THRESHOLD, None),
                table("small", "MyISAM", 10, Some((1, 10))),
                table("tiny/odd", "InnoDB", RANGE_THRESHOLD, Some((7, 7))),
            ],
            views: vec!["v1".into(), "v2".into()],
        }
    }

    #[test]
    fn the_plan_cuts_a_large_table_with_a_key_and_ends_with_routines_then_views() {
        let options = BackupOptions::new(Engine::MySql, "db");
        let (tables, after) = plan(&catalog(), &options, 4, RANGE_THRESHOLD);
        let files: Vec<Vec<(&str, Option<&str>)>> = tables
            .iter()
            .map(|table| {
                table
                    .files
                    .iter()
                    .map(|piece| (piece.file.as_str(), piece.condition.as_deref()))
                    .collect()
            })
            .collect();
        assert_eq!(
            files,
            vec![
                vec![
                    ("0001-big.part01.sql", Some("`id` < 251")),
                    ("0001-big.part02.sql", Some("`id` >= 251 AND `id` < 501")),
                    ("0001-big.part03.sql", Some("`id` >= 501 AND `id` < 751")),
                    ("0001-big.part04.sql", Some("`id` >= 751")),
                ],
                // No usable key: whole, by one process.
                vec![("0002-big_no_key.sql", None)],
                vec![("0003-small.sql", None)],
                // One row: one range is the whole table.
                vec![("0004-tiny_odd.sql", None)],
            ]
        );
        assert!(tables[0].concurrent() && !tables[2].concurrent());
        assert_eq!(
            after,
            vec![
                AfterFile {
                    file: "routines.sql".into(),
                    kind: AfterKind::Routines,
                    objects: vec![],
                },
                AfterFile {
                    file: "views.sql".into(),
                    kind: AfterKind::Views,
                    objects: vec!["v1".into(), "v2".into()],
                },
            ]
        );
        // One process: nothing to cut.
        let (one, _) = plan(&catalog(), &options, 1, RANGE_THRESHOLD);
        assert_eq!(one[0].files.len(), 1);
    }

    #[test]
    fn the_plan_follows_the_tables_named_and_the_content() {
        let mut options = BackupOptions::new(Engine::MySql, "db");
        options.tables = vec![TableRef::new("db", "small"), TableRef::new("db", "v2")];
        let (tables, after) = plan(&catalog(), &options, 4, RANGE_THRESHOLD);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].files[0].file, "0001-small.sql");
        assert_eq!(after[0].objects, vec!["small".to_string()]);
        assert_eq!(after[1].objects, vec!["v2".to_string()]);

        options.tables.clear();
        options.content = BackupContent::SchemaOnly;
        let (tables, after) = plan(&catalog(), &options, 4, RANGE_THRESHOLD);
        assert!(
            tables.iter().all(|table| table.files.len() == 1),
            "no rows: no ranges"
        );
        assert_eq!(after.len(), 2);
        options.content = BackupContent::DataOnly;
        let (tables, after) = plan(&catalog(), &options, 4, RANGE_THRESHOLD);
        assert_eq!(tables[0].files.len(), 4);
        assert!(after.is_empty(), "no definitions: no routines nor views");
        options.content = BackupContent::All;
        if let EngineBackup::MySql(my) = &mut options.engine {
            (my.routines, my.events, my.triggers) = (false, false, false);
        }
        let (_, after) = plan(&catalog(), &options, 4, RANGE_THRESHOLD);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].kind, AfterKind::Views);
    }

    fn manifest() -> Manifest {
        let options = BackupOptions::new(Engine::MySql, "db");
        let (tables, after) = plan(&catalog(), &options, 4, RANGE_THRESHOLD);
        Manifest {
            kerno_backup: MANIFEST_VERSION,
            engine: Engine::MariaDb,
            database: "db".into(),
            server_version: "11.8.9-MariaDB".into(),
            client: "mysqldump  Ver 10.13 Distrib 5.7.44".into(),
            content: BackupContent::All,
            options: MySqlBackup {
                jobs: Some(4),
                ..MySqlBackup::default()
            },
            tables,
            after,
        }
    }

    #[test]
    fn the_manifest_round_trips_and_refuses_a_name_outside_its_folder() {
        let folder = tempfile::tempdir().unwrap();
        let manifest = manifest();
        for file in manifest.files() {
            std::fs::write(folder.path().join(file), "-- a dump\n").unwrap();
        }
        let write = |manifest: &Manifest| {
            std::fs::write(
                folder.path().join(MANIFEST),
                serde_json::to_string_pretty(manifest).unwrap(),
            )
            .unwrap()
        };
        write(&manifest);
        assert_eq!(read_manifest(folder.path()).unwrap(), manifest);

        for bad in [
            "../outside.sql",
            "/etc/passwd",
            "a\\b.sql",
            "..",
            ".hidden",
            "c:x.sql",
        ] {
            let mut evil = manifest.clone();
            evil.after[0].file = bad.into();
            write(&evil);
            let refused = read_manifest(folder.path()).unwrap_err();
            assert!(
                refused.contains("not a file of the backup's folder"),
                "{bad}: {refused}"
            );
        }
        std::fs::remove_file(folder.path().join("views.sql")).unwrap();
        write(&manifest);
        assert!(
            read_manifest(folder.path())
                .unwrap_err()
                .contains("views.sql is missing")
        );
        let mut newer = manifest;
        newer.kerno_backup = 2;
        write(&newer);
        assert!(
            read_manifest(folder.path())
                .unwrap_err()
                .contains("another version")
        );
    }

    fn version(text: &str) -> ToolVersion {
        super::super::parse_version(text).unwrap()
    }

    fn folder_backup(client: &str, method: DumpMethod) -> FolderBackup {
        let mut options = BackupOptions::new(Engine::MySql, "db");
        options.format = BackupFormat::Directory;
        options.engine = EngineBackup::MySql(MySqlBackup {
            method,
            jobs: Some(4),
            ..MySqlBackup::default()
        });
        options.extra_args = "--max-allowed-packet=64M".into();
        FolderBackup {
            tool_path: PathBuf::from("/x/mysqldump"),
            params: crate::testing::params(Engine::MySql),
            options,
            folder: PathBuf::from("/backups/db-folder"),
            client: Some(version(client)),
            range_threshold: RANGE_THRESHOLD,
        }
    }

    #[test]
    fn each_file_has_its_own_mysqldump_command_line_per_version() {
        let mysql57 = "mysqldump  Ver 10.13 Distrib 5.7.44, for osx10.19 (x86_64)";
        let job = folder_backup(mysql57, DumpMethod::SingleTransaction);
        let (tables, after) = plan(&catalog(), &job.options, 4, RANGE_THRESHOLD);
        let args = |command: ToolCommand| command.args;

        // The first range of an InnoDB table: its definition, one snapshot.
        let first = args(job.piece_command(&tables[0], &tables[0].files[0]).unwrap());
        let tail: Vec<&str> = first
            .iter()
            .map(String::as_str)
            .skip_while(|a| !a.starts_with("--verbose"))
            .collect();
        assert_eq!(
            tail,
            [
                "--verbose",
                "--single-transaction",
                "--skip-triggers",
                "--hex-blob",
                "--result-file=/backups/db-folder/0001-big.part01.sql",
                "--set-gtid-purged=OFF",
                "--where=`id` < 251",
                "--skip-add-locks",
                "--max-allowed-packet=64M",
                "db",
                "big",
            ]
        );
        // The next ranges add rows only.
        let second = args(job.piece_command(&tables[0], &tables[0].files[1]).unwrap());
        assert!(
            second.contains(&"--no-create-info".to_string()),
            "{second:?}"
        );
        // A MyISAM table is locked while it is read.
        let myisam = args(job.piece_command(&tables[2], &tables[2].files[0]).unwrap());
        assert!(myisam.contains(&"--lock-tables".to_string()), "{myisam:?}");
        assert!(
            !myisam
                .iter()
                .any(|a| a == "--single-transaction" || a.starts_with("--where"))
        );
        // Routines, triggers and events: neither definitions nor rows.
        let routines = args(job.after_command(&after[0]).unwrap());
        for flag in [
            "--no-data",
            "--no-create-info",
            "--routines",
            "--skip-lock-tables",
        ] {
            assert!(routines.contains(&flag.to_string()), "{flag}: {routines:?}");
        }
        assert!(!routines.contains(&"--skip-triggers".to_string()));
        assert_eq!(routines.last().unwrap(), "db");
        let views = args(job.after_command(&after[1]).unwrap());
        assert!(views.contains(&"--skip-triggers".to_string()));
        assert!(!views.contains(&"--routines".to_string()));
        assert_eq!(&views[views.len() - 3..], ["db", "v1", "v2"]);

        // Kerno's read lock: no lock of mysqldump's own.
        let locked = folder_backup(mysql57, DumpMethod::LockAllTables);
        assert!(locked.locks());
        let piece = args(
            locked
                .piece_command(&tables[2], &tables[2].files[0])
                .unwrap(),
        );
        assert!(
            piece.contains(&"--skip-lock-tables".to_string()),
            "{piece:?}"
        );
        assert!(
            !piece
                .iter()
                .any(|a| a == "--lock-all-tables" || a == "--lock-tables")
        );

        // GTIDs: MySQL 5.6 on only (MariaDB's has no such option).
        for (client, gtid) in [
            (
                "mysqldump  Ver 26.10.0 for macos27.0 on arm64 (Homebrew)",
                true,
            ),
            (
                "mysqldump  Ver 10.13 Distrib 5.5.62, for linux (x86_64)",
                false,
            ),
            (
                "mariadb-dump from 11.8.9-MariaDB, client 10.20 for debian-linux-gnu",
                false,
            ),
        ] {
            let job = folder_backup(client, DumpMethod::SingleTransaction);
            let piece = args(job.piece_command(&tables[1], &tables[1].files[0]).unwrap());
            assert_eq!(
                piece.contains(&"--set-gtid-purged=OFF".to_string()),
                gtid,
                "{client}"
            );
        }
        // A --where of the user's would replace a range's: refused there only.
        let mut own = folder_backup(mysql57, DumpMethod::SingleTransaction);
        own.options.extra_args = "--where=1".into();
        assert!(own.piece_command(&tables[0], &tables[0].files[0]).is_err());
        assert!(own.piece_command(&tables[1], &tables[1].files[0]).is_ok());
        let sample = folder_backup(mysql57, DumpMethod::SingleTransaction)
            .sample_command()
            .unwrap()
            .display();
        assert!(
            sample.contains("'--result-file=/backups/db-folder/<table>.sql'"),
            "{sample}"
        );
        assert!(!sample.contains(crate::testing::DB_PASSWORD));
    }

    #[test]
    fn a_failed_restore_says_what_is_in_and_what_is_not() {
        let manifest = manifest();
        let mut tables = PoolEnd::default();
        for file in [
            "0001-big.part01.sql",
            "0001-big.part02.sql",
            "0003-small.sql",
        ] {
            tables.states.insert(file.into(), FileState::Done);
        }
        tables
            .states
            .insert("0001-big.part03.sql".into(), FileState::Stopped);
        tables
            .states
            .insert("0002-big_no_key.sql".into(), FileState::Failed);
        let text = restore_summary(&manifest, &tables, None);
        assert_eq!(
            text,
            "Loaded: “small”. Partly loaded (some rows, or a table stopped midway): “big”, \
             “big no key”. Not loaded: “tiny/odd”. Not loaded either: the routines, triggers and \
             events and the views. What was loaded stays."
        );
    }

    #[test]
    fn an_old_server_is_said_with_the_client_that_works() {
        let client = |text: &str| version(text);
        let m26 = client("mysqldump  Ver 26.10.0 for macos27.0 on arm64 (Homebrew)");
        let m84 =
            client("mysqldump  Ver 8.4.13 for Linux on aarch64 (MySQL Community Server - GPL)");
        let m80 = client("mysqldump  Ver 8.0.36 for Linux on x86_64");
        let m57 = client("mysqldump  Ver 10.13 Distrib 5.7.44, for osx10.19 (x86_64)");
        let maria = client("mariadb-dump from 11.8.9-MariaDB, client 10.20 for debian-linux-gnu");
        let found = [(PathBuf::from("/opt/mysql@5.7/bin"), m57.clone())];
        let problem = |client: &ToolVersion, server, engine| {
            dump_client_problem(Some(client), engine, server, &found)
        };
        let said = problem(&m26, (5, 1, 73), Engine::MySql).unwrap();
        assert!(
            said.contains("mysqldump (26.10.0) reads no column"),
            "{said}"
        );
        assert!(said.contains("/opt/mysql@5.7/bin (5.7.44) works"), "{said}");
        assert!(problem(&m84, (5, 1, 73), Engine::MySql).is_some());
        assert!(problem(&maria, (5, 1, 73), Engine::MySql).is_some());
        assert!(problem(&maria, (5, 6, 51), Engine::MySql).is_some());
        // Reproduced working: 8.4 / 26.10 from 5.5 on, MariaDB's from 5.7,
        // any client on MariaDB; 8.0 was not tried, and is not said.
        assert!(problem(&m26, (5, 5, 62), Engine::MySql).is_none());
        assert!(problem(&maria, (5, 7, 44), Engine::MySql).is_none());
        assert!(problem(&m80, (5, 1, 73), Engine::MySql).is_none());
        assert!(problem(&m57, (5, 1, 73), Engine::MySql).is_none());
        assert!(problem(&m26, (11, 8, 9), Engine::MariaDb).is_none());
        let alone = dump_client_problem(Some(&m26), Engine::MySql, (5, 1, 73), &[]).unwrap();
        assert!(alone.contains("none was found"), "{alone}");
        assert!(dump_client_problem(None, Engine::MySql, (5, 1, 73), &found).is_none());
    }
}
