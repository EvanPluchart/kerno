//! Shared facts about the `kerno-*` test containers (see
//! `docker-compose.yml`), for integration tests across the workspace.
//!
//! Tests call [`available`] first and return early when the container is
//! down, after printing why. With `KERNO_REQUIRE_CONTAINERS=1` (set by `scripts/check.sh`) a
//! missing container fails the test instead.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use crate::config::{
    ConnectParams, ConnectionConfig, Engine, Secret, Secrets, SshAuth, SshClient, SshConfig,
    TlsConfig, TlsMode,
};
use crate::connector::HostKeyStore;

/// Password of the `kerno` database user in every test container.
pub const DB_PASSWORD: &str = "kerno_test_pw";
/// Password of the `kerno` user on the SSH bastion.
pub const SSH_PASSWORD: &str = "kerno_ssh_pw";
/// Passphrase of `id_ed25519_enc`.
pub const SSH_KEY_PASSPHRASE: &str = "kerno_key_passphrase";
/// Host port of the SSH bastion.
pub const SSH_PORT: u16 = 52222;
/// Host port of the jump host in front of the bastion (`kerno-ssh-jump`),
/// which reaches it as `kerno-ssh:22`.
pub const SSH_JUMP_PORT: u16 = 52223;

/// Host port a test container listens on.
pub fn port(engine: Engine) -> u16 {
    match engine {
        Engine::Postgres => 55432,
        Engine::MySql => 53306,
        Engine::MariaDb => 53307,
    }
}

/// Name the container has on the compose network, as the SSH bastion
/// resolves it.
pub fn service_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "kerno-postgres",
        Engine::MySql => "kerno-mysql",
        Engine::MariaDb => "kerno-mariadb",
    }
}

fn reachable(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()
}

fn check(name: &str, port: u16) -> bool {
    if reachable(port) {
        return true;
    }
    let message = format!(
        "container {name} is not reachable on 127.0.0.1:{port}; start it with `scripts/test-env.sh up`"
    );
    if std::env::var_os("KERNO_REQUIRE_CONTAINERS").is_some() {
        panic!("{message}");
    }
    eprintln!("SKIPPED: {message}");
    false
}

/// Whether the test container of `engine` is up. Prints a `SKIPPED` line
/// when it is not.
pub fn available(engine: Engine) -> bool {
    exclusive_run();
    check(service_name(engine), port(engine))
}

/// Whether the SSH bastion is up. Prints a `SKIPPED` line when it is not.
pub fn ssh_available() -> bool {
    exclusive_run();
    check("kerno-ssh", SSH_PORT)
}

/// Whether the bastion and the jump host in front of it are up. Prints a
/// `SKIPPED` line when one is not.
pub fn ssh_jump_available() -> bool {
    ssh_available() && check("kerno-ssh-jump", SSH_JUMP_PORT)
}

/// An OpenSSH configuration of the test containers, in a scratch
/// directory, that never reads the developer's own SSH files:
///
/// - `kerno-bastion`: the bastion (127.0.0.1:52222), user and key set;
/// - `kerno-jump`: the jump host (127.0.0.1:52223), user and key set;
/// - `kerno-behind-jump`: the bastion as `kerno-ssh:22`, through
///   `ProxyJump kerno-jump`;
/// - `localhost`: the key, for `-J kerno@localhost:52223`;
/// - every host: `IdentitiesOnly`, no agent, the scratch known_hosts only.
///
/// 127.0.0.1 itself names no key: a test that reaches it gives its own.
#[derive(Debug, Clone)]
pub struct OpenSshFixture {
    /// The configuration file (`ssh -F`).
    pub config: PathBuf,
    /// The known_hosts file it names.
    pub known_hosts: PathBuf,
    /// The OpenSSH environment the connector takes.
    pub env: crate::openssh::OpenSshEnv,
}

/// Writes [`OpenSshFixture`] into `dir`. With `known`, the containers'
/// host keys are in its known_hosts already. `askpass` is the program `ssh`
/// asks questions through.
pub fn openssh(dir: &std::path::Path, askpass: PathBuf, known: bool) -> OpenSshFixture {
    let key = generated("ssh/id_ed25519");
    let known_hosts = dir.join("known_hosts");
    let config = dir.join("ssh_config");
    let key = key.display();
    let text = format!(
        "Host kerno-bastion\n  HostName 127.0.0.1\n  Port {SSH_PORT}\n  User kerno\n  IdentityFile {key}\n\
         Host kerno-jump\n  HostName 127.0.0.1\n  Port {SSH_JUMP_PORT}\n  User kerno\n  IdentityFile {key}\n\
         Host kerno-behind-jump\n  HostName kerno-ssh\n  Port 22\n  User kerno\n  ProxyJump kerno-jump\n  IdentityFile {key}\n\
         Host localhost\n  IdentityFile {key}\n\
         Host *\n  IdentitiesOnly yes\n  IdentityAgent none\n  UserKnownHostsFile {}\n  \
         GlobalKnownHostsFile /dev/null\n  UpdateHostKeys no\n  ConnectTimeout 10\n",
        known_hosts.display()
    );
    std::fs::write(&config, text).expect("write the test ssh_config");
    let lines = if known {
        let bastion = host_key(SSH_PORT);
        let jump = host_key(SSH_JUMP_PORT);
        format!(
            "[127.0.0.1]:{SSH_PORT} {bastion}\nkerno-ssh {bastion}\n\
             [127.0.0.1]:{SSH_JUMP_PORT} {jump}\n[localhost]:{SSH_JUMP_PORT} {jump}\n"
        )
    } else {
        String::new()
    };
    std::fs::write(&known_hosts, lines).expect("write the test known_hosts");
    let env = crate::openssh::OpenSshEnv {
        ssh: crate::openssh::find_ssh(std::env::var_os("PATH").as_deref())
            .expect("OpenSSH (ssh) is needed for the tunnel tests"),
        config: Some(config.clone()),
        aliases_from: Some(config.clone()),
        askpass,
    };
    OpenSshFixture {
        config,
        known_hosts,
        env,
    }
}

