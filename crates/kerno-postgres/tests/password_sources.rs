//! J3 on PostgreSQL (DBeaver #3270, #9413; Beekeeper #1443, #530): logging
//! in with the password a command prints (the AWS RDS IAM preset through a
//! fake `aws`, over TLS), with `~/.pgpass` from a temporary `HOME`, and the
//! failures told without the command's output.

use std::sync::Arc;

use kerno_core::config::{ConnectionConfig, PasswordSource, Secrets, TlsMode};
use kerno_core::connector::{Connector, HostKeyStore};
use kerno_core::error::explain;
use kerno_core::password::{PasswordEnv, PasswordOrigin};
use kerno_core::{Engine, Error, exec, testing};
use kerno_postgres::PostgresDriver;

const ENGINE: Engine = Engine::Postgres;

async fn open(config: ConnectionConfig, env: &PasswordEnv) -> kerno_core::Result<Connector> {
    let host_keys = HostKeyStore {
        password: Some(env.clone()),
        ..HostKeyStore::default()
    };
    Connector::open(
        Arc::new(PostgresDriver),
        config,
        Secrets::default(),
        &host_keys,
    )
    .await
}

async fn scalar(connector: &Connector, sql: &str) -> String {
    let session = connector.session(None).await.unwrap();
    let rows = exec::collect(session.clone(), sql).await.unwrap();
    session.close().await;
    rows.results[0].rows()[0][0].clone().unwrap()
}

#[tokio::test]
async fn the_rds_iam_preset_logs_in_over_tls_with_what_aws_prints() {
    if !testing::available(ENGINE) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let env = testing::fake_aws(dir.path());
    let mut config = testing::config(ENGINE);
    config.password_source = PasswordSource::RdsIam {
        profile: None,
        region: "us-east-1".into(),
    };
    // RDS IAM refuses a connection that could go without TLS.
    let refused = open(config.clone(), &env).await.err();
    assert!(matches!(refused, Some(Error::Config(_))), "{refused:?}");
    assert!(testing::fake_aws_args(dir.path()).is_empty(), "aws ran");

    config.tls.mode = TlsMode::Require;
    let connector = open(config, &env).await.unwrap();
    let ssl = "SELECT ssl::text FROM pg_stat_ssl WHERE pid = pg_backend_pid()";
    assert_eq!(scalar(&connector, ssl).await, "true");
    assert_eq!(
        connector.password_origin(),
        Some(PasswordOrigin::Command("aws".into()))
    );
    assert_eq!(
        testing::fake_aws_args(dir.path()),
        [
            "rds",
            "generate-db-auth-token",
            "--hostname",
            "127.0.0.1",
            "--port",
            "55432",
            "--username",
            "kerno",
            "--region",
            "us-east-1"
        ]
    );
    // A second session in another database reuses the token.
    std::fs::remove_file(dir.path().join("aws-args")).unwrap();
    let other = connector.session(Some("kerno_other")).await.unwrap();
    other.close().await;
    assert!(testing::fake_aws_args(dir.path()).is_empty());
}

