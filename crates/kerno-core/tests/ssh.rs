//! SSH tunnel tests against the `kerno-ssh` bastion container. Skipped with
//! a message when the container is down.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use kerno_core::config::{Engine, Secret, SshAuth, SshConfig};
use kerno_core::connector::HostKeyStore;
use kerno_core::ssh::{SshError, Tunnel, TunnelRequest};
use kerno_core::testing;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// PostgreSQL's SSLRequest packet. A server with TLS on answers `S`.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 0x04, 0xd2, 0x16, 0x2f];

struct Bastion {
    _dir: tempfile::TempDir,
    host_keys: HostKeyStore,
    password: Secret,
    passphrase: Secret,
}

impl Bastion {
    /// `None` (after printing why) when the containers are down.
    async fn new() -> Option<Self> {
        if !testing::ssh_available() || !testing::available(Engine::Postgres) {
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let host_keys = testing::trusted_host_keys(dir.path().join("known_hosts")).await;
        Some(Self {
            _dir: dir,
            host_keys,
            password: Secret::new(testing::SSH_PASSWORD),
            passphrase: Secret::new(testing::SSH_KEY_PASSPHRASE),
        })
    }

    fn request<'a>(&'a self, ssh: &'a SshConfig) -> TunnelRequest<'a> {
        TunnelRequest {
            ssh,
            password: Some(&self.password),
            key_passphrase: Some(&self.passphrase),
            known_hosts: &self.host_keys.known_hosts,
            learn_into: self.host_keys.learn_into.as_deref(),
            trusted_fingerprint: self.host_keys.trusted_fingerprint.as_deref(),
        }
    }

    async fn open(&self, auth: SshAuth) -> Result<Tunnel, SshError> {
        let ssh = testing::ssh(auth);
        Tunnel::open(self.request(&ssh)).await
    }
}

fn key(name: &str) -> SshAuth {
    SshAuth::KeyFile {
        path: testing::generated(&format!("ssh/{name}")),
    }
}

/// Sends an SSLRequest over `stream` and returns the server's one-byte answer.
async fn ssl_probe<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(stream: &mut S) -> u8 {
    stream.write_all(&SSL_REQUEST).await.unwrap();
    stream.flush().await.unwrap();
    let mut answer = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut answer))
        .await
        .expect("no answer through the tunnel")
        .unwrap();
    answer[0]
}

