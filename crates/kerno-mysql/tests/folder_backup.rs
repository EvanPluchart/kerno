//! The folder backup of K63 against the real servers and client tools:
//! several mysqldump at once into a folder, a table cut into key ranges,
//! restored by several mysql clients into a second scratch database and
//! compared table by table, with its views, routines, triggers and event;
//! a cancel that leaves no tool, no folder and no lock. On MySQL 8.4,
//! MariaDB 11 and the legacy servers that are up (5.1 to 5.7, with a MySQL
//! 5.7 client: the newer ones cannot read 5.1). K74: a folder restored again
//! into a database that holds its tables. K78: a backup whose triggers,
//! routines or views name their database, restored into another one. Every
//! scratch database is `kerno_test_k63_*`, `kerno_test_k74_*` or
//! `kerno_test_k78_*`, dropped also when the test fails.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{exec_ok, rows, run};
use kerno_core::Session;
use kerno_core::config::{ConnectParams, Engine};
use kerno_core::testing;
use kerno_core::tools::mysql_folder::{self, FolderBackup, FolderRestore};
use kerno_core::tools::{
    self, BackupFormat, BackupOptions, ClientFamily, ClientRoots, DumpMethod, EngineBackup,
    MySqlBackup, RestoreOptions, Tool, ToolVersion,
};

const SOURCE: &str = "kerno_test_k63_src";
const TARGET: &str = "kerno_test_k63_dst";
const CANCEL: &str = "kerno_test_k63_cancel";
// Each test its own databases: they run side by side.
const FAIL_SOURCE: &str = "kerno_test_k63_fail_src";
const FAIL_TARGET: &str = "kerno_test_k63_fail_dst";
const OLD: &str = "kerno_test_k63_old";
const HELD: &str = "kerno_test_k63_held";
const LOST: &str = "kerno_test_k63_lost";
const NAMES: &str = "kerno_test_k63_names";
#[cfg(unix)]
const MOVING: &str = "kerno_test_k63_moving";
const WAIT: &str = "kerno_test_k63_wait";
const SIZES: &str = "kerno_test_k63_sizes";
#[cfg(unix)]
const BUSY: &str = "kerno_test_k63_busy";
const ARGS: &str = "kerno_test_k63_args";
const STUCK: &str = "kerno_test_k63_stuck";
const AGAIN_SOURCE: &str = "kerno_test_k74_src";
const AGAIN_TARGET: &str = "kerno_test_k74_dst";
const NEWLINE_SOURCE: &str = "kerno_test_k74_nl_src";
const NEWLINE_TARGET: &str = "kerno_test_k74_nl_dst";
const NAMED_SOURCE: &str = "kerno_test_k78_src";
const NAMED_TARGET: &str = "kerno_test_k78_dst";

/// One test of this file at a time: a test that holds the server's read
/// lock would make the others' writes wait.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A server to test on, with the mysqldump that can back it up.
struct Server {
    label: String,
    params: ConnectParams,
    dump: PathBuf,
    dump_version: Option<ToolVersion>,
}

/// A MySQL client of 5.7.11 or later and before 8.0 (the one that reads
/// every legacy server and takes Kerno's `--ssl-mode`), as detected.
fn client_57() -> Option<(PathBuf, ToolVersion)> {
    tools::detect_clients(&ClientRoots::system(Vec::new()))
        .into_iter()
        .filter(|install| install.family == ClientFamily::MySql)
        .find_map(|install| {
            let version = install.version.clone()?;
            let fits =
                !version.mariadb && version.number >= (5, 7, 11) && version.number < (8, 0, 0);
            fits.then(|| Some((install.path(Tool::MysqlDump)?.to_path_buf(), version)))?
        })
}

/// The servers that are up, each with its mysqldump; `None` after a
/// `SKIPPED` line when the tools are missing.
fn servers() -> Option<(Vec<Server>, PathBuf)> {
    let (Some(dump), Some(client)) = (tools::find(Tool::MysqlDump), tools::find(Tool::Mysql))
    else {
        eprintln!("SKIPPED: the MySQL client tools are not installed");
        return None;
    };
    let version = tools::version(&dump);
    let mut servers: Vec<Server> = common::engines()
        .into_iter()
        .map(|engine| Server {
            label: engine.label().to_string(),
            params: testing::params(engine),
            dump: dump.clone(),
            dump_version: version.clone(),
        })
        .collect();
    let legacy = testing::legacy_mysql();
    match client_57() {
        Some((old, old_version)) => {
            servers.extend(legacy.into_iter().map(|(name, params)| Server {
                label: name.to_string(),
                params,
                dump: old.clone(),
                dump_version: Some(old_version.clone()),
            }))
        }
        None if !legacy.is_empty() => {
            eprintln!("SKIPPED on the legacy servers: no MySQL 5.7 client (5.7.11 or later)");
        }
        None => {}
    }
    Some((servers, client))
}

/// A session on `params`, in `database` when given; its connection is one
/// the test's guard kills if the test runs past [`LIMIT`].
async fn session(params: &ConnectParams, database: Option<&str>) -> Arc<dyn Session> {
    let mut params = params.clone();
    params.database = database.map(str::to_string);
    let session = common::connect(&params)
        .await
        .unwrap_or_else(|error| panic!("connect to port {}: {error}", params.port));
    if let Some(id) = rows(&session, "SELECT CONNECTION_ID()").await[0][0].clone() {
        OPENED.lock().unwrap().push((params.port, id));
    }
    session
}

/// How long one test body may run on one server. Past it, the guard kills
/// the test's connections (and with them any lock they hold), and the test
/// fails: no lock a test takes can outlive it, whatever the code under test
/// does.
const LIMIT: Duration = Duration::from_secs(120);

/// The connections the running test opened, by port (one test at a time:
/// [`SERIAL`]).
static OPENED: Mutex<Vec<(u16, String)>> = Mutex::new(Vec::new());

/// Kills, from a thread, a runtime and a connection of its own (opened
/// first), every connection the test opened on `params`' server and every
/// one working in one of `databases`, the test's own (the mysqldump and
/// mysql processes connect into them): once told the test's body ended, or
/// after [`LIMIT`] whatever happens. Its answer: whether the time ran out.
fn guard(
    params: &ConnectParams,
    databases: &[&str],
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<bool>) {
    let params = params.clone();
    // Only the test's own databases, by their exact names: another test's
    // or another run's are never touched (K74, L6).
    let named: Vec<String> = databases.iter().map(|db| format!("DB = '{db}'")).collect();
    let (ended, wait) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("the guard's runtime");
        runtime.block_on(async move {
            let killer = common::connect(&params).await;
            let late = matches!(
                wait.recv_timeout(LIMIT),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            );
            let Ok(killer) = killer else {
                eprintln!("GUARD: no connection to kill the test's ones");
                return late;
            };
            let ours: Vec<String> = OPENED
                .lock()
                .unwrap()
                .iter()
                .filter(|(port, _)| *port == params.port)
                .map(|(_, id)| id.clone())
                .collect();
            let listed = format!(
                "SELECT ID FROM information_schema.PROCESSLIST WHERE ID <> CONNECTION_ID() AND \
                 (ID IN ({}){})",
                if ours.is_empty() {
                    "0".into()
                } else {
                    ours.join(", ")
                },
                named
                    .iter()
                    .map(|clause| format!(" OR {clause}"))
                    .collect::<String>()
            );
            let found = run(&killer, &listed)
                .await
                .ok()
                .and_then(|done| done.results.first().map(|result| result.rows()))
                .unwrap_or_default();
            for row in found {
                if let Some(id) = row.first().cloned().flatten() {
                    eprintln!("GUARD: KILL {id}");
                    let _ = run(&killer, &format!("KILL {id}")).await;
                }
            }
            killer.close().await;
            late
        })
    });
    (ended, thread)
}

/// Drops `databases` once `body` has run, also when it panicked or ran past
/// [`LIMIT`] (its connections then killed by [`guard`]).
async fn with_scratch<F>(params: &ConnectParams, databases: &[&str], body: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    OPENED
        .lock()
        .unwrap()
        .retain(|(port, _)| *port != params.port);
    let (ended, guarding) = guard(params, databases);
    let drop = |db: &str| format!("DROP DATABASE IF EXISTS {db}");
    let admin = session(params, None).await;
    for db in databases {
        exec_ok(&admin, &drop(db)).await;
    }
    let mut task = tokio::spawn(body);
    let outcome = tokio::time::timeout(LIMIT + Duration::from_secs(20), &mut task).await;
    if outcome.is_err() {
        task.abort();
    }
    let _ = ended.send(());
    let killed = guarding.join().unwrap_or(true);
    // A mysqldump killed mid-run may leave its server thread finishing a
    // statement: the drop waits for it, 60 s at most.
    let admin = session(params, None).await;
    let _ = run(&admin, "SET SESSION lock_wait_timeout = 60").await;
    for db in databases {
        tokio::time::timeout(Duration::from_secs(90), run(&admin, &drop(db)))
            .await
            .unwrap_or_else(|_| panic!("{}: still waiting after 90 s", drop(db)))
            .unwrap_or_else(|error| panic!("{}: {error}", drop(db)));
    }
    match outcome {
        Err(_) => panic!("the test ran past {LIMIT:?}: its connections were killed"),
        Ok(Err(error)) => std::panic::resume_unwind(error.into_panic()),
        Ok(Ok(())) if killed => panic!("the test ran past {LIMIT:?}: its connections were killed"),
        Ok(Ok(())) => {}
    }
}

/// The source database: tables of each kind, routines, views over a
/// function and over each other, a trigger and an event.
const SCHEMA: [&str; 11] = [
    "CREATE TABLE big (id INT NOT NULL PRIMARY KEY, label VARCHAR(60), raw VARBINARY(16), \
     note VARCHAR(70)) ENGINE=InnoDB",
    "CREATE TABLE plain (id BIGINT UNSIGNED NOT NULL PRIMARY KEY, label VARCHAR(60)) \
     ENGINE=MyISAM",
    "CREATE TABLE pairs (a INT NOT NULL, b INT NOT NULL, label VARCHAR(60), PRIMARY KEY (a, b)) \
     ENGINE=InnoDB",
    "CREATE TABLE loose (label VARCHAR(60)) ENGINE=InnoDB",
    "CREATE TABLE empty_one (id INT NOT NULL PRIMARY KEY) ENGINE=InnoDB",
    "CREATE FUNCTION twice(x INT) RETURNS INT DETERMINISTIC NO SQL RETURN x * 2",
    "CREATE PROCEDURE count_big(OUT n INT) READS SQL DATA SELECT COUNT(*) INTO n FROM big",
    "CREATE VIEW big_twice AS SELECT id, twice(id) AS doubled FROM big",
    "CREATE VIEW big_twice_odd AS SELECT id, doubled FROM big_twice WHERE MOD(id, 2) = 1",
    "CREATE TRIGGER big_note BEFORE INSERT ON big FOR EACH ROW \
     SET NEW.note = CONCAT('t:', NEW.label)",
    "CREATE EVENT nightly ON SCHEDULE EVERY 1 DAY DISABLE DO DELETE FROM loose WHERE label = 'x'",
];