/// Settings of the test container of `engine` through OpenSSH and the jump
/// host: the alias `kerno-behind-jump` of [`openssh`] alone (its port, user,
/// key and ProxyJump are in the file).
pub fn openssh_config(engine: Engine) -> ConnectionConfig {
    let mut config = self::config(engine);
    config.host = service_name(engine).into();
    config.port = match engine {
        Engine::Postgres => 5432,
        Engine::MySql | Engine::MariaDb => 3306,
    };
    config.ssh = Some(SshConfig {
        host: "kerno-behind-jump".into(),
        port: 0,
        user: String::new(),
        auth: SshAuth::Agent { socket: None },
        keepalive_secs: 30,
        client: SshClient::OpenSsh,
        jump: None,
    });
    config
}

/// The ed25519 host key of the SSH container on `port` (`type base64`),
/// read with `ssh-keyscan`.
pub fn host_key(port: u16) -> String {
    let output = std::process::Command::new("ssh-keyscan")
        .args(["-t", "ed25519", "-p", &port.to_string(), "127.0.0.1"])
        .output()
        .expect("run ssh-keyscan");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find(|line| !line.starts_with('#') && !line.trim().is_empty())
        .and_then(|line| line.split_once(' ').map(|(_, key)| key.to_string()))
        .unwrap_or_else(|| panic!("no host key from 127.0.0.1:{port}"))
}

/// The repository's `tests/fixtures` directory.
pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// A file generated by `scripts/test-env.sh` (TLS certificates, SSH keys).
pub fn generated(relative: &str) -> PathBuf {
    fixtures().join("generated").join(relative)
}

/// Settings of the test container of `engine`, reached directly.
pub fn config(engine: Engine) -> ConnectionConfig {
    ConnectionConfig {
        name: format!("test {}", engine.label()),
        host: "127.0.0.1".into(),
        port: port(engine),
        user: "kerno".into(),
        database: Some("kerno".into()),
        tls: TlsConfig {
            mode: TlsMode::Disable,
            ..TlsConfig::default()
        },
        ..ConnectionConfig::new(engine)
    }
}

/// The secrets that go with [`config`].
pub fn secrets() -> Secrets {
    Secrets {
        password: Some(Secret::new(DB_PASSWORD)),
        ssh_password: Some(Secret::new(SSH_PASSWORD)),
        ssh_key_passphrase: Some(Secret::new(SSH_KEY_PASSPHRASE)),
    }
}

/// Driver parameters for the test container of `engine`, reached directly,
/// without TLS.
pub fn params(engine: Engine) -> ConnectParams {
    let config = config(engine);
    ConnectParams {
        engine,
        host: config.host,
        port: config.port,
        tunnel: None,
        user: config.user,
        password: Some(Secret::new(DB_PASSWORD)),
        database: config.database,
        tls: config.tls,
        read_only: false,
        options: BTreeMap::new(),
    }
}

/// The SSH bastion, with the given authentication method.
pub fn ssh(auth: SshAuth) -> SshConfig {
    SshConfig {
        host: "127.0.0.1".into(),
        port: SSH_PORT,
        user: "kerno".into(),
        auth,
        keepalive_secs: 30,
        client: SshClient::Builtin,
        jump: None,
    }
}

/// Reads the bastion's current host key fingerprint by connecting once
/// with an empty known-hosts list, and returns a store that trusts it.
/// `known_hosts` is a scratch file the key is recorded in.
pub async fn trusted_host_keys(known_hosts: PathBuf) -> HostKeyStore {
    let ssh = ssh(SshAuth::Password);
    let password = Secret::new(SSH_PASSWORD);
    let files = [known_hosts.clone()];
    let attempt = crate::ssh::Tunnel::open(crate::ssh::TunnelRequest {
        ssh: &ssh,
        password: Some(&password),
        key_passphrase: None,
        known_hosts: &files,
        learn_into: None,
        trusted_fingerprint: None,
    })
    .await;
    let trusted_fingerprint = match attempt {
        Err(crate::ssh::SshError::UnknownHostKey { fingerprint, .. }) => Some(fingerprint),
        Ok(_) => None,
        Err(other) => panic!("could not read the bastion's host key: {other}"),
    };
    HostKeyStore {
        known_hosts: vec![known_hosts.clone()],
        learn_into: Some(known_hosts),
        trusted_fingerprint,
        openssh: None,
    }
}

/// Makes this test process the only one talking to the test servers: a
/// second process (another `cargo test` started beside this one) waits here
/// until the first has exited. Tests name their scratch objects after
/// themselves, so two runs of the same test would otherwise trip over each
/// other. Called by every test that connects; the lock is taken once and
/// held for the life of the process.
pub fn exclusive_run() {
    use std::sync::OnceLock;
    static LOCK: OnceLock<Option<std::fs::File>> = OnceLock::new();
    LOCK.get_or_init(|| {
        let path = std::env::temp_dir().join("kerno-test-servers.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .ok()?;
        // Without the lock the tests still run; they only lose the guarantee.
        file.lock().ok()?;
        Some(file)
    });
}
