//! Connecting to MySQL and MariaDB: plain, TLS in every mode, read-only,
//! through the SSH bastion, and the errors each can produce. Runs against
//! the `kerno-mysql`, `kerno-mariadb` and `kerno-ssh` containers; skipped
//! with a message when they are down.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{connect, db_error, engines, exec_ok, finish, scalar};
use kerno_core::config::{ConnectParams, Engine, Secret, SshAuth, TlsConfig, TlsMode};
use kerno_core::connector::{Connector, HostKeyStore};
use kerno_core::exec::{self, RowLimit};
use kerno_core::ssh::{Tunnel, TunnelRequest};
use kerno_core::{Driver, Error, Session, TxState, testing};
use kerno_mysql::MysqlDriver;

fn tls(mode: TlsMode, ca: Option<&str>) -> TlsConfig {
    TlsConfig {
        mode,
        ca_file: ca.map(|name| testing::generated(&format!("tls/{name}"))),
        ..TlsConfig::default()
    }
}

fn with_tls(engine: Engine, tls: TlsConfig) -> ConnectParams {
    ConnectParams {
        tls,
        ..testing::params(engine)
    }
}

/// The cipher of the session's connection; empty when it is not encrypted.
async fn cipher(session: &Arc<dyn Session>) -> String {
    common::rows(session, "SHOW SESSION STATUS LIKE 'Ssl_cipher'").await[0][1]
        .clone()
        .unwrap_or_default()
}

/// Counts 5,000 rows at 20 ms each: 100 s if nothing stops it, and both
/// engines answer a kill with an error (a lone `SLEEP()` does not on MySQL).
const SLOW: &str = "SELECT COUNT(*) FROM kerno.orders WHERE SLEEP(0.02) = 0";

#[tokio::test]
async fn reports_the_server_and_database() {
    for engine in engines() {
        let session = common::session(engine).await;
        // What the server is, read from the server.
        assert_eq!(session.engine(), engine);
        assert_eq!(session.dialect().engine(), engine);
        let server = session.server();
        match engine {
            Engine::MariaDb => {
                assert_eq!(server.version_number.0, 11);
                assert!(server.version.contains("MariaDB"), "{}", server.version);
            }
            _ => {
                assert_eq!((server.version_number.0, server.version_number.1), (8, 4));
                assert!(server.version.starts_with("8.4."), "{}", server.version);
            }
        }
        assert_eq!(server.version, scalar(&session, "SELECT VERSION()").await);
        assert_eq!(session.database().as_deref(), Some("kerno"));
        assert_eq!(session.tx_state(), TxState::Idle);
        assert!(!session.dialect().has_schemas());
        session.ping().await.unwrap();
        // The connection speaks utf8mb4, whatever the server's default.
        assert_eq!(
            scalar(&session, "SELECT @@character_set_client").await,
            "utf8mb4"
        );
        assert_eq!(
            scalar(&session, "SELECT @@character_set_results").await,
            "utf8mb4"
        );
    }
}

#[tokio::test]
async fn the_engine_is_what_the_server_says_not_what_was_asked() {
    for engine in engines() {
        let other = match engine {
            Engine::MariaDb => Engine::MySql,
            _ => Engine::MariaDb,
        };
        let mut params = testing::params(engine);
        params.engine = other;
        let session = connect(&params).await.unwrap();
        assert_eq!(session.engine(), engine);
        assert_eq!(session.dialect().engine(), engine);
    }
}