/// Rows: a sparse key below and above zero, quotes, backslashes and binary
/// bytes; a BIGINT UNSIGNED key near its top; a composite key; no key.
fn data() -> Vec<String> {
    let label = |i: i64| format!("'row {i} it''s a \\\\ back'");
    let big: Vec<String> = (0..600)
        .map(|i| format!("({}, {}, X'00FF5C27{:04X}')", i * 7 - 300, label(i), i))
        .collect();
    let plain: Vec<String> = (0..300)
        .map(|i| {
            format!(
                "({}, {})",
                18_446_744_073_709_551_000u64 + i as u64,
                label(i)
            )
        })
        .collect();
    let pairs: Vec<String> = (0..100)
        .map(|i| format!("({}, {}, {})", i / 10, i % 10, label(i)))
        .collect();
    let loose: Vec<String> = (0..50).map(|i| format!("({})", label(i))).collect();
    vec![
        format!("INSERT INTO big (id, label, raw) VALUES {}", big.join(", ")),
        format!("INSERT INTO plain VALUES {}", plain.join(", ")),
        format!("INSERT INTO pairs VALUES {}", pairs.join(", ")),
        format!("INSERT INTO loose VALUES {}", loose.join(", ")),
    ]
}

const TABLES: [&str; 6] = ["big", "plain", "pairs", "loose", "empty_one", "chars"];

/// A table of a 4-byte character (where the server has utf8mb4) and of
/// every byte value.
fn chars(utf8mb4: bool) -> Vec<String> {
    let every: String = (0..=255u8).map(|byte| format!("{byte:02X}")).collect();
    if utf8mb4 {
        vec![
            "CREATE TABLE chars (id INT NOT NULL PRIMARY KEY, label VARCHAR(20) CHARACTER SET \
             utf8mb4, raw VARBINARY(300)) ENGINE=InnoDB"
                .into(),
            format!("INSERT INTO chars VALUES (1, 'a\u{1F600}b\u{00E9}', X'{every}')"),
        ]
    } else {
        vec![
            "CREATE TABLE chars (id INT NOT NULL PRIMARY KEY, label VARCHAR(20), \
             raw VARBINARY(300)) ENGINE=InnoDB"
                .into(),
            format!("INSERT INTO chars VALUES (1, 'ab', X'{every}')"),
        ]
    }
}

/// What must come back: per table its rows and checksum, then the views,
/// routines, triggers and events, the database's name taken out.
async fn contents(admin: &Arc<dyn Session>, db: &str) -> Vec<Vec<Option<String>>> {
    let mut all = Vec::new();
    for table in TABLES {
        let count = rows(admin, &format!("SELECT COUNT(*) FROM {db}.{table}")).await;
        let checksum = rows(admin, &format!("CHECKSUM TABLE {db}.{table}")).await;
        all.push(vec![
            Some(table.to_string()),
            count[0][0].clone(),
            checksum[0][1].clone(),
        ]);
    }
    for sql in [
        format!(
            "SELECT TABLE_NAME, VIEW_DEFINITION FROM information_schema.VIEWS \
             WHERE TABLE_SCHEMA = '{db}' ORDER BY 1"
        ),
        format!(
            "SELECT ROUTINE_NAME, ROUTINE_TYPE, ROUTINE_DEFINITION FROM information_schema.ROUTINES \
             WHERE ROUTINE_SCHEMA = '{db}' ORDER BY 1"
        ),
        format!(
            "SELECT TRIGGER_NAME, EVENT_OBJECT_TABLE, ACTION_TIMING, ACTION_STATEMENT \
             FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = '{db}' ORDER BY 1"
        ),
        format!(
            "SELECT EVENT_NAME, EVENT_DEFINITION, STATUS FROM information_schema.EVENTS \
             WHERE EVENT_SCHEMA = '{db}' ORDER BY 1"
        ),
        format!("SELECT COUNT(*), SUM(doubled) FROM {db}.big_twice_odd"),
        format!("SELECT id, HEX(label), HEX(raw) FROM {db}.chars ORDER BY id"),
    ] {
        all.extend(rows(admin, &sql).await);
    }
    all.into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| cell.map(|text| text.replace(db, "<db>")))
                .collect()
        })
        .collect()
}

/// Where a run writes its lines.
type OnLine = Arc<dyn Fn(String) + Send + Sync>;

/// The lines a run writes, kept and echoed.
fn sink() -> (OnLine, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let kept = lines.clone();
    (
        Arc::new(move |line: String| {
            eprintln!("  | {line}");
            kept.lock().unwrap().push(line);
        }),
        lines,
    )
}

fn folder_options(database: &str, jobs: u32, method: DumpMethod) -> BackupOptions {
    BackupOptions {
        format: BackupFormat::Directory,
        engine: EngineBackup::MySql(MySqlBackup {
            jobs: Some(jobs),
            method,
            events: true,
            ..MySqlBackup::default()
        }),
        ..BackupOptions::new(Engine::MySql, database)
    }
}

