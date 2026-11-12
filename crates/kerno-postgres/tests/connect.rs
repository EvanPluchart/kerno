//! Connecting to PostgreSQL: plain, TLS in every mode, client certificates,
//! through the SSH bastion, and the errors each can produce. Runs against
//! the `kerno-postgres` and `kerno-ssh` containers; skipped with a message
//! when they are down.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{ENGINE, connect, db_error, scalar};
use kerno_core::config::{ConnectParams, Secret, SshAuth, TlsConfig, TlsMode};
use kerno_core::connector::{Connector, HostKeyStore};
use kerno_core::exec::{self, ExecEvent, RowLimit};
use kerno_core::ssh::{Tunnel, TunnelRequest};
use kerno_core::{Driver, Error, TxState, testing};
use kerno_postgres::PostgresDriver;

fn tls(mode: TlsMode, ca: Option<&str>) -> TlsConfig {
    TlsConfig {
        mode,
        ca_file: ca.map(|name| testing::generated(&format!("tls/{name}"))),
        ..TlsConfig::default()
    }
}

fn with_tls(tls: TlsConfig, host: &str) -> ConnectParams {
    ConnectParams {
        tls,
        host: host.into(),
        ..testing::params(ENGINE)
    }
}

const SSL_IN_USE: &str = "SELECT ssl::text FROM pg_stat_ssl WHERE pid = pg_backend_pid()";

#[tokio::test]
async fn reports_the_server_and_database() {
    let Some(session) = common::session().await else {
        return;
    };
    assert_eq!(session.engine(), ENGINE);
    assert_eq!(session.server().version_number.0, 17);
    assert!(
        session.server().version.starts_with("PostgreSQL 17"),
        "{}",
        session.server().version
    );
    assert_eq!(session.database().as_deref(), Some("kerno"));
    assert_eq!(session.tx_state(), TxState::Idle);
    assert!(session.dialect().has_schemas());
    session.ping().await.unwrap();
    assert_eq!(
        scalar(&session, "SELECT current_setting('application_name')").await,
        "Kerno"
    );
}

#[tokio::test]
async fn default_database_and_options() {
    if !testing::available(ENGINE) {
        return;
    }
    let mut params = testing::params(ENGINE);
    params.database = None;
    let session = connect(&params).await.unwrap();
    assert_eq!(session.database().as_deref(), Some("postgres"));

    let mut params = testing::params(ENGINE);
    params
        .options
        .insert("application_name".into(), "kerno tests".into());
    params
        .options
        .insert("search_path".into(), "shop, public".into());
    params
        .options
        .insert("statement_timeout".into(), "12345".into());
    let session = connect(&params).await.unwrap();
    assert_eq!(
        scalar(&session, "SELECT current_setting('application_name')").await,
        "kerno tests"
    );
    assert_eq!(scalar(&session, "SHOW search_path").await, "shop, public");
    assert_eq!(scalar(&session, "SHOW statement_timeout").await, "12345ms");
    // search_path took effect: an unqualified shop table resolves.
    assert_eq!(
        scalar(&session, "SELECT count(*) FROM categories").await,
        "10"
    );
}

#[tokio::test]
async fn connection_failures_say_why() {
    if !testing::available(ENGINE) {
        return;
    }
    let failure = |params: ConnectParams| async move {
        match connect(&params).await {
            Err(error) => error,
            Ok(_) => panic!("the connection should have failed"),
        }
    };

    let mut params = testing::params(ENGINE);
    params.password = Some(Secret::new("not the password"));
    let error = failure(params).await;
    assert!(matches!(error, Error::Connect(_)), "{error:?}");
    assert!(
        error.to_string().contains("password authentication failed"),
        "{error}"
    );
    assert!(error.to_string().contains("28P01"), "{error}");
    assert!(
        !error.to_string().contains("not the password"),
        "the password must not be echoed: {error}"
    );

    let mut params = testing::params(ENGINE);
    params.database = Some("no_such_database".into());
    let error = failure(params).await;
    assert!(error.to_string().contains("no_such_database"), "{error}");
    assert!(error.to_string().contains("3D000"), "{error}");

    let mut params = testing::params(ENGINE);
    params.user = "no_such_user".into();
    let error = failure(params).await;
    assert!(matches!(error, Error::Connect(_)), "{error:?}");

    // Nothing listens on port 1.
    let mut params = testing::params(ENGINE);
    params.port = 1;
    let started = Instant::now();
    let error = failure(params).await;
    assert!(matches!(error, Error::Connect(_)), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(10));

    let mut params = testing::params(ENGINE);
    params
        .options
        .insert("connect_timeout".into(), "soon".into());
    assert!(matches!(failure(params).await, Error::Config(_)));
}

