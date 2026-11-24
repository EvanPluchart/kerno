//! A MySQL / MariaDB backup into a folder, written by several `mysqldump` at
//! once, and its restore by several `mysql` clients (K63, D-135). mysqldump
//! reads with one connection and has no parallel mode that writes a script
//! (mysqlpump is gone from MySQL 8.4, `mariadb-dump --parallel` needs
//! `--tab`), so the parallelism is Kerno's own, with the stock tools, from
//! MySQL 5.1 on.
//!
//! The folder holds one file per table, each written by its own mysqldump
//! of that one table; a table of at least [`RANGE_THRESHOLD`] bytes with a
//! single-column integer primary key may be cut into ranges of that key
//! ([`ranges`]), one `mysqldump --where` per range, the first carrying the
//! table's definition, where every range reads one moment ([`cut_rule`]). What is not a table's rows comes once, after the
//! tables, in mysqldump's own order: the routines, triggers and events,
//! then the views (which may call a routine and read each other). The
//! manifest ([`MANIFEST`]) names the files and their order; a folder
//! without it is not a Kerno backup.
//!
//! Each mysqldump reads at its own moment. With the default "single
//! transaction" an InnoDB table (or one range of it) is read in one
//! snapshot and any other table under `LOCK TABLES … READ` while it is
//! read; the ranges of one table start under Kerno's lock of that table, so
//! that they read one moment; two tables are not read at the same instant. With
//! "lock all tables", Kerno's own session holds `FLUSH TABLES WITH READ
//! LOCK` from before the first mysqldump until the last has ended, so that
//! everything is read at one moment; any end of the run releases it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{
    BackupContent, BackupFormat, BackupOptions, CANCEL_POLL, DumpMethod, EngineBackup, MySqlBackup,
    RestoreOptions, StdinPart, ToolCommand, ToolVersion, backup_command, check_script,
    restore_command, run,
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
    /// The server compares table names without case
    /// (`lower_case_table_names` 1 or 2).
    pub fold_names: bool,
    /// The server's version, which says how a table may be cut ([`cut_rule`]).
    pub server: (u32, u32, u32),
}

/// Which tables a folder backup may cut into ranges: only where every
/// range of a table reads one moment (D-135).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cut {
    /// Under the server's read lock ("lock all tables"): nothing changes
    /// while the ranges are read, so any table, of any engine.
    AnyEngine,
    /// An InnoDB table, its ranges started together while Kerno holds the
    /// table's read lock: each takes its snapshot while no write can
    /// commit to the table. That needs metadata locks (MySQL 5.5.3 on,
    /// every MariaDB Kerno supports): before them, a transaction that
    /// already wrote to the table can commit under `LOCK TABLES … READ`.
    InnoDbUnderTableLock,
    /// No table is cut: "no locks" asked for none, and an older server
    /// without the server's read lock cannot give one moment.
    Never,
}

/// The rule of [`Cut`] for `method` on a server of version `server`.
pub fn cut_rule(method: DumpMethod, server: (u32, u32, u32)) -> Cut {
    match method {
        DumpMethod::LockAllTables => Cut::AnyEngine,
        DumpMethod::NoLocks => Cut::Never,
        DumpMethod::SingleTransaction if server >= (5, 5, 3) => Cut::InnoDbUnderTableLock,
        DumpMethod::SingleTransaction => Cut::Never,
    }
}

/// `options` with each table and view it names spelled as the catalog
/// spells it (a name that differs only by case matches where the server
/// compares names without case), or the names the catalog does not hold
/// (renamed or dropped since they were picked).
pub fn resolve_names(
    catalog: &Catalog,
    options: &BackupOptions,
) -> std::result::Result<BackupOptions, Vec<String>> {
    let known: Vec<&str> = catalog
        .tables
        .iter()
        .map(|table| table.name.as_str())
        .chain(catalog.views.iter().map(String::as_str))
        .collect();
    let mut resolved = options.clone();
    let mut missing = Vec::new();
    for table in &mut resolved.tables {
        let found = known.iter().find(|name| **name == table.name).or_else(|| {
            known
                .iter()
                .find(|name| catalog.fold_names && name.to_lowercase() == table.name.to_lowercase())
        });
        match found {
            Some(name) => table.name = name.to_string(),
            None => missing.push(table.name.clone()),
        }
    }
    if missing.is_empty() {
        Ok(resolved)
    } else {
        Err(missing)
    }
}

