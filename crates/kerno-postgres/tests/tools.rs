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
    self, BackupContent, BackupFormat, BackupOptions, FALLBACK_DIRS, RestoreOptions, ScriptCheck,
    Tool, ToolCommand,
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
    tools::version(tool).map(|version| version.number.0)
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
                format,
                content: BackupContent::All,
                ..BackupOptions::new(Engine::Postgres, "kerno_other")
            },
            &path,
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
            &RestoreOptions::new(Engine::Postgres, SCRATCH, path, format),
        )
        .unwrap();
        // psql reads a script, checked before it starts; pg_restore does not.
        assert_eq!(restore.script_check.is_some(), format == BackupFormat::Sql);
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
            format: BackupFormat::Sql,
            content: BackupContent::SchemaOnly,
            ..BackupOptions::new(Engine::Postgres, "kerno_other")
        },
        &output,
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
            format: BackupFormat::Sql,
            content: BackupContent::SchemaOnly,
            ..BackupOptions::new(Engine::Postgres, "kerno_other")
        },
        &files.path().join("never.sql"),
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

const SOURCE: &str = "kerno_test_restore_src";
const COPY: &str = "kerno_test_restore_copy";

/// What a genuine dump holds and the client-command check must let through:
/// backslashes in COPY data (written `\\`, `\t`, `\n`, `\N`), regular
/// expressions in a function and a domain, a trigger.
const GENUINE: [&str; 6] = [
    "CREATE TABLE paths (id int PRIMARY KEY, body text)",
    concat!(
        r"INSERT INTO paths VALUES (1, 'C:\Users\x\out.txt'), (2, E'tab\there\nline\r\n'), ",
        r"(3, '\! touch /tmp/never \o x \set y'), (4, '\.'), (5, '\'), ",
        r"(6, 'caf' || chr(233) || '\x'), (7, NULL), (8, 'use `code`')",
    ),
    concat!(
        "CREATE FUNCTION squash(t text) RETURNS text LANGUAGE sql IMMUTABLE ",
        r"AS $$ SELECT regexp_replace(t, '\s+', ' ', 'g') $$",
    ),
    "CREATE FUNCTION squash_body() RETURNS trigger LANGUAGE plpgsql \
     AS $$ BEGIN NEW.body := squash(NEW.body); RETURN NEW; END $$",
    "CREATE TRIGGER paths_squash BEFORE UPDATE ON paths FOR EACH ROW EXECUTE FUNCTION squash_body()",
    r"CREATE DOMAIN email AS text CHECK (VALUE ~ '^[^@\s]+@[^@\s]+$')",
];

/// The rows, functions and triggers of `database`.
async fn genuine_contents(database: &str) -> Vec<Vec<Option<String>>> {
    let mut params = testing::params(Engine::Postgres);
    params.database = Some(database.into());
    let session = common::connect(&params).await.expect("connect");
    let mut all = rows(&session, "SELECT id::text, body FROM paths ORDER BY id").await;
    all.extend(
        rows(
            &session,
            "SELECT p.proname || ': ' || p.prosrc FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'public' \
             UNION ALL SELECT tgname FROM pg_trigger WHERE NOT tgisinternal \
             UNION ALL SELECT typname FROM pg_type WHERE typname = 'email' ORDER BY 1",
        )
        .await,
    );
    all
}

/// A plain dump holding backslashes, regular expressions and a trigger is
/// read by the check without a false alarm and restores; so does a dump of
/// the fixture database (every type, functions, triggers).
#[tokio::test]
async fn a_genuine_plain_dump_passes_the_check_and_restores() {
    let Some(session) = common::session().await else {
        return;
    };
    let Some(dir) = tool_dir(server_major(&session).await) else {
        return;
    };
    let cleanup = [SOURCE, COPY].map(|db| format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)"));
    let body = genuine_round_trip(session.clone(), dir);
    cleaning_up(&session, &cleanup, body).await;
}