#[tokio::test]
async fn a_custom_command_gets_its_placeholders_and_a_wrong_output_is_refused() {
    if !testing::available(ENGINE) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let env = testing::fake_aws(dir.path());
    let mut config = testing::config(ENGINE);
    config.password_source = PasswordSource::Command {
        program: "aws".into(),
        args: vec!["${user}@${host}:${port}/${database}".into()],
        timeout_secs: 10,
    };
    let connector = open(config.clone(), &env).await.unwrap();
    assert_eq!(
        scalar(&connector, "SELECT current_user::text").await,
        "kerno"
    );
    assert_eq!(
        testing::fake_aws_args(dir.path()),
        ["kerno@127.0.0.1:55432/kerno"]
    );

    // A command that prints another password: the server refuses, and the
    // words point at the command.
    testing::password_file(
        dir.path(),
        "wrong",
        "#!/bin/sh\necho not-the-password\n",
        0o700,
    );
    config.password_source = PasswordSource::Command {
        program: "wrong".into(),
        args: vec![],
        timeout_secs: 10,
    };
    let connector = open(config.clone(), &env).await.unwrap();
    let error = connector.session(None).await.err().unwrap().to_string();
    assert!(!error.contains("not-the-password"), "{error}");
    let explained = explain(&error, &config);
    assert!(
        explained.next_step.contains("password command"),
        "{explained:?}"
    );

    // A command that fails: its status and message, never its output.
    testing::password_file(
        dir.path(),
        "broken",
        "#!/bin/sh\necho half-a-secret\necho 'no session: run op signin' >&2\nexit 1\n",
        0o700,
    );
    config.password_source = PasswordSource::Command {
        program: "broken".into(),
        args: vec![],
        timeout_secs: 10,
    };
    let connector = open(config.clone(), &env).await.unwrap();
    let error = connector.session(None).await.err().unwrap().to_string();
    assert!(error.contains("exit status 1"), "{error}");
    assert!(error.contains("no session: run op signin"), "{error}");
    assert!(!error.contains("half-a-secret"), "{error}");
    assert_eq!(
        explain(&error, &config).headline,
        "The password command “broken” failed."
    );
}

#[tokio::test]
async fn an_imported_command_does_not_run_before_it_is_confirmed() {
    if !testing::available(ENGINE) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let env = testing::fake_aws(dir.path());
    let mut config = testing::config(ENGINE);
    config.tls.mode = TlsMode::Require;
    config.password_source = PasswordSource::RdsIam {
        profile: None,
        region: "us-east-1".into(),
    };
    let file = kerno_core::config::export_connections(std::slice::from_ref(&config)).unwrap();
    let imported = kerno_core::config::import_connections(&file)
        .unwrap()
        .remove(0);
    let connector = open(imported, &env).await.unwrap();
    let error = connector.session(None).await.err().unwrap();
    assert!(matches!(error, Error::Config(_)), "{error:?}");
    assert!(testing::fake_aws_args(dir.path()).is_empty(), "aws ran");
}

#[tokio::test]
async fn pgpass_in_the_home_directory_gives_the_password() {
    if !testing::available(ENGINE) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let env = testing::fake_aws(dir.path());
    testing::password_file(
        dir.path(),
        ".pgpass",
        &format!(
            "# Kerno test\nelsewhere:*:*:*:wrong\n127.0.0.1:55432:*:kerno:{}\n",
            testing::DB_PASSWORD
        ),
        0o600,
    );
    let config = testing::config(ENGINE);
    let connector = open(config.clone(), &env).await.unwrap();
    assert_eq!(scalar(&connector, "SELECT 1::text").await, "1");
    assert_eq!(
        connector.password_origin(),
        Some(PasswordOrigin::File("~/.pgpass".into()))
    );
    assert!(testing::fake_aws_args(dir.path()).is_empty());

    // Readable by others: ignored, as libpq does, and said in plain words.
    testing::password_file(
        dir.path(),
        ".pgpass",
        &format!("*:*:*:*:{}\n", testing::DB_PASSWORD),
        0o644,
    );
    let connector = open(config.clone(), &env).await.unwrap();
    let error = connector.session(None).await.err().unwrap().to_string();
    assert!(!error.contains(testing::DB_PASSWORD), "{error}");
    let explained = explain(&error, &config);
    assert_eq!(
        explained.headline,
        "The server asks for a password, and ~/.pgpass was ignored."
    );
    assert!(explained.next_step.contains("chmod 600"), "{explained:?}");

    // Without an environment (the drivers' other tests) no file is read.
    let connector = Connector::open(
        Arc::new(PostgresDriver),
        config,
        Secrets::default(),
        &HostKeyStore::default(),
    )
    .await
    .unwrap();
    let error = connector.session(None).await.err().unwrap();
    assert!(matches!(error, Error::PasswordRequired), "{error:?}");
}