/// Backs the source up into a folder on 4 processes with `method`, each
/// table with an integer key that may be cut cut into ranges, then restores
/// it on 4 clients.
async fn round_trip(server: &'static Server, client: PathBuf, method: DumpMethod) {
    let admin = session(&server.params, None).await;
    for db in [SOURCE, TARGET] {
        exec_ok(&admin, &format!("CREATE DATABASE {db}")).await;
    }
    // In the database itself: a trigger written with its database's name
    // keeps it (MariaDB, 5.1), and would land in the source again.
    let source = session(&server.params, Some(SOURCE)).await;
    for sql in SCHEMA.iter().map(|sql| sql.to_string()).chain(data()) {
        exec_ok(&source, &sql).await;
    }
    // A 4-byte character where the server has utf8mb4, and every byte value
    // in a binary column: compared byte for byte (F3).
    let server_version = admin.server().version_number;
    for sql in chars(server_version >= (5, 5, 3)) {
        exec_ok(&source, &sql).await;
    }
    let files = tempfile::tempdir().unwrap();
    let folder = files.path().join("backup");
    let (on_line, lines) = sink();
    let mut options = folder_options(SOURCE, 4, method);
    options.utf8mb4 = tools::utf8mb4(server.dump_version.as_ref(), server_version);
    let ended = mysql_folder::back_up(
        FolderBackup {
            tool_path: server.dump.clone(),
            params: server.params.clone(),
            options,
            folder: folder.clone(),
            client: server.dump_version.clone(),
            // Every table with an integer key and rows is cut.
            range_threshold: 1,
        },
        source.clone(),
        on_line,
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    ended
        .outcome
        .unwrap_or_else(|error| panic!("{}: the backup failed: {error}", server.label));
    let log = lines.lock().unwrap().join("\n");
    assert!(!log.contains(testing::DB_PASSWORD), "{log}");
    let manifest = mysql_folder::read_manifest(&folder).unwrap();
    let pieces: Vec<(String, usize)> = manifest
        .tables
        .iter()
        .map(|table| (table.table.clone(), table.files.len()))
        .collect();
    // Under the server's read lock any engine is cut, the MyISAM table
    // too; otherwise an InnoDB table, from 5.5.3 on (D-135).
    let (big, plain) = match method {
        DumpMethod::LockAllTables => (4, 4),
        _ if server_version >= (5, 5, 3) => (4, 1),
        _ => (1, 1),
    };
    assert_eq!(
        pieces,
        [
            ("big", big),
            ("chars", 1),
            ("empty_one", 1),
            ("loose", 1),
            ("pairs", 1),
            ("plain", plain)
        ]
        .map(|(name, n)| (name.to_string(), n)),
        "{}: {method:?}",
        server.label
    );
    assert_eq!(manifest.after.len(), 2);

    let (on_line, _) = sink();
    let restored = mysql_folder::restore(
        FolderRestore {
            tool_path: client.clone(),
            params: server.params.clone(),
            options: RestoreOptions {
                tool_version: tools::version(&client),
                jobs: Some(4),
                utf8mb4: tools::utf8mb4(tools::version(&client).as_ref(), server_version),
                ..RestoreOptions::new(Engine::MySql, TARGET, &folder, BackupFormat::Directory)
            },
            manifest,
            sent: Arc::default(),
        },
        on_line,
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    restored
        .outcome
        .unwrap_or_else(|error| panic!("{}: the restore failed: {error}", server.label));
    let (source, target) = (
        contents(&admin, SOURCE).await,
        contents(&admin, TARGET).await,
    );
    assert_eq!(target, source, "{}", server.label);
    assert_eq!(
        source.len(),
        TABLES.len() + 2 + 2 + 1 + 1 + 1 + 1,
        "{source:?}"
    );
}

#[tokio::test]
async fn a_folder_backup_restores_identical_on_every_server() {
    let _serial = SERIAL.lock().await;
    let Some((servers, client)) = servers() else {
        return;
    };
    for server in servers {
        eprintln!(
            "--- {} with {:?}",
            server.label,
            server.dump_version.as_ref().map(|v| &v.text)
        );
        let server: &'static Server = Box::leak(Box::new(server));
        let client = client.clone();
        with_scratch(
            &server.params,
            &[SOURCE, TARGET],
            round_trip(server, client, DumpMethod::SingleTransaction),
        )
        .await;
    }
}

/// Under the server's read lock every table is read at one moment, so a
/// large table of any engine is cut, MyISAM included; the restore loads its
/// ranges one after the other, and gives back the same bytes (G1).
#[tokio::test]
async fn under_the_read_lock_a_myisam_table_is_cut_too() {
    let _serial = SERIAL.lock().await;
    let Some((servers, client)) = servers() else {
        return;
    };
    let wanted = ["MySQL", "kerno-mysql51"];
    for server in servers
        .into_iter()
        .filter(|s| wanted.contains(&s.label.as_str()))
    {
        eprintln!("--- {} under the read lock", server.label);
        let server: &'static Server = Box::leak(Box::new(server));
        let client = client.clone();
        with_scratch(
            &server.params,
            &[SOURCE, TARGET],
            round_trip(server, client, DumpMethod::LockAllTables),
        )
        .await;
    }
}

/// The newest client on MySQL 5.1: said before the run, and the run itself
/// fails (each table "has no fields") instead of writing an empty backup.
#[tokio::test]
async fn a_client_that_cannot_read_5_1_is_refused_and_its_empty_backup_fails() {
    let _serial = SERIAL.lock().await;
    let Some(dump) = tools::find(Tool::MysqlDump) else {
        eprintln!("SKIPPED: the MySQL client tools are not installed");
        return;
    };
    let Some((_, params)) = testing::legacy_mysql()
        .into_iter()
        .find(|(name, _)| *name == "kerno-mysql51")
    else {
        return;
    };
    let version = tools::version(&dump);
    let said = mysql_folder::dump_client_problem(version.as_ref(), Engine::MySql, (5, 1, 73), &[]);
    if said.is_none() {
        eprintln!("SKIPPED: {version:?} is not a client known to fail on 5.1");
        return;
    }
    let server: &'static ConnectParams = Box::leak(Box::new(params));
    with_scratch(server, &[OLD], async move {
        let admin = session(server, None).await;
        exec_ok(&admin, &format!("CREATE DATABASE {OLD}")).await;
        let source = session(server, Some(OLD)).await;
        for sql in [&SCHEMA[0], &SCHEMA[3]] {
            exec_ok(&source, sql).await;
        }
        exec_ok(&source, "INSERT INTO loose VALUES ('a'), ('b')").await;
        let files = tempfile::tempdir().unwrap();
        let folder = files.path().join("backup");
        let (on_line, _) = sink();
        let ended = mysql_folder::back_up(
            FolderBackup {
                tool_path: dump,
                params: server.clone(),
                options: folder_options(OLD, 2, DumpMethod::SingleTransaction),
                folder: folder.clone(),
                client: version,
                range_threshold: mysql_folder::RANGE_THRESHOLD,
            },
            source,
            on_line,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        let error = ended.outcome.unwrap_err().to_string();
        assert!(error.contains("found no columns"), "{error}");
        assert!(ended.removed && !folder.exists());
    })
    .await;
}

/// A restore that fails in one file stops, names the table and the
/// server's error, and says what is in and what is not; the files after the
/// tables do not run. With a client that has no sandbox (the scan), a
/// client command in any file refuses the restore before a single file
/// loads.
#[tokio::test]
async fn a_failed_restore_says_what_is_in_and_a_refused_one_loads_nothing() {
    let _serial = SERIAL.lock().await;
    let Some((servers, client)) = servers() else {
        return;
    };
    let Some(server) = servers.into_iter().find(|server| server.label == "MySQL") else {
        return;
    };
    let server: &'static Server = Box::leak(Box::new(server));
    with_scratch(
        &server.params,
        &[FAIL_SOURCE, FAIL_TARGET],
        failed(server, client),
    )
    .await;
}

async fn failed(server: &'static Server, client: PathBuf) {
    let admin = session(&server.params, None).await;
    for db in [FAIL_SOURCE, FAIL_TARGET] {
        exec_ok(&admin, &format!("CREATE DATABASE {db}")).await;
    }
    let source = session(&server.params, Some(FAIL_SOURCE)).await;
    for sql in SCHEMA.iter().map(|sql| sql.to_string()).chain(data()) {
        exec_ok(&source, &sql).await;
    }
    let files = tempfile::tempdir().unwrap();
    let folder = files.path().join("backup");
    let (on_line, _) = sink();
    mysql_folder::back_up(
        FolderBackup {
            tool_path: server.dump.clone(),
            params: server.params.clone(),
            options: folder_options(FAIL_SOURCE, 4, DumpMethod::SingleTransaction),
            folder: folder.clone(),
            client: server.dump_version.clone(),
            range_threshold: 1,
        },
        source,
        on_line,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .outcome
    .unwrap();
    let manifest = mysql_folder::read_manifest(&folder).unwrap();
    let restore = |client: PathBuf, manifest: mysql_folder::Manifest| FolderRestore {
        options: RestoreOptions {
            tool_version: tools::version(&client),
            jobs: Some(2),
            ..RestoreOptions::new(Engine::MySql, FAIL_TARGET, &folder, BackupFormat::Directory)
        },
        tool_path: client,
        params: server.params.clone(),
        manifest,
        sent: Arc::default(),
    };
    let tables_in = |admin: Arc<dyn Session>| async move {
        rows(
            &admin,
            &format!(
                "SELECT TABLE_NAME FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{FAIL_TARGET}' \
                 ORDER BY 1"
            ),
        )
        .await
    };

    // The second range of `big` is broken: it and what runs beside it stop.
    let broken = folder.join(&manifest.tables[0].files[1].file);
    let mut text = std::fs::read_to_string(&broken).unwrap();
    text.push_str("\nTHIS IS NOT SQL;\n");
    std::fs::write(&broken, text).unwrap();
    let (on_line, _) = sink();
    let ended = mysql_folder::restore(
        restore(client.clone(), manifest.clone()),
        on_line,
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    let said = ended.outcome.unwrap_err().to_string();
    assert!(
        said.contains("mysql failed on big 2/4: ERROR 1064") && said.contains("at line"),
        "{said}"
    );
    assert!(
        said.contains("Partly loaded (some of their rows): “big”"),
        "{said}"
    );
    assert!(
        said.contains("Not loaded either: the routines, triggers and events and the views."),
        "{said}"
    );
    let listed = tables_in(admin.clone()).await;
    assert!(
        !listed
            .iter()
            .any(|row| row[0].as_deref() == Some("big_twice")),
        "{listed:?}"
    );

    // A client command, in the last file: refused before any file loads.
    let Some((old, _)) = client_57() else {
        eprintln!("SKIPPED: the scan needs a client without a sandbox (MySQL 5.7)");
        return;
    };
    let old = old.with_file_name("mysql");
    std::fs::write(&broken, "SELECT 1;\n").unwrap();
    exec_ok(&admin, &format!("DROP DATABASE {FAIL_TARGET}")).await;
    exec_ok(&admin, &format!("CREATE DATABASE {FAIL_TARGET}")).await;
    // In the last table's last file: every other table would load first.
    let last_file = manifest
        .tables
        .last()
        .unwrap()
        .files
        .last()
        .unwrap()
        .file
        .clone();
    let last = folder.join(&last_file);
    let mut text = std::fs::read_to_string(&last).unwrap();
    text.push_str("\nsystem echo never\n");
    std::fs::write(&last, text).unwrap();
    let (on_line, _) = sink();
    let ended = mysql_folder::restore(
        restore(old, manifest),
        on_line,
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    let said = ended.outcome.unwrap_err().to_string();
    assert!(
        said.contains("client command") && said.contains(&last_file),
        "{said}"
    );
    assert_eq!(tables_in(admin).await, Vec::<Vec<Option<String>>>::new());
}

/// The mysqldump processes still running with `database` in their
/// arguments, as `ps` lists them.
#[cfg(unix)]
fn dumps_running(database: &str) -> Vec<String> {
    let listed = std::process::Command::new("ps")
        .args(["-A", "-o", "args="])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter(|line| line.contains("mysqldump") && line.contains(database))
        .map(str::to_string)
        .collect()
}

/// `rows` rows of about 200 bytes in `db.big` (InnoDB, an integer key), by
/// doubling, and a one-row table `db.t1` to write to: a mysqldump of `big`
/// takes long enough to be seen running.
async fn fill_big(admin: &Arc<dyn Session>, db: &str, rows: u64) {
    exec_ok(admin, &format!("CREATE DATABASE {db}")).await;
    exec_ok(
        admin,
        &format!(
            "CREATE TABLE {db}.big (id INT NOT NULL PRIMARY KEY, pad CHAR(200) NOT NULL) \
             ENGINE=InnoDB"
        ),
    )
    .await;
    exec_ok(
        admin,
        &format!("INSERT INTO {db}.big VALUES (1, REPEAT('x', 200))"),
    )
    .await;
    let mut have = 1u64;
    while have < rows {
        exec_ok(
            admin,
            &format!(
                "INSERT INTO {db}.big SELECT id + {have}, pad FROM {db}.big \
                 WHERE id + {have} <= {rows}"
            ),
        )
        .await;
        have *= 2;
    }
    exec_ok(
        admin,
        &format!("CREATE TABLE {db}.t1 (id INT PRIMARY KEY) ENGINE=InnoDB"),
    )
    .await;
    exec_ok(admin, &format!("INSERT INTO {db}.t1 VALUES (1)")).await;
}

/// The folder backup of `db` on `server` with `method`, its big table
/// left whole, started in the background.
fn start_backup(
    server: &'static Server,
    db: &str,
    method: DumpMethod,
    folder: &std::path::Path,
    kerno: Arc<dyn Session>,
    on_line: OnLine,
    cancel: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<mysql_folder::FolderEnd> {
    tokio::spawn(mysql_folder::back_up(
        FolderBackup {
            tool_path: server.dump.clone(),
            params: server.params.clone(),
            options: folder_options(db, 3, method),
            folder: folder.to_path_buf(),
            client: server.dump_version.clone(),
            range_threshold: u64::MAX,
        },
        kerno,
        on_line,
        cancel,
    ))
}

/// Waits (30 s at most) for `flag`.
async fn until(flag: &AtomicBool, what: &str) {
    for _ in 0..600 {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{what} did not come");
}

/// Cancelled while mysqldump reads a large table, with and without Kerno's
/// read lock: every mysqldump is stopped, the folder is removed, and a
/// write then goes through at once.
#[tokio::test]
async fn a_cancelled_backup_leaves_no_tool_no_folder_and_no_lock() {
    let _serial = SERIAL.lock().await;
    let Some((servers, _)) = servers() else {
        return;
    };
    let wanted = ["MySQL", "kerno-mysql51"];
    for server in servers
        .into_iter()
        .filter(|s| wanted.contains(&s.label.as_str()))
    {
        let server: &'static Server = Box::leak(Box::new(server));
        with_scratch(&server.params, &[CANCEL], cancelled(server)).await;
    }
}

async fn cancelled(server: &'static Server) {
    eprintln!("--- {}", server.label);
    let admin = session(&server.params, None).await;
    fill_big(&admin, CANCEL, 400_000).await;
    for method in [DumpMethod::SingleTransaction, DumpMethod::LockAllTables] {
        let files = tempfile::tempdir().unwrap();
        let folder = files.path().join("backup");
        let cancel = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let seen = started.clone();
        let on_line: OnLine = Arc::new(move |line: String| {
            eprintln!("  | {line}");
            if line.starts_with("[big] -- Retrieving rows") {
                seen.store(true, Ordering::Relaxed);
            }
        });
        // Kept open past the run, as the app keeps its session until the
        // end: the lock goes with UNLOCK TABLES, not with the connection.
        let kerno = session(&server.params, None).await;
        let running = start_backup(
            server,
            CANCEL,
            method,
            &folder,
            kerno.clone(),
            on_line,
            cancel.clone(),
        );
        until(&started, "the rows of the big table").await;
        assert!(folder.exists());
        #[cfg(unix)]
        assert!(!dumps_running(CANCEL).is_empty());
        cancel.store(true, Ordering::Relaxed);
        let ended = running.await.unwrap();
        assert!(
            matches!(ended.outcome, Err(kerno_core::Error::Cancelled)),
            "{method:?}: {:?}",
            ended.outcome
        );
        assert!(ended.removed && !folder.exists(), "{method:?}");
        #[cfg(unix)]
        assert_eq!(dumps_running(CANCEL), Vec::<String>::new(), "{method:?}");
        // No read lock left: a write goes through at once.
        let writer = session(&server.params, None).await;
        let write = format!(
            "INSERT INTO {CANCEL}.t1 VALUES ({})",
            if method == DumpMethod::LockAllTables {
                11
            } else {
                10
            }
        );
        tokio::time::timeout(Duration::from_secs(10), run(&writer, &write))
            .await
            .unwrap_or_else(|_| panic!("{method:?}: the write waited: a lock is left"))
            .unwrap();
        kerno.close().await;
    }
}

/// A session that keeps every statement it was sent, in order: what the
/// server saw from Kerno's own session. One statement at a time, as a real
/// session; each is also noted when it is asked for (`asked: …`). It can
/// make the catalog's read of `information_schema.TABLES`, or the n-th
/// `SELECT CONNECTION_ID()`, take longer, or leave every check from the
/// n-th on without an answer until it is cancelled, as a stuck statement.
struct Recording {
    inner: Arc<dyn Session>,
    sent: Arc<Mutex<Vec<String>>>,
    one_at_a_time: tokio::sync::Mutex<()>,
    slow_catalog: Option<Duration>,
    slow_check: Option<(usize, Duration)>,
    stuck_from: Option<usize>,
    cancelled: tokio::sync::Notify,
}

impl Recording {
    fn new(inner: Arc<dyn Session>, sent: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            inner,
            sent,
            one_at_a_time: tokio::sync::Mutex::new(()),
            slow_catalog: None,
            slow_check: None,
            stuck_from: None,
            cancelled: tokio::sync::Notify::new(),
        }
    }
}

#[async_trait::async_trait]
impl Session for Recording {
    fn engine(&self) -> Engine {
        self.inner.engine()
    }
    fn dialect(&self) -> &dyn kerno_core::Dialect {
        self.inner.dialect()
    }
    fn server(&self) -> &kerno_core::schema::ServerInfo {
        self.inner.server()
    }
    fn database(&self) -> Option<String> {
        self.inner.database()
    }
    fn tx_state(&self) -> kerno_core::TxState {
        self.inner.tx_state()
    }
    async fn execute(&self, sql: &str, sink: &mut kerno_core::ExecSink) -> kerno_core::Result<()> {
        let checks = {
            let mut sent = self.sent.lock().unwrap();
            sent.push(format!("asked: {sql}"));
            sent.iter()
                .filter(|line| *line == "asked: SELECT CONNECTION_ID()")
                .count()
        };
        let _one = self.one_at_a_time.lock().await;
        self.sent.lock().unwrap().push(sql.to_string());
        let check = sql == "SELECT CONNECTION_ID()";
        let delay = match (self.slow_check, self.stuck_from) {
            // "Never" for the backup's 60 s check: the test's guard ends it
            // anyway.
            (_, Some(n)) if check && checks >= n => Some(Duration::from_secs(300)),
            (Some((n, delay)), _) if check && checks == n => Some(delay),
            _ if sql.contains("FROM information_schema.TABLES") => self.slow_catalog,
            _ => None,
        };
        if let Some(delay) = delay {
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = self.cancelled.notified() => {
                    self.sent.lock().unwrap().push(format!("cancelled: {sql}"));
                    return Err(kerno_core::Error::Cancelled);
                }
            }
        }
        self.inner.execute(sql, sink).await
    }
    async fn cancel(&self) -> kerno_core::Result<()> {
        self.cancelled.notify_waiters();
        self.inner.cancel().await
    }
    async fn ping(&self) -> kerno_core::Result<()> {
        self.inner.ping().await
    }
    async fn close(&self) {
        self.inner.close().await
    }
    async fn databases(&self) -> kerno_core::Result<Vec<kerno_core::schema::DatabaseInfo>> {
        self.inner.databases().await
    }
    async fn schemas(&self) -> kerno_core::Result<Vec<kerno_core::schema::SchemaInfo>> {
        self.inner.schemas().await
    }
    async fn objects(&self, schema: &str) -> kerno_core::Result<kerno_core::schema::SchemaObjects> {
        self.inner.objects(schema).await
    }
    async fn table_structure(
        &self,
        table: &kerno_core::schema::TableRef,
    ) -> kerno_core::Result<kerno_core::schema::TableStructure> {
        self.inner.table_structure(table).await
    }
    async fn ddl(&self, object: &kerno_core::schema::ObjectRef) -> kerno_core::Result<String> {
        self.inner.ddl(object).await
    }
    async fn extensions(&self) -> kerno_core::Result<Vec<kerno_core::schema::ExtensionInfo>> {
        self.inner.extensions().await
    }
    async fn completion_catalog(&self) -> kerno_core::Result<kerno_core::sql::Catalog> {
        self.inner.completion_catalog().await
    }
    fn admin_query(&self, view: kerno_core::AdminView) -> Option<String> {
        self.inner.admin_query(view)
    }
    async fn cancel_session(&self, id: &str) -> kerno_core::Result<()> {
        self.inner.cancel_session(id).await
    }
    async fn terminate_session(&self, id: &str) -> kerno_core::Result<()> {
        self.inner.terminate_session(id).await
    }
}

/// With "lock all tables", the read lock is held while mysqldump runs: a
/// write from another session waits, and lands only once Kerno started to
/// release the lock at the end. The lock comes before the catalog is read
/// (F8), as the server saw Kerno's statements.
#[tokio::test]
async fn the_read_lock_holds_writes_until_the_end() {
    let _serial = SERIAL.lock().await;
    let Some((servers, _)) = servers() else {
        return;
    };
    let wanted = ["MySQL", "kerno-mysql51"];
    for server in servers
        .into_iter()
        .filter(|s| wanted.contains(&s.label.as_str()))
    {
        let server: &'static Server = Box::leak(Box::new(server));
        with_scratch(&server.params, &[HELD], held(server)).await;
    }
}

async fn held(server: &'static Server) {
    eprintln!("--- {}", server.label);
    let admin = session(&server.params, None).await;
    fill_big(&admin, HELD, 400_000).await;
    let files = tempfile::tempdir().unwrap();
    let folder = files.path().join("backup");
    let (started, releasing) = (Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(None)));
    let (seen, released) = (started.clone(), releasing.clone());
    let on_line: OnLine = Arc::new(move |line: String| {
        eprintln!("  | {line}");
        if line.starts_with("[big] -- Retrieving rows") {
            seen.store(true, Ordering::Relaxed);
        }
        // Said just before UNLOCK TABLES is sent: no write may land
        // before this instant.
        if line == "Kerno: releasing the read lock" {
            *released.lock().unwrap() = Some(std::time::Instant::now());
        }
    });
    let sent = Arc::new(Mutex::new(Vec::new()));
    let kerno: Arc<dyn Session> = Arc::new(Recording::new(
        session(&server.params, None).await,
        sent.clone(),
    ));
    let running = start_backup(
        server,
        HELD,
        DumpMethod::LockAllTables,
        &folder,
        kerno.clone(),
        on_line,
        Arc::new(AtomicBool::new(false)),
    );
    until(&started, "the rows of the big table").await;
    let writer = session(&server.params, None).await;
    let writing = tokio::spawn(async move {
        run(&writer, &format!("INSERT INTO {HELD}.t1 VALUES (2)"))
            .await
            .unwrap();
        std::time::Instant::now()
    });
    let ended = running.await.unwrap();
    ended.outcome.unwrap();
    let written = writing.await.unwrap();
    let released = releasing.lock().unwrap().expect("the lock released");
    assert!(
        written >= released,
        "the write landed while the lock was held"
    );
    // What the server saw from Kerno's session: the lock, then the
    // catalog, then the release (F8).
    let sent: Vec<String> = sent
        .lock()
        .unwrap()
        .iter()
        .filter(|sql| !sql.starts_with("asked: "))
        .cloned()
        .collect();
    let at = |what: &str| {
        sent.iter()
            .position(|sql| sql.contains(what))
            .unwrap_or_else(|| panic!("{what} not sent: {sent:?}"))
    };
    let lock = at("FLUSH TABLES WITH READ LOCK");
    assert!(lock < at("information_schema.TABLES"), "{sent:?}");
    assert!(lock < at("@@lower_case_table_names"), "{sent:?}");
    assert_eq!(
        sent.last().map(String::as_str),
        Some("UNLOCK TABLES"),
        "{sent:?}"
    );
    kerno.close().await;
}

/// A slow catalog read, and a check of the read lock's session that does
/// not answer for longer than the old 10 s, are no evidence that the lock
/// was lost: the session is checked only once the catalog is read, and a
/// check that has not answered yet is waited for, not counted as lost
/// (G4). Each made deterministic by a session that holds them.
#[cfg(unix)]
#[tokio::test]
async fn a_slow_session_is_not_a_lost_lock() {
    let _serial = SERIAL.lock().await;
    let Some((servers, _)) = servers() else {
        return;
    };
    let Some(server) = servers.into_iter().find(|server| server.label == "MySQL") else {
        return;
    };
    let server: &'static Server = Box::leak(Box::new(server));
    with_scratch(&server.params, &[BUSY], async move {
        let admin = session(&server.params, None).await;
        fill_big(&admin, BUSY, 1_000).await;
        let files = tempfile::tempdir().unwrap();
        let folder = files.path().join("backup");
        // Each mysqldump starts 7 s late: the run lasts past a few checks.
        let wrapper = files.path().join("mysqldump");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nsleep 7\nexec '{}' \"$@\"\n",
                server.dump.display()
            ),
        )
        .unwrap();
        kerno_core::private::restrict(&wrapper, 0o755).unwrap();
        let sent = Arc::new(Mutex::new(Vec::new()));
        let kerno = Recording {
            // Past the old watcher's 10 s.
            slow_catalog: Some(Duration::from_secs(12)),
            // The first check of the watcher (the second CONNECTION_ID,
            // the first read the lock's id).
            slow_check: Some((2, Duration::from_secs(12))),
            ..Recording::new(session(&server.params, None).await, sent.clone())
        };
        let (on_line, _) = sink();
        let ended = mysql_folder::back_up(
            FolderBackup {
                tool_path: wrapper,
                params: server.params.clone(),
                options: folder_options(BUSY, 2, DumpMethod::LockAllTables),
                folder: folder.clone(),
                client: server.dump_version.clone(),
                range_threshold: mysql_folder::RANGE_THRESHOLD,
            },
            Arc::new(kerno),
            on_line,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        ended.outcome.unwrap();
        assert!(mysql_folder::read_manifest(&folder).is_ok());
        // No check was asked for while the catalog was read.
        let sent = sent.lock().unwrap().clone();
        let catalog_end = sent
            .iter()
            .rposition(|sql| sql.contains("information_schema"))
            .unwrap();
        let checks: Vec<usize> = sent
            .iter()
            .enumerate()
            .filter(|(_, sql)| *sql == "asked: SELECT CONNECTION_ID()")
            .map(|(at, _)| at)
            .collect();
        assert!(checks.len() >= 3, "{sent:?}");
        assert!(checks[1] > catalog_end, "{sent:?}");
    })
    .await;
}

/// The last check of the read lock never answers: the stuck statement is
/// cancelled, the lock released, and the backup fails in words within a
/// bound; the session then closes at once and a write goes through.
#[tokio::test]
async fn a_session_that_never_answers_ends_the_backup() {
    let _serial = SERIAL.lock().await;
    let Some((servers, _)) = servers() else {
        return;
    };
    let Some(server) = servers.into_iter().find(|server| server.label == "MySQL") else {
        return;
    };
    let server: &'static Server = Box::leak(Box::new(server));
    with_scratch(&server.params, &[STUCK], async move {
        let admin = session(&server.params, None).await;
        fill_big(&admin, STUCK, 1_000).await;
        let files = tempfile::tempdir().unwrap();
        let folder = files.path().join("backup");
        let sent = Arc::new(Mutex::new(Vec::new()));
        let kerno: Arc<dyn Session> = Arc::new(Recording {
            // Every check after the one that reads the lock's id.
            stuck_from: Some(2),
            ..Recording::new(session(&server.params, None).await, sent.clone())
        });
        let (on_line, _) = sink();
        let start = std::time::Instant::now();
        let ended = mysql_folder::back_up(
            FolderBackup {
                tool_path: server.dump.clone(),
                params: server.params.clone(),
                options: folder_options(STUCK, 2, DumpMethod::LockAllTables),
                folder: folder.clone(),
                client: server.dump_version.clone(),
                range_threshold: mysql_folder::RANGE_THRESHOLD,
            },
            kerno.clone(),
            on_line,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        let took = start.elapsed();
        let said = ended.outcome.unwrap_err().to_string();
        assert!(
            said.contains("could not confirm that its read lock held"),
            "{said}"
        );
        assert!(ended.removed && !folder.exists());
        // The 60 s check, then 10 s at most for the cancel and 10 s for
        // UNLOCK TABLES.
        assert!(took < Duration::from_secs(85), "{took:?}");
        let sent = sent.lock().unwrap().clone();
        assert!(
            sent.iter().any(|sql| sql.starts_with("cancelled: ")),
            "{sent:?}"
        );
        assert_eq!(
            sent.last().map(String::as_str),
            Some("UNLOCK TABLES"),
            "{sent:?}"
        );
        tokio::time::timeout(Duration::from_secs(10), kerno.close())
            .await
            .expect("the session closes");
        let writer = session(&server.params, None).await;
        tokio::time::timeout(
            Duration::from_secs(10),
            run(&writer, &format!("INSERT INTO {STUCK}.t1 VALUES (2)")),
        )
        .await
        .expect("no lock left")
        .unwrap();
    })
    .await;
}

/// The list of long statements names one sent as a prepared statement
/// (`Execute` in the process list), not only text queries (G8).
#[tokio::test]
async fn a_long_prepared_statement_is_listed() {
    let _serial = SERIAL.lock().await;
    let Some((servers, _)) = servers() else {
        return;
    };
    let Some(server) = servers.into_iter().find(|server| server.label == "MySQL") else {
        return;
    };
    let params = &server.params;
    let opts = mysql_async::OptsBuilder::default()
        .ip_or_hostname(params.host.clone())
        .tcp_port(params.port)
        .user(Some(params.user.clone()))
        .pass(Some(testing::DB_PASSWORD))
        .prefer_socket(false);
    let mut prepared = match mysql_async::Conn::new(opts).await {
        Ok(conn) => conn,
        Err(error) => panic!("a binary-protocol connection: {error}"),
    };
    let sleeping = tokio::spawn(async move {
        use mysql_async::prelude::Queryable;
        let _: Vec<i64> = prepared.exec("SELECT SLEEP(?)", (9,)).await.unwrap();
        prepared.disconnect().await.unwrap();
    });
    tokio::time::sleep(Duration::from_secs(6)).await;
    let kerno = session(params, None).await;
    let long = mysql_folder::long_statements(&kerno, 5).await.unwrap();
    assert!(
        long.iter()
            .any(|(_, seconds, sql)| *seconds >= 5 && sql.contains("SLEEP")),
        "{long:?}"
    );
    sleeping.await.unwrap();
}

/// Arguments that would stall the ranges behind Kerno's table lock, or hide
/// the lines Kerno reads from mysqldump, are refused before anything runs,
/// in plain words (G2): the reviewer's two runs.
#[tokio::test]
async fn arguments_that_stall_or_blind_the_backup_are_refused() {
    let _serial = SERIAL.lock().await;
    let Some((servers, _)) = servers() else {
        return;
    };
    let Some(server) = servers.into_iter().find(|server| server.label == "MySQL") else {
        return;
    };
    let server: &'static Server = Box::leak(Box::new(server));
    with_scratch(&server.params, &[ARGS], async move {
        let admin = session(&server.params, None).await;
        fill_big(&admin, ARGS, 1_000).await;
        let files = tempfile::tempdir().unwrap();
        let log = files.path().join("mysqldump.err");
        for (extra, said) in [
            (
                "--source-data=2",
                "use the plain backup (one file) to seed a replica",
            ),
            ("--flush-logs", "use the plain backup for that"),
            (
                &*format!("--log-error={}", log.display()),
                "would hide the lines Kerno reads",
            ),
            ("--skip-verbose", "would hide the lines Kerno reads"),
        ] {
            let folder = files.path().join("backup");
            let mut options = folder_options(ARGS, 2, DumpMethod::SingleTransaction);
            options.extra_args = extra.into();
            let (on_line, lines) = sink();
            let ended = mysql_folder::back_up(
                FolderBackup {
                    tool_path: server.dump.clone(),
                    params: server.params.clone(),
                    options,
                    folder: folder.clone(),
                    client: server.dump_version.clone(),
                    range_threshold: 1,
                },
                session(&server.params, None).await,
                on_line,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
            let refused = ended.outcome.unwrap_err().to_string();
            assert!(refused.contains(said), "{extra}: {refused}");
            assert!(!folder.exists(), "{extra}");
            assert!(
                lines
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|line| !line.starts_with('[')),
                "{extra}: a mysqldump ran"
            );
            assert!(!log.exists());
        }
        // The plain backup refuses what hides mysqldump's lines too.
        for extra in ["--skip-verbose", "--log-error=/tmp/x"] {
            let options = BackupOptions {
                extra_args: extra.into(),
                ..BackupOptions::new(Engine::MySql, ARGS)
            };
            let refused = tools::backup_command(
                &server.dump,
                &server.params,
                &options,
                &files.path().join("plain.sql"),
            )
            .unwrap_err()
            .to_string();
            assert!(
                refused.contains("would hide the lines Kerno reads"),
                "{refused}"
            );
        }
    })
    .await;
}

/// The read lock lost during the run (Kerno's session closed by the server,
/// as an idle timeout or a proxy would): the backup fails in plain words and
/// its folder is removed.
#[tokio::test]
async fn a_lost_read_lock_fails_the_backup() {
    let _serial = SERIAL.lock().await;
    let Some((servers, _)) = servers() else {
        return;
    };
    let wanted = ["MySQL", "kerno-mysql57"];
    for server in servers
        .into_iter()
        .filter(|s| wanted.contains(&s.label.as_str()))
    {
        let server: &'static Server = Box::leak(Box::new(server));
        with_scratch(&server.params, &[LOST], lost(server)).await;
    }
}

async fn lost(server: &'static Server) {
    eprintln!("--- {}", server.label);
    let admin = session(&server.params, None).await;
    fill_big(&admin, LOST, 400_000).await;
    let files = tempfile::tempdir().unwrap();
    let folder = files.path().join("backup");
    let started = Arc::new(AtomicBool::new(false));
    let seen = started.clone();
    let on_line: OnLine = Arc::new(move |line: String| {
        eprintln!("  | {line}");
        if line.starts_with("[big] -- Retrieving rows") {
            seen.store(true, Ordering::Relaxed);
        }
    });
    let kerno = session(&server.params, None).await;
    let id = rows(&kerno, "SELECT CONNECTION_ID()").await[0][0]
        .clone()
        .unwrap();
    let running = start_backup(
        server,
        LOST,
        DumpMethod::LockAllTables,
        &folder,
        kerno,
        on_line,
        Arc::new(AtomicBool::new(false)),
    );
    until(&started, "the rows of the big table").await;
    exec_ok(&admin, &format!("KILL {id}")).await;
    let ended = running.await.unwrap();
    let said = ended.outcome.unwrap_err().to_string();
    assert!(
        said.contains("The lock that kept writes out was lost at")
            && said.contains("the backup is not consistent"),
        "{said}"
    );
    assert!(ended.removed && !folder.exists());
}

/// Tables named in the settings that the database no longer holds stop
/// the backup before anything runs, each named; a name that differs only
/// by case is missing on a server that compares names with case.
#[tokio::test]
async fn named_tables_that_are_gone_stop_the_backup() {
    let _serial = SERIAL.lock().await;
    let Some((servers, _)) = servers() else {
        return;
    };
    let Some(server) = servers.into_iter().find(|server| server.label == "MySQL") else {
        return;
    };
    let server: &'static Server = Box::leak(Box::new(server));
    with_scratch(&server.params, &[NAMES], async move {
        let admin = session(&server.params, None).await;
        exec_ok(&admin, &format!("CREATE DATABASE {NAMES}")).await;
        for table in ["kept", "Orders"] {
            exec_ok(
                &admin,
                &format!("CREATE TABLE {NAMES}.{table} (id INT PRIMARY KEY)"),
            )
            .await;
        }
        let fold = rows(&admin, "SELECT @@lower_case_table_names").await[0][0].clone();
        // A server on Windows or macOS folds table names: `orders` is then
        // `Orders`, and the test has nothing to say there.
        if fold.as_deref() != Some("0") {
            eprintln!(
                "SKIPPED: this server folds table names (lower_case_table_names = {fold:?}); \
                 the test needs one that compares them with case"
            );
            return;
        }
        let mut options = folder_options(NAMES, 2, DumpMethod::SingleTransaction);
        options.tables = ["kept", "orders", "renamed_away"]
            .map(|name| kerno_core::schema::TableRef::new(NAMES, name))
            .to_vec();
        let files = tempfile::tempdir().unwrap();
        let folder = files.path().join("backup");
        let (on_line, _) = sink();
        let ended = mysql_folder::back_up(
            FolderBackup {
                tool_path: server.dump.clone(),
                params: server.params.clone(),
                options,
                folder: folder.clone(),
                client: server.dump_version.clone(),
                range_threshold: mysql_folder::RANGE_THRESHOLD,
            },
            session(&server.params, None).await,
            on_line,
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        let said = ended.outcome.unwrap_err().to_string();
        assert!(
            said.contains("does not hold: “orders”, “renamed_away”")
                && said.contains("Nothing was run"),
            "{said}"
        );
        assert!(!folder.exists());
    })
    .await;
}

/// A mysqldump that starts a second late when its file is the second range:
/// the way two ranges of one table start apart under load.
#[cfg(unix)]
fn late_second_range(dir: &std::path::Path, real: &std::path::Path) -> PathBuf {
    let path = dir.join("mysqldump");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncase \"$*\" in *part02*) sleep 1;; esac\nexec '{}' \"$@\"\n",
            real.display()
        ),
    )
    .unwrap();
    kerno_core::private::restrict(&path, 0o755).unwrap();
    path
}

/// How a row of `moving` is moved from the first range (`id < 501`) into
/// the second while the backup runs.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum Writer {
    /// An `UPDATE` on its own, sent once the first range has its snapshot.
    Autocommit,
    /// The `UPDATE` in a transaction opened before the backup, committed
    /// once the first range has its snapshot (the reviewer's `opentx`).
    OpenTransaction,
}

/// Inside one table every range reads one moment: a row whose key moves
/// from the first range into the second while the backup runs is restored
/// once, never lost nor doubled. On 5.1, where `LOCK TABLES … READ` does not
/// wait for a transaction that already wrote, a table is cut only under the
/// server's read lock, which holds the COMMIT until the end (G1).
#[cfg(unix)]
#[tokio::test]
async fn every_range_of_a_table_reads_one_moment() {
    let _serial = SERIAL.lock().await;
    let Some((servers, client)) = servers() else {
        return;
    };
    use DumpMethod::{LockAllTables, SingleTransaction};
    use Writer::{Autocommit, OpenTransaction};
    let cases = [
        ("MySQL", SingleTransaction, Autocommit, 2),
        // The table lock waits for the open writer, 10 s, then the table
        // is read whole.
        ("MySQL", SingleTransaction, OpenTransaction, 1),
        ("MySQL", LockAllTables, OpenTransaction, 2),
        ("MariaDB", SingleTransaction, Autocommit, 2),
        // No metadata locks: not cut, the table in one snapshot.
        ("kerno-mysql51", SingleTransaction, OpenTransaction, 1),
        ("kerno-mysql51", SingleTransaction, Autocommit, 1),
        // The server's read lock: cut, the COMMIT waits until the end.
        ("kerno-mysql51", LockAllTables, OpenTransaction, 2),
    ];
    let servers: Vec<&'static Server> = servers
        .into_iter()
        .map(|server| &*Box::leak(Box::new(server)))
        .collect();
    for (label, method, writer, files) in cases {
        let Some(&server) = servers.iter().find(|server| server.label == label) else {
            continue;
        };
        let client = client.clone();
        let target = format!("{MOVING}_dst");
        let target: &'static str = Box::leak(target.into_boxed_str());
        with_scratch(
            &server.params,
            &[MOVING, target],
            moving(server, client, target, method, writer, files),
        )
        .await;
    }
}

#[cfg(unix)]
async fn moving(
    server: &'static Server,
    client: PathBuf,
    target: &'static str,
    method: DumpMethod,
    writer: Writer,
    files: usize,
) {
    eprintln!("--- {} {method:?} {writer:?}", server.label);
    let admin = session(&server.params, None).await;
    exec_ok(&admin, &format!("CREATE DATABASE {MOVING}")).await;
    exec_ok(&admin, &format!("CREATE DATABASE {target}")).await;
    exec_ok(
        &admin,
        &format!("CREATE TABLE {MOVING}.moving (id INT NOT NULL PRIMARY KEY, v INT) ENGINE=InnoDB"),
    )
    .await;
    let values: Vec<String> = (1..=1000).map(|i| format!("({i}, {i})")).collect();
    exec_ok(
        &admin,
        &format!("INSERT INTO {MOVING}.moving VALUES {}", values.join(", ")),
    )
    .await;
    let update = format!("UPDATE {MOVING}.moving SET id = 2000 WHERE id = 5");
    let writing = session(&server.params, None).await;
    if let Writer::OpenTransaction = writer {
        exec_ok(&writing, "SET autocommit = 0").await;
        exec_ok(&writing, &update).await;
    }
    let files_dir = tempfile::tempdir().unwrap();
    let folder = files_dir.path().join("backup");
    let wrapper = late_second_range(files_dir.path(), &server.dump);
    let (started, releasing) = (Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(None)));
    let (seen, released) = (started.clone(), releasing.clone());
    let on_line: OnLine = Arc::new(move |line: String| {
        eprintln!("  | {line}");
        if line.starts_with("[moving") && line.contains("-- Retrieving table structure") {
            seen.store(true, Ordering::Relaxed);
        }
        if line == "Kerno: releasing the read lock" {
            *released.lock().unwrap() = Some(std::time::Instant::now());
        }
    });
    let backing = tokio::spawn(mysql_folder::back_up(
        FolderBackup {
            tool_path: wrapper,
            params: server.params.clone(),
            options: folder_options(MOVING, 2, method),
            folder: folder.clone(),
            client: server.dump_version.clone(),
            range_threshold: 1,
        },
        session(&server.params, None).await,
        on_line,
        Arc::new(AtomicBool::new(false)),
    ));
    until(&started, "the first range's snapshot").await;
    let moved = tokio::spawn(async move {
        match writer {
            Writer::Autocommit => run(&writing, &update).await.unwrap(),
            Writer::OpenTransaction => run(&writing, "COMMIT").await.unwrap(),
        };
        std::time::Instant::now()
    });
    backing.await.unwrap().outcome.unwrap();
    let moved_at = moved.await.unwrap();
    if method == DumpMethod::LockAllTables {
        let released = releasing.lock().unwrap().expect("the lock released");
        assert!(
            moved_at >= released,
            "{}: the write committed while the server's read lock was held",
            server.label
        );
    }
    let manifest = mysql_folder::read_manifest(&folder).unwrap();
    assert_eq!(
        manifest.tables[0].files.len(),
        files,
        "{}: {method:?}",
        server.label
    );
    let (on_line, _) = sink();
    mysql_folder::restore(
        FolderRestore {
            tool_path: client.clone(),
            params: server.params.clone(),
            options: RestoreOptions {
                tool_version: tools::version(&client),
                jobs: Some(2),
                ..RestoreOptions::new(Engine::MySql, target, &folder, BackupFormat::Directory)
            },
            manifest,
            sent: Arc::default(),
        },
        on_line,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .outcome
    .unwrap();
    let restored = rows(
        &admin,
        &format!("SELECT COUNT(*), SUM(id IN (5, 2000)) FROM {target}.moving"),
    )
    .await;
    assert_eq!(
        restored[0],
        [Some("1000".to_string()), Some("1".to_string())],
        "{}: {method:?} {writer:?}: the moved row lost or doubled",
        server.label
    );
}

/// A read lock that does not come (a statement reads a table for longer
/// than the lock may wait) stops the start after `LOCK_WAIT`, in plain
/// words, before any folder is made; the statement is seen in the list of
/// long statements the confirmation names.
#[tokio::test]
async fn a_read_lock_that_does_not_come_stops_the_start() {
    let _serial = SERIAL.lock().await;
    let Some((servers, _)) = servers() else {
        return;
    };
    let Some(server) = servers.into_iter().find(|server| server.label == "MySQL") else {
        return;
    };
    let server: &'static Server = Box::leak(Box::new(server));
    with_scratch(&server.params, &[WAIT], async move {
        let admin = session(&server.params, None).await;
        exec_ok(&admin, &format!("CREATE DATABASE {WAIT}")).await;
        exec_ok(
            &admin,
            &format!("CREATE TABLE {WAIT}.t (id INT PRIMARY KEY)"),
        )
        .await;
        exec_ok(&admin, &format!("INSERT INTO {WAIT}.t VALUES (1)")).await;
        let reader = session(&server.params, None).await;
        let reading = tokio::spawn(async move {
            let _ = run(&reader, &format!("SELECT SLEEP(16) FROM {WAIT}.t")).await;
        });
        tokio::time::sleep(Duration::from_secs(2)).await;
        let kerno = session(&server.params, None).await;
        let long = mysql_folder::long_statements(&kerno, 1).await.unwrap();
        assert!(
            long.iter().any(|(_, _, text)| text.contains("SLEEP(16)")),
            "{long:?}"
        );
        let files = tempfile::tempdir().unwrap();
        let folder = files.path().join("backup");
        let (on_line, _) = sink();
        let since = std::time::Instant::now();
        let ended = start_backup(
            server,
            WAIT,
            DumpMethod::LockAllTables,
            &folder,
            kerno,
            on_line,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
        let said = ended.outcome.unwrap_err().to_string();
        assert!(
            said.contains("The read lock did not come within 10 s"),
            "{said}"
        );
        assert!(
            since.elapsed() < Duration::from_secs(14),
            "{:?}",
            since.elapsed()
        );
        assert!(!folder.exists());
        reading.await.unwrap();
    })
    .await;
}

/// The sizes are read fresh (MySQL 8.0.3 on caches them for a day): a
/// table that grew past the threshold since its size was last read is cut,
/// one just under it is not.
#[tokio::test]
async fn the_sizes_are_read_fresh_and_only_a_table_above_the_threshold_is_cut() {
    let _serial = SERIAL.lock().await;
    if !testing::available(Engine::MySql) {
        return;
    }
    let params = testing::params(Engine::MySql);
    let params: &'static ConnectParams = Box::leak(Box::new(params));
    with_scratch(params, &[SIZES], async move {
        let admin = session(params, None).await;
        exec_ok(&admin, &format!("CREATE DATABASE {SIZES}")).await;
        for (table, rows_wanted) in [("above", 170_000), ("below", 110_000)] {
            exec_ok(
                &admin,
                &format!("CREATE TABLE {SIZES}.{table} LIKE kerno_analytics.big_table"),
            )
            .await;
            // Read once while empty: the server keeps that size.
            rows(
                &admin,
                &format!(
                    "SELECT DATA_LENGTH FROM information_schema.TABLES \
                     WHERE TABLE_SCHEMA = '{SIZES}' AND TABLE_NAME = '{table}'"
                ),
            )
            .await;
            exec_ok(
                &admin,
                &format!(
                    "INSERT INTO {SIZES}.{table} SELECT * FROM kerno_analytics.big_table \
                     WHERE id <= {rows_wanted}"
                ),
            )
            .await;
        }
        // InnoDB's own estimate follows a bulk load within seconds.
        let fresh = session(params, None).await;
        exec_ok(&fresh, "SET SESSION information_schema_stats_expiry = 0").await;
        let size = |table: &'static str| {
            let fresh = fresh.clone();
            async move {
                rows(
                    &fresh,
                    &format!(
                        "SELECT DATA_LENGTH FROM information_schema.TABLES \
                         WHERE TABLE_SCHEMA = '{SIZES}' AND TABLE_NAME = '{table}'"
                    ),
                )
                .await[0][0]
                    .clone()
                    .unwrap()
                    .parse::<u64>()
                    .unwrap()
            }
        };
        for _ in 0..60 {
            if size("above").await > mysql_folder::RANGE_THRESHOLD {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let (above, below) = (size("above").await, size("below").await);
        eprintln!("above {above}, below {below}");
        assert!(above > mysql_folder::RANGE_THRESHOLD && below < mysql_folder::RANGE_THRESHOLD);
        // A session that keeps the default expiry still sees the old size.
        let cached = rows(
            &admin,
            &format!(
                "SELECT DATA_LENGTH FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = '{SIZES}' AND TABLE_NAME = 'above'"
            ),
        )
        .await[0][0]
            .clone()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        eprintln!("cached {cached}");
        let catalog = mysql_folder::read_catalog(
            &session(params, None).await,
            SIZES,
            mysql_folder::RANGE_THRESHOLD,
        )
        .await
        .unwrap();
        let options = folder_options(SIZES, 4, DumpMethod::SingleTransaction);
        let (tables, _) = mysql_folder::plan(&catalog, &options, 4, mysql_folder::RANGE_THRESHOLD);
        let files: Vec<(String, usize)> = tables
            .iter()
            .map(|table| (table.table.clone(), table.files.len()))
            .collect();
        assert_eq!(files, [("above".to_string(), 4), ("below".to_string(), 1)]);
    })
    .await;
}

/// `can_lock` reads the RELOAD privilege of the account in use.
#[tokio::test]
async fn the_account_s_reload_privilege_is_read() {
    let _serial = SERIAL.lock().await;
    for engine in common::engines() {
        let admin = common::session(engine).await;
        assert!(mysql_folder::can_lock(&admin).await.unwrap(), "{engine:?}");
    }
    for (name, params) in testing::legacy_mysql() {
        let admin = session(&params, None).await;
        assert!(mysql_folder::can_lock(&admin).await.unwrap(), "{name}");
    }
}

/// The shape the pass in the real app restored twice (K74): a table of
/// 20,000 rows with a trigger and a view, a parent with a UNIQUE column and
/// a child with a foreign key to it.
const AGAIN_SCHEMA: [&str; 5] = [
    "CREATE TABLE big (id INT NOT NULL PRIMARY KEY, label VARCHAR(60), note VARCHAR(70)) \
     ENGINE=InnoDB",
    "CREATE TABLE small_a (id INT NOT NULL PRIMARY KEY, code VARCHAR(20) NOT NULL UNIQUE) \
     ENGINE=InnoDB",
    "CREATE TABLE small_b (id INT NOT NULL PRIMARY KEY, a_id INT NOT NULL, \
     FOREIGN KEY (a_id) REFERENCES small_a (id)) ENGINE=InnoDB",
    "CREATE VIEW big_view AS SELECT id, label FROM big",
    "CREATE TRIGGER big_note BEFORE INSERT ON big FOR EACH ROW \
     SET NEW.note = CONCAT('t:', NEW.label)",
];

/// A backup folder restored with 8 clients into an empty database, then
/// again and again into that database, now full: every run ends well, with
/// the source's rows, never twice (K74: 1 run in 2 failed in the real app;
/// before the fix, on MySQL 8.4 ERROR 1213 at the child's or the parent's
/// `DROP TABLE` in 5 runs of 21 and 2 of 31, on MariaDB 11 ERROR 1205 at
/// the parent's in 11 of 31, on 5.7 none in 31). Then a file that fails
/// after its table's drop: the failure says which tables are missing or
/// empty, as the target holds them, and the next run loads everything.
/// `KERNO_K74_RUNS` sets the number of runs again (30).
#[tokio::test]
async fn a_folder_restored_again_into_a_full_database_never_deadlocks() {
    let _serial = SERIAL.lock().await;
    let Some((servers, client)) = servers() else {
        return;
    };
    let runs: usize = std::env::var("KERNO_K74_RUNS")
        .ok()
        .and_then(|runs| runs.parse().ok())
        .unwrap_or(30);
    // Every server's failures, said at the end: one server's do not hide
    // the next one's.
    let failures: Arc<Mutex<Vec<String>>> = Arc::default();
    for server in servers {
        if !matches!(server.label.as_str(), "MySQL" | "MariaDB" | "kerno-mysql57") {
            continue;
        }
        let server: &'static Server = Box::leak(Box::new(server));
        with_scratch(
            &server.params,
            &[AGAIN_SOURCE, AGAIN_TARGET],
            again(server, client.clone(), runs, failures.clone()),
        )
        .await;
    }
    let failures = failures.lock().unwrap();
    assert!(failures.is_empty(), "{failures:#?}");
}

async fn again(
    server: &'static Server,
    client: PathBuf,
    runs: usize,
    failures: Arc<Mutex<Vec<String>>>,
) {
    let admin = session(&server.params, None).await;
    for db in [AGAIN_SOURCE, AGAIN_TARGET] {
        exec_ok(&admin, &format!("CREATE DATABASE {db}")).await;
    }
    let source = session(&server.params, Some(AGAIN_SOURCE)).await;
    for sql in AGAIN_SCHEMA {
        exec_ok(&source, sql).await;
    }
    for chunk in 0..20 {
        let values: Vec<String> = (chunk * 1000..(chunk + 1) * 1000)
            .map(|i| format!("({i}, 'row {i}')"))
            .collect();
        let insert = format!("INSERT INTO big (id, label) VALUES {}", values.join(", "));
        exec_ok(&source, &insert).await;
    }
    let small: Vec<String> = (0..300).map(|i| format!("({i}, 'c{i}')")).collect();
    exec_ok(
        &source,
        &format!("INSERT INTO small_a VALUES {}", small.join(", ")),
    )
    .await;
    let small: Vec<String> = (0..300).map(|i| format!("({i}, {})", 299 - i)).collect();
    exec_ok(
        &source,
        &format!("INSERT INTO small_b VALUES {}", small.join(", ")),
    )
    .await;
    let server_version = admin.server().version_number;
    let files = tempfile::tempdir().unwrap();
    let folder = files.path().join("backup");
    let (on_line, _) = sink();
    let mut options = folder_options(AGAIN_SOURCE, 8, DumpMethod::SingleTransaction);
    options.utf8mb4 = tools::utf8mb4(server.dump_version.as_ref(), server_version);
    mysql_folder::back_up(
        FolderBackup {
            tool_path: server.dump.clone(),
            params: server.params.clone(),
            options,
            folder: folder.clone(),
            client: server.dump_version.clone(),
            range_threshold: mysql_folder::RANGE_THRESHOLD,
        },
        source,
        on_line,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .outcome
    .unwrap_or_else(|error| panic!("{}: the backup failed: {error}", server.label));
    let manifest = mysql_folder::read_manifest(&folder).unwrap();
    let counts = |db: &'static str| {
        let admin = admin.clone();
        async move {
            let mut all = Vec::new();
            for table in ["big", "small_a", "small_b"] {
                all.extend(rows(&admin, &format!("SELECT COUNT(*) FROM {db}.{table}")).await);
            }
            let triggers = format!(
                "SELECT COUNT(*) FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = '{db}'"
            );
            all.extend(rows(&admin, &triggers).await);
            all
        }
    };
    let expected = counts(AGAIN_SOURCE).await;
    let restore = || {
        let (on_line, _) = sink();
        mysql_folder::restore(
            FolderRestore {
                tool_path: client.clone(),
                params: server.params.clone(),
                options: RestoreOptions {
                    tool_version: tools::version(&client),
                    jobs: Some(8),
                    utf8mb4: tools::utf8mb4(tools::version(&client).as_ref(), server_version),
                    ..RestoreOptions::new(
                        Engine::MySql,
                        AGAIN_TARGET,
                        &folder,
                        BackupFormat::Directory,
                    )
                },
                manifest: manifest.clone(),
                sent: Arc::default(),
            },
            on_line,
            Arc::new(AtomicBool::new(false)),
        )
    };
    let mut failed = 0;
    for run in 0..=runs {
        if let Err(error) = restore().await.outcome {
            eprintln!("RUN {run} FAILED: {error}");
            failed += 1;
            failures
                .lock()
                .unwrap()
                .push(format!("{}, run {run}: {error}", server.label));
            continue;
        }
        let restored = counts(AGAIN_TARGET).await;
        assert_eq!(restored, expected, "{}: run {run}", server.label);
    }
    eprintln!("{}: {failed} failed in {} restores", server.label, runs + 1);

    // `small_b`'s file fails after its drop, before its CREATE: the message
    // names it missing, and says of every table what the target holds.
    let child = folder.join(&manifest.tables[2].files[0].file);
    let original = std::fs::read_to_string(&child).unwrap();
    let at = original.find("CREATE TABLE").unwrap();
    let broken = format!("{}THIS IS NOT SQL;\n{}", &original[..at], &original[at..]);
    std::fs::write(&child, broken).unwrap();
    let said = restore().await.outcome.unwrap_err().to_string();
    eprintln!("{}: {said}", server.label);
    assert!(
        said.contains("mysql failed on small_b: ERROR 1064"),
        "{said}"
    );
    assert!(
        said.contains("Missing now: ") && said.contains("“small_b”"),
        "{said}"
    );
    assert!(said.contains("Run the restore again"), "{said}");
    let held = |table: &str| {
        let admin = admin.clone();
        let sql = format!(
            "SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{AGAIN_TARGET}' \
             AND TABLE_NAME = '{table}'"
        );
        async move { rows(&admin, &sql).await[0][0].clone() == Some("1".into()) }
    };
    assert!(!held("small_b").await, "{said}");
    // Each other table is where the message puts it.
    for (table, at) in [("big", 0), ("small_a", 1)] {
        let quoted = format!("“{table}”");
        let clause = said
            .split(". ")
            .find(|clause| clause.contains(&quoted))
            .unwrap_or_else(|| panic!("{table} is not said: {said}"))
            .to_string();
        let count = if held(table).await {
            rows(
                &admin,
                &format!("SELECT COUNT(*) FROM {AGAIN_TARGET}.{table}"),
            )
            .await[0][0]
                .clone()
        } else {
            None
        };
        let count = count.as_deref();
        if clause.starts_with("Loaded:") {
            assert_eq!(count, expected[at][0].as_deref(), "{table}: {said}");
        } else if clause.starts_with("Partly loaded") {
            assert!(count.is_some_and(|count| count != "0"), "{table}: {said}");
        } else if clause.starts_with("Empty now") {
            assert_eq!(count, Some("0"), "{table}: {said}");
        } else if clause.starts_with("Missing now") {
            assert_eq!(count, None, "{table}: {said}");
        } else {
            panic!("{table}: {clause} ({said})");
        }
    }
    std::fs::write(&child, original).unwrap();
    restore()
        .await
        .outcome
        .unwrap_or_else(|error| panic!("{}: after the failure: {error}", server.label));
    assert_eq!(counts(AGAIN_TARGET).await, expected, "{}", server.label);

    // A manifest whose first table has no file (edited by hand): the other
    // tables' definitions are still cut where they are (L5). And every
    // byte of the folder is counted once in the progress (L3).
    let mut ghost = manifest.clone();
    ghost.tables.insert(
        0,
        mysql_folder::TableFiles {
            table: "kerno_ghost".into(),
            engine: Some("InnoDB".into()),
            files: Vec::new(),
        },
    );
    let job = FolderRestore {
        tool_path: client.clone(),
        params: server.params.clone(),
        options: RestoreOptions {
            tool_version: tools::version(&client),
            jobs: Some(8),
            utf8mb4: tools::utf8mb4(tools::version(&client).as_ref(), server_version),
            ..RestoreOptions::new(
                Engine::MySql,
                AGAIN_TARGET,
                &folder,
                BackupFormat::Directory,
            )
        },
        manifest: ghost,
        sent: Arc::default(),
    };
    let (total, sent) = (job.total(), job.sent.clone());
    let (on_line, _) = sink();
    mysql_folder::restore(job, on_line, Arc::new(AtomicBool::new(false)))
        .await
        .outcome
        .unwrap_or_else(|error| panic!("{}: a table with no file: {error}", server.label));
    assert_eq!(counts(AGAIN_TARGET).await, expected, "{}", server.label);
    assert_eq!(sent.load(Ordering::Relaxed), total, "{}", server.label);
}

/// K74, F2: a table and a column whose names hold a newline and the line
/// that ends a definition (mysqldump writes names raw), backed up in a
/// folder and restored twice into the same database: both runs end well,
/// every table equal to the source (rows and `CHECKSUM TABLE`). Before the
/// fix the cut fell inside CREATE TABLE: ERROR 1064, and every run again
/// the same.
#[tokio::test]
async fn names_that_hold_a_newline_restore_again_and_again() {
    let _serial = SERIAL.lock().await;
    let Some((servers, client)) = servers() else {
        return;
    };
    for server in servers {
        if !matches!(server.label.as_str(), "MySQL" | "MariaDB") {
            continue;
        }
        let server: &'static Server = Box::leak(Box::new(server));
        with_scratch(
            &server.params,
            &[NEWLINE_SOURCE, NEWLINE_TARGET],
            newline_names(server, client.clone()),
        )
        .await;
    }
}

async fn newline_names(server: &'static Server, client: PathBuf) {
    let admin = session(&server.params, None).await;
    for db in [NEWLINE_SOURCE, NEWLINE_TARGET] {
        exec_ok(&admin, &format!("CREATE DATABASE {db}")).await;
    }
    let end = "/*!40101 SET character_set_client = @saved_cs_client */;";
    let table = format!("t\n{end}\nx");
    let column = format!("c\n{end}\ny");
    let source = session(&server.params, Some(NEWLINE_SOURCE)).await;
    for sql in [
        "CREATE TABLE parent (id INT NOT NULL PRIMARY KEY) ENGINE=InnoDB".to_string(),
        format!(
            "CREATE TABLE `{table}` (id INT NOT NULL PRIMARY KEY, `{column}` INT, \
             note VARCHAR(90) DEFAULT 'q\n{end}', CONSTRAINT fk FOREIGN KEY (`{column}`) \
             REFERENCES parent (id)) \
             ENGINE=InnoDB"
        ),
        format!("CREATE TABLE plain (id INT NOT NULL PRIMARY KEY, `{column}` INT) ENGINE=InnoDB"),
        "INSERT INTO parent VALUES (1), (2)".into(),
        format!("INSERT INTO `{table}` VALUES (1, 2, 'a\nb'), (2, 1, '{end}'), (3, NULL, NULL)"),
        "INSERT INTO plain VALUES (1, 1), (2, 22)".into(),
    ] {
        exec_ok(&source, &sql).await;
    }
    let files = tempfile::tempdir().unwrap();
    let folder = files.path().join("backup");
    let (on_line, _) = sink();
    let server_version = admin.server().version_number;
    let mut options = folder_options(NEWLINE_SOURCE, 4, DumpMethod::SingleTransaction);
    options.utf8mb4 = tools::utf8mb4(server.dump_version.as_ref(), server_version);
    mysql_folder::back_up(
        FolderBackup {
            tool_path: server.dump.clone(),
            params: server.params.clone(),
            options,
            folder: folder.clone(),
            client: server.dump_version.clone(),
            range_threshold: mysql_folder::RANGE_THRESHOLD,
        },
        source,
        on_line,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .outcome
    .unwrap_or_else(|error| panic!("{}: the backup failed: {error}", server.label));
    let manifest = mysql_folder::read_manifest(&folder).unwrap();
    let state = |db: &'static str| {
        let (admin, table) = (admin.clone(), table.clone());
        async move {
            let mut all = Vec::new();
            for name in ["parent", table.as_str(), "plain"] {
                let quoted = format!("`{db}`.`{name}`");
                all.extend(rows(&admin, &format!("SELECT COUNT(*) FROM {quoted}")).await);
                let sum = rows(&admin, &format!("CHECKSUM TABLE {quoted}")).await;
                all.push(vec![sum[0][1].clone()]);
            }
            all
        }
    };
    let expected = state(NEWLINE_SOURCE).await;
    for run in 0..3 {
        // The third run: the parent's file without its definition's end
        // line, which runs whole, in a client of its own (F2).
        if run == 2 {
            let parent = manifest
                .tables
                .iter()
                .find(|table| table.table == "parent")
                .unwrap();
            let path = folder.join(&parent.files[0].file);
            let text = std::fs::read_to_string(&path).unwrap();
            let cut = text.replacen(&format!("{end}\n"), "", 1);
            assert_ne!(cut, text);
            std::fs::write(&path, cut).unwrap();
            // A row the restore must replace.
            exec_ok(
                &admin,
                &format!("INSERT INTO {NEWLINE_TARGET}.parent VALUES (3)"),
            )
            .await;
        }
        let (on_line, lines) = sink();
        mysql_folder::restore(
            FolderRestore {
                tool_path: client.clone(),
                params: server.params.clone(),
                options: RestoreOptions {
                    tool_version: tools::version(&client),
                    jobs: Some(4),
                    utf8mb4: tools::utf8mb4(tools::version(&client).as_ref(), server_version),
                    ..RestoreOptions::new(
                        Engine::MySql,
                        NEWLINE_TARGET,
                        &folder,
                        BackupFormat::Directory,
                    )
                },
                manifest: manifest.clone(),
                sent: Arc::default(),
            },
            on_line,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .outcome
        .unwrap_or_else(|error| panic!("{}: run {run}: {error}", server.label));
        assert_eq!(
            state(NEWLINE_TARGET).await,
            expected,
            "{}: run {run}",
            server.label
        );
        let alone = lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.starts_with("Kerno: 1 of the tables' first files run whole"));
        assert_eq!(alone, run == 2, "{}: run {run}", server.label);
    }
}

/// K78: the pass's shape, written from another database's tab with the
/// source's name: a trigger whose header names it (MariaDB keeps it as
/// typed), one whose body writes into it, a procedure and an event whose
/// bodies name it, a function that names it only in a string and a
/// comment, a view. Restored into another database: by default nothing is
/// created or changed in the source (its triggers, routines, events and
/// views compared before and after), the tables are loaded, the left-out
/// objects named; with "load them as written", what the statements say.
#[tokio::test]
async fn a_backup_that_names_its_database_restores_nothing_into_it_by_default() {
    let _serial = SERIAL.lock().await;
    let Some((servers, client)) = servers() else {
        return;
    };
    for server in servers {
        if !matches!(server.label.as_str(), "MySQL" | "MariaDB") {
            continue;
        }
        let server: &'static Server = Box::leak(Box::new(server));
        with_scratch(
            &server.params,
            &[NAMED_SOURCE, NAMED_TARGET],
            named_source(server, client.clone()),
        )
        .await;
    }
}

/// The objects of `db` that are not tables, with their definitions.
async fn objects_of(admin: &Arc<dyn Session>, db: &str) -> Vec<Vec<Option<String>>> {
    let mut all = Vec::new();
    for sql in [
        format!(
            "SELECT TRIGGER_NAME, ACTION_STATEMENT FROM information_schema.TRIGGERS \
             WHERE TRIGGER_SCHEMA = '{db}' ORDER BY 1"
        ),
        format!(
            "SELECT ROUTINE_NAME, ROUTINE_DEFINITION FROM information_schema.ROUTINES \
             WHERE ROUTINE_SCHEMA = '{db}' ORDER BY 1"
        ),
        format!(
            "SELECT EVENT_NAME, EVENT_DEFINITION FROM information_schema.EVENTS \
             WHERE EVENT_SCHEMA = '{db}' ORDER BY 1"
        ),
        format!(
            "SELECT TABLE_NAME, VIEW_DEFINITION FROM information_schema.VIEWS \
             WHERE TABLE_SCHEMA = '{db}' ORDER BY 1"
        ),
    ] {
        all.extend(rows(admin, &sql).await);
    }
    all
}

async fn named_source(server: &'static Server, client: PathBuf) {
    let (src, dst) = (NAMED_SOURCE, NAMED_TARGET);
    // Kerno's own session, in no database: names written out, as from a
    // tab on another database.
    let admin = session(&server.params, None).await;
    for db in [src, dst] {
        exec_ok(&admin, &format!("CREATE DATABASE {db}")).await;
    }
    for sql in [
        format!("CREATE TABLE {src}.big (id INT NOT NULL PRIMARY KEY, n INT) ENGINE=InnoDB"),
        format!(
            "CREATE TABLE {src}.audit (id INT NOT NULL AUTO_INCREMENT PRIMARY KEY, \
             note VARCHAR(200)) ENGINE=InnoDB"
        ),
        format!(
            "create trigger {src}.big_bi before insert on {src}.big for each row \
             set new.n = new.id * 2"
        ),
        format!(
            "CREATE TRIGGER {src}.big_ai AFTER INSERT ON {src}.big FOR EACH ROW \
             INSERT INTO {src}.audit (note) VALUES (CONCAT('{src}.big ', NEW.id))"
        ),
        // Typed under ANSI_QUOTES: MariaDB keeps it as typed (M1).
        "SET SESSION sql_mode = 'ANSI_QUOTES'".to_string(),
        format!(
            "create trigger \"{src}\".\"big_bu\" after update on \"{src}\".\"big\" for each row \
             insert into \"{src}\".\"audit\"(note) values ('ansi')"
        ),
        "SET SESSION sql_mode = DEFAULT".to_string(),
        // Enabled: it would run against the source on its schedule (M6).
        format!(
            "CREATE EVENT {src}.hourly ON SCHEDULE EVERY 1 HOUR ENABLE \
             DO DELETE FROM {src}.audit WHERE id < 0"
        ),
        format!(
            "CREATE PROCEDURE {src}.count_big(OUT c INT) READS SQL DATA \
             SELECT COUNT(*) INTO c FROM {src}.big"
        ),
        format!(
            "CREATE FUNCTION {src}.quiet() RETURNS VARCHAR(100) DETERMINISTIC NO SQL \
             RETURN '{src}.big is not a hit' /* {src}.big */"
        ),
        format!(
            "CREATE EVENT {src}.nightly ON SCHEDULE EVERY 1 DAY DISABLE \
             DO DELETE FROM {src}.audit WHERE id < 0"
        ),
        format!("CREATE VIEW {src}.big_v AS SELECT id, n FROM {src}.big"),
        format!("INSERT INTO {src}.big (id) VALUES (1), (2), (3)"),
    ] {
        exec_ok(&admin, &sql).await;
    }
    let server_version = admin.server().version_number;
    let files = tempfile::tempdir().unwrap();
    let folder = files.path().join("backup");
    let (on_line, _) = sink();
    let mut options = folder_options(src, 2, DumpMethod::SingleTransaction);
    options.utf8mb4 = tools::utf8mb4(server.dump_version.as_ref(), server_version);
    mysql_folder::back_up(
        FolderBackup {
            tool_path: server.dump.clone(),
            params: server.params.clone(),
            options,
            folder: folder.clone(),
            client: server.dump_version.clone(),
            range_threshold: mysql_folder::RANGE_THRESHOLD,
        },
        admin.clone(),
        on_line,
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .outcome
    .unwrap_or_else(|error| panic!("{}: the backup failed: {error}", server.label));
    let manifest = mysql_folder::read_manifest(&folder).unwrap();
    // The source without its triggers: a restore that creates one there is
    // seen, not hidden by "already exists" (the pass's ERROR 1359).
    for trigger in ["big_bi", "big_ai", "big_bu"] {
        exec_ok(&admin, &format!("DROP TRIGGER {src}.{trigger}")).await;
    }
    let before = objects_of(&admin, src).await;
    let count = |db: &'static str, table: &'static str| {
        let admin = admin.clone();
        async move { rows(&admin, &format!("SELECT COUNT(*) FROM {db}.{table}")).await }
    };
    let (big, audit) = (count(src, "big").await, count(src, "audit").await);

    let restore = |load_as_written: bool| {
        let (on_line, lines) = sink();
        let job = FolderRestore {
            tool_path: client.clone(),
            params: server.params.clone(),
            options: RestoreOptions {
                tool_version: tools::version(&client),
                jobs: Some(2),
                utf8mb4: tools::utf8mb4(tools::version(&client).as_ref(), server_version),
                load_as_written,
                ..RestoreOptions::new(Engine::MySql, dst, &folder, BackupFormat::Directory)
            },
            manifest: manifest.clone(),
            sent: Arc::default(),
        };
        let (total, sent) = (job.total(), job.sent.clone());
        async move {
            let end = mysql_folder::restore(job, on_line, Arc::new(AtomicBool::new(false))).await;
            // Every byte counted, the left-out ones too (K74, L3).
            assert_eq!(sent.load(Ordering::Relaxed), total);
            let log = lines.lock().unwrap().join("\n");
            (end, log)
        }
    };
    let names = |db: &'static str, kind: &'static str| {
        let admin = admin.clone();
        let sql = match kind {
            "trigger" => format!(
                "SELECT TRIGGER_NAME FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = \
                 '{db}' ORDER BY 1"
            ),
            "event" => format!(
                "SELECT EVENT_NAME FROM information_schema.EVENTS WHERE EVENT_SCHEMA = '{db}' \
                 ORDER BY 1"
            ),
            "view" => format!(
                "SELECT TABLE_NAME FROM information_schema.VIEWS WHERE TABLE_SCHEMA = '{db}' \
                 ORDER BY 1"
            ),
            _ => format!(
                "SELECT ROUTINE_NAME FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = \
                 '{db}' ORDER BY 1"
            ),
        };
        async move {
            rows(&admin, &sql)
                .await
                .into_iter()
                .filter_map(|row| row[0].clone())
                .collect::<Vec<_>>()
        }
    };
    let mariadb = server.label == "MariaDB";
    let (end, log) = restore(false).await;
    end.outcome
        .unwrap_or_else(|error| panic!("{}: the restore failed: {error}", server.label));
    let said = end.said.unwrap_or_default();
    eprintln!("{}: {said}", server.label);
    assert_eq!(
        objects_of(&admin, src).await,
        before,
        "{}: the source changed: {log}",
        server.label
    );
    assert_eq!(count(dst, "big").await, big, "{}", server.label);
    assert_eq!(count(dst, "audit").await, audit, "{}", server.label);
    // The routines, the view and the disabled event are in the target; the
    // enabled event is left out (M6); so are the triggers MariaDB kept
    // written for the source, the ANSI_QUOTES one too (M1).
    assert_eq!(
        names(dst, "routine").await,
        ["count_big", "quiet"],
        "{}",
        server.label
    );
    assert_eq!(names(dst, "view").await, ["big_v"], "{}", server.label);
    assert_eq!(names(dst, "event").await, ["nightly"], "{}", server.label);
    if mariadb {
        assert!(names(dst, "trigger").await.is_empty(), "{log}");
        assert!(
            said.starts_with(
                "Left out: 3 triggers (“big_bi”, “big_ai”, “big_bu”), written for the database \
                 “kerno_test_k78_src”"
            ),
            "{said}"
        );
    } else {
        // MySQL rewrites a trigger's header: only bodies name the source,
        // and they are loaded.
        assert_eq!(names(dst, "trigger").await, ["big_ai", "big_bi", "big_bu"]);
        assert!(!said.contains("triggers ("), "{said}");
    }
    assert!(
        said.contains(
            "Left out: 1 event that is enabled (“hourly”), whose body names “kerno_test_k78_src”"
        ),
        "{said}"
    );
    assert!(
        said.contains("read or write “kerno_test_k78_src” by name"),
        "{said}"
    );
    assert!(!said.contains("“quiet”"), "{said}");

    // Loaded as written: what the statements say happens, the triggers
    // created in the source, the enabled event in the target.
    let (end, log) = restore(true).await;
    end.outcome
        .unwrap_or_else(|error| panic!("{}: as written: {error}: {log}", server.label));
    assert_eq!(
        names(dst, "event").await,
        ["hourly", "nightly"],
        "{}",
        server.label
    );
    if mariadb {
        assert_eq!(names(src, "trigger").await, ["big_ai", "big_bi", "big_bu"]);
        assert!(names(dst, "trigger").await.is_empty());
        assert!(
            end.said
                .unwrap_or_default()
                .starts_with("Loaded as written, into “kerno_test_k78_src”: 3 triggers")
        );
    } else {
        assert_eq!(objects_of(&admin, src).await, before, "{}", server.label);
    }
}
