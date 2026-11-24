//! The folder backup of K63 against the real servers and client tools:
//! several mysqldump at once into a folder, a table cut into key ranges,
//! restored by several mysql clients into a second scratch database and
//! compared table by table, with its views, routines, triggers and event;
//! a cancel that leaves no tool, no folder and no lock. On MySQL 8.4,
//! MariaDB 11 and the legacy servers that are up (5.1 to 5.7, with a MySQL
//! 5.7 client: the newer ones cannot read 5.1). Every scratch database is
//! `kerno_test_k63_*`, dropped also when the test fails.

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

/// A session on `params`, in `database` when given.
async fn session(params: &ConnectParams, database: Option<&str>) -> Arc<dyn Session> {
    let mut params = params.clone();
    params.database = database.map(str::to_string);
    common::connect(&params)
        .await
        .unwrap_or_else(|error| panic!("connect to port {}: {error}", params.port))
}

/// Drops `databases` once `body` has run, also when it panicked.
async fn with_scratch<F>(params: &ConnectParams, databases: &[&str], body: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let admin = session(params, None).await;
    let drop = |db: &str| format!("DROP DATABASE IF EXISTS {db}");
    for db in databases {
        exec_ok(&admin, &drop(db)).await;
    }
    let outcome = tokio::spawn(body).await;
    // A mysqldump killed mid-run may leave its server thread finishing a
    // statement: the drop waits for it.
    let admin = session(params, None).await;
    for db in databases {
        run(&admin, &drop(db))
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", drop(db)));
    }
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
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

const TABLES: [&str; 5] = ["big", "plain", "pairs", "loose", "empty_one"];

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

/// Backs the source up into a folder on 4 processes, every table with an
/// integer key cut into ranges, then restores it on 4 clients.
async fn round_trip(server: &'static Server, client: PathBuf) {
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
    let files = tempfile::tempdir().unwrap();
    let folder = files.path().join("backup");
    let (on_line, lines) = sink();
    let ended = mysql_folder::back_up(
        FolderBackup {
            tool_path: server.dump.clone(),
            params: server.params.clone(),
            options: folder_options(SOURCE, 4, DumpMethod::SingleTransaction),
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
    assert_eq!(
        pieces,
        [
            ("big", 4),
            ("empty_one", 1),
            ("loose", 1),
            ("pairs", 1),
            ("plain", 4)
        ]
        .map(|(name, n)| (name.to_string(), n)),
        "{}",
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
    assert_eq!(source.len(), TABLES.len() + 2 + 2 + 1 + 1 + 1, "{source:?}");
}

#[tokio::test]
async fn a_folder_backup_restores_identical_on_every_server() {
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
            round_trip(server, client),
        )
        .await;
    }
}

/// The newest client on MySQL 5.1: said before the run, and the run itself
/// fails (each table "has no fields") instead of writing an empty backup.
#[tokio::test]
async fn a_client_that_cannot_read_5_1_is_refused_and_its_empty_backup_fails() {
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
        said.contains("Partly loaded (some rows, or a table stopped midway): “big”"),
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

/// Cancelled while each mysqldump waits on the server (`SLEEP` in its
/// `--where`), with and without Kerno's read lock: every mysqldump is
/// stopped, the folder is removed, and a write then goes through at once.
#[tokio::test]
async fn a_cancelled_backup_leaves_no_tool_no_folder_and_no_lock() {
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
    exec_ok(&admin, &format!("CREATE DATABASE {CANCEL}")).await;
    for table in ["t1", "t2", "t3"] {
        exec_ok(
            &admin,
            &format!("CREATE TABLE {CANCEL}.{table} (id INT PRIMARY KEY) ENGINE=InnoDB"),
        )
        .await;
        exec_ok(&admin, &format!("INSERT INTO {CANCEL}.{table} VALUES (1)")).await;
    }
    for method in [DumpMethod::SingleTransaction, DumpMethod::LockAllTables] {
        let files = tempfile::tempdir().unwrap();
        let folder = files.path().join("backup");
        let cancel = Arc::new(AtomicBool::new(false));
        let (stop, started) = (cancel.clone(), Arc::new(AtomicBool::new(false)));
        let seen = started.clone();
        let on_line: OnLine = Arc::new(move |line: String| {
            eprintln!("  | {line}");
            // A mysqldump has connected: stop the run soon after.
            if line.starts_with("[t") && line.contains("Retrieving") {
                seen.store(true, Ordering::Relaxed);
            }
        });
        let mut options = folder_options(CANCEL, 3, method);
        options.extra_args = "--where=SLEEP(5)=0".into();
        // Kept open past the run, as the app keeps its session until the
        // end: the lock goes with UNLOCK TABLES, not with the connection.
        let kerno = session(&server.params, None).await;
        let running = tokio::spawn(mysql_folder::back_up(
            FolderBackup {
                tool_path: server.dump.clone(),
                params: server.params.clone(),
                options,
                folder: folder.clone(),
                client: server.dump_version.clone(),
                range_threshold: mysql_folder::RANGE_THRESHOLD,
            },
            kerno.clone(),
            on_line,
            cancel,
        ));
        for _ in 0..100 {
            if started.load(Ordering::Relaxed) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(started.load(Ordering::Relaxed), "no mysqldump started");
        assert!(folder.exists());
        #[cfg(unix)]
        assert!(!dumps_running(CANCEL).is_empty());
        stop.store(true, Ordering::Relaxed);
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

/// `can_lock` reads the RELOAD privilege of the account in use.
#[tokio::test]
async fn the_account_s_reload_privilege_is_read() {
    for engine in common::engines() {
        let admin = common::session(engine).await;
        assert!(mysql_folder::can_lock(&admin).await.unwrap(), "{engine:?}");
    }
    for (name, params) in testing::legacy_mysql() {
        let admin = session(&params, None).await;
        assert!(mysql_folder::can_lock(&admin).await.unwrap(), "{name}");
    }
}
