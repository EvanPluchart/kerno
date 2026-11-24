//! MySQL through Windows' own `ssh.exe` (K69, D-138), against the MySQL 8.4
//! server a throwaway Windows runner starts
//! (`.github/workflows/windows-ssh-probe.yml`). Skipped elsewhere. The MySQL
//! driver dials an address (`mysql_async` takes no stream): each session goes
//! through the tunnel's single-use loopback port (`Tunnel::forward_once`),
//! then its own `ssh -W`.

#![cfg(windows)]

mod common;

use std::sync::Arc;

use common::scalar;
use kerno_core::config::{Engine, SshAuth, TlsMode};
use kerno_core::connector::{Connector, HostKeyStore};
use kerno_core::testing::{self, k69};
use kerno_mysql::MysqlDriver;

/// The runner's MySQL server, on 127.0.0.1.
const MYSQL_PORT: u16 = 53316;

/// No question is expected: a program that does not exist answers none.
const NO_ASKPASS: &str = r"C:\k69-no-askpass\askpass.exe";

#[tokio::test]
async fn a_mysql_query_through_ssh_exe() {
    if !k69::runner() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = k69::openssh(dir.path(), NO_ASKPASS.into(), true);
    let host_keys = HostKeyStore {
        openssh: Some(fixture.env.clone()),
        ..HostKeyStore::default()
    };
    let mut config = testing::config(Engine::MySql);
    config.port = MYSQL_PORT;
    config.database = Some(k69::DATABASE.into());
    config.tls.mode = TlsMode::Disable;
    config.ssh = Some(k69::ssh(
        SshAuth::KeyFile {
            path: k69::key("kerno_k69"),
        },
        None,
    ));
    let connector = Connector::open(
        Arc::new(MysqlDriver),
        config,
        testing::secrets(),
        &host_keys,
    )
    .await
    .expect("the tunnel");
    let session = connector.session(None).await.expect("a session");
    assert_eq!(scalar(&session, "SELECT DATABASE()").await, k69::DATABASE);
    eprintln!(
        "K69 PROVED: MySQL {} through ssh.exe",
        scalar(&session, "SELECT VERSION()").await
    );
    let other = connector.session(None).await.expect("a second session");
    assert_eq!(scalar(&other, "SELECT 40 + 2").await, "42");
    session.close().await;
    other.close().await;
    drop((session, other, connector));
    k69::wait_until_gone("kerno-ssh-").await;
    eprintln!("K69 PROVED: no ssh.exe left after the MySQL sessions closed");
}
