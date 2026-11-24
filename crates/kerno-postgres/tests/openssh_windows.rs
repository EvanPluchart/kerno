//! PostgreSQL through Windows' own `ssh.exe` (K69, D-138): a query, a
//! cancelled query and a database the SSH server cannot reach, against the
//! servers a throwaway Windows runner starts
//! (`.github/workflows/windows-ssh-probe.yml`). Skipped elsewhere.

#![cfg(windows)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::scalar;
use kerno_core::Error;
use kerno_core::config::SshAuth;
use kerno_core::connector::{Connector, HostKeyStore};
use kerno_core::exec::{self, ExecEvent, RowLimit};
use kerno_core::ssh::SshError;
use kerno_core::testing::{self, k69};
use kerno_postgres::PostgresDriver;

/// No question is expected: a program that does not exist answers none.
const NO_ASKPASS: &str = r"C:\k69-no-askpass\askpass.exe";

#[tokio::test]
async fn a_query_and_a_cancel_through_ssh_exe() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), NO_ASKPASS.into(), true);
    let host_keys = HostKeyStore {
        openssh: Some(fixture.env.clone()),
        ..HostKeyStore::default()
    };
    let key = SshAuth::KeyFile {
        path: k69::key("kerno_k69"),
    };
    let connector = Connector::open(
        Arc::new(PostgresDriver),
        k69::config(k69::ssh(key.clone(), None)),
        testing::secrets(),
        &host_keys,
    )
    .await
    .expect("the tunnel");
    let marker = connector_marker();
    let session = connector.session(None).await.expect("a session");
    assert_eq!(
        scalar(&session, "SELECT current_database()::text").await,
        k69::DATABASE
    );
    eprintln!(
        "K69 PROVED: {} through ssh.exe",
        scalar(&session, "SELECT version()").await
    );
    // A second session over the one tunnel: its own ssh.exe.
    let other = connector.session(None).await.expect("a second session");
    assert_eq!(scalar(&other, "SELECT 40 + 2").await, "42");

    // A cancel request goes through a new stream, then that ssh.exe ends.
    let mut execution = exec::run(session.clone(), "SELECT pg_sleep(30)".into(), RowLimit::All);
    tokio::time::sleep(Duration::from_millis(500)).await;
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
    eprintln!("K69 PROVED: a query cancelled through ssh.exe");

    session.close().await;
    other.close().await;
    drop(session);
    drop(other);
    drop(execution);
    drop(connector);
    k69::wait_until_gone(&marker).await;
    eprintln!("K69 PROVED: no ssh.exe left after the sessions and the tunnel closed");

    // A database the SSH server cannot reach: said by the SSH server.
    let mut config = k69::config(k69::ssh(key, None));
    config.port = 1;
    let connector = Connector::open(
        Arc::new(PostgresDriver),
        config,
        testing::secrets(),
        &host_keys,
    )
    .await
    .expect("the tunnel logs in");
    let error = connector
        .session(None)
        .await
        .err()
        .expect("nothing listens there");
    assert!(
        matches!(&error, Error::Ssh(ssh) if matches!(**ssh, SshError::Forward { .. })),
        "{error:?}"
    );
    eprintln!("K69 PROVED: a refused forward: {error}");
}

/// Every `ssh.exe` of the tests names its tunnel's folder, `kerno-ssh-…`
/// (`-F kerno-ssh-…/config`): what the waits look for.
fn connector_marker() -> String {
    "kerno-ssh-".into()
}

/// A TLS file of another computer named by a connection built by hand (as
/// an imported one: no form on the way) is refused by the driver's connect
/// before it is read. Mutation seen: the driver's check removed (the TLS
/// reader's own error instead).
#[tokio::test]
async fn a_tls_file_of_another_computer_is_never_opened() {
    if !k69::runner() {
        return;
    }
    let share = r"\\k69-nowhere\share";
    let mut config = testing::config(kerno_core::config::Engine::Postgres);
    config.port = k69::PG_PORT;
    config.database = Some(k69::DATABASE.into());
    config.tls.mode = kerno_core::config::TlsMode::VerifyCa;
    config.tls.ca_file = Some(format!(r"{share}\ca.pem").into());
    let connector = Connector::open(
        Arc::new(PostgresDriver),
        config,
        testing::secrets(),
        &HostKeyStore::default(),
    )
    .await
    .expect("no tunnel");
    let Err(error) = connector.session(None).await else {
        panic!("refused");
    };
    assert!(
        matches!(&error, Error::Config(message) if message.contains("CA certificate")
            && message.contains("not a file of this computer")),
        "{error:?}"
    );
    eprintln!("K69 PROVED: {error}");
}

/// The built-in SSH client's key of another computer is refused before it
/// is read. Mutation seen: the check in `ssh::authenticate` removed (the
/// file's own read error instead).
#[tokio::test]
async fn a_built_in_client_key_of_another_computer_is_never_read() {
    if !k69::runner() {
        return;
    }
    let share = r"\\k69-nowhere\share";
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), NO_ASKPASS.into(), true);
    let mut ssh = k69::ssh(
        SshAuth::KeyFile {
            path: format!(r"{share}\id").into(),
        },
        None,
    );
    ssh.client = kerno_core::config::SshClient::Builtin;
    let error = kerno_core::ssh::Tunnel::open(kerno_core::ssh::TunnelRequest {
        ssh: &ssh,
        password: None,
        key_passphrase: None,
        known_hosts: std::slice::from_ref(&fixture.known_hosts),
        learn_into: None,
        trusted_fingerprint: None,
    })
    .await
    .expect_err("refused");
    assert!(
        matches!(&error, SshError::Key { reason, .. } if reason.contains("another computer")),
        "{error:?}"
    );
    eprintln!("K69 PROVED: {error}");
}