/// Whether a table's engine takes row locks and snapshots: outside the
/// server's read lock, only such a table is cut into ranges, each read in a
/// snapshot taken while Kerno holds the table's read lock.
fn transactional(engine: Option<&str>) -> bool {
    engine.is_some_and(|engine| engine.eq_ignore_ascii_case("innodb"))
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
    let rule = cut_rule(my.method, catalog.server);
    let cuttable = |engine: Option<&str>| match rule {
        Cut::AnyEngine => true,
        Cut::InnoDbUnderTableLock => transactional(engine),
        Cut::Never => false,
    };
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
                .filter(|_| data && table.bytes >= threshold && cuttable(table.engine.as_deref()))
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

/// The user's arguments a folder backup takes although they name an option
/// it sets: `--set-gtid-purged=OFF` and `--skip-add-locks` are kept (the
/// first is what it sets, the second only leaves out a lock around the
/// rows of a whole table at the restore); `--skip-lock-tables` and
/// `--skip-triggers` are taken over by [`FolderBackup::settled`].
const FOLDER_KEEPS: [&str; 4] = [
    "--set-gtid-purged=off",
    "--skip-add-locks",
    "--skip-lock-tables",
    "--skip-triggers",
];

/// mysqldump's options the folder backup sets for each file (or that would
/// break it), by their long names, with what it does instead: one of the
/// user's (any abbreviation mysqldump would take for it, a `skip-` or
/// `loose-` form) would come after and win.
const FOLDER_OWNS: [(&str, &str); 32] = [
    (
        "compatible",
        "it changes how mysqldump quotes names, and the restore reads the files to find where \
         each table's definition ends (K74, N2)",
    ),
    ("where", WHERE),
    (
        "tab",
        "the folder backup writes SQL files itself; --tab has the server write text files",
    ),
    ("tables", WHICH),
    ("databases", WHICH),
    ("all-databases", WHICH),
    ("ignore-table", WHICH),
    ("ignore-table-data", WHICH),
    ("no-data", WHICH),
    ("no-create-info", WHICH),
    ("xml", "the folder backup writes SQL files"),
    ("single-transaction", HOW),
    ("lock-all-tables", HOW),
    ("lock-tables", HOW),
    ("add-locks", HOW),
    ("opt", OPT),
    ("compact", OPT),
    ("set-charset", OPT),
    ("add-drop-table", OPT),
    ("triggers", AFTER),
    ("routines", AFTER),
    ("events", AFTER),
    (
        "set-gtid-purged",
        "the folder backup sets --set-gtid-purged=OFF itself",
    ),
    ("source-data", REPLICA),
    ("master-data", REPLICA),
    ("dump-replica", REPLICA),
    ("dump-slave", REPLICA),
    ("apply-replica-statements", REPLICA),
    ("apply-slave-statements", REPLICA),
    ("include-source-host-port", REPLICA),
    ("include-master-host-port", REPLICA),
    ("flush-logs", FLUSH),
];

const WHERE: &str = "the folder backup sets its own --where for each range of a large table";
const WHICH: &str = "the folder backup sets which tables each file holds and what of them (the \
                     Objects page and the Content choice)";
const HOW: &str = "the folder backup sets how each file is read (the Consistency choice)";
const OPT: &str = "it turns off what the folder's files rely on (DROP TABLE before each table, the \
                   character set, the table locks), which the backup's Settings choose";
const AFTER: &str = "the folder backup writes them in a file of their own (the checkboxes in \
                     Settings)";
const REPLICA: &str = "the folder backup does not write a replication position: use the plain backup \
                       (one file) to seed a replica";
const FLUSH: &str = "each file would flush the server's logs, and wait behind Kerno's table locks: \
                     use the plain backup for that";

/// Their short letters.
const FOLDER_OWNS_LETTERS: [(char, &str); 12] = [
    ('w', WHERE),
    (
        'T',
        "the folder backup writes SQL files itself; -T has the server write text files",
    ),
    ('d', WHICH),
    ('t', WHICH),
    ('B', WHICH),
    ('A', WHICH),
    ('X', "the folder backup writes SQL files"),
    ('x', HOW),
    ('l', HOW),
    ('E', AFTER),
    ('R', AFTER),
    ('F', FLUSH),
];

/// Why the folder backup refuses the user's argument `word`, or `None`
/// when it keeps it.
fn folder_refusal(word: &str) -> Option<String> {
    let normal = word.to_ascii_lowercase().replace('_', "-");
    if FOLDER_KEEPS.contains(&normal.as_str()) {
        return None;
    }
    let why = if let Some(long) = normal.strip_prefix("--") {
        let mut name = long.split('=').next().unwrap_or_default().to_string();
        const PREFIXES: [&str; 5] = ["skip-", "enable-", "disable-", "loose-", "maximum-"];
        while let Some(rest) = PREFIXES.iter().find_map(|prefix| name.strip_prefix(prefix)) {
            name = rest.to_string();
        }
        if name.is_empty() {
            return None;
        }
        FOLDER_OWNS
            .iter()
            .find(|(own, _)| own.starts_with(&name))
            .map(|(_, why)| *why)
    } else {
        let cluster = word.strip_prefix('-')?;
        FOLDER_OWNS_LETTERS
            .iter()
            .find(|(letter, _)| cluster.contains(*letter))
            .map(|(_, why)| *why)
    }?;
    Some(format!(
        "“{word}”: {why}. Leave it out of the folder backup"
    ))
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
    /// The backup with the user's arguments that the folder takes over
    /// ([`FOLDER_KEEPS`]) taken out: `--skip-triggers` turns the triggers
    /// off (in the plan, the files and the manifest); `--skip-lock-tables`
    /// is dropped, as the folder sets how each file is locked. [`back_up`]
    /// runs this.
    pub fn settled(mut self) -> Result<Self> {
        let words = super::split_words(&self.options.extra_args)?;
        let taken = |word: &String, name: &str| word.to_ascii_lowercase().replace('_', "-") == name;
        let no_triggers = words.iter().any(|word| taken(word, "--skip-triggers"));
        let kept: Vec<String> = words
            .into_iter()
            .filter(|word| !taken(word, "--skip-triggers") && !taken(word, "--skip-lock-tables"))
            .map(|word| {
                // Quoted as `split_words` reads it back.
                let escaped = word
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('$', "\\$")
                    .replace('`', "\\`");
                format!("\"{escaped}\"")
            })
            .collect();
        self.options.extra_args = kept.join(" ");
        if no_triggers && let EngineBackup::MySql(my) = &mut self.options.engine {
            my.triggers = false;
        }
        Ok(self)
    }

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
        if let Some(why) = super::split_words(&self.options.extra_args)?
            .iter()
            .find_map(|word| folder_refusal(word))
        {
            return Err(Error::Unsupported(why));
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
    /// A cancelled restore: what it left, as the database holds it (K74,
    /// F3).
    pub said: Option<String>,
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
    let mut catalog = Catalog {
        server: session.server().version_number,
        ..Catalog::default()
    };
    let fold = rows(session, "SELECT @@lower_case_table_names".into()).await?;
    catalog.fold_names = fold
        .first()
        .and_then(|row| row.first().cloned().flatten())
        .is_some_and(|value| value != "0");
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

/// Whether the account sees every account's running statements (the
/// global PROCESS privilege): without it, [`long_statements`] lists only
/// its own.
pub async fn sees_every_statement(session: &Arc<dyn Session>) -> Result<bool> {
    has_privilege(session, "PROCESS").await
}

/// Whether the account has the global privilege `privilege`.
async fn has_privilege(session: &Arc<dyn Session>, privilege: &str) -> Result<bool> {
    let found = rows(
        session,
        format!(
            "SELECT COUNT(*) FROM information_schema.USER_PRIVILEGES WHERE PRIVILEGE_TYPE = \
             '{privilege}' AND GRANTEE = CONCAT('''', SUBSTRING_INDEX(CURRENT_USER(), '@', 1), \
             '''@''', SUBSTRING_INDEX(CURRENT_USER(), '@', -1), '''')"
        ),
    )
    .await?;
    Ok(found
        .first()
        .and_then(|row| row.first().cloned().flatten())
        .is_some_and(|count| count != "0"))
}

/// Whether the account may hold `FLUSH TABLES WITH READ LOCK`: it has the
/// global RELOAD privilege (a privilege that only an active role of MySQL
/// 8 gives is not seen, and the choice stays off).
pub async fn can_lock(session: &Arc<dyn Session>) -> Result<bool> {
    has_privilege(session, "RELOAD").await
}

/// One tool run of a pool.
struct Job {
    /// Which file, as the manifest names it.
    file: String,
    /// The log's prefix.
    label: String,
    command: ToolCommand,
}

/// The ranges of one table, started together while Kerno's session holds
/// the table's read lock, so that every range's snapshot reads the same
/// moment (D-135); the table dumped whole by one process when the lock
/// does not come.
struct Gate {
    /// The table's name, for the log and a failure.
    label: String,
    /// `LOCK TABLES … READ` of the table.
    lock: String,
    /// Its index in the plan, said back when it was dumped whole.
    table: usize,
    ranges: Vec<Job>,
    whole: Job,
}

/// A stage of a group: files side by side, or the ranges of one table
/// started together under its lock.
enum Stage {
    Side(Vec<Job>),
    Together(Box<Gate>),
}

/// What waits for a free process.
enum Ready {
    One(usize, Job),
    Together(usize, Gate),
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
/// absent), the first failure (its label and what the tool said), and the
/// tables dumped whole because their lock did not come.
#[derive(Debug, Default)]
struct PoolEnd {
    states: BTreeMap<String, FileState>,
    failure: Option<(String, String)>,
    cancelled: bool,
    whole: Vec<usize>,
}

/// What a tool line says of a failure: the last line that is not one of
/// mysqldump's `--verbose` notes nor a warning.
fn failure_line(line: &str) -> bool {
    !line.starts_with("-- ") && !line.contains("Warning") && !line.trim().is_empty()
}

/// What a tool said of its failure: its last `ERROR …` line, else its last
/// line that says a failure ([`failure_line`]). A server error that quotes
/// several lines is not reduced to the last of them (K74, L1).
#[derive(Debug, Clone, Default)]
struct Said {
    error: Option<String>,
    other: Option<String>,
}

impl Said {
    fn hear(&mut self, line: &str) {
        if line.starts_with("ERROR ") {
            self.error = Some(line.to_string());
        } else if failure_line(line) {
            self.other = Some(line.to_string());
        }
    }

    fn last(&self) -> Option<String> {
        self.error.clone().or_else(|| self.other.clone())
    }
}

/// mysqldump's note when it reads no column of a table: a client that does
/// not understand this server (8.4 and later on MySQL 5.1); it still exits
/// 0 when the triggers are left out, with no table in the file.
const NO_FIELDS: &str = "it has no fields";

/// Whether `line` is mysqldump's own `--verbose` note that it read no
/// column of a table ("-- Skipping dump data for table 't', it has no
/// fields"), not a line that only holds the phrase.
pub fn no_fields_line(line: &str) -> bool {
    line.starts_with("-- Skipping dump data for table") && line.trim_end().ends_with(NO_FIELDS)
}

/// The `--verbose` line mysqldump writes once its snapshot is taken (it
/// starts the transaction first): checked with the clients 5.5 to 26.10
/// and MariaDB's 11.8, on MySQL 5.1 to 8.4 and MariaDB 11.
const SNAPSHOT_TAKEN: &str = "-- Retrieving table structure for table";

/// How long a table's ranges may take to start under its lock before the
/// lock is released and the backup fails.
const START_WAIT: Duration = Duration::from_secs(30);

/// How long Kerno waits for a lock (a table's, or the server's read lock)
/// before it gives up: `lock_wait_timeout` on the servers that have it
/// (5.5 on), Kerno's own timer on the others.
pub const LOCK_WAIT: Duration = Duration::from_secs(10);

/// Where the tools' lines go.
type OnLine = Arc<dyn Fn(String) + Send + Sync>;

/// What a finished task hands back: its outcome, the last line that says
/// a failure, and whether the client found no columns.
type Ran = (Result<std::process::ExitStatus>, Option<String>, bool);

/// Starts `job` in `running`. `started` counts the job once its snapshot
/// is taken (or once it ended, so that nobody waits for it).
fn spawn_job(
    running: &mut tokio::task::JoinSet<Ran>,
    names: &mut BTreeMap<tokio::task::Id, (usize, String, String)>,
    group: usize,
    job: Job,
    on_line: &OnLine,
    stop: &Arc<AtomicBool>,
    started: Option<Arc<AtomicUsize>>,
) {
    let said: Arc<Mutex<(Said, bool)>> = Arc::default();
    let counted = Arc::new(AtomicBool::new(false));
    let (sink, heard, label) = (on_line.clone(), said.clone(), job.label.clone());
    let (seen, count) = (counted.clone(), started.clone());
    let stop = stop.clone();
    let handle = running.spawn(async move {
        let outcome = run(
            job.command,
            move |line| {
                if let Ok(mut said) = heard.lock() {
                    said.1 |= no_fields_line(&line);
                    said.0.hear(&line);
                }
                if line.starts_with(SNAPSHOT_TAKEN)
                    && let Some(count) = &count
                    && !seen.swap(true, Ordering::Relaxed)
                {
                    count.fetch_add(1, Ordering::Relaxed);
                }
                sink(format!("[{label}] {line}"));
            },
            stop,
        )
        .await;
        if let Some(started) = started
            && !counted.swap(true, Ordering::Relaxed)
        {
            started.fetch_add(1, Ordering::Relaxed);
        }
        let (last, no_fields) = said
            .lock()
            .map(|said| (said.0.last(), said.1))
            .unwrap_or_default();
        (outcome, last, no_fields)
    });
    names.insert(handle.id(), (group, job.file, job.label));
}

/// Takes `lock` on `session`, within [`LOCK_WAIT`] (and a little more for
/// a server without `lock_wait_timeout`); why not, in words.
async fn lock_within(session: &Arc<dyn Session>, lock: &str) -> std::result::Result<(), String> {
    match tokio::time::timeout(
        LOCK_WAIT + Duration::from_secs(2),
        exec::collect(session.clone(), lock),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(crate::describe(&error).to_string()),
        Err(_) => Err(format!(
            "the lock did not come within {} s",
            LOCK_WAIT.as_secs()
        )),
    }
}

/// Moves the next stage of `group` to `ready`.
fn queue_next(
    groups: &mut [VecDeque<Stage>],
    group: usize,
    left: &mut [usize],
    ready: &mut VecDeque<Ready>,
) {
    match groups[group].pop_front() {
        Some(Stage::Side(jobs)) => {
            left[group] = jobs.len();
            ready.extend(jobs.into_iter().map(|job| Ready::One(group, job)));
        }
        Some(Stage::Together(gate)) => {
            left[group] = gate.ranges.len();
            ready.push_back(Ready::Together(group, *gate));
        }
        None => {}
    }
}

/// The first of `ready` that can start with `running` of `jobs` processes
/// busy: a single file in any free process, a table's ranges once they all
/// have one. Files behind a table that waits for its processes fill the
/// free ones meanwhile.
fn startable(ready: &VecDeque<Ready>, running: usize, jobs: usize) -> Option<usize> {
    ready.iter().position(|item| match item {
        Ready::One(..) => running < jobs,
        Ready::Together(_, gate) => running == 0 || running + gate.ranges.len() <= jobs,
    })
}

/// Runs `groups` with at most `jobs` tools at once. A group is a list of
/// stages: a stage's files run side by side, the next stage once they all
/// succeeded. A table's ranges start together on `locker`, Kerno's session
/// (see [`Gate`]). The first failure, or `cancel`, stops every tool still
/// running and starts no other. Every tool has ended when it returns.
async fn run_pool(
    groups: Vec<VecDeque<Stage>>,
    jobs: usize,
    on_line: OnLine,
    cancel: &AtomicBool,
    locker: Option<&Arc<dyn Session>>,
    backup: bool,
) -> PoolEnd {
    let stop = Arc::new(AtomicBool::new(false));
    let mut groups = groups;
    let mut ready: VecDeque<Ready> = VecDeque::new();
    let mut left: Vec<usize> = vec![0; groups.len()];
    for group in 0..groups.len() {
        queue_next(&mut groups, group, &mut left, &mut ready);
    }
    let jobs = jobs.max(1);
    let mut end = PoolEnd::default();
    let mut running = tokio::task::JoinSet::new();
    let mut names = BTreeMap::new();
    let mut tick = tokio::time::interval(CANCEL_POLL);
    loop {
        while !stop.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
            let Some(at) = startable(&ready, running.len(), jobs) else {
                break;
            };
            match ready.remove(at) {
                Some(Ready::One(group, job)) => {
                    spawn_job(&mut running, &mut names, group, job, &on_line, &stop, None);
                }
                Some(Ready::Together(group, gate)) => {
                    let Gate {
                        label,
                        lock,
                        table,
                        ranges,
                        whole,
                    } = gate;
                    let locked = match locker {
                        Some(session) => lock_within(session, &lock).await,
                        None => Err("Kerno has no session to lock it".into()),
                    };
                    if let Err(why) = locked {
                        on_line(format!(
                            "Kerno: “{label}” is read whole by one process ({why})."
                        ));
                        end.whole.push(table);
                        left[group] = 1;
                        ready.push_front(Ready::One(group, whole));
                        continue;
                    }
                    let count = ranges.len();
                    let started = Arc::new(AtomicUsize::new(0));
                    for job in ranges {
                        let counter = Some(started.clone());
                        spawn_job(
                            &mut running,
                            &mut names,
                            group,
                            job,
                            &on_line,
                            &stop,
                            counter,
                        );
                    }
                    let since = std::time::Instant::now();
                    let all = loop {
                        if started.load(Ordering::Relaxed) >= count {
                            break true;
                        }
                        if stop.load(Ordering::Relaxed)
                            || cancel.load(Ordering::Relaxed)
                            || since.elapsed() > START_WAIT
                        {
                            break false;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    };
                    let unlocked = match locker {
                        Some(session) => exec::collect(session.clone(), "UNLOCK TABLES").await,
                        None => Ok(exec::Collected::default()),
                    };
                    let why = if unlocked.is_err() {
                        Some("Kerno could not release its lock of the table".to_string())
                    } else if !all
                        && !stop.load(Ordering::Relaxed)
                        && !cancel.load(Ordering::Relaxed)
                    {
                        Some(format!(
                            "its parts did not all start within {} s; Kerno released the \
                             table's lock",
                            START_WAIT.as_secs()
                        ))
                    } else {
                        None
                    };
                    if let Some(why) = why {
                        end.failure.get_or_insert((label, why));
                        stop.store(true, Ordering::Relaxed);
                    }
                }
                None => break,
            }
        }
        if running.is_empty() {
            break;
        }
        tokio::select! {
            joined = running.join_next_with_id() => {
                let (id, ran) = match joined {
                    Some(Ok((id, ran))) => (id, Ok(ran)),
                    Some(Err(error)) => (error.id(), Err(error)),
                    None => continue,
                };
                let Some((group, file, label)) = names.remove(&id) else {
                    continue;
                };
                let failed = match ran {
                    // A task that panicked: that file failed (F5).
                    Err(error) => Some(format!("Kerno's task stopped unexpectedly ({error})")),
                    Ok((Ok(_), _, true)) if backup => Some(
                        "the client found no columns in the table: it cannot read this \
                         server's tables (choose another client with “Local client…”)"
                            .to_string(),
                    ),
                    Ok((Ok(status), _, _)) if status.success() => None,
                    Ok((Ok(status), last, _)) => Some(last.unwrap_or_else(|| format!("{status}"))),
                    Ok((Err(Error::Cancelled), ..)) => {
                        end.states.insert(file, FileState::Stopped);
                        continue;
                    }
                    Ok((Err(error), ..)) => Some(error.to_string()),
                };
                match failed {
                    None => {
                        end.states.insert(file, FileState::Done);
                        left[group] -= 1;
                        if left[group] == 0 {
                            queue_next(&mut groups, group, &mut left, &mut ready);
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
    // A cancel that came while the last tool ended, or before any started.
    end.cancelled |= end.failure.is_none() && cancel.load(Ordering::Relaxed);
    end
}

/// Runs `sql` on `session`, given up (and cancelled on the server) when
/// `cancel` is set.
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

/// The session's `CONNECTION_ID()`, within a few seconds.
async fn connection_id(session: &Arc<dyn Session>) -> Option<String> {
    let read = tokio::time::timeout(
        Duration::from_secs(10),
        rows(session, "SELECT CONNECTION_ID()".into()),
    )
    .await;
    read.ok()?
        .ok()?
        .first()
        .and_then(|row| row.first().cloned().flatten())
}

/// The time of day, as the log and a lost lock say it.
fn clock() -> String {
    rusqlite::Connection::open_in_memory()
        .and_then(|db| {
            db.query_row(
                "SELECT strftime('%H:%M:%S', 'now', 'localtime')",
                [],
                |row| row.get(0),
            )
        })
        .unwrap_or_default()
}

/// Gets Kerno's session ready to hold a lock for the whole run: the server
/// does not close it while it waits idle (`wait_timeout`), and a lock that
/// does not come stops after [`LOCK_WAIT`] (`lock_wait_timeout`, 5.5 on)
/// instead of leaving every write of the server waiting behind it.
async fn prepare_locks(session: &Arc<dyn Session>) {
    let _ = exec::collect(session.clone(), "SET SESSION wait_timeout = 86400").await;
    if session.server().version_number >= (5, 5, 0) {
        let sql = format!("SET SESSION lock_wait_timeout = {}", LOCK_WAIT.as_secs());
        let _ = exec::collect(session.clone(), sql).await;
    }
}

/// Takes the server's read lock on `session` and returns the session's
/// connection id, or why it could not, in words.
async fn take_read_lock(session: &Arc<dyn Session>, cancel: &AtomicBool) -> Result<String> {
    let taking = tokio::time::timeout(
        LOCK_WAIT + Duration::from_secs(2),
        statement(session, "FLUSH TABLES WITH READ LOCK", cancel),
    )
    .await;
    let timed_out = |text: &str| text.contains("1205") || text.contains("Lock wait timeout");
    match taking {
        Ok(Ok(())) => connection_id(session).await.ok_or_else(|| {
            Error::Config(
                "Kerno's session was lost as it took the read lock. Nothing was backed up.".into(),
            )
        }),
        Err(_) => Err(Error::Config(lock_not_taken())),
        Ok(Err(error)) if timed_out(&error.to_string()) => Err(Error::Config(lock_not_taken())),
        Ok(Err(error)) => Err(error),
    }
}

/// Why the backup did not start when the read lock did not come.
fn lock_not_taken() -> String {
    format!(
        "The read lock did not come within {} s: a statement that reads or writes tables was \
         running, and the lock waits for it to end. Nothing was backed up; try again once it \
         has ended, or back up without blocking writes. Statements that came after the lock \
         may still wait for that statement to end.",
        LOCK_WAIT.as_secs()
    )
}

/// What a backup says when the read lock was lost during the run.
fn lock_lost(at: &str) -> String {
    format!(
        "The lock that kept writes out was lost at {at} (the server closed Kerno's session): \
         the backup is not consistent, and its folder was removed. Back up again."
    )
}

/// The statements running for at least `seconds` on the server, other
/// than this session's: their session id, how long, and the start of
/// their text (as far as this account may see them).
pub async fn long_statements(
    session: &Arc<dyn Session>,
    seconds: u32,
) -> Result<Vec<(String, u64, String)>> {
    let found = rows(
        session,
        format!(
            "SELECT ID, TIME, LEFT(COALESCE(INFO, ''), 80) FROM information_schema.PROCESSLIST \
             WHERE COMMAND IN ('Query', 'Execute') AND TIME >= {seconds} \
             AND ID <> CONNECTION_ID() \
             ORDER BY TIME DESC"
        ),
    )
    .await?;
    Ok(found
        .into_iter()
        .filter_map(|row| {
            let text = |at: usize| row.get(at).cloned().flatten();
            Some((
                text(0)?,
                text(1)?.parse().ok()?,
                text(2).unwrap_or_default(),
            ))
        })
        .collect())
}

/// Backs `job.options.database` up into the new folder `job.folder`, on
/// [`FolderBackup::jobs`] mysqldump at once, on `session` (a session of its
/// own, in the database): with "lock all tables", the server's read lock is
/// taken first, then the catalog read, and the lock is watched for the whole
/// run; otherwise each table cut into ranges is locked while its ranges
/// start. The manifest is written last. A failure, a lost lock or `cancel`
/// stops every mysqldump, releases the lock and removes the folder this run
/// made; a folder already there is neither written into nor removed.
pub async fn back_up(
    job: FolderBackup,
    session: Arc<dyn Session>,
    on_line: Arc<dyn Fn(String) + Send + Sync>,
    cancel: Arc<AtomicBool>,
) -> FolderEnd {
    let mut made = false;
    // Windows: a folder of another computer is never made nor written (K69,
    // D-138); each mysqldump's file is checked again where it runs.
    let checked = crate::local_path::check(&job.folder, "backup folder");
    let job = match checked.and_then(|()| job.settled()) {
        Ok(job) => job,
        Err(error) => {
            return FolderEnd {
                outcome: Err(error),
                removed: false,
                said: None,
            };
        }
    };
    let outcome = back_up_in(&job, &session, &on_line, &cancel, &mut made).await;
    let removed = outcome.is_err() && made && std::fs::remove_dir_all(&job.folder).is_ok();
    FolderEnd {
        outcome,
        removed,
        said: None,
    }
}

async fn back_up_in(
    job: &FolderBackup,
    session: &Arc<dyn Session>,
    on_line: &OnLine,
    cancel: &AtomicBool,
    made: &mut bool,
) -> Result<()> {
    prepare_locks(session).await;
    if !job.locks() {
        let prepared = prepare(job, session, on_line, made).await?;
        let (tables, after) = dump_all(job, session, on_line, cancel, prepared).await?;
        return write_manifest(job, session, tables, after);
    }
    // The lock first, then the catalog (as `--lock-all-tables`): a table
    // made meanwhile is not missed.
    on_line("Kerno: FLUSH TABLES WITH READ LOCK (writes wait until the backup ends)".into());
    let id = match take_read_lock(session, cancel).await {
        Ok(id) => id,
        Err(error) => {
            let _ = exec::collect(session.clone(), "UNLOCK TABLES").await;
            return Err(error);
        }
    };
    let prepared = match prepare(job, session, on_line, made).await {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = exec::collect(session.clone(), "UNLOCK TABLES").await;
            return Err(error);
        }
    };
    // Watched while the tools run, once the catalog is read: Kerno sends
    // nothing else on the session meanwhile. A server or a proxy may close
    // an idle session, and its lock with it.
    let lost: Mutex<Option<String>> = Mutex::new(None);
    let (inner, done) = (AtomicBool::new(false), AtomicBool::new(false));
    let dumping = async {
        let dumped = dump_all(job, session, on_line, &inner, prepared).await;
        done.store(true, Ordering::Relaxed);
        dumped
    };
    let watching = async {
        // One check at a time, never dropped half-way (that could break the
        // session): one that has not answered yet is no evidence, only an
        // error or another connection's id is.
        let mut check: Option<tokio::task::JoinHandle<Held>> = None;
        let mut checked = std::time::Instant::now();
        while !done.load(Ordering::Relaxed) {
            if cancel.load(Ordering::Relaxed) {
                inner.store(true, Ordering::Relaxed);
            }
            if check.as_ref().is_some_and(|running| running.is_finished())
                && let Some(running) = check.take()
                && let Ok(Held::Lost) = running.await
            {
                if let Ok(mut lost) = lost.lock() {
                    *lost = Some(clock());
                }
                inner.store(true, Ordering::Relaxed);
                break;
            }
            if check.is_none() && checked.elapsed() >= WATCH_EVERY {
                checked = std::time::Instant::now();
                check = Some(tokio::spawn(held(session.clone(), id.clone())));
            }
            tokio::time::sleep(CANCEL_POLL).await;
        }
        check
    };
    let (dumped, check) = tokio::join!(dumping, watching);
    // Still the same session at the end, then released: otherwise the lock
    // was lost at some point and the backup is not consistent.
    let lost_at = lost.lock().ok().and_then(|lost| lost.clone());
    let at_end = match lost_at {
        Some(_) => Held::Lost,
        None => {
            let (checked, id) = (session.clone(), id.clone());
            let mut last = tokio::spawn(async move {
                // The session answers in order: the check under way first.
                if let Some(check) = check {
                    let _ = check.await;
                }
                held(checked, id).await
            });
            match tokio::time::timeout(FINAL_CHECK, &mut last).await {
                Ok(Ok(found)) => found,
                Ok(Err(_)) => Held::Lost,
                Err(_) => {
                    // No new statement after this one; the one that does
                    // not answer is cancelled, so that the lock can be
                    // released and the session closed (each wait bounded:
                    // the run always ends).
                    last.abort();
                    on_line("Kerno: the read lock's session does not answer; cancelling.".into());
                    let _ = tokio::time::timeout(UNSTICK_WAIT, session.cancel()).await;
                    let _ = tokio::time::timeout(
                        UNSTICK_WAIT,
                        exec::collect(session.clone(), "UNLOCK TABLES"),
                    )
                    .await;
                    Held::NoAnswer
                }
            }
        }
    };
    if let Held::NoAnswer = at_end {
        return Err(Error::Config(format!(
            "Kerno could not confirm that its read lock held to the end (its session did not \
             answer within {} s): the backup may not be consistent, and its folder was removed. \
             Back up again.",
            FINAL_CHECK.as_secs()
        )));
    }
    on_line("Kerno: releasing the read lock".into());
    let unlocked = exec::collect(session.clone(), "UNLOCK TABLES")
        .await
        .is_ok();
    on_line("Kerno: UNLOCK TABLES".into());
    if !(matches!(at_end, Held::Yes) && unlocked) {
        let at = lost_at.unwrap_or_else(clock);
        on_line(format!("Kerno: the read lock was lost at {at}."));
        return Err(Error::Config(lock_lost(&at)));
    }
    let (tables, after) = dumped?;
    write_manifest(job, session, tables, after)
}

/// How long the last check of the read lock's session may take.
const FINAL_CHECK: Duration = Duration::from_secs(60);

/// How long a session that does not answer gets for its cancel, then for
/// `UNLOCK TABLES`.
const UNSTICK_WAIT: Duration = Duration::from_secs(10);

/// What a check of the read lock's session found.
enum Held {
    /// The same connection answered.
    Yes,
    /// It failed, or another connection answered: the lock is gone.
    Lost,
    /// No answer within the wait: not evidence either way.
    NoAnswer,
}

/// Checks that `session` is still the connection `id` that holds the lock.
async fn held(session: Arc<dyn Session>, id: String) -> Held {
    match rows(&session, "SELECT CONNECTION_ID()".into()).await {
        Err(_) => Held::Lost,
        Ok(found) => {
            let answered = found.first().and_then(|row| row.first().cloned().flatten());
            if answered.as_deref() == Some(id.as_str()) {
                Held::Yes
            } else {
                Held::Lost
            }
        }
    }
}

/// How often the read lock's session is checked during the run.
const WATCH_EVERY: Duration = Duration::from_secs(2);

/// A backup ready to run: its tables and the files after them, as the
/// manifest will list them, and the jobs.
struct Prepared {
    tables: Vec<TableFiles>,
    after: Vec<AfterFile>,
    groups: Vec<VecDeque<Stage>>,
    finishing: VecDeque<Stage>,
    /// The table locks of [`Cut::InnoDbUnderTableLock`] are taken.
    gated: bool,
}

/// Reads the catalog, checks the names picked, builds every command and
/// makes the folder.
async fn prepare(
    job: &FolderBackup,
    session: &Arc<dyn Session>,
    on_line: &OnLine,
    made: &mut bool,
) -> Result<Prepared> {
    let database = &job.options.database;
    let catalog = read_catalog(session, database, job.range_threshold).await?;
    let options = resolve_names(&catalog, &job.options).map_err(|missing| {
        let names: Vec<String> = missing.iter().map(|name| format!("“{name}”")).collect();
        Error::Config(format!(
            "The backup names tables or views that “{database}” does not hold: {}. They were \
             renamed or dropped since they were picked: pick them again (Objects page, or the \
             saved task's settings). Nothing was run.",
            names.join(", ")
        ))
    })?;
    let (tables, after) = plan(&catalog, &options, job.jobs(), job.range_threshold);
    let gated = job
        .my()
        .is_ok_and(|my| cut_rule(my.method, catalog.server) == Cut::InnoDbUnderTableLock);
    // Every command is built before anything runs: a refused option stops
    // the backup before the folder exists.
    let quote = |name: &str| session.dialect().quote_ident(name);
    let (mut groups, mut side) = (Vec::new(), Vec::new());
    for (index, table) in tables.iter().enumerate() {
        let n = table.files.len();
        let label = |at: usize| match n {
            1 => table.table.clone(),
            _ => format!("{} {}/{n}", table.table, at + 1),
        };
        let mut jobs = Vec::new();
        for (at, piece) in table.files.iter().enumerate() {
            jobs.push(Job {
                file: piece.file.clone(),
                label: label(at),
                command: job.piece_command(table, piece)?,
            });
        }
        if n > 1 && gated {
            let whole = TableFiles {
                files: vec![Piece {
                    file: table.files[0].file.replace(".part01.sql", ".sql"),
                    condition: None,
                }],
                ..table.clone()
            };
            groups.push(VecDeque::from([Stage::Together(Box::new(Gate {
                label: table.table.clone(),
                lock: format!(
                    "LOCK TABLES {}.{} READ",
                    quote(database),
                    quote(&table.table)
                ),
                table: index,
                whole: Job {
                    file: whole.files[0].file.clone(),
                    label: table.table.clone(),
                    command: job.piece_command(&whole, &whole.files[0])?,
                },
                ranges: jobs,
            }))]));
        } else {
            side.extend(
                jobs.into_iter()
                    .map(|job| VecDeque::from([Stage::Side(vec![job])])),
            );
        }
    }
    // The tables cut into ranges first: each needs several processes at
    // once, and the single files fill the others meanwhile.
    groups.extend(side);
    let mut finishing = VecDeque::new();
    for file in &after {
        finishing.push_back(Stage::Side(vec![Job {
            file: file.file.clone(),
            label: file.label().into(),
            command: job.after_command(file)?,
        }]));
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
    Ok(Prepared {
        tables,
        after,
        groups,
        finishing,
        gated,
    })
}

/// Runs every mysqldump of `prepared`.
async fn dump_all(
    job: &FolderBackup,
    session: &Arc<dyn Session>,
    on_line: &OnLine,
    cancel: &AtomicBool,
    prepared: Prepared,
) -> Result<(Vec<TableFiles>, Vec<AfterFile>)> {
    let Prepared {
        mut tables,
        after,
        groups,
        finishing,
        gated,
    } = prepared;
    let locker = gated.then_some(session);
    let jobs = job.jobs() as usize;
    let end = run_pool(groups, jobs, on_line.clone(), cancel, locker, true).await;
    for &index in &end.whole {
        let table = &mut tables[index];
        table.files = vec![Piece {
            file: table.files[0].file.replace(".part01.sql", ".sql"),
            condition: None,
        }];
    }
    settle(end, "mysqldump")?;
    let end = run_pool(vec![finishing], 1, on_line.clone(), cancel, None, true).await;
    settle(end, "mysqldump")?;
    Ok((tables, after))
}

/// Writes the manifest, last: the folder is then a complete backup.
fn write_manifest(
    job: &FolderBackup,
    session: &Arc<dyn Session>,
    tables: Vec<TableFiles>,
    after: Vec<AfterFile>,
) -> Result<()> {
    let manifest = Manifest {
        kerno_backup: MANIFEST_VERSION,
        engine: job.params.engine,
        database: job.options.database.clone(),
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
    // Windows: a folder of another computer is never read (K69, D-138).
    crate::local_path::check(folder, "backup folder").map_err(|error| error.to_string())?;
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

    /// The `mysql` command that runs `sql` itself, not a file, with the
    /// account and options of the files: Kerno's own statements before and
    /// after them (K74).
    fn statement_command(&self, sql: &str) -> Result<ToolCommand> {
        let mut command = self.file_command(MANIFEST)?;
        command.stdin = None;
        command.script_check = None;
        command.args.push("--batch".into());
        command.args.push("--skip-column-names".into());
        command.args.push(format!("--execute={sql}"));
        Ok(command)
    }

    /// Runs `sql` on one `mysql` client: the lines it wrote, or a failure
    /// with its last line.
    async fn run_statements(&self, sql: &str, cancel: Arc<AtomicBool>) -> Result<Vec<String>> {
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let kept = lines.clone();
        let ran = run(
            self.statement_command(sql)?,
            move |line| {
                if let Ok(mut lines) = kept.lock() {
                    lines.push(line);
                }
            },
            cancel,
        )
        .await?;
        let lines = lines.lock().map(|lines| lines.clone()).unwrap_or_default();
        if ran.success() {
            return Ok(lines);
        }
        let mut said = Said::default();
        lines.iter().for_each(|line| said.hear(line));
        Err(Error::Config(
            said.last().unwrap_or_else(|| format!("{ran}")),
        ))
    }
}

/// How long one statement of a `mysql --execute` may be: well under the
/// command line's limit on every system (32,767 characters on Windows).
const EXECUTE_LIMIT: usize = 8_000;

/// `items` joined by `separator` into texts of at most [`EXECUTE_LIMIT`]
/// bytes after `head` (a single item longer than that alone).
fn batches(head: &str, items: &[String], separator: &str) -> Vec<String> {
    let mut batches: Vec<String> = Vec::new();
    let mut current = String::new();
    for item in items {
        if !current.is_empty() && current.len() + separator.len() + item.len() > EXECUTE_LIMIT {
            batches.push(format!("{head}{current}"));
            current.clear();
        }
        if !current.is_empty() {
            current.push_str(separator);
        }
        current.push_str(item);
    }
    if !current.is_empty() {
        batches.push(format!("{head}{current}"));
    }
    batches
}

/// What a tool's failure says, without the error kind's own words.
fn said(error: &Error) -> String {
    match error {
        Error::Config(text) => text.clone(),
        other => other.to_string(),
    }
}

fn backticked(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// Where a table's definition is in the first file of its table, as
/// mysqldump writes it: from its `DROP TABLE` (or `CREATE TABLE`) to the
/// line that ends it, `/*!40101 SET character_set_client = @saved_cs_client
/// */;` (written after every CREATE TABLE by mysqldump 5.7.44 and 26.10,
/// from MySQL and MariaDB servers: `--skip-set-charset`, which drops it, is
/// refused).
/// What comes before is the file's settings, what comes after its rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Definition {
    pub start: u64,
    pub end: u64,
    /// The file's size.
    pub len: u64,
    /// The lines before `end`.
    pub lines: u64,
}

/// How far into a file Kerno looks for the end of its table's definition.
const DEFINITION_WITHIN: u64 = 64 << 20;

/// Where the SQL text is at the end of a line: in code, or inside a quote or
/// a comment that goes on to the next line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quoted {
    Code,
    Backtick,
    Single,
    Double,
    Comment,
}

impl Quoted {
    /// The state after `line`, starting in `self`. mysqldump writes the
    /// newlines of a string as `\n` but an identifier's raw: a table or
    /// column name may hold a newline and anything after it (K74, F2).
    fn after(mut self, line: &[u8]) -> Self {
        let mut at = 0;
        while at < line.len() {
            let (byte, next) = (line[at], line.get(at + 1).copied());
            match self {
                Quoted::Code => match byte {
                    b'`' => self = Quoted::Backtick,
                    b'\'' => self = Quoted::Single,
                    b'"' => self = Quoted::Double,
                    b'#' => return self,
                    b'-' if next == Some(b'-')
                        && line
                            .get(at + 2)
                            .is_none_or(|after| after.is_ascii_whitespace()) =>
                    {
                        return self;
                    }
                    // `/*!…*/` and `/*+…*/` are code the server runs.
                    b'/' if next == Some(b'*')
                        && !matches!(line.get(at + 2), Some(b'!') | Some(b'+')) =>
                    {
                        self = Quoted::Comment;
                        at += 1;
                    }
                    _ => {}
                },
                Quoted::Backtick if byte == b'`' => {
                    if next == Some(b'`') {
                        at += 1;
                    } else {
                        self = Quoted::Code;
                    }
                }
                Quoted::Single | Quoted::Double if byte == b'\\' => at += 1,
                Quoted::Single | Quoted::Double => {
                    let quote = if self == Quoted::Single { b'\'' } else { b'"' };
                    if byte == quote {
                        if next == Some(quote) {
                            at += 1;
                        } else {
                            self = Quoted::Code;
                        }
                    }
                }
                Quoted::Comment if byte == b'*' && next == Some(b'/') => {
                    self = Quoted::Code;
                    at += 1;
                }
                Quoted::Backtick | Quoted::Comment => {}
            }
            at += 1;
        }
        self
    }
}

/// Finds the definition in the first file of a table, or `None` when it is
/// not where mysqldump puts it (no definition, rows before it, its end not
/// found): such a file then runs whole, in a client of its own (K74). Every
/// line is read for what it is: a line inside a quoted name or a comment is
/// never taken for the definition's start or end (F2). Blocking.
pub fn find_definition(path: &Path) -> std::io::Result<Option<Definition>> {
    use std::io::BufRead as _;
    let len = std::fs::metadata(path)?.len();
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let (mut at, mut start, mut created, mut lines) = (0u64, None, false, 0u64);
    let mut state = Quoted::Code;
    let mut line = Vec::new();
    while at < DEFINITION_WITHIN {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)? as u64;
        if read == 0 {
            break;
        }
        lines += 1;
        // Only a line that starts in code says anything.
        let starts_in_code = state == Quoted::Code;
        state = state.after(&line);
        if !starts_in_code {
            at += read;
            continue;
        }
        let text = String::from_utf8_lossy(&line);
        let opens = text.starts_with("DROP TABLE ")
            || text.starts_with("/*!40101 SET @saved_cs_client")
            || text.starts_with("CREATE TABLE ");
        if start.is_none() && opens {
            start = Some(at);
        }
        if start.is_some() && text.starts_with("CREATE TABLE ") {
            created = true;
        }
        if created && text.trim_end() == "/*!40101 SET character_set_client = @saved_cs_client */;"
        {
            // The definition's last line ends with its newline.
            return Ok(start
                .filter(|_| line.ends_with(b"\n"))
                .map(|start| Definition {
                    start,
                    end: at + read,
                    len,
                    lines,
                }));
        }
        let rows = ["INSERT ", "REPLACE ", "LOCK TABLES "]
            .iter()
            .any(|word| text.starts_with(word));
        if rows {
            return Ok(None);
        }
        at += read;
    }
    Ok(None)
}

/// What Kerno writes after each table's definition in the definitions'
/// run, and reads back from the client: that definition ran.
const DEFINED: &str = "kerno-defined:";

/// What a table of the backup is in the target after a stopped restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Now {
    Missing,
    Empty,
    Rows,
}

/// What Kerno knows of the backup's tables in the target after a stopped
/// restore.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dropped {
    /// The files define no table (rows only): their states say it.
    No,
    /// Read from the target once the clients ended, per table.
    Read(BTreeMap<String, Now>),
    /// Tables were (or may have been) dropped, and the target could not be
    /// read: why.
    Unread(String),
}

/// Reads, on one `mysql` client, which of `tables` the target holds, and
/// whether each holds a row. Within 30 s each read.
async fn tables_now(
    job: &FolderRestore,
    tables: &[String],
) -> std::result::Result<BTreeMap<String, Now>, String> {
    // Never behind a lock a stopped client's statement still holds for long.
    let head = "/*!50503 SET SESSION lock_wait_timeout = 10 */; ";
    let read = async |queries: Vec<String>| -> std::result::Result<BTreeMap<usize, bool>, String> {
        let mut found = BTreeMap::new();
        for sql in batches(head, &queries, "; ") {
            let cancel = Arc::new(AtomicBool::new(false));
            let lines =
                tokio::time::timeout(Duration::from_secs(30), job.run_statements(&sql, cancel))
                    .await
                    .map_err(|_| "it did not answer within 30 s".to_string())?
                    .map_err(|error| said(&error))?;
            for line in lines {
                if let Some((at, yes)) = line.split_once('\t')
                    && let Ok(at) = at.trim().parse::<usize>()
                {
                    found.insert(at, yes.trim() == "1");
                }
            }
        }
        Ok(found)
    };
    let hex = |name: &str| -> String { name.bytes().map(|byte| format!("{byte:02X}")).collect() };
    let there = read(
        tables
            .iter()
            .enumerate()
            .map(|(at, name)| {
                format!(
                    "SELECT {at}, EXISTS(SELECT 1 FROM information_schema.TABLES WHERE \
                     TABLE_SCHEMA = DATABASE() AND TABLE_TYPE = 'BASE TABLE' AND TABLE_NAME = \
                     _utf8 X'{}')",
                    hex(name)
                )
            })
            .collect(),
    )
    .await?;
    let held = read(
        tables
            .iter()
            .enumerate()
            .filter(|(at, _)| there.get(at) == Some(&true))
            .map(|(at, name)| format!("SELECT {at}, EXISTS(SELECT 1 FROM {})", backticked(name)))
            .collect(),
    )
    .await?;
    let mut now = BTreeMap::new();
    for (at, name) in tables.iter().enumerate() {
        let state = match (there.get(&at), held.get(&at)) {
            (Some(false), _) => Now::Missing,
            (Some(true), Some(false)) => Now::Empty,
            (Some(true), Some(true)) => Now::Rows,
            _ => return Err(format!("no answer for “{name}”")),
        };
        now.insert(name.clone(), state);
    }
    Ok(now)
}

/// The `mysql` client's options that change what it prints: Kerno reads
/// that output in a folder restore (the definitions' markers, the read of
/// the database after a failure), by their long names.
const PRINTS: [&str; 9] = [
    "table",
    "xml",
    "html",
    "vertical",
    "auto-vertical-output",
    "silent",
    "verbose",
    "column-type-info",
    "execute",
];

/// The same by their letters: `-t -X -H -E -s -v -m -e`.
const PRINTS_LETTERS: [char; 8] = ['t', 'X', 'H', 'E', 's', 'v', 'm', 'e'];

/// The client's letters whose value may follow them in the same word
/// (`-hhost`): the rest of the word is that value.
const VALUE_LETTERS: [char; 6] = ['h', 'u', 'p', 'P', 'S', 'D'];

/// Why the user's extra arguments `text` cannot go to a folder restore's
/// clients, or `None` (K74, F4): an option that changes what the client
/// prints, in any spelling the client takes (a prefix, a `loose-` form,
/// letters together).
pub fn restore_refusal(text: &str) -> Option<String> {
    // Words the client gets; one that does not split is refused elsewhere.
    let words = super::extra_args(text, super::ClientFamily::MySql).unwrap_or_default();
    words.iter().find_map(|word| {
        let refused = if let Some(long) = word.strip_prefix("--") {
            let mut name = long
                .split('=')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .replace('_', "-");
            while let Some(rest) = ["loose-", "enable-"]
                .iter()
                .find_map(|prefix| name.strip_prefix(prefix))
            {
                name = rest.to_string();
            }
            !name.is_empty() && PRINTS.iter().any(|own| own.starts_with(&name))
        } else if let Some(letters) = word.strip_prefix('-') {
            letters
                .chars()
                .take_while(|letter| !VALUE_LETTERS.contains(letter))
                .any(|letter| PRINTS_LETTERS.contains(&letter))
        } else {
            false
        };
        refused.then(|| {
            format!(
                "“{word}” is not allowed for a folder restore: it changes what the mysql client \
                 prints, which Kerno reads to know which tables are in. Leave it out."
            )
        })
    })
}

/// How a stopped step of the restore ended: cancelled (`Ok`), or failed on
/// a label with what the client said.
type Stopped = Option<std::result::Result<(), (String, String)>>;

/// Says a client's "at line N" of the definitions' run as the line of the
/// table's own file, which starts on line `start` of the run.
fn in_its_file(said: &str, start: Option<u64>, file: &str) -> String {
    let Some((before, after)) = said.split_once(" at line ") else {
        return said.to_string();
    };
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    match (digits.parse::<u64>(), start) {
        (Ok(line), Some(start)) if line >= start => format!(
            "{before} at line {} of {file}{}",
            line - start + 1,
            &after[digits.len()..]
        ),
        _ => said.to_string(),
    }
}

/// Runs every table's definition on one client, in the manifest's order:
/// the first file of each table up to the end of its definition (the whole
/// file where [`find_definition`] found none), each followed by a marker
/// the client prints back, so that `defined` holds the tables whose
/// definition ran, also after a failure or a cancel.
async fn define(
    job: &FolderRestore,
    definitions: &[Option<Definition>],
    defined: &mut BTreeSet<usize>,
    on_line: &Arc<dyn Fn(String) + Send + Sync>,
    cancel: &Arc<AtomicBool>,
) -> Stopped {
    let manifest = &job.manifest;
    // Only the definitions found: a file whose definition was not found
    // runs whole, in a client of its own (F2).
    let found: Vec<(usize, Definition, &Piece)> = manifest
        .tables
        .iter()
        .enumerate()
        .filter_map(|(index, table)| {
            Some((
                index,
                definitions.get(index).copied().flatten()?,
                table.files.first()?,
            ))
        })
        .collect();
    let (_, _, first) = found.first()?;
    let mut parts = Vec::new();
    // The line of the run each table's part starts on.
    let mut starts = BTreeMap::new();
    let mut line = 1u64;
    for (index, definition, piece) in &found {
        starts.insert(*index, line);
        let path = job.options.input.join(&piece.file);
        // The file's settings are counted with its rows, fed again later
        // (L3).
        parts.push(StdinPart::File {
            path: path.clone(),
            range: 0..definition.start,
            counted: false,
        });
        parts.push(StdinPart::File {
            path,
            range: definition.start..definition.end,
            counted: true,
        });
        parts.push(StdinPart::Text(format!("\nSELECT '{DEFINED}{index}';\n")));
        line += definition.lines + 2;
    }
    let label = |index: Option<usize>| {
        index
            .and_then(|index| manifest.tables.get(index))
            .map_or_else(
                || "the tables' definitions".to_string(),
                |table| table.table.clone(),
            )
    };
    let mut command = match job.file_command(&first.file) {
        Ok(command) => command,
        Err(error) => return Some(Err((label(None), said(&error)))),
    };
    command.stdin = None;
    command.script_check = None;
    command.stdin_parts = parts;
    command.args.extend([
        "--unbuffered".to_string(),
        "--skip-column-names".to_string(),
    ]);
    let heard: Arc<Mutex<(BTreeSet<usize>, Said)>> = Arc::default();
    let (marks, sink) = (heard.clone(), on_line.clone());
    let ran = run(
        command,
        move |line| {
            let Ok(mut marks) = marks.lock() else {
                return;
            };
            if let Some(index) = line
                .strip_prefix(DEFINED)
                .and_then(|index| index.trim().parse().ok())
            {
                marks.0.insert(index);
                return;
            }
            marks.1.hear(&line);
            sink(format!("[definitions] {line}"));
        },
        cancel.clone(),
    )
    .await;
    let (marked, last) = heard
        .lock()
        .map(|heard| (heard.0.clone(), heard.1.last()))
        .unwrap_or_default();
    // Only what the client said back: a definition is in once its marker
    // came (F1).
    defined.extend(marked);
    // The table whose definition was running when it stopped.
    let at = found
        .iter()
        .map(|(index, ..)| *index)
        .find(|index| !defined.contains(index));
    let file = at
        .and_then(|index| manifest.tables.get(index))
        .and_then(|table| table.files.first())
        .map(|piece| piece.file.as_str())
        .unwrap_or_default();
    let start = at.and_then(|index| starts.get(&index).copied());
    match ran {
        Ok(status) if status.success() && at.is_none() => None,
        Ok(status) if status.success() => Some(Err((
            label(at),
            "the mysql client ended without saying that this table's definition ran".into(),
        ))),
        Err(Error::Cancelled) => Some(Ok(())),
        Ok(status) => Some(Err((
            label(at),
            last.map_or_else(
                || format!("{status}"),
                |last| in_its_file(&last, start, file),
            ),
        ))),
        // A part that could not be read: the error names its file.
        Err(error) => Some(Err((label(None), said(&error)))),
    }
}

/// Loads a backup folder: every table's definition first, one after the
/// other on one client ([`define`]); then the tables' rows,
/// [`FolderRestore::options`]'s jobs clients at once (the first file of a
/// table without its definition, then its ranges side by side on InnoDB,
/// one after the other otherwise); then the files after the tables, in
/// order, by one client. Every file is checked for client commands before
/// any client starts. A failure stops the others and says which table,
/// where, what is in and what is not, and which tables it left empty or
/// missing, as the target holds them; what was loaded stays, as with a
/// plain SQL file.
pub async fn restore(
    job: FolderRestore,
    on_line: Arc<dyn Fn(String) + Send + Sync>,
    cancel: Arc<AtomicBool>,
) -> FolderEnd {
    let mut said = None;
    FolderEnd {
        outcome: restore_in(&job, &on_line, &cancel, &mut said).await,
        removed: false,
        said,
    }
}

async fn restore_in(
    job: &FolderRestore,
    on_line: &Arc<dyn Fn(String) + Send + Sync>,
    cancel: &Arc<AtomicBool>,
    stopped_said: &mut Option<String>,
) -> Result<()> {
    let manifest = &job.manifest;
    // Options that change what the client prints would hide the markers
    // and the read afterwards (F4).
    if let Some(why) = restore_refusal(&job.options.extra_args) {
        return Err(Error::Config(why));
    }
    let mut checks = Vec::new();
    for file in manifest.files() {
        checks.extend(job.file_command(file)?.script_check.take());
    }
    // Every table's definition first, one after the other on one client
    // (K74, D-143): files that drop and create tables side by side deadlock
    // where a foreign key links them, and a table created while another is
    // locked for its rows fails it (MariaDB).
    let defines = manifest.content != BackupContent::DataOnly;
    // One per table, in the manifest's order (L5).
    let firsts: Vec<Option<PathBuf>> = manifest
        .tables
        .iter()
        .map(|table| {
            table
                .files
                .first()
                .filter(|_| defines)
                .map(|piece| job.options.input.join(&piece.file))
        })
        .collect();
    // All the files before the first client: a refusal loads nothing.
    let checking = cancel.clone();
    let definitions = tokio::task::spawn_blocking(move || {
        checks
            .iter()
            .try_for_each(|check| check_script(check, &checking))?;
        firsts
            .iter()
            .map(|path| match path {
                Some(path) => find_definition(path).map_err(|error| {
                    Error::io(format!("could not read {}", path.display()), error)
                }),
                None => Ok(None),
            })
            .collect::<Result<Vec<_>>>()
    })
    .await
    .map_err(|error| Error::io("checking the backup's files", std::io::Error::other(error)))??;
    let mut defined = BTreeSet::new();
    let mut stopped = if defines && !manifest.tables.is_empty() {
        on_line(format!(
            "Kerno: the {} tables' definitions first, one after the other on one connection.",
            manifest.tables.len()
        ));
        define(job, &definitions, &mut defined, on_line, cancel).await
    } else {
        None
    };
    // A first file whose definition Kerno could not find runs whole, each
    // in a client of its own, one after the other, before any rows: never
    // inside the definitions' run, which a file cut inside a quote would
    // break (F2).
    let mut alone = VecDeque::new();
    for (index, table) in manifest.tables.iter().enumerate() {
        if let (true, None, Some(piece)) = (
            defines,
            definitions.get(index).copied().flatten(),
            table.files.first(),
        ) {
            let mut command = job.file_command(&piece.file)?;
            command.script_check = None;
            alone.push_back(Stage::Side(vec![Job {
                file: piece.file.clone(),
                label: table.table.clone(),
                command,
            }]));
        }
    }
    let wholes = if stopped.is_none() && !alone.is_empty() {
        on_line(format!(
            "Kerno: {} of the tables' first files run whole, each on a connection of its own, \
             one after the other: Kerno could not find where their definition ends.",
            alone.len()
        ));
        let wholes = run_pool(vec![alone], 1, on_line.clone(), cancel, None, false).await;
        if let Some(failure) = &wholes.failure {
            stopped = Some(Err(failure.clone()));
        } else if wholes.cancelled {
            stopped = Some(Ok(()));
        }
        wholes
    } else {
        PoolEnd::default()
    };
    // Each table's rows, side by side: the first file without its
    // definition (one that ran whole above is not run again), then its
    // ranges.
    let mut groups = Vec::new();
    let mut ran_whole = Vec::new();
    for (index, table) in manifest.tables.iter().enumerate() {
        let mut stages = VecDeque::new();
        let mut rest = Vec::new();
        for (at, piece) in table.files.iter().enumerate() {
            let mut command = job.file_command(&piece.file)?;
            command.script_check = None;
            if at == 0 && defines {
                match definitions.get(index).copied().flatten() {
                    Some(definition) if manifest.content != BackupContent::SchemaOnly => {
                        let path = command.stdin.take().unwrap_or_default();
                        command.stdin_parts = vec![
                            StdinPart::File {
                                path: path.clone(),
                                range: 0..definition.start,
                                counted: true,
                            },
                            StdinPart::File {
                                path,
                                range: definition.end..definition.len,
                                counted: true,
                            },
                        ];
                    }
                    // Nothing but the definition, which ran.
                    Some(_) => {
                        ran_whole.push((index, piece.file.clone()));
                        continue;
                    }
                    // Run whole above.
                    None => continue,
                }
            }
            let file = Job {
                file: piece.file.clone(),
                label: match table.files.len() {
                    1 => table.table.clone(),
                    n => format!("{} {}/{n}", table.table, at + 1),
                },
                command,
            };
            if at == 0 {
                stages.push_back(Stage::Side(vec![file]));
            } else if table.concurrent() {
                rest.push(file);
            } else {
                stages.push_back(Stage::Side(vec![file]));
            }
        }
        if !rest.is_empty() {
            stages.push_back(Stage::Side(rest));
        }
        groups.push(stages);
    }
    let mut finishing = VecDeque::new();
    for after in &manifest.after {
        let mut command = job.file_command(&after.file)?;
        command.script_check = None;
        finishing.push_back(Stage::Side(vec![Job {
            file: after.file.clone(),
            label: after.label().into(),
            command,
        }]));
    }
    let (mut tables, finished) = if stopped.is_none() {
        on_line(format!(
            "Kerno: {} tables from {} files, {} mysql clients at once.",
            manifest.tables.len(),
            manifest.files().count(),
            job.jobs()
        ));
        let tables = run_pool(groups, job.jobs(), on_line.clone(), cancel, None, false).await;
        let finished = if tables.failure.is_none() && !tables.cancelled {
            Some(run_pool(vec![finishing], 1, on_line.clone(), cancel, None, false).await)
        } else {
            None
        };
        (tables, finished)
    } else {
        (PoolEnd::default(), None)
    };
    // A file that is only its definition is in once that ran; a file run
    // whole is as its run ended.
    for (index, file) in ran_whole {
        if defined.contains(&index) {
            tables.states.insert(file, FileState::Done);
        }
    }
    tables.states.extend(wholes.states);
    let failure = match &stopped {
        Some(Err(failure)) => Some(failure.clone()),
        _ => tables
            .failure
            .clone()
            .or_else(|| finished.as_ref().and_then(|end| end.failure.clone())),
    };
    let cancelled = matches!(stopped, Some(Ok(())))
        || tables.cancelled
        || finished.as_ref().is_some_and(|end| end.cancelled);
    if failure.is_none() && !cancelled {
        return Ok(());
    }
    // What the definitions left: read from the target once every client
    // ended.
    let dropped = if defines {
        let names: Vec<String> = manifest
            .tables
            .iter()
            .map(|table| table.table.clone())
            .collect();
        match tables_now(job, &names).await {
            Ok(now) => Dropped::Read(now),
            Err(why) => Dropped::Unread(why),
        }
    } else {
        Dropped::No
    };
    let defined: BTreeSet<&str> = defined
        .iter()
        .filter_map(|index| manifest.tables.get(*index))
        .map(|table| table.table.as_str())
        .collect();
    let summary = restore_summary(
        manifest,
        &job.options.database,
        &tables,
        finished.as_ref(),
        &Reached { defined, dropped },
    );
    match failure {
        Some((label, reason)) => Err(Error::Config(format!(
            "mysql failed on {label}: {reason}. {summary}"
        ))),
        None => {
            on_line(format!("Kerno: {summary}"));
            // For the dialog's own words after a cancel (F3).
            *stopped_said = Some(summary);
            Err(Error::Cancelled)
        }
    }
}

/// What a stopped restore reached besides its files: the tables whose
/// definition ran, and what the target holds.
struct Reached<'a> {
    defined: BTreeSet<&'a str>,
    dropped: Dropped,
}

/// What a stopped restore into `database` loaded: the tables in, partly in,
/// not started, and the files after the tables; for a backup that defines
/// its tables, which ones the target now holds empty or not at all (K74).
fn restore_summary(
    manifest: &Manifest,
    database: &str,
    tables: &PoolEnd,
    after: Option<&PoolEnd>,
    reached: &Reached,
) -> String {
    let dropped = &reached.dropped;
    let after_left: Vec<&str> = manifest
        .after
        .iter()
        .filter(|file| {
            !after.is_some_and(|end| end.states.get(&file.file) == Some(&FileState::Done))
        })
        .map(AfterFile::label)
        .collect();
    let after_text = if after_left.is_empty() {
        String::new()
    } else {
        format!(" Not loaded either: the {}.", after_left.join(" and the "))
    };
    if *dropped == Dropped::No {
        return format!(
            "{}{after_text} What was loaded stays.",
            loaded_by_files(manifest, tables)
        );
    }
    let list = capped;
    let (mut whole, mut partly, mut empty, mut missing, mut kept, mut unsure) = (
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    for table in &manifest.tables {
        let name = format!("“{}”", table.table);
        let done = table
            .files
            .iter()
            .all(|piece| tables.states.get(&piece.file) == Some(&FileState::Done));
        let started = reached.defined.contains(table.table.as_str())
            || table
                .files
                .iter()
                .any(|piece| tables.states.contains_key(&piece.file));
        let now = match dropped {
            Dropped::Read(now) => now.get(&table.table).copied(),
            _ => None,
        };
        match now {
            _ if done => whole.push(name),
            Some(Now::Missing) => missing.push(name),
            // Empty is empty, whether or not its marker came back (L2).
            Some(Now::Empty) => empty.push(name),
            // Neither its definition nor its rows ran (the definition that
            // failed may have stopped before its DROP).
            Some(Now::Rows) if !started => kept.push(name),
            Some(Now::Rows) => partly.push(name),
            None => unsure.push(name),
        }
    }
    let mut text = format!(
        "Loaded: {}.",
        if whole.is_empty() {
            "none".to_string()
        } else {
            list(&whole)
        }
    );
    for (names, what) in [
        (&partly, "Partly loaded (some of their rows)"),
        (&empty, "Empty now"),
        (&missing, "Missing now"),
        (&kept, "Not loaded, as they were before"),
    ] {
        if !names.is_empty() {
            text.push_str(&format!(" {what}: {}.", list(names)));
        }
    }
    if let Dropped::Unread(why) = dropped
        && !unsure.is_empty()
    {
        text.push_str(&format!(
            " Not finished: {}; each may now be empty, partly loaded or missing (Kerno could not \
             read “{database}” to tell: {why}).",
            list(&unsure)
        ));
    }
    let drops = manifest.options.add_drop_table;
    if !(empty.is_empty() && missing.is_empty() && unsure.is_empty()) {
        text.push_str(&format!(
            " The restore {} each of the backup's tables in “{database}” first, then loads \
             their rows: a table it did not finish stays empty or missing.",
            if drops {
                "drops and creates"
            } else {
                "creates"
            }
        ));
    }
    text.push_str(&after_text);
    text.push_str(if drops {
        " Run the restore again: it drops each of the backup's tables and loads it from the \
         start."
    } else {
        " The backup has no DROP TABLE statements: drop the tables it created before you run \
         it again."
    });
    text
}

/// `names` in a message: the first ten, then how many more (F1).
fn capped(names: &[String]) -> String {
    const SHOWN: usize = 10;
    if names.len() > SHOWN {
        format!(
            "{} and {} more",
            names[..SHOWN].join(", "),
            names.len() - SHOWN
        )
    } else {
        names.join(", ")
    }
}

/// The tables a stopped restore loaded, partly loaded and did not start,
/// as its files' states say, for a restore that dropped nothing first.
fn loaded_by_files(manifest: &Manifest, tables: &PoolEnd) -> String {
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
            capped(names)
        }
    };
    format!(
        "Loaded: {}. Partly loaded (some rows, or a table stopped midway): {}. Not loaded: {}.",
        list(&whole),
        list(&partly),
        list(&none)
    )
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
                // Large, with a key, but MyISAM: no snapshot, cut only
                // under the server's read lock.
                table("small", "MyISAM", 2 * RANGE_THRESHOLD, Some((1, 10))),
                table("tiny/odd", "InnoDB", RANGE_THRESHOLD, Some((7, 7))),
            ],
            views: vec!["v1".into(), "v2".into()],
            fold_names: false,
            server: (8, 4, 13),
        }
    }

    #[test]
    fn a_name_the_catalog_does_not_hold_is_said_and_case_follows_the_server() {
        let mut options = BackupOptions::new(Engine::MySql, "db");
        options.tables = ["big", "BIG", "gone", "V1"]
            .map(|name| TableRef::new("db", name))
            .to_vec();
        let exact = catalog();
        assert_eq!(
            resolve_names(&exact, &options).unwrap_err(),
            ["BIG", "gone", "V1"].map(String::from)
        );
        let folded = Catalog {
            fold_names: true,
            ..catalog()
        };
        assert_eq!(
            resolve_names(&folded, &options).unwrap_err(),
            ["gone".to_string()]
        );
        options.tables.retain(|table| table.name != "gone");
        let resolved = resolve_names(&folded, &options).unwrap();
        let names: Vec<&str> = resolved.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["big", "big", "v1"]);
    }

    #[test]
    fn the_folder_backup_refuses_the_options_it_sets_itself_in_every_spelling() {
        for (word, said) in [
            ("--where=1", "its own --where"),
            ("--wher=1", "its own --where"),
            ("--w=1", "its own --where"),
            ("-w1", "its own --where"),
            ("--WHERE=x", "its own --where"),
            ("--loose-where=1", "its own --where"),
            ("--tab=/tmp", "text files"),
            ("-T/tmp", "text files"),
            ("--ta=/tmp", "text files"),
            ("--tables", "which tables"),
            ("--no-data", "which tables"),
            ("-d", "which tables"),
            ("--no-create-info", "which tables"),
            ("-t", "which tables"),
            ("--no-c", "which tables"),
            ("--ignore-table=db.t", "which tables"),
            ("--ignore-table-data=db.t", "which tables"),
            ("--databases", "which tables"),
            ("-B", "which tables"),
            ("--all-databases", "which tables"),
            ("-A", "which tables"),
            ("--single-transaction", "how each file is read"),
            ("--single", "how each file is read"),
            ("--lock-all-tables", "how each file is read"),
            ("-x", "how each file is read"),
            ("--lock-tables", "how each file is read"),
            ("-l", "how each file is read"),
            ("--add-locks", "how each file is read"),
            ("--opt", "which the backup's Settings choose"),
            ("--skip-opt", "which the backup's Settings choose"),
            ("--compact", "which the backup's Settings choose"),
            ("--compatible=ansi", "how mysqldump quotes names"),
            ("--skip-set-charset", "which the backup's Settings choose"),
            (
                "--skip-add-drop-table",
                "which the backup's Settings choose",
            ),
            ("--triggers", "a file of their own"),
            ("--routines", "a file of their own"),
            ("-R", "a file of their own"),
            ("--events", "a file of their own"),
            ("-E", "a file of their own"),
            ("--xml", "SQL files"),
            ("-X", "SQL files"),
            ("--set-gtid-purged=ON", "--set-gtid-purged=OFF itself"),
            ("--source-data=2", "seed a replica"),
            ("--master-data", "seed a replica"),
            ("--dump-replica=1", "seed a replica"),
            ("--dump-slave", "seed a replica"),
            ("--apply-replica-statements", "seed a replica"),
            ("--include-master-host-port", "seed a replica"),
            ("--flush-logs", "flush the server's logs"),
            ("-F", "flush the server's logs"),
            ("-vw", "its own --where"),
        ] {
            let refused = folder_refusal(word).unwrap_or_else(|| panic!("{word} kept"));
            assert!(
                refused.starts_with(&format!("“{word}”")) && refused.contains(said),
                "{refused}"
            );
        }
        // Equal to what the folder backup sets, changing nothing it relies
        // on, or taken over by `settled` (G9).
        for word in [
            "--set-gtid-purged=OFF",
            "--skip-lock-tables",
            "--skip-add-locks",
            "--skip-triggers",
            "--max-allowed-packet=64M",
            "--net-buffer-length=1M",
            "--quick",
            "--compress",
            "--column-statistics=0",
            "--hex-blob",
            "--tz-utc",
            "--order-by-primary",
            "-q",
            "-C",
        ] {
            assert_eq!(folder_refusal(word), None, "{word}");
        }
        let mysql57 = "mysqldump  Ver 10.13 Distrib 5.7.44, for osx10.19 (x86_64)";
        let mut job = folder_backup(mysql57, DumpMethod::SingleTransaction);
        let (tables, _) = plan(&catalog(), &job.options, 4, RANGE_THRESHOLD);
        for extra in ["-w1=1", "--wher=1", "--tab=/tmp", "--source-data=2", "-F"] {
            job.options.extra_args = extra.into();
            let refused = job
                .piece_command(&tables[1], &tables[1].files[0])
                .unwrap_err();
            assert!(
                refused
                    .to_string()
                    .contains("Leave it out of the folder backup"),
                "{extra}: {refused}"
            );
        }
        // `--skip-lock-tables` is dropped: the MyISAM table keeps the lock
        // it is read under; `--skip-triggers` turns the triggers off.
        job.options.extra_args =
            "--skip-triggers --skip-lock-tables --set-gtid-purged=OFF '--net-buffer-length=1 M'"
                .into();
        let job = job.settled().unwrap();
        assert!(!job.my().unwrap().triggers);
        let small = tables.iter().find(|table| table.table == "small").unwrap();
        let kept = job.piece_command(small, &small.files[0]).unwrap();
        assert!(
            kept.args.contains(&"--lock-tables".to_string()),
            "{:?}",
            kept.args
        );
        assert!(
            !kept.args.contains(&"--skip-lock-tables".to_string()),
            "{:?}",
            kept.args
        );
        assert!(
            kept.args.ends_with(&[
                "--set-gtid-purged=OFF".to_string(),
                "--net-buffer-length=1 M".to_string(),
                "db".to_string(),
                "small".to_string(),
            ]),
            "{:?}",
            kept.args
        );
    }

    /// A table is cut only where every range reads one moment (G1, G7):
    /// under the server's read lock, any engine; with the table's lock, an
    /// InnoDB table from 5.5.3 on; under "no locks", or before 5.5.3
    /// without the server's lock, none.
    #[test]
    fn a_table_is_cut_only_where_its_ranges_read_one_moment() {
        let cut = |method: DumpMethod, server: (u32, u32, u32)| {
            let mut options = BackupOptions::new(Engine::MySql, "db");
            if let EngineBackup::MySql(my) = &mut options.engine {
                my.method = method;
            }
            let catalog = Catalog {
                server,
                ..catalog()
            };
            let (tables, _) = plan(&catalog, &options, 4, RANGE_THRESHOLD);
            tables
                .iter()
                .filter(|table| table.files.len() > 1)
                .map(|table| table.table.clone())
                .collect::<Vec<_>>()
        };
        let none: Vec<String> = Vec::new();
        assert_eq!(cut(DumpMethod::SingleTransaction, (8, 4, 13)), ["big"]);
        assert_eq!(cut(DumpMethod::SingleTransaction, (5, 5, 3)), ["big"]);
        assert_eq!(cut(DumpMethod::SingleTransaction, (5, 5, 2)), none);
        assert_eq!(cut(DumpMethod::SingleTransaction, (5, 1, 73)), none);
        // The large MyISAM table too, under the server's lock.
        assert_eq!(cut(DumpMethod::LockAllTables, (5, 1, 73)), ["big", "small"]);
        assert_eq!(cut(DumpMethod::LockAllTables, (8, 4, 13)), ["big", "small"]);
        assert_eq!(cut(DumpMethod::NoLocks, (8, 4, 13)), none);
        assert_eq!(
            cut_rule(DumpMethod::SingleTransaction, (11, 8, 9)),
            Cut::InnoDbUnderTableLock
        );
    }

    /// Single files fill the processes a table's ranges wait for (G5).
    #[test]
    fn single_files_start_while_a_table_waits_for_its_processes() {
        let command = ToolCommand {
            program: PathBuf::from("/x/mysqldump"),
            args: Vec::new(),
            env: Vec::new(),
            password: None,
            stdin: None,
            stdin_parts: Vec::new(),
            sent: Arc::default(),
            tunnel: None,
            script_check: None,
            files: Vec::new(),
        };
        let job = |file: &str| Job {
            file: file.into(),
            label: file.into(),
            command: command.clone(),
        };
        let gate = |n: usize| Gate {
            label: "big".into(),
            lock: String::new(),
            table: 0,
            ranges: (0..n).map(|at| job(&format!("r{at}"))).collect(),
            whole: job("whole"),
        };
        let ready = VecDeque::from([
            Ready::Together(0, gate(4)),
            Ready::One(1, job("a")),
            Ready::One(2, job("b")),
        ]);
        // Nothing runs: the table's ranges first.
        assert_eq!(startable(&ready, 0, 4), Some(0));
        // Two run: the ranges wait, a single file takes a free process.
        assert_eq!(startable(&ready, 2, 4), Some(1));
        // Every process busy: nothing.
        assert_eq!(startable(&ready, 4, 4), None);
        // Fewer ranges than processes: they start as soon as they fit.
        let ready = VecDeque::from([Ready::Together(0, gate(2)), Ready::One(1, job("a"))]);
        assert_eq!(startable(&ready, 2, 4), Some(0));
        assert_eq!(startable(&ready, 3, 4), Some(1));
    }

    #[test]
    fn only_mysqldump_s_own_line_says_no_fields() {
        assert!(no_fields_line(
            "-- Skipping dump data for table 't', it has no fields"
        ));
        assert!(!no_fields_line(
            "INSERT INTO notes VALUES ('it has no fields')"
        ));
        assert!(!no_fields_line("ERROR: it has no fields"));
    }

    #[test]
    fn a_backup_refuses_what_would_hide_mysqldump_s_lines() {
        for word in [
            "--log-error=/tmp/x",
            "--log-err=/tmp/x",
            "--LOG_ERROR=/tmp/x",
            "--loose-log-error=/tmp/x",
            "--log=/tmp/x",
            "--skip-verbose",
            "--disable-verbose",
            "--loose-skip-verbose",
            "--skip-verb",
            "--verbose=0",
            "--verbose=FALSE",
            "--verbose=off",
        ] {
            assert!(super::super::silences(word), "{word}");
        }
        for word in [
            "--verbose",
            "-v",
            "--verbose=1",
            "--lock-tables",
            "--log-queries",
        ] {
            assert!(!super::super::silences(word), "{word}");
        }
        let job = folder_backup(
            "mysqldump  Ver 26.10.0 for macos27.0 on arm64 (Homebrew)",
            DumpMethod::SingleTransaction,
        );
        let options = BackupOptions {
            extra_args: "--log-error=/tmp/kerno-k63".into(),
            ..BackupOptions::new(Engine::MySql, "db")
        };
        let refused = backup_command(&job.tool_path, &job.params, &options, Path::new("/b/x.sql"))
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("would hide the lines Kerno reads"),
            "{refused}"
        );
    }

    #[test]
    fn the_tools_talk_utf8mb4_where_the_server_and_the_client_have_it() {
        let m57 = version("mysqldump  Ver 10.13 Distrib 5.7.44, for osx10.19 (x86_64)");
        let m26 = version("mysqldump  Ver 26.10.0 for macos27.0 on arm64 (Homebrew)");
        let m51 = version("mysqldump  Ver 10.13 Distrib 5.1.73, for unknown-linux-gnu (x86_64)");
        let maria = version("mariadb-dump from 11.8.9-MariaDB, client 10.20 for debian-linux-gnu");
        use super::super::utf8mb4;
        assert!(utf8mb4(Some(&m57), (8, 4, 13)) && utf8mb4(Some(&m57), (5, 5, 62)));
        assert!(utf8mb4(Some(&m26), (11, 8, 9)) && utf8mb4(Some(&maria), (5, 7, 44)));
        assert!(!utf8mb4(Some(&m57), (5, 1, 73)) && !utf8mb4(Some(&m57), (5, 5, 2)));
        assert!(!utf8mb4(Some(&m51), (5, 7, 44)) && !utf8mb4(None, (8, 4, 0)));
        let mut job = folder_backup(&m57.text, DumpMethod::SingleTransaction);
        let (tables, _) = plan(&catalog(), &job.options, 4, RANGE_THRESHOLD);
        let charset = "--default-character-set=utf8mb4".to_string();
        assert!(
            !job.piece_command(&tables[1], &tables[1].files[0])
                .unwrap()
                .args
                .contains(&charset)
        );
        job.options.utf8mb4 = true;
        assert!(
            job.piece_command(&tables[1], &tables[1].files[0])
                .unwrap()
                .args
                .contains(&charset)
        );
        let restore = RestoreOptions {
            utf8mb4: true,
            ..RestoreOptions::new(Engine::MySql, "db", "/b/x.sql", BackupFormat::Sql)
        };
        let command = restore_command(Path::new("/x/mysql"), &job.params, &restore).unwrap();
        assert!(command.args.contains(&charset), "{:?}", command.args);
    }

    /// Windows: a backup folder on another computer is never read (K69).
    #[cfg(windows)]
    #[test]
    fn a_folder_of_another_computer_is_never_read() {
        let error = read_manifest(Path::new(r"\\k69-nowhere\share\backup")).unwrap_err();
        assert!(error.contains("another computer"), "{error}");
    }

    /// A pool task that panics is a failure of its file, with its label.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_pool_task_that_panics_fails_its_file() {
        let command = |text: &str| ToolCommand {
            program: PathBuf::from("/bin/echo"),
            args: vec![text.into()],
            env: Vec::new(),
            password: None,
            stdin: None,
            stdin_parts: Vec::new(),
            sent: Arc::default(),
            tunnel: None,
            script_check: None,
            files: Vec::new(),
        };
        let job = |file: &str| Job {
            file: file.into(),
            label: format!("t {file}"),
            command: command(file),
        };
        let groups = vec![
            VecDeque::from([Stage::Side(vec![job("a"), job("boom")])]),
            VecDeque::from([Stage::Side(vec![job("b")]), Stage::Side(vec![job("c")])]),
        ];
        let on_line: OnLine = Arc::new(|line: String| {
            assert!(!line.ends_with("] boom"), "a panic in the task");
        });
        let end = run_pool(groups, 1, on_line, &AtomicBool::new(false), None, true).await;
        let (label, why) = end.failure.expect("a failure");
        assert_eq!(label, "t boom");
        assert!(why.contains("stopped unexpectedly"), "{why}");
        assert_eq!(end.states.get("boom"), Some(&FileState::Failed));
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
        // The file's path is joined with the system's separator.
        let file = format!(
            "--result-file={}",
            job.folder.join("0001-big.part01.sql").display()
        );
        assert_eq!(
            tail,
            [
                "--verbose",
                "--single-transaction",
                "--skip-triggers",
                "--hex-blob",
                file.as_str(),
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
        // A --where of the user's would replace a range's: refused for
        // every file of the folder backup.
        let mut own = folder_backup(mysql57, DumpMethod::SingleTransaction);
        own.options.extra_args = "--where=1".into();
        assert!(own.piece_command(&tables[0], &tables[0].files[0]).is_err());
        assert!(own.piece_command(&tables[1], &tables[1].files[0]).is_err());
        let sample = folder_backup(mysql57, DumpMethod::SingleTransaction)
            .sample_command()
            .unwrap()
            .display();
        let sample_file = format!(
            "'--result-file={}'",
            job.folder.join("<table>.sql").display()
        );
        assert!(sample.contains(&sample_file), "{sample}");
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
        let rows_only = Reached {
            defined: BTreeSet::new(),
            dropped: Dropped::No,
        };
        let text = restore_summary(&manifest, "db", &tables, None, &rows_only);
        assert_eq!(
            text,
            "Loaded: “small”. Partly loaded (some rows, or a table stopped midway): “big”, \
             “big no key”. Not loaded: “tiny/odd”. Not loaded either: the routines, triggers and \
             events and the views. What was loaded stays."
        );
    }

    /// K74: the definition in a table's first file, as mysqldump 26.10
    /// wrote it from MySQL 8.4 and MariaDB 11 and mysqldump 5.7.44 from 5.7
    /// (`tests/fixtures/mysqldump`): a partitioned table, an empty one,
    /// without comments, without DROP TABLE, definitions only, `--replace`,
    /// `--insert-ignore`, a column comment that reads like a statement.
    /// Before it only settings, in it the DROP and CREATE, after it only
    /// rows and settings; a file that is not shaped so runs whole.
    #[test]
    fn a_table_s_definition_is_found_where_mysqldump_writes_it() {
        let folder = crate::testing::fixtures().join("mysqldump");
        let end = "/*!40101 SET character_set_client = @saved_cs_client */;\n";
        for (file, opens, rows) in [
            (
                "84-child.sql",
                "DROP TABLE IF EXISTS `child`;",
                Some("INSERT INTO `child`"),
            ),
            (
                "84-parted.sql",
                "DROP TABLE IF EXISTS `parted`;",
                Some("INSERT INTO"),
            ),
            ("84-child-schema.sql", "DROP TABLE IF EXISTS `child`;", None),
            (
                "84-child-replace.sql",
                "DROP TABLE",
                Some("REPLACE INTO `child`"),
            ),
            (
                "maria-parted-nocomments.sql",
                "DROP TABLE",
                Some("LOCK TABLES `parted`"),
            ),
            (
                "maria-child-ignore.sql",
                "DROP TABLE",
                Some("INSERT IGNORE INTO"),
            ),
            (
                "57-child-nodrop.sql",
                "/*!40101 SET @saved_cs_client",
                Some("INSERT INTO"),
            ),
            (
                "57-child-ignore.sql",
                "DROP TABLE",
                Some("INSERT  IGNORE INTO"),
            ),
            ("57-nothing.sql", "DROP TABLE IF EXISTS `nothing`;", None),
            // A table and a column whose names hold a newline and the end
            // line (F2): mysqldump writes names raw.
            (
                "84-newline-table.sql",
                "DROP TABLE IF EXISTS `t\n",
                Some("INSERT INTO `t\n"),
            ),
            (
                "84-newline-column.sql",
                "DROP TABLE IF EXISTS `plain`;",
                Some("INSERT INTO `plain`"),
            ),
            (
                "maria-newline-table.sql",
                "DROP TABLE IF EXISTS `t\n",
                Some("INSERT INTO `t\n"),
            ),
            (
                "maria-newline-column.sql",
                "DROP TABLE IF EXISTS `plain`;",
                Some("INSERT INTO `plain`"),
            ),
        ] {
            let path = folder.join(file);
            let text = std::fs::read_to_string(&path).unwrap();
            let found = find_definition(&path)
                .unwrap()
                .unwrap_or_else(|| panic!("{file}"));
            let (start, end_at) = (found.start as usize, found.end as usize);
            assert_eq!(found.len as usize, text.len(), "{file}");
            assert_eq!(
                found.lines as usize,
                text[..end_at].lines().count(),
                "{file}"
            );
            let (before, definition, after) =
                (&text[..start], &text[start..end_at], &text[end_at..]);
            assert!(definition.starts_with(opens), "{file}: {definition}");
            assert!(definition.ends_with(end), "{file}: {definition}");
            assert!(definition.contains("\nCREATE TABLE `"), "{file}");
            // The whole CREATE TABLE, its last line included.
            assert!(definition.contains("\n) ENGINE="), "{file}: {definition}");
            for word in ["DROP TABLE", "CREATE TABLE", "INSERT", "LOCK TABLES"] {
                assert!(!before.contains(word), "{file}: {word} before");
            }
            assert!(!after.contains("CREATE TABLE") && !after.contains("DROP TABLE"));
            match rows {
                Some(rows) => assert!(after.contains(rows), "{file}: {after}"),
                None => assert!(!after.contains("INSERT"), "{file}: {after}"),
            }
        }
        // Rows only, rows before a definition, a definition never ended:
        // the file runs whole.
        let scratch = tempfile::tempdir().unwrap();
        for text in [
            "/*!40014 SET FOREIGN_KEY_CHECKS=0 */;\nLOCK TABLES `t` WRITE;\nINSERT INTO `t` VALUES \
             (1);\nUNLOCK TABLES;\n",
            "INSERT INTO `t` VALUES (1);\nDROP TABLE IF EXISTS `t`;\nCREATE TABLE `t` (a int);\n\
             /*!40101 SET character_set_client = @saved_cs_client */;\n",
            "DROP TABLE IF EXISTS `t`;\nCREATE TABLE `t` (a int);\nINSERT INTO `t` VALUES (1);\n",
            "DROP TABLE IF EXISTS `t`;\nCREATE TABLE `t` (a int);\n\
             /*!40101 SET character_set_client = @saved_cs_client */;",
        ] {
            let path = scratch.path().join("t.sql");
            std::fs::write(&path, text).unwrap();
            assert_eq!(find_definition(&path).unwrap(), None, "{text}");
        }
    }

    /// K74: a failure of the definitions' run is said at the line of the
    /// table's own file.
    #[test]
    fn a_line_of_the_definitions_run_is_said_in_its_own_file() {
        let said = "ERROR 1050 (42S01) at line 64: Table 'b' already exists";
        assert_eq!(
            in_its_file(said, Some(42), "0002-b.sql"),
            "ERROR 1050 (42S01) at line 23 of 0002-b.sql: Table 'b' already exists"
        );
        assert_eq!(in_its_file(said, None, "0002-b.sql"), said);
        // A server error quoting several lines is said by its ERROR line,
        // not by the last line it quoted (L1).
        let mut heard = Said::default();
        for line in [
            "ERROR 1064 (42000) at line 3: You have an error near 'x",
            "SELECT 'kerno-defi' at line 1",
            "",
        ] {
            heard.hear(line);
        }
        assert_eq!(
            heard.last().as_deref(),
            Some("ERROR 1064 (42000) at line 3: You have an error near 'x")
        );
        let mut heard = Said::default();
        heard.hear("mysqldump: Got error: 1044");
        assert_eq!(heard.last().as_deref(), Some("mysqldump: Got error: 1044"));
        assert_eq!(
            in_its_file("ERROR 2013: lost", Some(1), "x.sql"),
            "ERROR 2013: lost"
        );
    }

    /// K74, F4: options that change what the mysql client prints are
    /// refused for a folder restore, in every spelling; the others pass.
    #[test]
    fn a_folder_restore_refuses_what_changes_the_client_s_output() {
        for refused in [
            "--table",
            "--tab",
            "--xml",
            "--html",
            "--vertical",
            "--loose-vertical",
            "--auto-vertical-output",
            "--silent",
            "--verbose",
            "--column-type-info",
            "--vert",
            "-t",
            "-X",
            "-H",
            "-E",
            "-s",
            "-vv",
            "-m",
            "-Ns",
            "--force -t",
        ] {
            let why = restore_refusal(refused).unwrap_or_else(|| panic!("{refused} let through"));
            assert!(
                why.contains("changes what the mysql client prints"),
                "{why}"
            );
        }
        for taken in [
            "",
            "--force",
            "--skip-column-names",
            "-N",
            "--show-warnings",
            "--tee=/tmp/x",
            "--default-character-set=utf8mb4",
            "-hlocalhost",
            "-utest",
            "--max-allowed-packet=1G",
            "--version-check",
        ] {
            assert_eq!(restore_refusal(taken), None, "{taken}");
        }
    }

    /// K74: a stopped restore of a backup that defines its tables says
    /// which are in, which hold some of their rows, which are empty or
    /// missing, from what the target holds, and which it never reached;
    /// never "partly loaded" for a table with no row.
    #[test]
    fn a_failed_restore_says_which_tables_it_left_empty_or_missing() {
        let manifest = manifest();
        let all = ["big", "big no key", "small", "tiny/odd"];
        let reached = |defined: &[&'static str], states: [Now; 4]| Reached {
            defined: defined.iter().copied().collect(),
            dropped: Dropped::Read(
                all.iter()
                    .zip(states)
                    .map(|(name, now)| (name.to_string(), now))
                    .collect(),
            ),
        };
        // Every definition ran; then a file failed.
        let mut tables = PoolEnd::default();
        for (file, state) in [
            ("0001-big.part01.sql", FileState::Done),
            ("0001-big.part02.sql", FileState::Stopped),
            ("0002-big_no_key.sql", FileState::Stopped),
            ("0003-small.sql", FileState::Failed),
            ("0004-tiny_odd.sql", FileState::Done),
        ] {
            tables.states.insert(file.into(), state);
        }
        let read = reached(&all, [Now::Rows, Now::Empty, Now::Missing, Now::Rows]);
        let text = restore_summary(&manifest, "db", &tables, None, &read);
        assert_eq!(
            text,
            "Loaded: “tiny/odd”. Partly loaded (some of their rows): “big”. Empty now: “big no \
             key”. Missing now: “small”. The restore drops and creates each of the backup's \
             tables in “db” first, then loads their rows: a table it did not finish stays empty \
             or missing. Not loaded either: the routines, triggers and events and the views. Run \
             the restore again: it drops each of the backup's tables and loads it from the \
             start."
        );
        // The second definition failed after its DROP: the first table is
        // empty, the second missing, the third as it was, rows and all; the
        // fourth is empty, and said so although its definition is not known
        // to have run (L2: a table created whose marker never came back).
        let read = reached(&["big"], [Now::Empty, Now::Missing, Now::Rows, Now::Empty]);
        let text = restore_summary(&manifest, "db", &PoolEnd::default(), None, &read);
        assert!(
            text.starts_with(
                "Loaded: none. Empty now: “big”, “tiny/odd”. Missing now: “big no key”. Not \
                 loaded, as they were before: “small”."
            ),
            "{text}"
        );
        // A long list is cut, with how many more (F1).
        let mut long = manifest.clone();
        let one = long.tables[0].clone();
        long.tables = (0..25)
            .map(|at| TableFiles {
                table: format!("t{at:02}"),
                ..one.clone()
            })
            .collect();
        let none = Reached {
            defined: BTreeSet::new(),
            dropped: Dropped::Read(
                (0..25)
                    .map(|at| (format!("t{at:02}"), Now::Missing))
                    .collect(),
            ),
        };
        let text = restore_summary(&long, "db", &PoolEnd::default(), None, &none);
        assert!(
            text.starts_with(
                "Loaded: none. Missing now: “t00”, “t01”, “t02”, “t03”, “t04”, “t05”, “t06”, \
                 “t07”, “t08”, “t09” and 15 more."
            ),
            "{text}"
        );
        // The target could not be read.
        let unread = Reached {
            defined: all.into_iter().collect(),
            dropped: Dropped::Unread("it did not answer within 30 s".into()),
        };
        let text = restore_summary(&manifest, "db", &tables, None, &unread);
        assert!(
            text.starts_with(
                "Loaded: “tiny/odd”. Not finished: “big”, “big no key”, “small”; each may now be \
                 empty, partly loaded or missing (Kerno could not read “db” to tell: it did not \
                 answer within 30 s)."
            ),
            "{text}"
        );
        // A backup without DROP TABLE says so.
        let mut manifest = manifest;
        manifest.options.add_drop_table = false;
        let text = restore_summary(&manifest, "db", &tables, None, &read);
        assert!(
            text.ends_with(
                "The backup has no DROP TABLE statements: drop the tables it created before you \
                 run it again."
            ),
            "{text}"
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
