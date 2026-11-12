//! Backup and restore through the real PostgreSQL client tools: `pg_dump` of
//! the `kerno_other` test database, restored with `psql` (plain SQL) and
//! `pg_restore` (custom format) into a scratch database `kerno_test_restore`
//! that the test creates and drops. Skipped with a message when the
//! container is down or no installed `pg_dump` is recent enough for the
//! server (`pg_dump` refuses a server of a newer major version).

mod common;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use common::{exec_ok, rows, run, scalar};
use kerno_core::config::{ConnectParams, Engine, SshAuth, TlsConfig};
use kerno_core::ssh::{Tunnel, TunnelRequest};
use kerno_core::testing;
use kerno_core::tools::{
    self, BackupContent, BackupFormat, BackupOptions, FALLBACK_DIRS, RestoreOptions, Tool,
    ToolCommand,
};
use kerno_core::{Session, TlsMode};

const SCRATCH: &str = "kerno_test_restore";

/// Runs the `cleanup` statements, then `body`, then `cleanup` again whether
/// `body` passed or panicked.
async fn cleaning_up(
    session: &Arc<dyn Session>,
    cleanup: &[String],
    body: impl Future<Output = ()> + Send + 'static,
) {
    for sql in cleanup {
        exec_ok(session, sql).await;
    }
    let outcome = tokio::spawn(body).await;
    let mut failed = Vec::new();
    for sql in cleanup {
        if let Err(error) = run(session, sql).await {
            failed.push(format!("{sql}: {error}"));
        }
    }
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
    assert!(failed.is_empty(), "cleanup failed: {failed:#?}");
}

/// The major version `tool --version` prints ("pg_dump (PostgreSQL) 17.2").
fn major_version(tool: &Path) -> Option<u32> {
    let output = std::process::Command::new(tool)
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text
        .split_whitespace()
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))?;
    version.split('.').next()?.parse().ok()
}

async fn server_major(session: &Arc<dyn Session>) -> u32 {
    scalar(session, "SHOW server_version_num")
        .await
        .parse::<u32>()
        .expect("server_version_num")
        / 10_000
}

/// The directory of the first `pg_dump` (in `$PATH`, then the usual
/// install directories) able to dump a server of major `server`, or `None`
/// after a `SKIPPED` line naming what was found.
fn tool_dir(server: u32) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::env::split_paths(&path).chain(FALLBACK_DIRS.iter().map(PathBuf::from));
    let mut found = Vec::new();
    for dir in dirs {
        let Some(pg_dump) = tools::find_in(Tool::PgDump, None, std::slice::from_ref(&dir)) else {
            continue;
        };
        let major = major_version(&pg_dump);
        if major.is_some_and(|major| major >= server) {
            eprintln!("using {} (major {major:?})", pg_dump.display());
            return Some(dir);
        }
        found.push(format!("{} (major {major:?})", pg_dump.display()));
    }
    eprintln!(
        "SKIPPED: no pg_dump can back up this PostgreSQL {server} server: pg_dump refuses a server \
         newer than itself, and the ones found are older or missing: {found:?}"
    );
    None
}

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

/// Tables and rows of a database, compared between the source and the copy.
async fn contents(database: &str) -> Vec<Vec<Option<String>>> {
    let mut params = testing::params(Engine::Postgres);
    params.database = Some(database.into());
    let session = common::connect(&params).await.expect("connect");
    let mut all = rows(
        &session,
        "SELECT table_schema || '.' || table_name FROM information_schema.tables \
         WHERE table_schema NOT IN ('pg_catalog', 'information_schema') ORDER BY 1",
    )
    .await;
    all.extend(rows(&session, "SELECT id::text, body FROM notes ORDER BY id").await);
    all
}

#[tokio::test]
async fn pg_dump_then_psql_and_pg_restore_copy_a_database() {
    let Some(session) = common::session().await else {
        return;
    };
    let Some(dir) = tool_dir(server_major(&session).await) else {
        return;
    };
    let cleanup = [format!("DROP DATABASE IF EXISTS {SCRATCH} WITH (FORCE)")];
    let body = round_trips(session.clone(), dir);
    cleaning_up(&session, &cleanup, body).await;
}