#[tokio::test]
async fn default_database_and_options() {
    for engine in engines() {
        // No database: the session is in none, and still sees everything.
        let mut params = testing::params(engine);
        params.database = None;
        let session = connect(&params).await.unwrap();
        assert_eq!(session.database(), None);
        assert_eq!(
            scalar(&session, "SELECT COUNT(*) FROM kerno.categories").await,
            "10"
        );
        assert!(
            session
                .databases()
                .await
                .unwrap()
                .iter()
                .any(|d| d.name == "kerno")
        );
        assert_eq!(
            session.objects("kerno_other").await.unwrap().relations[0].name,
            "notes"
        );

        // Options are session variables.
        let mut params = testing::params(engine);
        params
            .options
            .insert("sql_mode".into(), "ANSI_QUOTES,NO_BACKSLASH_ESCAPES".into());
        params.options.insert("wait_timeout".into(), "1234".into());
        params.options.insert("time_zone".into(), "+02:00".into());
        let session = connect(&params).await.unwrap();
        assert_eq!(
            scalar(&session, "SELECT @@session.sql_mode").await,
            "ANSI_QUOTES,NO_BACKSLASH_ESCAPES"
        );
        assert_eq!(
            scalar(&session, "SELECT @@session.wait_timeout").await,
            "1234"
        );
        assert_eq!(
            scalar(&session, "SELECT @@session.time_zone").await,
            "+02:00"
        );
        // A backslash is an ordinary character in this mode. The literal
        // the dialect writes does not depend on the mode, and the server
        // reads it as the three characters it holds.
        let literal = session.dialect().quote_string(r"a\b");
        assert_eq!(literal, "_utf8mb4 X'615c62'");
        assert_eq!(
            scalar(&session, &format!("SELECT LENGTH({literal})")).await,
            "3"
        );
        assert_eq!(scalar(&session, &format!("SELECT {literal}")).await, r"a\b");
        // The lexer, which splits what the user types, does follow the mode.
        assert!(!session.dialect().lex_rules().backslash_escapes);

        let mut params = testing::params(engine);
        params.options.insert("no_such_variable".into(), "1".into());
        let error = connect(&params).await.err().expect("an unknown variable");
        assert!(matches!(error, Error::Connect(_)), "{error:?}");
        assert!(error.to_string().contains("no_such_variable"), "{error}");
    }
}

#[tokio::test]
async fn the_dialect_follows_sql_mode_changes() {
    for engine in engines() {
        let session = common::session(engine).await;
        let text = r"C:\dir\'file'";
        let length = text.len().to_string();
        let stored = |session: &Arc<dyn Session>| {
            let literal = session.dialect().quote_string(text);
            let session = session.clone();
            async move { scalar(&session, &format!("SELECT LENGTH({literal})")).await }
        };
        assert_eq!(stored(&session).await, length);
        assert!(session.dialect().lex_rules().backslash_escapes);

        exec_ok(&session, "SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'").await;
        assert!(!session.dialect().lex_rules().backslash_escapes);
        assert_eq!(
            stored(&session).await,
            length,
            "the literal must mean the same text in this mode"
        );

        exec_ok(&session, "SELECT 1; SET sql_mode = DEFAULT").await;
        assert!(session.dialect().lex_rules().backslash_escapes);
        assert_eq!(stored(&session).await, length);
    }
}

#[tokio::test]
async fn connection_failures_say_why() {
    for engine in engines() {
        let failure = |params: ConnectParams| async move {
            match connect(&params).await {
                Err(error) => error,
                Ok(_) => panic!("the connection should have failed"),
            }
        };

        let mut params = testing::params(engine);
        params.password = Some(Secret::new("not the password"));
        let error = failure(params).await;
        assert!(matches!(error, Error::Connect(_)), "{error:?}");
        assert!(error.to_string().contains("Access denied"), "{error}");
        assert!(error.to_string().contains("1045 (28000)"), "{error}");
        assert!(
            !format!("{error} {error:?}").contains("not the password"),
            "the password must not be echoed: {error:?}"
        );

        // No password at all is its own error, so the app can ask for one.
        let mut params = testing::params(engine);
        params.password = None;
        let error = failure(params).await;
        assert!(
            matches!(error, Error::PasswordRequired),
            "{engine:?}: {error:?}"
        );

        let mut params = testing::params(engine);
        params.database = Some("no_such_database".into());
        let error = failure(params).await;
        assert!(error.to_string().contains("no_such_database"), "{error}");
        assert!(error.to_string().contains("1049 (42000)"), "{error}");

        let mut params = testing::params(engine);
        params.user = "no_such_user".into();
        let error = failure(params).await;
        assert!(matches!(error, Error::Connect(_)), "{error:?}");
        assert!(
            !format!("{error} {error:?}").contains(testing::DB_PASSWORD),
            "the password must not be echoed: {error:?}"
        );

        // Nothing listens on port 1.
        let mut params = testing::params(engine);
        params.port = 1;
        let started = Instant::now();
        let error = failure(params).await;
        assert!(matches!(error, Error::Connect(_)), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(10));

        let mut params = testing::params(engine);
        params
            .options
            .insert("connect_timeout".into(), "soon".into());
        assert!(matches!(failure(params).await, Error::Config(_)));
    }
}

