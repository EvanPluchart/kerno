//! J3 on MySQL and MariaDB (DBeaver #3270, #9413; Beekeeper #1443, #530):
//! logging in with the password a command prints (the AWS RDS IAM preset
//! through a fake `aws`, over TLS), with `~/.my.cnf` from a temporary
//! `HOME`, and an imported command that waits for confirmation.

// The fake `aws` is a `/bin/sh` script, the files have modes: Unix only (K64).
#![cfg(unix)]

mod common;

use std::sync::Arc;

use common::{engines, rows};
use kerno_core::config::{ConnectionConfig, PasswordSource, Secrets, TlsMode};
use kerno_core::connector::{Connector, HostKeyStore};
use kerno_core::error::explain;
use kerno_core::password::{PasswordEnv, PasswordOrigin};
use kerno_core::{Error, testing};
use kerno_mysql::MysqlDriver;

async fn open(config: ConnectionConfig, env: &PasswordEnv) -> kerno_core::Result<Connector> {
    let host_keys = HostKeyStore {
        password: Some(env.clone()),
        ..HostKeyStore::default()
    };
    Connector::open(
        Arc::new(MysqlDriver),
        config,
        Secrets::default(),
        &host_keys,
    )
    .await
}

/// The session's TLS cipher; empty without TLS.
async fn cipher(connector: &Connector) -> String {
    let session = connector.session(None).await.unwrap();
    let cipher = rows(&session, "SHOW SESSION STATUS LIKE 'Ssl_cipher'").await[0][1]
        .clone()
        .unwrap_or_default();
    session.close().await;
    cipher
}

#[tokio::test]
async fn the_rds_iam_preset_logs_in_over_tls_with_what_aws_prints() {
    for engine in engines() {
        eprintln!("--- {}", engine.label());
        let dir = tempfile::tempdir().unwrap();
        let env = testing::fake_aws(dir.path());
        let mut config = testing::config(engine);
        config.password_source = PasswordSource::RdsIam {
            profile: Some("kerno-profile".into()),
            region: "eu-west-3".into(),
        };
        let refused = open(config.clone(), &env).await.err();
        assert!(matches!(refused, Some(Error::Config(_))), "{refused:?}");

        config.tls.mode = TlsMode::Require;
        let connector = open(config, &env).await.unwrap();
        assert_ne!(cipher(&connector).await, "", "RDS IAM goes over TLS");
        assert_eq!(
            connector.password_origin(),
            Some(PasswordOrigin::Command("aws".into()))
        );
        let port = testing::port(engine).to_string();
        assert_eq!(
            testing::fake_aws_args(dir.path()),
            [
                "rds",
                "generate-db-auth-token",
                "--hostname",
                "127.0.0.1",
                "--port",
                port.as_str(),
                "--username",
                "kerno",
                "--region",
                "eu-west-3",
                "--profile",
                "kerno-profile"
            ]
        );
    }
}

#[tokio::test]
async fn my_cnf_in_the_home_directory_gives_the_password() {
    for engine in engines() {
        eprintln!("--- {}", engine.label());
        let dir = tempfile::tempdir().unwrap();
        let env = testing::fake_aws(dir.path());
        testing::password_file(
            dir.path(),
            ".my.cnf",
            &format!(
                "[mysqld]\npassword=wrong\n[client]\n# from the test\npassword = \"{}\"\n",
                testing::DB_PASSWORD
            ),
            0o600,
        );
        let config = testing::config(engine);
        let connector = open(config.clone(), &env).await.unwrap();
        let session = connector.session(None).await.unwrap();
        assert_eq!(common::scalar(&session, "SELECT 1").await, "1");
        session.close().await;
        assert_eq!(
            connector.password_origin(),
            Some(PasswordOrigin::File("~/.my.cnf".into()))
        );

        // A command's output wins over the file, and a wrong one is
        // refused with words that point at the command.
        testing::password_file(dir.path(), "wrong", "#!/bin/sh\necho nope\n", 0o700);
        let mut wrong = config.clone();
        wrong.password_source = PasswordSource::Command {
            program: "wrong".into(),
            args: vec![],
            timeout_secs: 5,
        };
        let connector = open(wrong.clone(), &env).await.unwrap();
        let error = connector.session(None).await.err().unwrap().to_string();
        assert!(!error.contains("nope"), "{error}");
        assert!(
            explain(&error, &wrong)
                .next_step
                .contains("password command"),
            "{error}"
        );

        // World-writable: ignored, as MySQL does.
        testing::password_file(
            dir.path(),
            ".my.cnf",
            &format!("[client]\npassword={}\n", testing::DB_PASSWORD),
            0o666,
        );
        let connector = open(config.clone(), &env).await.unwrap();
        let error = connector.session(None).await.err().unwrap().to_string();
        assert_eq!(
            explain(&error, &config).headline,
            "The server asks for a password, and ~/.my.cnf was ignored.",
            "{error}"
        );
    }
}

#[tokio::test]
async fn an_imported_command_does_not_run_before_it_is_confirmed() {
    for engine in engines() {
        let dir = tempfile::tempdir().unwrap();
        let env = testing::fake_aws(dir.path());
        let mut config = testing::config(engine);
        config.password_source = PasswordSource::Command {
            program: "aws".into(),
            args: vec!["${user}".into()],
            timeout_secs: 5,
        };
        let file = kerno_core::config::export_connections(std::slice::from_ref(&config)).unwrap();
        let mut imported = kerno_core::config::import_connections(&file)
            .unwrap()
            .remove(0);
        let connector = open(imported.clone(), &env).await.unwrap();
        let error = connector.session(None).await.err().unwrap();
        assert!(matches!(error, Error::Config(_)), "{error:?}");
        assert!(testing::fake_aws_args(dir.path()).is_empty(), "aws ran");
        // Once confirmed, it runs and logs in.
        imported.password_command_unconfirmed = false;
        let connector = open(imported, &env).await.unwrap();
        connector.session(None).await.unwrap().close().await;
        assert_eq!(testing::fake_aws_args(dir.path()), ["kerno"]);
    }
}