async fn genuine_round_trip(session: Arc<dyn Session>, dir: PathBuf) {
    for db in [SOURCE, COPY] {
        exec_ok(&session, &format!("CREATE DATABASE {db}")).await;
    }
    let mut params = testing::params(Engine::Postgres);
    params.database = Some(SOURCE.into());
    let source = common::connect(&params).await.expect("connect");
    for sql in GENUINE {
        exec_ok(&source, sql).await;
    }
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("genuine.sql");
    let params = testing::params(Engine::Postgres);
    let backup = |database: &str, output: &Path| {
        tools::backup_command(
            &dir.join("pg_dump"),
            &params,
            &BackupOptions {
                format: BackupFormat::Sql,
                content: BackupContent::All,
                ..BackupOptions::new(Engine::Postgres, database)
            },
            output,
        )
        .unwrap()
    };
    run_ok(backup(SOURCE, &path)).await;
    let dump = std::fs::read_to_string(&path).unwrap();
    assert!(
        dump.contains("C:\\\\Users") && dump.contains("\\\\! touch") && dump.contains("\\s+"),
        "{dump}"
    );

    let psql = dir.join("psql");
    let restore = tools::restore_command(
        &psql,
        &params,
        &RestoreOptions {
            allow_client_commands: false,
            tool_version: tools::version(&psql),
            ..RestoreOptions::new(Engine::Postgres, COPY, path.clone(), BackupFormat::Sql)
        },
    )
    .unwrap();
    let check = restore.script_check.clone().expect("checked");
    assert_eq!((check.tool, &check.path), (Tool::Psql, &path));
    assert!(check.version.is_some(), "{check:?}");
    run_ok(restore).await;
    let copied = genuine_contents(COPY).await;
    assert_eq!(copied, genuine_contents(SOURCE).await);
    assert!(copied.len() > 8, "{copied:?}");

    // The fixture database, without its million-row table.
    let fixture = files.path().join("kerno.sql");
    let mut command = backup("kerno", &fixture);
    command
        .args
        .push("--exclude-table-data=analytics.big_table".into());
    run_ok(command).await;
    eprintln!(
        "kerno.sql: {} bytes",
        std::fs::metadata(&fixture).unwrap().len()
    );
    // With the version of the psql that restores it: no false alarm.
    let mut check = ScriptCheck {
        tool: Tool::Psql,
        path: fixture.clone(),
        version: tools::version(&psql),
    };
    tools::check_script(&check, &AtomicBool::new(false))
        .expect("the fixture dump holds no client command");
    // Unknown version, the strict rule of a psql older than 8.4: the `\s`
    // of the `email` domain's regular expression reads as `\s` (history
    // to a file), on that dump and on the one above.
    check.version = None;
    let dump = std::fs::read_to_string(&fixture).unwrap();
    let regex_line = dump
        .lines()
        .position(|line| line.contains("@[^@\\s]+$"))
        .unwrap()
        + 1;
    let refusal = tools::check_script(&check, &AtomicBool::new(false)).unwrap_err();
    assert!(
        refusal.to_string().contains(&format!(
            "kerno.sql, line {regex_line}: \\s]+@[^@ is a psql"
        )),
        "{refusal}"
    );
    check.path = path;
    assert!(tools::check_script(&check, &AtomicBool::new(false)).is_err());
}

/// A file with client commands is refused before psql starts: the marker
/// file the commands would create never appears. With the opt-in the
/// command is built without the check (and not run here).
#[tokio::test]
async fn a_plain_file_with_client_commands_runs_nothing() {
    let Some(psql) = tools::find(Tool::Psql) else {
        eprintln!("SKIPPED: no psql installed");
        return;
    };
    let files = tempfile::tempdir().unwrap();
    let pwned = files.path().join("pwned");
    let pwned = pwned.display();
    let input = files.path().join("hostile.sql");
    // Glued arguments, refused for a psql of unknown version (one before
    // 8.4 runs them); a refused command with a space whatever the version.
    let known = tools::version(&psql);
    let cases = [
        (format!("SELECT 1;\n\\!touch {pwned}\n"), 2, None),
        (format!("SELECT 1 \\o|touch {pwned}\n"), 1, None),
        (format!("SELECT 1;\n\\g|touch {pwned}\n"), 2, None),
        (format!("\\i{pwned}\n"), 1, None),
    ];
    for (text, line, version) in cases.into_iter().chain([
        (format!("SELECT 1;\n\\! touch {pwned}\n"), 2, known.clone()),
        (
            format!("\\restrict k\nSELECT 1; \\! touch {pwned}\n\\unrestrict k\n"),
            2,
            known.clone(),
        ),
        (format!("SELECT 1 \\g |touch {pwned}\n"), 1, known.clone()),
        (
            format!("COPY t (a) FROM stdin;\n1\n\\.\n\\copy t TO PROGRAM 'touch {pwned}'\n"),
            4,
            known.clone(),
        ),
        (
            format!("\\set x '\\\\! touch {pwned}'\nSELECT :x;\n"),
            1,
            known.clone(),
        ),
    ]) {
        std::fs::write(&input, &text).unwrap();
        let options = RestoreOptions {
            allow_client_commands: false,
            tool_version: version,
            ..RestoreOptions::new(
                Engine::Postgres,
                "kerno_test_never",
                input.clone(),
                BackupFormat::Sql,
            )
        };
        let command =
            tools::restore_command(&psql, &testing::params(Engine::Postgres), &options).unwrap();
        let error = tools::run(command, |_| {}, Arc::new(AtomicBool::new(false)))
            .await
            .expect_err(&text);
        eprintln!("{error}");
        assert!(
            matches!(error, kerno_core::Error::Config(_))
                && error
                    .to_string()
                    .contains(&format!("hostile.sql, line {line}: \\")),
            "{text}: {error}"
        );
        assert!(!files.path().join("pwned").exists(), "{text}");

        let allowed = RestoreOptions {
            allow_client_commands: true,
            ..options
        };
        let command =
            tools::restore_command(&psql, &testing::params(Engine::Postgres), &allowed).unwrap();
        assert_eq!(command.script_check, None);
        assert!(
            command
                .args
                .contains(&format!("--file={}", input.display()))
        );
    }
}