#[tokio::test]
async fn unknown_host_key_is_refused_then_trusted_then_remembered() {
    if !testing::ssh_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    let files = [known_hosts.clone()];
    let ssh = testing::ssh(SshAuth::Password);
    let password = Secret::new(testing::SSH_PASSWORD);
    let request = |trusted: Option<&'static str>| TunnelRequest {
        ssh: &ssh,
        password: Some(&password),
        key_passphrase: None,
        known_hosts: &files,
        learn_into: Some(&known_hosts),
        trusted_fingerprint: trusted,
    };

    // 1. Never seen: refused, with the fingerprint to show the user.
    let fingerprint = match Tunnel::open(request(None)).await {
        Err(SshError::UnknownHostKey {
            host,
            port,
            algorithm,
            fingerprint,
        }) => {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(port, testing::SSH_PORT);
            assert!(
                algorithm.starts_with("ssh-") || algorithm.starts_with("ecdsa-"),
                "{algorithm}"
            );
            assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
            fingerprint
        }
        other => panic!("expected UnknownHostKey, got {other:?}"),
    };
    assert!(
        !known_hosts.exists(),
        "nothing may be recorded before the user agrees"
    );

    // 2. A different fingerprint than the one presented is not consent.
    let wrong = Tunnel::open(request(Some(
        "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )))
    .await;
    assert!(
        matches!(wrong, Err(SshError::UnknownHostKey { .. })),
        "{wrong:?}"
    );

    // 3. The user agreed to that fingerprint: accepted and recorded.
    let leaked: &'static str = Box::leak(fingerprint.clone().into_boxed_str());
    Tunnel::open(request(Some(leaked)))
        .await
        .expect("trusted host key");
    let recorded = std::fs::read_to_string(&known_hosts).unwrap();
    assert!(recorded.contains("127.0.0.1"), "{recorded}");

    // 4. Known from now on, without asking again.
    Tunnel::open(request(None))
        .await
        .expect("remembered host key");
}

#[tokio::test]
async fn changed_host_key_is_always_refused() {
    if !testing::ssh_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let known_hosts = dir.path().join("known_hosts");
    // Learn the real key, then replace it with another key of the same
    // algorithm, as if the server had been swapped.
    let store = testing::trusted_host_keys(known_hosts.clone()).await;
    let ssh = testing::ssh(SshAuth::Password);
    let password = Secret::new(testing::SSH_PASSWORD);
    let request = |trusted: Option<&'static str>| TunnelRequest {
        ssh: &ssh,
        password: Some(&password),
        key_passphrase: None,
        known_hosts: &store.known_hosts,
        learn_into: Some(&known_hosts),
        trusted_fingerprint: trusted,
    };
    let fingerprint: &'static str =
        Box::leak(store.trusted_fingerprint.clone().unwrap().into_boxed_str());
    Tunnel::open(request(Some(fingerprint)))
        .await
        .expect("learn the real key");

    let recorded = std::fs::read_to_string(&known_hosts).unwrap();
    let line = recorded
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap();
    let mut fields = line.split_whitespace();
    let (hosts, algorithm) = (fields.next().unwrap(), fields.next().unwrap());
    assert_eq!(algorithm, "ssh-ed25519", "the fixture key below is ed25519");
    let other_key = std::fs::read_to_string(testing::generated("ssh/id_ed25519.pub")).unwrap();
    let other_key = other_key.split_whitespace().nth(1).unwrap();
    std::fs::write(&known_hosts, format!("{hosts} {algorithm} {other_key}\n")).unwrap();

    // Even with the user's "trust this fingerprint", a changed key is refused.
    for trusted in [None, Some(fingerprint)] {
        match Tunnel::open(request(trusted)).await {
            Err(SshError::HostKeyChanged { file, line, .. }) => {
                assert_eq!(Path::new(&file), known_hosts);
                assert_eq!(line, 1);
            }
            other => panic!("expected HostKeyChanged, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn password_authentication() {
    let Some(bastion) = Bastion::new().await else {
        return;
    };
    bastion
        .open(SshAuth::Password)
        .await
        .expect("password login");

    let ssh = testing::ssh(SshAuth::Password);
    let wrong = Secret::new("not the password");
    let mut request = bastion.request(&ssh);
    request.password = Some(&wrong);
    match Tunnel::open(request).await {
        Err(SshError::AuthRejected { user, method }) => {
            assert_eq!(user, "kerno");
            assert_eq!(method, "password");
        }
        other => panic!("expected AuthRejected, got {other:?}"),
    }

    let mut request = bastion.request(&ssh);
    request.password = None;
    assert!(matches!(
        Tunnel::open(request).await,
        Err(SshError::PasswordMissing)
    ));
}

#[tokio::test]
async fn key_file_authentication() {
    let Some(bastion) = Bastion::new().await else {
        return;
    };
    bastion.open(key("id_ed25519")).await.expect("ed25519 key");
    bastion
        .open(key("id_ed25519_enc"))
        .await
        .expect("encrypted key with its passphrase");

    let ssh = testing::ssh(key("id_ed25519_enc"));
    let mut request = bastion.request(&ssh);
    request.key_passphrase = None;
    match Tunnel::open(request).await {
        Err(SshError::Passphrase { reason, .. }) => assert_eq!(reason, "a passphrase is required"),
        other => panic!("expected Passphrase, got {other:?}"),
    }

    let wrong = Secret::new("not the passphrase");
    let mut request = bastion.request(&ssh);
    request.key_passphrase = Some(&wrong);
    match Tunnel::open(request).await {
        Err(SshError::Passphrase { reason, .. }) => assert_eq!(reason, "the passphrase is wrong"),
        other => panic!("expected Passphrase, got {other:?}"),
    }

    match bastion.open(key("no_such_key")).await {
        Err(SshError::Key { path, .. }) => assert!(path.ends_with("no_such_key"), "{path}"),
        other => panic!("expected Key, got {other:?}"),
    }

    // A key the server does not know: the public half of nothing it trusts.
    let dir = tempfile::tempdir().unwrap();
    let stranger = dir.path().join("stranger");
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&stranger)
        .status()
        .unwrap();
    assert!(status.success());
    match bastion.open(SshAuth::KeyFile { path: stranger }).await {
        Err(SshError::AuthRejected { method, .. }) => assert_eq!(method, "private key"),
        other => panic!("expected AuthRejected, got {other:?}"),
    }
}

/// RSA key files are refused before any network authentication, in every
/// format, with or without a passphrase at hand. The bastion trusts
/// `id_rsa`, so the refusal is Kerno's, not the server's.
#[tokio::test]
async fn rsa_key_files_are_refused_with_a_clear_message() {
    let Some(bastion) = Bastion::new().await else {
        return;
    };
    let expect_refused = |outcome: Result<Tunnel, SshError>, path: &Path| match outcome {
        Err(error @ SshError::RsaKeyFile { .. }) => {
            assert_eq!(
                error.to_string(),
                format!(
                    "the private key {} is an RSA key. RSA key files are not supported in this \
                     build: load the key in your SSH agent, or use an Ed25519 key",
                    path.display()
                )
            );
        }
        other => panic!("expected RsaKeyFile for {}, got {other:?}", path.display()),
    };

    let fixture = testing::generated("ssh/id_rsa");
    expect_refused(bastion.open(key("id_rsa")).await, &fixture);

    let dir = tempfile::tempdir().unwrap();
    // OpenSSH and PEM PKCS#1, plain and encrypted; PKCS#8 plain only
    // (russh cannot decrypt OpenSSL's encrypted PKCS#8, whatever the key).
    let variants = [
        ("RFC4716", ""),
        ("RFC4716", "kerno_rsa_passphrase"),
        ("PEM", ""),
        ("PEM", "kerno_rsa_passphrase"),
        ("PKCS8", ""),
    ];
    for (format, passphrase) in variants {
        let path = dir
            .path()
            .join(format!("rsa_{format}_{}", passphrase.len()));
        let status = Command::new("ssh-keygen")
            .args([
                "-q", "-t", "rsa", "-b", "2048", "-m", format, "-N", passphrase, "-f",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success(), "ssh-keygen {format}");
        let ssh = testing::ssh(SshAuth::KeyFile { path: path.clone() });
        for given in [None, Some(Secret::new(passphrase))] {
            let mut request = bastion.request(&ssh);
            request.key_passphrase = given.as_ref();
            expect_refused(Tunnel::open(request).await, &path);
        }
    }
}

/// An `ssh-agent` holding one key, killed when dropped.
struct Agent {
    child: Child,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Agent {
    fn start(with_key: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agent.sock");
        let child = Command::new("ssh-agent")
            .arg("-D")
            .arg("-a")
            .arg(&socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("ssh-agent is needed for this test");
        for _ in 0..50 {
            if socket.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(socket.exists(), "ssh-agent did not create its socket");
        if let Some(name) = with_key {
            let status = Command::new("ssh-add")
                .arg("-q")
                .arg(testing::generated(&format!("ssh/{name}")))
                .env("SSH_AUTH_SOCK", &socket)
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "ssh-add failed");
        }
        Self {
            child,
            socket,
            _dir: dir,
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn agent_authentication() {
    let Some(bastion) = Bastion::new().await else {
        return;
    };

    let agent = Agent::start(Some("id_ed25519"));
    bastion
        .open(SshAuth::Agent {
            socket: Some(agent.socket.clone()),
        })
        .await
        .expect("agent login");

    // The agent signs, so an RSA key it holds still works.
    let rsa = Agent::start(Some("id_rsa"));
    bastion
        .open(SshAuth::Agent {
            socket: Some(rsa.socket.clone()),
        })
        .await
        .expect("agent login with an RSA key");

    let empty = Agent::start(None);
    match bastion
        .open(SshAuth::Agent {
            socket: Some(empty.socket.clone()),
        })
        .await
    {
        Err(SshError::Agent(message)) => assert!(message.contains("holds no key"), "{message}"),
        other => panic!("expected Agent error, got {other:?}"),
    }

    match bastion
        .open(SshAuth::Agent {
            socket: Some("/nonexistent/agent.sock".into()),
        })
        .await
    {
        Err(SshError::Agent(message)) => {
            assert!(message.contains("/nonexistent/agent.sock"), "{message}")
        }
        other => panic!("expected Agent error, got {other:?}"),
    }
}

#[tokio::test]
async fn unreachable_ssh_server_is_reported() {
    // Port 1 on localhost: nothing listens there.
    let ssh = SshConfig {
        host: "127.0.0.1".into(),
        port: 1,
        user: "kerno".into(),
        auth: SshAuth::Password,
        keepalive_secs: 0,
    };
    let password = Secret::new("x");
    let outcome = Tunnel::open(TunnelRequest {
        ssh: &ssh,
        password: Some(&password),
        key_passphrase: None,
        known_hosts: &[],
        learn_into: None,
        trusted_fingerprint: None,
    })
    .await;
    match outcome {
        Err(SshError::Connect { host, port, .. }) => {
            assert_eq!(host, "127.0.0.1");
            assert_eq!(port, 1);
        }
        other => panic!("expected Connect, got {other:?}"),
    }
}

#[tokio::test]
async fn stream_reaches_the_database_without_a_local_port() {
    let Some(bastion) = Bastion::new().await else {
        return;
    };
    let tunnel = bastion.open(SshAuth::Password).await.unwrap();
    let mut stream = tunnel
        .open_stream(testing::service_name(Engine::Postgres), 5432)
        .await
        .expect("forward to PostgreSQL");
    assert_eq!(ssl_probe(&mut stream).await, b'S');

    // Several streams share the one SSH connection.
    let mut second = tunnel
        .open_stream(testing::service_name(Engine::Postgres), 5432)
        .await
        .unwrap();
    assert_eq!(ssl_probe(&mut second).await, b'S');
    assert!(!tunnel.is_closed());
}

#[tokio::test]
async fn forward_failures_name_the_target() {
    let Some(bastion) = Bastion::new().await else {
        return;
    };
    let tunnel = bastion.open(SshAuth::Password).await.unwrap();
    // A closed port on a host the bastion can reach.
    match tunnel
        .open_stream(testing::service_name(Engine::Postgres), 1)
        .await
    {
        Err(SshError::Forward { host, port, .. }) => {
            assert_eq!(host, "kerno-postgres");
            assert_eq!(port, 1);
        }
        other => panic!("expected Forward, got {:?}", other.map(|_| "a stream")),
    }
}

#[tokio::test]
async fn single_use_port_admits_exactly_one_connection() {
    let Some(bastion) = Bastion::new().await else {
        return;
    };
    let tunnel = bastion.open(SshAuth::Password).await.unwrap();
    let addr = tunnel
        .forward_once(testing::service_name(Engine::Postgres), 5432)
        .await
        .unwrap();
    assert!(addr.ip().is_loopback());

    let mut first = tokio::net::TcpStream::connect(addr)
        .await
        .expect("the one admitted connection");
    assert_eq!(ssl_probe(&mut first).await, b'S');

    // The port stopped listening as soon as the first connection was taken.
    let second = tokio::net::TcpStream::connect(addr).await;
    assert!(second.is_err(), "a second connection must be refused");
}

/// Current-thread runtime on purpose: the forwarding task cannot run until
/// this test yields, so both connections are queued before it looks.
#[tokio::test(flavor = "current_thread")]
async fn single_use_port_refuses_everyone_when_contested() {
    let Some(bastion) = Bastion::new().await else {
        return;
    };
    let tunnel = bastion.open(SshAuth::Password).await.unwrap();
    let addr = tunnel
        .forward_once(testing::service_name(Engine::Postgres), 5432)
        .await
        .unwrap();

    // Two connections land in the listener's queue before the tunnel takes
    // the first: it cannot know which one is the driver's. They are opened
    // with blocking calls so that nothing else runs in between.
    let a = std::net::TcpStream::connect(addr).unwrap();
    let b = std::net::TcpStream::connect(addr).unwrap();

    // From here on everything is asynchronous, so the tunnel's task is free
    // to forward: if it did, the database would answer `S`.
    for stream in [a, b] {
        stream.set_nonblocking(true).unwrap();
        let mut stream = tokio::net::TcpStream::from_std(stream).unwrap();
        // The write may fail if the tunnel already closed the connection.
        let _ = stream.write_all(&SSL_REQUEST).await;
        let mut answer = [0u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut answer))
            .await
            .expect("a refused connection must be closed, not left hanging");
        match read {
            Ok(0) | Err(_) => {}
            Ok(_) => panic!(
                "a contested connection was forwarded (the database answered {:?})",
                answer[0] as char
            ),
        }
    }
}
