//! Backup and restore through the real MySQL / MariaDB client tools:
//! `mysqldump` of the `kerno_other` test database, restored with `mysql` into
//! a scratch database `kerno_test_restore` that the test creates and drops.
//! Skipped with a message when the tools are not installed (checked before
//! the containers), or when a container is down.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use common::{exec_ok, rows, run};
use kerno_core::config::Engine;
use kerno_core::testing;
use kerno_core::tools::{
    self, BackupContent, BackupFormat, BackupOptions, RestoreOptions, ScriptCheck, Tool,
    ToolCommand,
};

const SCRATCH: &str = "kerno_test_restore";

/// Runs `command`, prints and returns its output, and asserts it succeeded.
async fn run_ok(command: ToolCommand) -> Vec<String> {
    let shown = command.display();
    eprintln!("$ {shown}");
    assert!(!shown.contains(testing::DB_PASSWORD), "{shown}");
    assert!(!format!("{command:?}").contains(testing::DB_PASSWORD));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    let status = tools::run(
        command,
        move |line| {
            eprintln!("  | {line}");
            sink.lock().unwrap().push(line);
        },
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap_or_else(|error| panic!("{shown}: {error}"));
    let lines = lines.lock().unwrap().clone();
    assert!(status.success(), "{shown}: {status}\n{lines:#?}");
    lines
}

#[tokio::test]
async fn mysqldump_then_mysql_copy_a_database() {
    let (Some(dump), Some(client)) = (tools::find(Tool::MysqlDump), tools::find(Tool::Mysql))
    else {
        eprintln!(
            "SKIPPED: the MySQL client tools are not installed (looked for {:?} and {:?} in $PATH \
             and {:?})",
            Tool::MysqlDump.names(),
            Tool::Mysql.names(),
            tools::FALLBACK_DIRS
        );
        return;
    };
    for engine in common::engines() {
        round_trip(engine, &dump, &client).await;
    }
}

async fn round_trip(engine: Engine, dump: &Path, client: &Path) {
    let session = common::session(engine).await;
    let drop = format!("DROP DATABASE IF EXISTS {SCRATCH}");
    exec_ok(&session, &drop).await;
    exec_ok(&session, &format!("CREATE DATABASE {SCRATCH}")).await;
    let outcome = tokio::spawn(copy(engine, dump.to_path_buf(), client.to_path_buf())).await;
    let cleanup = run(&session, &drop).await;
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
    cleanup.unwrap_or_else(|error| panic!("{drop}: {error}"));
}

async fn copy(engine: Engine, dump: PathBuf, client: PathBuf) {
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("other.sql");
    let params = testing::params(engine);

    let backup = tools::backup_command(
        &dump,
        &params,
        &BackupOptions {
            format: BackupFormat::Sql,
            content: BackupContent::All,
            ..BackupOptions::new(Engine::MySql, "kerno_other")
        },
        &path,
    )
    .unwrap();
    run_ok(backup).await;
    assert!(std::fs::metadata(&path).unwrap().len() > 0);

    let restore = tools::restore_command(
        &client,
        &params,
        &RestoreOptions {
            allow_client_commands: false,
            tool_version: tools::version(&client),
            ..RestoreOptions::new(Engine::MySql, SCRATCH, path, BackupFormat::Sql)
        },
    )
    .unwrap();
    run_ok(restore).await;

    let session = common::session(engine).await;
    let contents = |database: &str| {
        format!(
            "SELECT table_name FROM information_schema.tables WHERE table_schema = '{database}' \
             ORDER BY 1"
        )
    };
    assert_eq!(
        rows(&session, &contents(SCRATCH)).await,
        rows(&session, &contents("kerno_other")).await
    );
    let notes = |database: &str| format!("SELECT id, body FROM {database}.notes ORDER BY id");
    let source = rows(&session, &notes("kerno_other")).await;
    assert_eq!(source.len(), 2, "{source:?}");
    assert_eq!(rows(&session, &notes(SCRATCH)).await, source);
}

/// The installed dump and client tools, or `None` after a `SKIPPED` line.
fn installed() -> Option<(PathBuf, PathBuf)> {
    let found = tools::find(Tool::MysqlDump).zip(tools::find(Tool::Mysql));
    if found.is_none() {
        eprintln!("SKIPPED: the MySQL client tools are not installed");
    }
    found
}

/// What a genuine dump holds and the client-command check must let through:
/// backslashes and quotes in strings (written `\\`, `\n`, `\'`), binary
/// bytes, and routines and a trigger, which mysqldump writes between
/// `DELIMITER ;;` lines.
const GENUINE: [&str; 5] = [
    "CREATE TABLE paths (id INT PRIMARY KEY, body TEXT, raw VARBINARY(16))",
    concat!(
        r"INSERT INTO paths VALUES (1, 'C:\\Users\\x\\out.txt', X'DEADBEEF5C21'), ",
        r"(2, 'tab\there\nline\r\n it\'s', NULL), ",
        r"(3, '\\! touch /tmp/never \\. x', X'5C5C'), ",
        r"(4, 'system touch /tmp/never', NULL), (5, 'café \\', X'FF5C')",
    ),
    "CREATE FUNCTION shout(t TEXT) RETURNS TEXT DETERMINISTIC NO SQL \
     RETURN CONCAT(UPPER(t), '\\\\')",
    "CREATE PROCEDURE count_paths(OUT n INT) READS SQL DATA \
     BEGIN SELECT COUNT(*) INTO n FROM paths; SET n = n + 0; END",
    "CREATE TRIGGER paths_shout BEFORE UPDATE ON paths FOR EACH ROW \
     BEGIN SET NEW.body = shout(NEW.body); SET NEW.id = NEW.id; END",
];

/// The rows, routines and triggers of `db`.
async fn genuine_contents(
    session: &Arc<dyn kerno_core::Session>,
    db: &str,
) -> Vec<Vec<Option<String>>> {
    let mut all = rows(
        session,
        &format!("SELECT id, body, HEX(raw) FROM {db}.paths ORDER BY id"),
    )
    .await;
    all.extend(
        rows(
            session,
            &format!(
                "SELECT CONCAT(routine_name, ': ', routine_definition) FROM information_schema.routines \
                 WHERE routine_schema = '{db}' UNION ALL SELECT CONCAT(trigger_name, ': ', action_statement) \
                 FROM information_schema.triggers WHERE trigger_schema = '{db}' ORDER BY 1"
            ),
        )
        .await,
    );
    all
}

/// A dump holding backslashes, binary bytes (written in hex), routines and
/// a trigger restores through the client's own option (`--commands=OFF` /
/// `--sandbox --binary-mode`, when its version has it), carriage returns
/// included. Through the scan alone, the carriage return written `\r` is
/// the known false alarm (refused, with its line); without it the dump
/// passes and restores. The check of a dump of the fixture database
/// passes.
#[tokio::test]
async fn a_genuine_dump_passes_the_check_and_restores_both_ways() {
    let Some((dump, client)) = installed() else {
        return;
    };
    let version = tools::version(&client);
    eprintln!("client: {version:?}");
    for engine in common::engines() {
        let session = common::session(engine).await;
        let (source, copy) = ("kerno_test_restore_src", "kerno_test_restore_copy");
        let drop = [source, copy].map(|db| format!("DROP DATABASE IF EXISTS {db}"));
        let outcome = tokio::spawn(genuine_round_trip(
            engine,
            dump.clone(),
            client.clone(),
            version.clone(),
        ))
        .await;
        for sql in &drop {
            run(&session, sql)
                .await
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        if let Err(error) = outcome {
            std::panic::resume_unwind(error.into_panic());
        }
    }
}

async fn genuine_round_trip(
    engine: Engine,
    dump: PathBuf,
    client: PathBuf,
    version: Option<tools::ToolVersion>,
) {
    let session = common::session(engine).await;
    let source = common::fresh_database(&session, "restore_src").await;
    // In the database itself: MariaDB keeps a qualified trigger name as
    // written, and the dump would then create it in the source again.
    let mut params = testing::params(engine);
    params.database = Some(source.clone());
    let in_source = common::connect(&params).await.expect("connect");
    for sql in GENUINE {
        exec_ok(&in_source, sql).await;
    }
    let params = testing::params(engine);
    let files = tempfile::tempdir().unwrap();
    let backup = |database: &str, output: &Path| {
        tools::backup_command(
            &dump,
            &params,
            &BackupOptions {
                format: BackupFormat::Sql,
                content: BackupContent::All,
                ..BackupOptions::new(Engine::MySql, database)
            },
            output,
        )
        .unwrap()
    };
    let path = files.path().join("genuine.sql");
    run_ok(backup(&source, &path)).await;
    let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
    assert!(
        text.contains("DELIMITER ;;")
            && text.contains(r"C:\\Users")
            && text.contains(r"\\! touch")
            && text.contains(r"line\r\n")
            && text.contains("0xDEADBEEF5C21"),
        "{text}"
    );
    // The carriage return of row 2 is written `\r`, which the scan cannot
    // tell from `connect`: refused, with its line.
    let cr_line = text
        .lines()
        .position(|line| line.contains(r"line\r\n"))
        .unwrap()
        + 1;
    let check = |path: &Path| ScriptCheck {
        tool: Tool::Mysql,
        path: path.to_path_buf(),
        version: None,
    };
    let refusal = tools::check_script(&check(&path), &AtomicBool::new(false)).unwrap_err();
    assert!(
        refusal
            .to_string()
            .contains(&format!("genuine.sql, line {cr_line}: \\r is a mysql")),
        "{refusal}"
    );

    let expected = genuine_contents(&session, &source).await;
    assert!(expected.len() == 8, "{expected:#?}");
    let restore_into = |input: &Path, tool_version: Option<tools::ToolVersion>, copy: &str| {
        tools::restore_command(
            &client,
            &params,
            &RestoreOptions {
                allow_client_commands: false,
                tool_version,
                ..RestoreOptions::new(Engine::MySql, copy, input.to_path_buf(), BackupFormat::Sql)
            },
        )
        .unwrap()
    };

    // The client's own option: the dump restores whole, `\r\n` included.
    if let Some(version) = version {
        let copy = common::fresh_database(&session, "restore_copy").await;
        let restore = restore_into(&path, Some(version), &copy);
        eprintln!("{}", restore.display());
        assert!(
            restore
                .args
                .iter()
                .any(|arg| arg == "--commands=OFF" || arg == "--sandbox"),
            "{:?}",
            restore.args
        );
        assert_eq!(restore.script_check, None);
        run_ok(restore).await;
        assert_eq!(genuine_contents(&session, &copy).await, expected);
    }

    // The scan alone: that dump is refused before mysql starts; without
    // the carriage return the dump restores.
    let copy = common::fresh_database(&session, "restore_copy").await;
    let restore = restore_into(&path, None, &copy);
    assert_eq!(restore.script_check, Some(check(&path)));
    let refusal = tools::run(restore, |_| {}, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap_err();
    assert!(matches!(refusal, kerno_core::Error::Config(_)), "{refusal}");
    let tables =
        format!("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = '{copy}'");
    assert_eq!(
        common::scalar(&session, &tables).await,
        "0",
        "nothing restored"
    );

    exec_ok(
        &session,
        &format!("DELETE FROM {source}.paths WHERE id = 2"),
    )
    .await;
    let expected = genuine_contents(&session, &source).await;
    let path = files.path().join("genuine-lf.sql");
    run_ok(backup(&source, &path)).await;
    let restore = restore_into(&path, None, &copy);
    run_ok(restore).await;
    assert_eq!(genuine_contents(&session, &copy).await, expected);

    // The fixture database, without its million-row table.
    let fixture = files.path().join("kerno.sql");
    let mut command = backup("kerno", &fixture);
    let at = command.args.len() - 1;
    command
        .args
        .insert(at, "--ignore-table=kerno.big_table".into());
    run_ok(command).await;
    eprintln!(
        "kerno.sql: {} bytes",
        std::fs::metadata(&fixture).unwrap().len()
    );
    tools::check_script(&check(&fixture), &AtomicBool::new(false))
        .expect("the fixture dump holds no client command");
}

/// A file with client commands runs nothing: refused by the check before
/// mysql starts, and, run through a client that has the option, refused by
/// the client itself. The marker file the commands would create never
/// appears. With the opt-in the command is built without either (and not
/// run here).
#[tokio::test]
async fn a_file_with_client_commands_runs_nothing() {
    let Some((_, client)) = installed() else {
        return;
    };
    let version = tools::version(&client);
    let files = tempfile::tempdir().unwrap();
    let marker = files.path().join("pwned");
    let pwned = marker.display();
    let input = files.path().join("hostile.sql");
    for engine in common::engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "restore_pwn").await;
        for (text, line, command) in [
            (format!("SELECT 1;\nsystem touch {pwned}\n"), 2, "system"),
            (format!("SELECT 1; \\! touch {pwned}\n"), 1, "\\!"),
            (format!("/*!40101 \\! touch {pwned} */;\n"), 1, "\\!"),
            // connect, which would hand the password to the host it names.
            ("SELECT 1;\n\\r kerno evil.invalid\n".into(), 2, "\\r"),
            ("SELECT 1; \\r kerno evil.invalid\n".into(), 1, "\\r"),
            (
                "SELECT 1;\nconnect kerno evil.invalid\n".into(),
                2,
                "connect",
            ),
        ] {
            std::fs::write(&input, &text).unwrap();
            let options = RestoreOptions {
                allow_client_commands: false,
                tool_version: None,
                ..RestoreOptions::new(
                    Engine::MySql,
                    database.clone(),
                    input.clone(),
                    BackupFormat::Sql,
                )
            };
            let params = testing::params(engine);
            let restore = tools::restore_command(&client, &params, &options).unwrap();
            let error = tools::run(restore, |_| {}, Arc::new(AtomicBool::new(false)))
                .await
                .expect_err(&text);
            assert!(
                matches!(error, kerno_core::Error::Config(_))
                    && error
                        .to_string()
                        .contains(&format!("hostile.sql, line {line}: {command} is a mysql")),
                "{text}: {error}"
            );
            assert!(!marker.exists(), "{text}");

            // The client's own option, when its version has one: mysql runs
            // the file and refuses the command itself.
            let sandboxed = RestoreOptions {
                tool_version: version.clone(),
                ..options.clone()
            };
            let restore = tools::restore_command(&client, &params, &sandboxed).unwrap();
            if restore.script_check.is_none() {
                let lines = Arc::new(Mutex::new(Vec::new()));
                let sink = lines.clone();
                let status = tools::run(
                    restore,
                    move |line| sink.lock().unwrap().push(line),
                    Arc::new(AtomicBool::new(false)),
                )
                .await
                .unwrap();
                eprintln!("{text:?} -> {status}: {:?}", lines.lock().unwrap());
                assert!(!marker.exists(), "{text}");
            }

            let allowed = RestoreOptions {
                allow_client_commands: true,
                tool_version: version.clone(),
                ..options
            };
            let restore = tools::restore_command(&client, &params, &allowed).unwrap();
            assert_eq!(restore.script_check, None);
            assert!(
                !restore
                    .args
                    .iter()
                    .any(|arg| arg == "--commands=OFF" || arg == "--sandbox"),
                "{:?}",
                restore.args
            );
        }
        run(&session, &format!("DROP DATABASE IF EXISTS {database}"))
            .await
            .unwrap();
    }
}

/// K67 (D-136), on the Windows runner of the probe workflow only, whose
/// MySQL 8.4 listens where the container does and holds `kerno_test_k67`:
/// Kerno's own chain (the client detected in Program Files, the one
/// `client_for` picks, `backup_command`, `restore_command`, `run`) backs the
/// database up with `mysqldump.exe`, restores it with `mysql.exe` into a
/// scratch database, and a cancelled backup leaves no `mysqldump.exe`
/// running. Over TLS (the server's own certificate), so that
/// `caching_sha2_password` needs no password cached by an earlier login.
#[tokio::test]
async fn windows_k67_backup_restore_and_cancel() {
    if !testing::k67_runner() {
        return;
    }
    let mut params = testing::params(Engine::MySql);
    params.database = Some("kerno_test_k67".into());
    params.tls.mode = kerno_core::TlsMode::Require;
    let session = common::connect(&params).await.expect("connect");
    let drop = "DROP DATABASE IF EXISTS kerno_test_k67_restore";
    exec_ok(&session, drop).await;
    let outcome = tokio::spawn(windows_k67(params)).await;
    let cleanup = run(&session, drop).await;
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
    cleanup.unwrap_or_else(|error| panic!("{drop}: {error}"));
}

async fn windows_k67(params: kerno_core::ConnectParams) {
    let detected = tools::detect_clients(&tools::ClientRoots::system(Vec::new()));
    let wanted = tools::ClientWanted {
        engine: Engine::MySql,
        connection_dir: None,
        default_dir: None,
    };
    let install = tools::client_for(&detected, &wanted)
        .expect("a MySQL client")
        .install;
    eprintln!(
        "K67 client: {} in {}",
        install.label(),
        install.dir.display()
    );
    let mysqldump = install.path(Tool::MysqlDump).expect("mysqldump.exe");
    let backup = |path: &Path| {
        tools::backup_command(
            mysqldump,
            &params,
            &BackupOptions::new(Engine::MySql, "kerno_test_k67"),
            path,
        )
        .unwrap()
    };
    let files = tempfile::tempdir().unwrap();

    let dump = files.path().join("k67.sql");
    run_ok(backup(&dump)).await;
    let size = std::fs::metadata(&dump).unwrap().len();
    let text = std::fs::read_to_string(&dump).unwrap();
    assert!(text.contains("CREATE TABLE `k67_items`"), "{text}");
    eprintln!(
        "K67 backup: mysqldump.exe wrote {size} bytes to {}",
        dump.display()
    );

    let session = common::connect(&params).await.expect("connect");
    exec_ok(&session, "CREATE DATABASE kerno_test_k67_restore").await;
    let restore = tools::restore_command(
        install.path(Tool::Mysql).expect("mysql.exe"),
        &params,
        &RestoreOptions {
            allow_client_commands: false,
            tool_version: install.version.clone(),
            ..RestoreOptions::new(
                Engine::MySql,
                "kerno_test_k67_restore",
                dump,
                BackupFormat::Sql,
            )
        },
    )
    .unwrap();
    run_ok(restore).await;
    let back = rows(
        &session,
        "SELECT id, label FROM kerno_test_k67_restore.k67_items ORDER BY id",
    )
    .await;
    let expected: Vec<Vec<Option<String>>> = [("1", "one"), ("2", "two"), ("3", "three")]
        .map(|(id, label)| vec![Some(id.into()), Some(label.into())])
        .into();
    assert_eq!(back, expected);
    eprintln!(
        "K67 restore: mysql.exe gave back {} rows of k67_items in kerno_test_k67_restore: \
         {back:?}",
        back.len()
    );

    // A lock held here makes mysqldump wait, so the cancel stops a live process.
    exec_ok(&session, "LOCK TABLES k67_items WRITE").await;
    assert_eq!(testing::processes_named("mysqldump.exe"), 0);
    let cancel = Arc::new(AtomicBool::new(false));
    let running = tokio::spawn(tools::run(
        backup(&files.path().join("cancelled.sql")),
        |line| eprintln!("  | {line}"),
        cancel.clone(),
    ));
    let mut before = 0;
    for _ in 0..120 {
        before = testing::processes_named("mysqldump.exe");
        if before > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert_eq!(before, 1, "mysqldump.exe never showed in tasklist");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(!running.is_finished(), "mysqldump should wait on the lock");
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let outcome = running.await.unwrap();
    assert!(
        matches!(outcome, Err(kerno_core::Error::Cancelled)),
        "{outcome:?}"
    );
    let after = testing::processes_named("mysqldump.exe");
    eprintln!(
        "K67 cancel: tasklist listed {before} mysqldump.exe before the cancel, {after} after"
    );
    assert_eq!(after, 0);
    exec_ok(&session, "UNLOCK TABLES").await;
    session.close().await;
}
