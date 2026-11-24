//! Tunnels through the system's OpenSSH (CLAUDE.md §4 J2; DBeaver #5845,
//! #14749, #4516, #17976; Beekeeper #1240, #1344, #940), against the
//! `kerno-ssh` bastion and the `kerno-ssh-jump` host in front of it. Each
//! test writes its own `ssh_config` and known_hosts: the developer's SSH
//! files are never read.

// The tunnel runs on Unix sockets: Unix only (K64, D-131).
#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use kerno_core::Error;
use kerno_core::config::{
    ConnectionConfig, Engine, Secret, Secrets, SshAuth, SshClient, SshConfig,
};
use kerno_core::connector::{HostKeyStore, open_tunnel};
use kerno_core::error::{FailureKind, explain};
use kerno_core::ssh::{SshError, Tunnel};
use kerno_core::testing::{self, OpenSshFixture};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The askpass program: what the app's `main` does first, alone.
const ASKPASS: &str = env!("CARGO_BIN_EXE_kerno-askpass");

/// PostgreSQL's SSLRequest packet. A server with TLS on answers `S`.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];

struct Setup {
    dir: tempfile::TempDir,
    fixture: OpenSshFixture,
}

/// `None` (after printing why) when the containers are down. With `known`,
/// the containers' host keys are trusted already.
fn setup(known: bool) -> Option<Setup> {
    if !testing::ssh_jump_available()
        || !testing::available(Engine::Postgres)
        || !testing::available(Engine::MySql)
    {
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = testing::openssh(dir.path(), ASKPASS.into(), known);
    Some(Setup { dir, fixture })
}

/// OpenSSH settings: `port` 0 and an empty `user` leave them to the file.
fn openssh(host: &str, port: u16, user: &str, auth: SshAuth, jump: Option<&str>) -> SshConfig {
    SshConfig {
        host: host.into(),
        port,
        user: user.into(),
        auth,
        keepalive_secs: 30,
        client: SshClient::OpenSsh,
        jump: jump.map(str::to_string),
    }
}

fn key(name: &str) -> SshAuth {
    SshAuth::KeyFile {
        path: testing::generated(&format!("ssh/{name}")),
    }
}

/// The test server of `engine` as the bastion sees it, through `ssh`.
fn through(engine: Engine, ssh: SshConfig) -> ConnectionConfig {
    let mut config = testing::config(engine);
    config.host = testing::service_name(engine).into();
    config.port = match engine {
        Engine::Postgres => 5432,
        Engine::MySql | Engine::MariaDb => 3306,
        Engine::Sqlite => unreachable!("SQLite has no server to tunnel to"),
    };
    config.ssh = Some(ssh);
    config
}

impl Setup {
    async fn open(
        &self,
        config: &ConnectionConfig,
        passphrase: Option<&str>,
        trusted: Option<&str>,
    ) -> Result<Arc<Tunnel>, SshError> {
        let secrets = Secrets {
            ssh_key_passphrase: passphrase.map(Secret::new),
            ..Secrets::default()
        };
        let host_keys = HostKeyStore {
            // The built-in client's files (the RSA test uses it too).
            known_hosts: vec![self.fixture.known_hosts.clone()],
            trusted_fingerprint: trusted.map(str::to_string),
            openssh: Some(self.fixture.env.clone()),
            ..HostKeyStore::default()
        };
        match open_tunnel(config, &secrets, &host_keys).await {
            Ok(tunnel) => Ok(tunnel.expect("a tunnel")),
            Err(Error::Ssh(error)) => Err(*error),
            Err(other) => panic!("expected an SSH error, got {other}"),
        }
    }

    fn known_hosts(&self) -> String {
        std::fs::read_to_string(&self.fixture.known_hosts).unwrap_or_default()
    }
}

/// Checks that the database at the end of the tunnel answers.
async fn probe(tunnel: &Tunnel, config: &ConnectionConfig) {
    let mut stream = tunnel
        .open_stream(&config.host, config.port)
        .await
        .expect("open a stream");
    let mut answer = [0u8; 5];
    let read = async {
        match config.engine {
            Engine::Postgres => {
                stream.write_all(&SSL_REQUEST).await.unwrap();
                stream.read_exact(&mut answer[..1]).await.unwrap();
                assert_eq!(answer[0], b'S', "PostgreSQL answers the SSLRequest");
            }
            // The server speaks first: a packet header, then protocol 10.
            Engine::MySql | Engine::MariaDb => {
                stream.read_exact(&mut answer).await.unwrap();
                assert_eq!(answer[4], 10, "MySQL's handshake");
            }
            Engine::Sqlite => unreachable!("SQLite has no server to tunnel to"),
        }
    };
    tokio::time::timeout(Duration::from_secs(15), read)
        .await
        .expect("no answer through the tunnel");
}

#[tokio::test]
async fn an_ed25519_key_reaches_postgres_and_mysql() {
    let Some(setup) = setup(true) else {
        return;
    };
    for engine in [Engine::Postgres, Engine::MySql] {
        let ssh = openssh(
            "127.0.0.1",
            testing::SSH_PORT,
            "kerno",
            key("id_ed25519"),
            None,
        );
        let config = through(engine, ssh);
        let tunnel = setup.open(&config, None, None).await.expect("tunnel");
        probe(&tunnel, &config).await;
        // A driver that dials an address gets a single-use loopback port.
        let address = tunnel
            .forward_once(&config.host, config.port)
            .await
            .unwrap();
        assert!(address.ip().is_loopback());
        assert!(!tunnel.is_closed());
    }
}

/// GOAL-2 criterion 2: an RSA key file works through OpenSSH (the built-in
/// client, built without the `rsa` crate, refuses it and says where to go).
#[tokio::test]
async fn an_rsa_key_file_works_through_openssh() {
    let Some(setup) = setup(true) else {
        return;
    };
    let ssh = openssh("127.0.0.1", testing::SSH_PORT, "kerno", key("id_rsa"), None);
    let config = through(Engine::Postgres, ssh.clone());
    let tunnel = setup.open(&config, None, None).await.expect("RSA tunnel");
    probe(&tunnel, &config).await;

    let builtin = through(
        Engine::Postgres,
        SshConfig {
            client: SshClient::Builtin,
            ..ssh
        },
    );
    match setup.open(&builtin, None, None).await {
        Err(error @ SshError::RsaKeyFile { .. }) => {
            assert!(error.to_string().contains("OpenSSH"), "{error}")
        }
        other => panic!("the built-in client must refuse an RSA key: {other:?}"),
    }
}

#[tokio::test]
async fn a_passphrase_protected_key_is_opened_through_askpass() {
    let Some(setup) = setup(true) else {
        return;
    };
    let ssh = openssh(
        "127.0.0.1",
        testing::SSH_PORT,
        "kerno",
        key("id_ed25519_enc"),
        None,
    );
    let config = through(Engine::Postgres, ssh);

    // No passphrase stored: ssh asked, Kerno can ask the user.
    match setup.open(&config, None, None).await {
        Err(SshError::Passphrase { path, reason }) => {
            assert_eq!(reason, "a passphrase is required");
            assert!(path.ends_with("id_ed25519_enc"), "{path}");
        }
        other => panic!("expected a passphrase question, got {other:?}"),
    }
    // A wrong one is told apart.
    match setup.open(&config, Some("not the passphrase"), None).await {
        Err(SshError::Passphrase { reason, .. }) => assert_eq!(reason, "the passphrase is wrong"),
        other => panic!("expected a wrong passphrase, got {other:?}"),
    }
    let tunnel = setup
        .open(&config, Some(testing::SSH_KEY_PASSPHRASE), None)
        .await
        .expect("tunnel with the passphrase");
    probe(&tunnel, &config).await;
}

/// The Jump host field (`ssh -J`), as an alias and as `user@host:port`.
#[tokio::test]
async fn the_jump_host_field_goes_through_the_jump_host() {
    let Some(setup) = setup(true) else {
        return;
    };
    for jump in ["kerno-jump", "kerno@localhost:52223"] {
        let ssh = openssh("kerno-ssh", 22, "kerno", key("id_ed25519"), Some(jump));
        let config = through(Engine::MySql, ssh);
        let tunnel = setup.open(&config, None, None).await.expect(jump);
        probe(&tunnel, &config).await;
    }
}

/// An alias whose `ProxyJump`, port, user and key are all in the file.
#[tokio::test]
async fn a_proxyjump_alias_needs_no_other_field() {
    let Some(setup) = setup(true) else {
        return;
    };
    let ssh = openssh(
        "kerno-behind-jump",
        0,
        "",
        SshAuth::Agent { socket: None },
        None,
    );
    let config = through(Engine::Postgres, ssh);
    let tunnel = setup.open(&config, None, None).await.expect("alias tunnel");
    probe(&tunnel, &config).await;
}

#[tokio::test]
async fn an_unknown_host_key_is_shown_then_trusted_then_remembered() {
    let Some(setup) = setup(false) else {
        return;
    };
    let ssh = openssh(
        "127.0.0.1",
        testing::SSH_PORT,
        "kerno",
        key("id_ed25519"),
        None,
    );
    let config = through(Engine::Postgres, ssh);

    let fingerprint = match setup.open(&config, None, None).await {
        Err(SshError::UnknownHostKey {
            host,
            port,
            algorithm,
            fingerprint,
        }) => {
            assert_eq!((host.as_str(), port), ("127.0.0.1", testing::SSH_PORT));
            assert_eq!(algorithm, "ED25519");
            assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
            fingerprint
        }
        other => panic!("expected an unknown host key, got {other:?}"),
    };
    assert_eq!(setup.known_hosts(), "", "nothing recorded before consent");
    let explained = explain(
        &Error::from(SshError::UnknownHostKey {
            host: "127.0.0.1".into(),
            port: testing::SSH_PORT,
            algorithm: "ED25519".into(),
            fingerprint: fingerprint.clone(),
        })
        .to_string(),
        &config,
    );
    assert_eq!(explained.kind, FailureKind::SshHostKeyUnknown);

    // Another fingerprint is not consent.
    let wrong = setup
        .open(
            &config,
            None,
            Some("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        )
        .await;
    assert!(
        matches!(wrong, Err(SshError::UnknownHostKey { .. })),
        "{wrong:?}"
    );

    // Trusted: OpenSSH records it in its known_hosts, and then knows it.
    let tunnel = setup
        .open(&config, None, Some(&fingerprint))
        .await
        .expect("trusted");
    probe(&tunnel, &config).await;
    assert!(
        setup.known_hosts().contains("[127.0.0.1]:52222"),
        "{}",
        setup.known_hosts()
    );
    drop(tunnel);
    setup.open(&config, None, None).await.expect("remembered");

    // Behind a jump host each hop is asked about, the jump host first.
    let ssh = openssh(
        "kerno-ssh",
        22,
        "kerno",
        key("id_ed25519"),
        Some("kerno-jump"),
    );
    let config = through(Engine::Postgres, ssh);
    // Trust what each question shows, as the user would (one per hop).
    let mut asked = Vec::new();
    let mut trusted: Option<String> = None;
    let tunnel = loop {
        match setup.open(&config, None, trusted.as_deref()).await {
            Ok(tunnel) => break tunnel,
            Err(SshError::UnknownHostKey {
                host,
                port,
                fingerprint,
                ..
            }) if asked.len() < 3 => {
                asked.push((host, port));
                trusted = Some(fingerprint);
            }
            other => panic!("expected a host key question, got {other:?}"),
        }
    };
    assert_eq!(
        asked.first(),
        Some(&("127.0.0.1".to_string(), testing::SSH_JUMP_PORT)),
        "the jump host is asked about first: {asked:?}"
    );
    probe(&tunnel, &config).await;
}

#[tokio::test]
async fn a_changed_host_key_is_refused_even_when_trusted() {
    let Some(setup) = setup(true) else {
        return;
    };
    // Another key recorded for the bastion: the user's own key will do.
    let other = std::fs::read_to_string(testing::generated("ssh/id_ed25519.pub")).unwrap();
    let other = other
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    std::fs::write(
        &setup.fixture.known_hosts,
        format!("[127.0.0.1]:{} {other}\n", testing::SSH_PORT),
    )
    .unwrap();
    let ssh = openssh(
        "127.0.0.1",
        testing::SSH_PORT,
        "kerno",
        key("id_ed25519"),
        None,
    );
    let config = through(Engine::Postgres, ssh);
    for trusted in [None, Some("SHA256:anything")] {
        match setup.open(&config, None, trusted).await {
            Err(error @ SshError::HostKeyChanged { .. }) => {
                let SshError::HostKeyChanged {
                    host,
                    port,
                    fingerprint,
                    file,
                    line,
                } = &error
                else {
                    unreachable!()
                };
                assert_eq!((host.as_str(), *port), ("127.0.0.1", testing::SSH_PORT));
                assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
                assert!(file.ends_with("known_hosts"), "{file}");
                assert_eq!(*line, 1);
                let explained = explain(&Error::from(error.clone()).to_string(), &config);
                assert_eq!(explained.kind, FailureKind::SshHostKeyChanged);
            }
            other => panic!("expected a changed host key, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_key_the_server_does_not_know_is_explained() {
    let Some(setup) = setup(true) else {
        return;
    };
    let stranger = setup.dir.path().join("id_stranger");
    let made = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&stranger)
        .status()
        .unwrap();
    assert!(made.success());
    let ssh = openssh(
        "127.0.0.1",
        testing::SSH_PORT,
        "kerno",
        SshAuth::KeyFile { path: stranger },
        None,
    );
    let config = through(Engine::Postgres, ssh);
    let error = setup.open(&config, None, None).await.unwrap_err();
    assert!(
        matches!(&error, SshError::OpenSsh { stderr } if stderr.contains("Permission denied")),
        "{error:?}"
    );
    let explained = explain(&Error::from(error).to_string(), &config);
    assert_eq!(explained.kind, FailureKind::SshAuthentication);
    assert_eq!(
        explained.headline,
        "The SSH server refused the login of “kerno@127.0.0.1”."
    );
    assert!(
        explained.raw.contains("Permission denied"),
        "{}",
        explained.raw
    );
}

#[tokio::test]
async fn a_jump_host_that_does_not_answer_is_named() {
    let Some(setup) = setup(true) else {
        return;
    };
    // Nothing listens on the port after the jump host's.
    let ssh = openssh(
        "kerno-ssh",
        22,
        "kerno",
        key("id_ed25519"),
        Some("kerno@127.0.0.1:52224"),
    );
    let config = through(Engine::Postgres, ssh);
    let error = setup.open(&config, None, None).await.unwrap_err();
    let explained = explain(&Error::from(error).to_string(), &config);
    assert_eq!(explained.kind, FailureKind::SshUnreachable, "{explained:?}");
    assert!(
        explained.headline.contains("jump host"),
        "{}",
        explained.headline
    );
    assert!(
        explained.headline.contains("127.0.0.1"),
        "{}",
        explained.headline
    );
}

#[tokio::test]
async fn a_database_the_server_cannot_reach_is_explained() {
    let Some(setup) = setup(true) else {
        return;
    };
    let ssh = openssh(
        "127.0.0.1",
        testing::SSH_PORT,
        "kerno",
        key("id_ed25519"),
        None,
    );
    let mut config = through(Engine::Postgres, ssh);
    config.host = "kerno-no-such-database".into();
    let tunnel = setup.open(&config, None, None).await.expect("tunnel");
    let mark = tunnel.forward_mark();
    let mut stream = tunnel.open_stream(&config.host, config.port).await.unwrap();
    // OpenSSH takes the connection, then closes it.
    let _ = stream.write_all(&SSL_REQUEST).await;
    let mut byte = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(15), stream.read(&mut byte)).await;
    assert!(matches!(read, Ok(Ok(0)) | Ok(Err(_))), "{read:?}");
    match tunnel
        .forward_failure(mark, &config.host, config.port)
        .await
    {
        Some(error @ SshError::Forward { .. }) => {
            let explained = explain(&Error::from(error).to_string(), &config);
            assert_eq!(explained.kind, FailureKind::SshForward);
        }
        other => panic!("expected a refused forward, got {other:?}"),
    }
}

/// `ssh`, the jump host's `ssh` and the private directory are gone once
/// the tunnel is dropped.
#[tokio::test]
async fn nothing_is_left_after_the_tunnel_is_dropped() {
    let Some(setup) = setup(true) else {
        return;
    };
    let ssh = openssh(
        "kerno-ssh",
        22,
        "kerno",
        key("id_ed25519"),
        Some("kerno-jump"),
    );
    let config = through(Engine::Postgres, ssh);
    let tunnel = setup.open(&config, None, None).await.expect("tunnel");
    let marker = tunnel
        .openssh_dir()
        .expect("an OpenSSH tunnel")
        .display()
        .to_string();
    // The tunnel's `ssh` and the jump host's both name the tunnel's directory.
    let running = processes(&marker);
    assert!(running.len() >= 2, "ssh and its jump host run: {running:?}");
    let socket = running
        .iter()
        .find_map(|command| command.split(" -L ").nth(1))
        .and_then(|forward| forward.split(':').next())
        .map(std::path::PathBuf::from)
        .expect("the forwarded socket");
    let dir = socket.parent().unwrap().to_path_buf();
    assert!(socket.exists());
    let stream = tunnel.open_stream(&config.host, config.port).await.unwrap();
    drop(stream);
    drop(tunnel);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !processes(&marker).is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "an ssh process outlived its tunnel: {:?}",
            processes(&marker)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!dir.exists(), "{} is left", dir.display());
}

/// The command lines of the processes that contain `marker`.
fn processes(marker: &str) -> Vec<String> {
    let output = std::process::Command::new("ps")
        .args(["axww", "-o", "command="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(marker) && !line.starts_with("ps "))
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn without_openssh_the_connection_says_so() {
    let ssh = openssh("bastion", 0, "", SshAuth::Agent { socket: None }, None);
    let config = through(Engine::Postgres, ssh);
    let error = open_tunnel(&config, &Secrets::default(), &HostKeyStore::default())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("OpenSSH was not found on this computer; use the built-in client"),
        "{error}"
    );
}

/// A key with a passphrase that the jump host and the target both use: each
/// `ssh` asks for it, and each gets it (not "the passphrase is wrong").
#[tokio::test]
async fn one_encrypted_key_opens_the_jump_host_and_the_target() {
    let Some(setup) = setup(true) else {
        return;
    };
    let encrypted = testing::generated("ssh/id_ed25519_enc");
    let text = std::fs::read_to_string(&setup.fixture.config)
        .unwrap()
        .replace(
            &testing::generated("ssh/id_ed25519").display().to_string(),
            &encrypted.display().to_string(),
        );
    std::fs::write(&setup.fixture.config, text).unwrap();
    let ssh = openssh(
        "kerno-ssh",
        22,
        "kerno",
        key("id_ed25519_enc"),
        Some("kerno-jump"),
    );
    let config = through(Engine::Postgres, ssh);
    let tunnel = setup
        .open(&config, Some(testing::SSH_KEY_PASSPHRASE), None)
        .await
        .expect("both hops opened with the one passphrase");
    probe(&tunnel, &config).await;
}

/// An OpenSSH tunnel whose `ssh` died says it is closed (auto-reconnect
/// relies on it).
#[tokio::test]
async fn a_tunnel_whose_ssh_died_says_it_is_closed() {
    let Some(setup) = setup(true) else {
        return;
    };
    let ssh = openssh(
        "127.0.0.1",
        testing::SSH_PORT,
        "kerno",
        key("id_ed25519"),
        None,
    );
    let config = through(Engine::Postgres, ssh);
    let tunnel = setup.open(&config, None, None).await.expect("tunnel");
    assert!(!tunnel.is_closed());
    let marker = tunnel
        .openssh_dir()
        .expect("an OpenSSH tunnel")
        .display()
        .to_string();
    let output = std::process::Command::new("ps")
        .args(["axww", "-o", "pid=,command="])
        .output()
        .unwrap();
    let pid = String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| line.contains(&marker) && line.contains(" -N "))
        .and_then(|line| line.split_whitespace().next().map(str::to_string))
        .expect("the tunnel's ssh");
    let killed = std::process::Command::new("kill")
        .arg(&pid)
        .status()
        .unwrap();
    assert!(killed.success());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !tunnel.is_closed() {
        assert!(std::time::Instant::now() < deadline, "still open");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A connection file edited by hand never gets a command into ProxyJump:
/// refused before any process starts.
#[tokio::test]
async fn an_unsafe_jump_host_never_reaches_a_shell() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = testing::openssh(dir.path(), ASKPASS.into(), false);
    let pwned = dir.path().join("pwned");
    let jump = format!("j;touch {}", pwned.display());
    let ssh = openssh(
        "bastion",
        0,
        "",
        SshAuth::Agent { socket: None },
        Some(&jump),
    );
    let config = through(Engine::Postgres, ssh);
    let host_keys = HostKeyStore {
        openssh: Some(fixture.env),
        ..HostKeyStore::default()
    };
    let error = open_tunnel(&config, &Secrets::default(), &host_keys)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("characters OpenSSH cannot take"),
        "{error}"
    );
    assert!(!pwned.exists());
}