async fn round_trips(session: Arc<dyn Session>, dir: PathBuf) {
    let files = tempfile::tempdir().unwrap();
    let params = testing::params(Engine::Postgres);
    assert_eq!(params.tls.mode, TlsMode::Disable);
    let source = contents("kerno_other").await;
    assert!(
        source.contains(&vec![Some("public.notes".into())])
            && source.contains(&vec![Some("2".into()), Some("second".into())]),
        "{source:?}"
    );

    for (format, file) in [
        (BackupFormat::Sql, "other.sql"),
        (BackupFormat::Custom, "other.dump"),
    ] {
        exec_ok(
            &session,
            &format!("DROP DATABASE IF EXISTS {SCRATCH} WITH (FORCE)"),
        )
        .await;
        exec_ok(&session, &format!("CREATE DATABASE {SCRATCH}")).await;
        let path = files.path().join(file);

        let backup = tools::backup_command(
            &dir.join("pg_dump"),
            &params,
            &BackupOptions {
                database: "kerno_other".into(),
                output: path.clone(),
                format,
                content: BackupContent::All,
                tables: Vec::new(),
            },
        )
        .unwrap();
        let log = run_ok(backup).await;
        // `--verbose` makes pg_dump say what it does, line by line.
        assert!(log.iter().any(|line| line.contains("notes")), "{log:#?}");
        assert!(std::fs::metadata(&path).unwrap().len() > 0);

        let tool = tools::restore_tool(Engine::Postgres, format);
        let restore = tools::restore_command(
            &dir.join(tool.names()[0]),
            &params,
            &RestoreOptions {
                database: SCRATCH.into(),
                input: path,
                format,
            },
        )
        .unwrap();
        run_ok(restore).await;
        assert_eq!(contents(SCRATCH).await, source, "{format:?}");
    }
}

/// Through the SSH bastion: the tool reaches the tunnel's single-use local
/// port, and with verify-full the certificate is checked against the
/// database host name, not against the local address.
#[tokio::test]
async fn pg_dump_through_the_ssh_tunnel_with_verify_full() {
    let Some(session) = common::session().await else {
        return;
    };
    if !testing::ssh_available() {
        return;
    }
    let Some(dir) = tool_dir(server_major(&session).await) else {
        return;
    };
    let keys = tempfile::tempdir().unwrap();
    let host_keys = testing::trusted_host_keys(keys.path().join("known_hosts")).await;
    let ssh = testing::ssh(SshAuth::Password);
    let secrets = testing::secrets();
    let tunnel = Tunnel::open(TunnelRequest {
        ssh: &ssh,
        password: secrets.ssh_password.as_ref(),
        key_passphrase: None,
        known_hosts: &host_keys.known_hosts,
        learn_into: host_keys.learn_into.as_deref(),
        trusted_fingerprint: host_keys.trusted_fingerprint.as_deref(),
    })
    .await
    .expect("open the tunnel");
    let params = ConnectParams {
        host: testing::service_name(Engine::Postgres).into(),
        port: 5432,
        tunnel: Some(Arc::new(tunnel)),
        tls: TlsConfig {
            mode: TlsMode::VerifyFull,
            ca_file: Some(testing::generated("tls/ca.crt")),
            ..TlsConfig::default()
        },
        ..testing::params(Engine::Postgres)
    };
    let output = keys.path().join("schema.sql");
    let command = tools::backup_command(
        &dir.join("pg_dump"),
        &params,
        &BackupOptions {
            database: "kerno_other".into(),
            output: output.clone(),
            format: BackupFormat::Sql,
            content: BackupContent::SchemaOnly,
            tables: Vec::new(),
        },
    )
    .unwrap();
    assert!(
        command.display().contains("PGHOSTADDR=127.0.0.1")
            && command
                .display()
                .contains("--host=kerno-postgres --port=<SSH tunnel>"),
        "{}",
        command.display()
    );
    run_ok(command).await;
    let dump = std::fs::read_to_string(&output).unwrap();
    assert!(dump.contains("CREATE TABLE public.notes"), "{dump}");
}

/// The `pg_dump` that [`tools::find`] picks, when it is older than the
/// server, fails: the non-zero exit and the tool's reason are reported.
#[tokio::test]
async fn an_older_pg_dump_fails_with_its_reason() {
    let Some(session) = common::session().await else {
        return;
    };
    let server = server_major(&session).await;
    let Some(pg_dump) = tools::find(Tool::PgDump) else {
        eprintln!("SKIPPED: no pg_dump installed");
        return;
    };
    let major = major_version(&pg_dump);
    if !major.is_some_and(|major| major < server) {
        eprintln!(
            "SKIPPED: {} (major {major:?}) is not older than the server ({server})",
            pg_dump.display()
        );
        return;
    }
    let files = tempfile::tempdir().unwrap();
    let command = tools::backup_command(
        &pg_dump,
        &testing::params(Engine::Postgres),
        &BackupOptions {
            database: "kerno_other".into(),
            output: files.path().join("never.sql"),
            format: BackupFormat::Sql,
            content: BackupContent::SchemaOnly,
            tables: Vec::new(),
        },
    )
    .unwrap();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    let status = tools::run(
        command,
        move |line| sink.lock().unwrap().push(line),
        Arc::new(AtomicBool::new(false)),
    )
    .await
    .unwrap();
    let lines = lines.lock().unwrap().clone();
    eprintln!("{} -> {status}: {lines:#?}", pg_dump.display());
    assert!(!status.success());
    assert!(
        lines.iter().any(|line| line.contains("version mismatch")),
        "{lines:#?}"
    );
}
