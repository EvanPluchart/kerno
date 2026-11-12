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
    self, BackupContent, BackupFormat, BackupOptions, RestoreOptions, Tool, ToolCommand,
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
            database: "kerno_other".into(),
            output: path.clone(),
            format: BackupFormat::Sql,
            content: BackupContent::All,
            tables: Vec::new(),
        },
    )
    .unwrap();
    run_ok(backup).await;
    assert!(std::fs::metadata(&path).unwrap().len() > 0);

    let restore = tools::restore_command(
        &client,
        &params,
        &RestoreOptions {
            database: SCRATCH.into(),
            input: path,
            format: BackupFormat::Sql,
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