#[tokio::test]
async fn tls_modes_without_verification() {
    if !testing::available(ENGINE) {
        return;
    }
    for (mode, expected) in [
        (TlsMode::Disable, "false"),
        // The server offers TLS, so prefer uses it.
        (TlsMode::Prefer, "true"),
        (TlsMode::Require, "true"),
    ] {
        let session = connect(&with_tls(tls(mode, None), "127.0.0.1"))
            .await
            .unwrap();
        assert_eq!(scalar(&session, SSL_IN_USE).await, expected, "{mode:?}");
    }
}

#[tokio::test]
async fn tls_verification() {
    if !testing::available(ENGINE) {
        return;
    }
    // The server certificate is signed by the test CA for localhost and 127.0.0.1.
    for host in ["localhost", "127.0.0.1"] {
        for mode in [TlsMode::VerifyCa, TlsMode::VerifyFull] {
            let session = connect(&with_tls(tls(mode, Some("ca.crt")), host))
                .await
                .unwrap_or_else(|error| panic!("{mode:?} to {host}: {error}"));
            assert_eq!(scalar(&session, SSL_IN_USE).await, "true");
        }
    }

    // A CA that did not sign the certificate: both verifying modes refuse.
    for mode in [TlsMode::VerifyCa, TlsMode::VerifyFull] {
        let error = connect(&with_tls(tls(mode, Some("wrong-ca.crt")), "127.0.0.1"))
            .await
            .err()
            .unwrap_or_else(|| panic!("{mode:?} must refuse an unknown CA"));
        assert!(matches!(error, Error::Tls(_)), "{mode:?}: {error:?}");
    }

    // Without a CA file the system store is used; it does not know the test CA.
    let error = connect(&with_tls(tls(TlsMode::VerifyFull, None), "127.0.0.1"))
        .await
        .err();
    assert!(matches!(error, Some(Error::Tls(_))), "{error:?}");

    // The non-verifying modes accept the same certificate without any CA.
    connect(&with_tls(tls(TlsMode::Require, None), "127.0.0.1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn client_certificate_authentication() {
    if !testing::available(ENGINE) {
        return;
    }
    let certificate = |cert: &str, key: &str| ConnectParams {
        user: "kerno_cert".into(),
        password: None,
        host: "localhost".into(),
        tls: TlsConfig {
            mode: TlsMode::VerifyFull,
            ca_file: Some(testing::generated("tls/ca.crt")),
            cert_file: Some(testing::generated(&format!("tls/{cert}"))),
            key_file: Some(testing::generated(&format!("tls/{key}"))),
        },
        ..testing::params(ENGINE)
    };
    let session = connect(&certificate("client.crt", "client.key"))
        .await
        .unwrap();
    assert_eq!(
        scalar(&session, "SELECT current_user::text").await,
        "kerno_cert"
    );

    // The role only authenticates by certificate: without one it is refused.
    let mut params = certificate("client.crt", "client.key");
    params.tls.cert_file = None;
    params.tls.key_file = None;
    assert!(connect(&params).await.is_err());

    // A certificate with a key that is not its own cannot be used.
    let error = connect(&certificate("client.crt", "server.key"))
        .await
        .err();
    assert!(error.is_some(), "a mismatched key must not authenticate");
}

#[tokio::test]
async fn read_only_sessions_refuse_writes() {
    if !testing::available(ENGINE) {
        return;
    }
    let mut params = testing::params(ENGINE);
    params.read_only = true;
    let session = connect(&params).await.unwrap();
    assert_eq!(
        scalar(&session, "SELECT count(*) FROM shop.categories").await,
        "10"
    );
    let error = db_error(&session, "DELETE FROM shop.categories WHERE id = -1").await;
    assert_eq!(error.code.as_deref(), Some("25006"), "{error:?}");
    let error = db_error(
        &session,
        "CREATE TABLE public.kerno_should_not_exist (id int)",
    )
    .await;
    assert_eq!(error.code.as_deref(), Some("25006"), "{error:?}");
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

#[tokio::test]
async fn through_the_ssh_tunnel() {
    if !testing::available(ENGINE) || !testing::ssh_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tunnel = tunnel(&dir, SshAuth::Password).await;

    // The database host is the name the bastion resolves, not localhost.
    let params = ConnectParams {
        host: testing::service_name(ENGINE).into(),
        port: 5432,
        tunnel: Some(tunnel.clone()),
        ..testing::params(ENGINE)
    };
    let session = connect(&params).await.expect("connect through the tunnel");
    assert_eq!(
        scalar(&session, "SELECT count(*) FROM shop.customers").await,
        "500"
    );
    // The server sees the bastion as the client, not this machine's loopback.
    let client = scalar(&session, "SELECT host(inet_client_addr())").await;
    assert_ne!(client, "127.0.0.1");

    // Streaming and cancel work over the tunnel too.
    let mut execution = exec::run(session.clone(), "SELECT pg_sleep(30)".into(), RowLimit::All);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    session.cancel().await.expect("cancel through the tunnel");
    let error = loop {
        match common::next_event(&mut execution).await {
            Some(ExecEvent::Finished(result)) => break result.unwrap_err(),
            Some(_) => {}
            None => panic!("the execution ended without a result"),
        }
    };
    assert_eq!(
        error.db().and_then(|db| db.code.as_deref()),
        Some("57014"),
        "{error:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(scalar(&session, "SELECT 1").await, "1");

    // A second session shares the tunnel.
    let other = connect(&params).await.unwrap();
    assert_ne!(
        scalar(&session, "SELECT pg_backend_pid()").await,
        scalar(&other, "SELECT pg_backend_pid()").await
    );
}

#[tokio::test]
async fn tls_through_the_tunnel_checks_the_database_host_name() {
    if !testing::available(ENGINE) || !testing::ssh_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tunnel = tunnel(
        &dir,
        SshAuth::KeyFile {
            path: testing::generated("ssh/id_ed25519"),
        },
    )
    .await;
    let through = |host: &str, mode: TlsMode| ConnectParams {
        host: host.into(),
        port: 5432,
        tunnel: Some(tunnel.clone()),
        tls: tls(mode, Some("ca.crt")),
        ..testing::params(ENGINE)
    };

    // The certificate names kerno-postgres: verify-full passes, and the
    // check is against that name, not against a tunnel-local address.
    let session = connect(&through("kerno-postgres", TlsMode::VerifyFull))
        .await
        .unwrap();
    assert_eq!(scalar(&session, SSL_IN_USE).await, "true");

    // The same server under a name its certificate does not carry.
    let error = connect(&through("kerno-pg-alias", TlsMode::VerifyFull))
        .await
        .err();
    assert!(
        matches!(error, Some(Error::Tls(_))),
        "verify-full must refuse a wrong name: {error:?}"
    );
    // verify-ca checks the chain only.
    let session = connect(&through("kerno-pg-alias", TlsMode::VerifyCa))
        .await
        .unwrap();
    assert_eq!(scalar(&session, SSL_IN_USE).await, "true");
}

#[tokio::test]
async fn tunnel_to_an_unreachable_database_names_it() {
    if !testing::available(ENGINE) || !testing::ssh_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tunnel = tunnel(&dir, SshAuth::Password).await;
    let params = ConnectParams {
        host: "kerno-postgres".into(),
        port: 1,
        tunnel: Some(tunnel),
        ..testing::params(ENGINE)
    };
    let error = connect(&params)
        .await
        .err()
        .expect("nothing listens on port 1");
    assert!(matches!(error, Error::Ssh(_)), "{error:?}");
    assert!(error.to_string().contains("kerno-postgres:1"), "{error}");
}

#[tokio::test]
async fn connector_opens_sessions_per_database() {
    if !testing::available(ENGINE) || !testing::ssh_available() {
        return;
    }
    // Direct.
    let connector = Connector::open(
        Arc::new(PostgresDriver),
        testing::config(ENGINE),
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
    assert_eq!(scalar(&other, "SELECT count(*) FROM notes").await, "2");

    // Through the bastion, from saved settings.
    let dir = tempfile::tempdir().unwrap();
    let host_keys = testing::trusted_host_keys(dir.path().join("known_hosts")).await;
    let mut config = testing::config(ENGINE);
    config.host = testing::service_name(ENGINE).into();
    config.port = 5432;
    config.ssh = Some(testing::ssh(SshAuth::Password));
    let connector = Connector::open(
        Arc::new(PostgresDriver),
        config,
        testing::secrets(),
        &host_keys,
    )
    .await
    .unwrap();
    let session = connector.session(None).await.unwrap();
    assert_eq!(
        scalar(&session, "SELECT current_database()::text").await,
        "kerno"
    );

    // Settings that cannot work are refused before any network traffic.
    let mut config = testing::config(ENGINE);
    config.host = String::new();
    let error = Connector::open(
        Arc::new(PostgresDriver),
        config,
        testing::secrets(),
        &HostKeyStore::default(),
    )
    .await
    .err();
    assert!(matches!(error, Some(Error::Config(_))), "{error:?}");
    let config = testing::config(kerno_core::Engine::MySql);
    let error = Connector::open(
        Arc::new(PostgresDriver),
        config,
        testing::secrets(),
        &HostKeyStore::default(),
    )
    .await
    .err();
    assert!(matches!(error, Some(Error::Config(_))), "{error:?}");
    assert_eq!(PostgresDriver.engines(), &[ENGINE]);
}