#[tokio::test]
async fn tls_modes_without_verification() {
    for engine in engines() {
        // Disable must not encrypt.
        let session = connect(&with_tls(engine, tls(TlsMode::Disable, None)))
            .await
            .unwrap();
        assert_eq!(cipher(&session).await, "");
        // The server offers TLS, so prefer uses it; require insists on it.
        for mode in [TlsMode::Prefer, TlsMode::Require] {
            let session = connect(&with_tls(engine, tls(mode, None)))
                .await
                .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
            assert_ne!(cipher(&session).await, "", "{mode:?} must encrypt");
            assert_eq!(scalar(&session, "SELECT 1").await, "1");
        }
    }
}

#[tokio::test]
async fn tls_verification() {
    for engine in engines() {
        // The test CA signed the server certificate, which carries 127.0.0.1.
        for mode in [TlsMode::VerifyCa, TlsMode::VerifyFull] {
            let session = connect(&with_tls(engine, tls(mode, Some("ca.crt"))))
                .await
                .unwrap_or_else(|error| panic!("{engine:?} {mode:?}: {error}"));
            assert_ne!(cipher(&session).await, "", "{mode:?} must encrypt");

            // A CA that did not sign it.
            let error = connect(&with_tls(engine, tls(mode, Some("wrong-ca.crt"))))
                .await
                .err()
                .unwrap_or_else(|| panic!("{mode:?} must refuse an unknown CA"));
            assert!(matches!(error, Error::Tls(_)), "{mode:?}: {error:?}");
            // Without a CA file the system store is used; it does not know
            // the test CA.
            let error = connect(&with_tls(engine, tls(mode, None)))
                .await
                .err()
                .unwrap_or_else(|| panic!("{mode:?} must refuse an unknown issuer"));
            assert!(matches!(error, Error::Tls(_)), "{mode:?}: {error:?}");
        }
        // A CA file that is not there is named.
        let error = connect(&with_tls(
            engine,
            tls(TlsMode::VerifyCa, Some("absent.crt")),
        ))
        .await
        .err()
        .expect("a missing CA file");
        assert!(error.to_string().contains("absent.crt"), "{error}");

        // The non-verifying modes accept the same certificate without any CA.
        connect(&with_tls(engine, tls(TlsMode::Require, None)))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn client_certificate_authentication() {
    for engine in engines() {
        let certificate = |cert: &str, key: &str| ConnectParams {
            user: "kerno_cert".into(),
            password: None,
            tls: TlsConfig {
                mode: TlsMode::VerifyFull,
                ca_file: Some(testing::generated("tls/ca.crt")),
                cert_file: Some(testing::generated(&format!("tls/{cert}"))),
                key_file: Some(testing::generated(&format!("tls/{key}"))),
            },
            ..testing::params(engine)
        };
        let session = connect(&certificate("client.crt", "client.key"))
            .await
            .unwrap_or_else(|error| panic!("{engine:?}: {error}"));
        assert_eq!(
            scalar(&session, "SELECT CURRENT_USER()").await,
            "kerno_cert@%"
        );

        // The account requires its certificate: without one it is refused.
        let mut params = certificate("client.crt", "client.key");
        params.tls.cert_file = None;
        params.tls.key_file = None;
        assert!(connect(&params).await.is_err(), "{engine:?}");

        // A certificate with a key that is not its own cannot be used.
        let error = connect(&certificate("client.crt", "server.key"))
            .await
            .err()
            .expect("a mismatched key");
        assert!(matches!(error, Error::Tls(_)), "{engine:?}: {error:?}");
    }
}

#[tokio::test]
async fn read_only_sessions_refuse_writes() {
    for engine in engines() {
        let mut params = testing::params(engine);
        params.read_only = true;
        let session = connect(&params).await.unwrap();
        assert_eq!(
            scalar(&session, "SELECT COUNT(*) FROM kerno.categories").await,
            "10"
        );
        // ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION, for data and structure.
        for sql in [
            "DELETE FROM kerno.categories WHERE id = -1",
            "UPDATE kerno.categories SET name = name WHERE id = 1",
            "INSERT INTO kerno.audit_log VALUES (NOW(), 'x', 'y', NULL)",
            "CREATE TABLE kerno.kerno_should_not_exist (id int)",
            "CREATE DATABASE kerno_test_should_not_exist",
        ] {
            let error = db_error(&session, sql).await;
            assert_eq!(
                error.code.as_deref(),
                Some("1792 (25006)"),
                "{sql}: {error:?}"
            );
        }
        assert_eq!(session.tx_state(), TxState::Idle);
        // Nothing was written.
        let writer = common::session(engine).await;
        assert_eq!(
            scalar(
                &writer,
                "SELECT COUNT(*) FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = 'kerno' AND TABLE_NAME = 'kerno_should_not_exist'"
            )
            .await,
            "0"
        );
        assert_eq!(
            scalar(&writer, "SELECT COUNT(*) FROM kerno.audit_log").await,
            "300"
        );
    }
}

/// Opens a tunnel to the bastion with a scratch known-hosts file.
async fn tunnel(dir: &tempfile::TempDir, auth: SshAuth) -> Arc<Tunnel> {
    let host_keys = testing::trusted_host_keys(dir.path().join("known_hosts")).await;
    let ssh = testing::ssh(auth);
    let secrets = testing::secrets();
    Arc::new(
        Tunnel::open(TunnelRequest {
            ssh: &ssh,
            password: secrets.ssh_password.as_ref(),
            key_passphrase: secrets.ssh_key_passphrase.as_ref(),
            known_hosts: &host_keys.known_hosts,
            learn_into: host_keys.learn_into.as_deref(),
            trusted_fingerprint: host_keys.trusted_fingerprint.as_deref(),
        })
        .await
        .expect("open the tunnel"),
    )
}

/// The database as the bastion reaches it: by its name on the compose
/// network and the engine's own port, not a port of this machine.
fn through(engine: Engine, tunnel: &Arc<Tunnel>) -> ConnectParams {
    ConnectParams {
        host: testing::service_name(engine).into(),
        port: 3306,
        tunnel: Some(tunnel.clone()),
        ..testing::params(engine)
    }
}

/// The address the server sees this session coming from.
async fn client_address(session: &Arc<dyn Session>) -> String {
    let id = scalar(session, "SELECT CONNECTION_ID()").await;
    let host = scalar(
        session,
        &format!("SELECT HOST FROM information_schema.PROCESSLIST WHERE ID = {id}"),
    )
    .await;
    host.rsplit_once(':')
        .map_or(host.clone(), |(address, _)| address.to_string())
}

#[tokio::test]
async fn through_the_ssh_tunnel() {
    if !testing::ssh_available() {
        return;
    }
    for engine in engines() {
        let dir = tempfile::tempdir().unwrap();
        let tunnel = tunnel(&dir, SshAuth::Password).await;
        let params = through(engine, &tunnel);
        let session = connect(&params).await.expect("connect through the tunnel");
        assert_eq!(session.engine(), engine);
        assert_eq!(
            scalar(&session, "SELECT COUNT(*) FROM kerno.customers").await,
            "500"
        );
        // The server sees the bastion as the client, not this machine: a
        // direct session comes from another address.
        let direct = common::session(engine).await;
        let via_bastion = client_address(&session).await;
        assert_ne!(via_bastion, client_address(&direct).await);

        // Cancel works over the tunnel: the side connection that carries
        // the kill takes a tunnel entrance of its own.
        let mut execution = exec::run(session.clone(), SLOW.into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = Instant::now();
        session.cancel().await.expect("cancel through the tunnel");
        let error = finish(&mut execution).await.unwrap_err();
        assert_eq!(
            error.db().and_then(|db| db.code.as_deref()),
            Some("1317 (70100)"),
            "{error:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(scalar(&session, "SELECT 1").await, "1");

        // So does abandoning a statement.
        let execution = exec::run(session.clone(), SLOW.into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(execution);
        let started = Instant::now();
        assert_eq!(scalar(&session, "SELECT 2").await, "2");
        assert!(started.elapsed() < Duration::from_secs(10));

        // A second session shares the tunnel, and comes from the bastion too.
        let other = connect(&params).await.unwrap();
        assert_ne!(
            scalar(&session, "SELECT CONNECTION_ID()").await,
            scalar(&other, "SELECT CONNECTION_ID()").await
        );
        assert_eq!(client_address(&other).await, via_bastion);
    }
}

#[tokio::test]
async fn tls_through_the_tunnel() {
    if !testing::ssh_available() {
        return;
    }
    for engine in engines() {
        let dir = tempfile::tempdir().unwrap();
        let tunnel = tunnel(
            &dir,
            SshAuth::KeyFile {
                path: testing::generated("ssh/id_ed25519"),
            },
        )
        .await;
        let alias = match engine {
            Engine::MariaDb => "kerno-mariadb-alias",
            _ => "kerno-mysql-alias",
        };
        let with_mode = |mode: TlsMode, host: Option<&str>| {
            let mut params = through(engine, &tunnel);
            params.tls = tls(mode, Some("ca.crt"));
            if let Some(host) = host {
                params.host = host.into();
            }
            params
        };
        // Encrypted end to end inside the tunnel.
        let session = connect(&with_mode(TlsMode::Require, None)).await.unwrap();
        assert_ne!(cipher(&session).await, "");
        let session = connect(&with_mode(TlsMode::Disable, None)).await.unwrap();
        assert_eq!(cipher(&session).await, "");

        // The same server under a name its certificate does not carry:
        // verify-full refuses it, verify-ca checks the chain only.
        let error = connect(&with_mode(TlsMode::VerifyFull, Some(alias)))
            .await
            .err();
        assert!(
            matches!(error, Some(Error::Tls(_))),
            "{engine:?}: verify-full must refuse a wrong name: {error:?}"
        );
        let session = connect(&with_mode(TlsMode::VerifyCa, Some(alias)))
            .await
            .unwrap_or_else(|error| panic!("{engine:?} verify-ca: {error}"));
        assert_ne!(cipher(&session).await, "");
        // The chain is still checked when the name is not.
        let mut params = with_mode(TlsMode::VerifyCa, Some(alias));
        params.tls = tls(TlsMode::VerifyCa, Some("wrong-ca.crt"));
        let error = connect(&params).await.err();
        assert!(
            matches!(error, Some(Error::Tls(_))),
            "{engine:?}: {error:?}"
        );
    }
}

#[tokio::test]
async fn tunnel_to_an_unreachable_database_names_it() {
    if !testing::ssh_available() {
        return;
    }
    for engine in engines() {
        let dir = tempfile::tempdir().unwrap();
        let tunnel = tunnel(&dir, SshAuth::Password).await;
        let host = testing::service_name(engine);
        let params = ConnectParams {
            port: 1,
            ..through(engine, &tunnel)
        };
        let error = connect(&params)
            .await
            .err()
            .expect("nothing listens on port 1");
        assert!(matches!(error, Error::Ssh(_)), "{error:?}");
        assert!(error.to_string().contains(&format!("{host}:1")), "{error}");
    }
}

#[tokio::test]
async fn connector_opens_sessions_per_database() {
    if !testing::ssh_available() {
        return;
    }
    for engine in engines() {
        // Direct.
        let connector = Connector::open(
            Arc::new(MysqlDriver),
            testing::config(engine),
            testing::secrets(),
            &HostKeyStore::default(),
        )
        .await
        .unwrap();
        assert!(!connector.tunnel_closed());
        let default = connector.session(None).await.unwrap();
        assert_eq!(default.database().as_deref(), Some("kerno"));
        let other = connector.session(Some("kerno_other")).await.unwrap();
        assert_eq!(other.database().as_deref(), Some("kerno_other"));
        assert_eq!(scalar(&other, "SELECT COUNT(*) FROM notes").await, "2");

        // Through the bastion, from saved settings.
        let dir = tempfile::tempdir().unwrap();
        let host_keys = testing::trusted_host_keys(dir.path().join("known_hosts")).await;
        let mut config = testing::config(engine);
        config.host = testing::service_name(engine).into();
        config.port = 3306;
        config.ssh = Some(testing::ssh(SshAuth::Password));
        let connector = Connector::open(
            Arc::new(MysqlDriver),
            config,
            testing::secrets(),
            &host_keys,
        )
        .await
        .unwrap();
        let session = connector.session(None).await.unwrap();
        assert_eq!(scalar(&session, "SELECT DATABASE()").await, "kerno");
    }

    // A driver for both engines, and for no other.
    assert_eq!(MysqlDriver.engines(), &[Engine::MySql, Engine::MariaDb]);
    let error = Connector::open(
        Arc::new(MysqlDriver),
        testing::config(Engine::Postgres),
        testing::secrets(),
        &HostKeyStore::default(),
    )
    .await
    .err();
    assert!(matches!(error, Some(Error::Config(_))), "{error:?}");
}

#[tokio::test]
async fn connector_runs_the_bootstrap_on_every_session() {
    for engine in engines() {
        let open = |config: kerno_core::ConnectionConfig| async move {
            Connector::open(
                Arc::new(MysqlDriver),
                config,
                testing::secrets(),
                &HostKeyStore::default(),
            )
            .await
            .unwrap()
        };
        let mut config = testing::config(engine);
        // No schemas on MySQL: the default database is the database field.
        config.default_schema = Some("kerno_other".into());
        config.init_sql = vec![
            "SET time_zone = '+05:30'".into(),
            "SET @kerno_boot = 'yes'".into(),
        ];
        let connector = open(config.clone()).await;
        for database in [None, Some("kerno_other")] {
            let session = connector.session(database).await.unwrap();
            assert_eq!(
                scalar(&session, "SELECT @@session.time_zone").await,
                "+05:30",
                "{engine:?}"
            );
            assert_eq!(scalar(&session, "SELECT @kerno_boot").await, "yes");
            assert_eq!(
                scalar(&session, "SELECT DATABASE()").await,
                database.unwrap_or("kerno")
            );
        }

        config.init_sql = vec!["SET kerno_nope = 1".into()];
        let error = open(config.clone())
            .await
            .session(None)
            .await
            .err()
            .expect("the bootstrap fails");
        let message = error.to_string();
        assert!(
            message.contains("bootstrap statement 1 failed: SET kerno_nope = 1")
                && message.contains("kerno_nope"),
            "{engine:?}: {message}"
        );

        config.ignore_init_errors = true;
        config.init_sql.push("SET @kerno_boot = 'after'".into());
        let session = open(config.clone()).await.session(None).await.unwrap();
        assert_eq!(scalar(&session, "SELECT @kerno_boot").await, "after");

        // A statement that is not a plain session setting never runs.
        config.init_sql = vec!["SET SESSION TRANSACTION READ WRITE".into()];
        // Refused when the connection is opened, before anything is sent.
        let error = config.validate().expect_err("the bootstrap is refused");
        assert!(
            error.to_string().contains("bootstrap statement 1"),
            "{engine:?}: {error}"
        );
        // A read-only connection is read-only after its bootstrap.
        config.read_only = true;
        config.init_sql = vec!["SET @kerno_boot = 'ro'".into()];
        let session = open(config).await.session(None).await.unwrap();
        assert_eq!(
            scalar(&session, "SELECT @@transaction_read_only").await,
            "1",
            "{engine:?}"
        );
    }
}

/// J2 (DBeaver #5845, #14749; Beekeeper #1240): the driver through the
/// system's OpenSSH, an alias whose ProxyJump goes through the jump host;
/// a database the SSH server cannot reach is told as such.
#[tokio::test]
async fn connector_goes_through_openssh_and_a_jump_host() {
    if !testing::ssh_jump_available() {
        return;
    }
    for engine in engines() {
        if !testing::available(engine) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let fixture = testing::openssh(dir.path(), "/usr/bin/false".into(), true);
        let host_keys = HostKeyStore {
            openssh: Some(fixture.env.clone()),
            ..HostKeyStore::default()
        };
        let connector = Connector::open(
            Arc::new(MysqlDriver),
            testing::openssh_config(engine),
            testing::secrets(),
            &host_keys,
        )
        .await
        .unwrap();
        // Several sessions over the one tunnel.
        for _ in 0..2 {
            let session = connector.session(None).await.unwrap();
            assert_eq!(scalar(&session, "SELECT DATABASE()").await, "kerno");
        }
        assert!(!connector.tunnel_closed());
        drop(connector);

        let mut config = testing::openssh_config(engine);
        config.host = "kerno-no-such-database".into();
        let connector = Connector::open(
            Arc::new(MysqlDriver),
            config,
            testing::secrets(),
            &host_keys,
        )
        .await
        .unwrap();
        let error = connector
            .session(None)
            .await
            .err()
            .expect("no database there");
        assert!(
            matches!(&error, Error::Ssh(ssh) if matches!(**ssh, kerno_core::ssh::SshError::Forward { .. })),
            "{error:?}"
        );
    }
}
