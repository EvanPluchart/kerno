//! Shared facts about the `kerno-*` test containers (see
//! `docker-compose.yml`), for integration tests across the workspace.
//!
//! Tests call [`available`] first and return early when the container is
//! down, after printing why. With `KERNO_REQUIRE_CONTAINERS=1` (set by `scripts/check.sh`) a
//! missing container fails the test instead; `KERNO_REQUIRE_SERVERS` does so for the containers it
//! names, comma-separated (the Windows runner's `kerno-postgres,kerno-mysql`, which has no MariaDB,
//! SSH or legacy MySQL server: K68).

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

/// What a SQLite test asked of a container: there is none.
const NO_CONTAINER: &str = "SQLite has no test container: its tests open files they create";

/// Host port a test container listens on.
///
/// # Panics
/// For SQLite, which needs no server.
pub fn port(engine: Engine) -> u16 {
    match engine {
        Engine::Postgres => 55432,
        Engine::MySql => 53306,
        Engine::MariaDb => 53307,
        Engine::Sqlite => panic!("{NO_CONTAINER}"),
    }
}

/// Name the container has on the compose network, as the SSH bastion
/// resolves it.
///
/// # Panics
/// For SQLite, which needs no server.
pub fn service_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "kerno-postgres",
        Engine::MySql => "kerno-mysql",
        Engine::MariaDb => "kerno-mariadb",
        Engine::Sqlite => panic!("{NO_CONTAINER}"),
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
    let legacy = if LEGACY_MYSQL.iter().any(|(legacy, _)| *legacy == name) {
        " --legacy"
    } else {
        ""
    };
    let message = format!(
        "container {name} is not reachable on 127.0.0.1:{port}; start it with `scripts/test-env.sh up{legacy}`"
    );
    let named = std::env::var("KERNO_REQUIRE_SERVERS").unwrap_or_default();
    if std::env::var_os("KERNO_REQUIRE_CONTAINERS").is_some() || names(&named, name) {
        panic!("{message}");
    }
    eprintln!("SKIPPED: {message}");
    false
}

/// Whether the comma-separated `list` names the container `name`.
fn names(list: &str, name: &str) -> bool {
    list.split(',').any(|item| item.trim() == name)
}

/// Whether the test container of `engine` is up. Prints a `SKIPPED` line
/// when it is not. Always true for SQLite, which needs none.
pub fn available(engine: Engine) -> bool {
    if engine.is_file() {
        return true;
    }
    exclusive_run();
    check(service_name(engine), port(engine))
}

/// The legacy MySQL servers of the compose file's `legacy` profile (K22),
/// oldest first: container name and host port. Same `kerno` user, password
/// and database as [`config`], seeded with `tests/fixtures/mysql-legacy`.
pub const LEGACY_MYSQL: [(&str, u16); 4] = [
    ("kerno-mysql51", 53351),
    ("kerno-mysql55", 53355),
    ("kerno-mysql56", 53356),
    ("kerno-mysql57", 53357),
];

/// The legacy MySQL servers that are up, with the parameters that reach
/// them directly without TLS. Prints a `SKIPPED` line for each one that is
/// down (start them with `scripts/test-env.sh up --legacy`).
pub fn legacy_mysql() -> Vec<(&'static str, ConnectParams)> {
    exclusive_run();
    LEGACY_MYSQL
        .into_iter()
        .filter(|(name, port)| check(name, *port))
        .map(|(name, port)| {
            (
                name,
                ConnectParams {
                    port,
                    ..params(Engine::MySql)
                },
            )
        })
        .collect()
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
        tunnels: None,
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
        Engine::Sqlite => panic!("{NO_CONTAINER}"),
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
        file: None,
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
        password: None,
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

/// A fake `aws` for the password-command tests (J3): an executable script
/// in `dir` that writes its arguments, one per line, to `dir/aws-args` and
/// prints the test servers' real password, so that a successful login
/// proves its output was used. The returned environment's `PATH` and
/// `HOME` are `dir` alone: the real `aws`, `op` and the developer's
/// `~/.pgpass` and `~/.my.cnf` are never reached.
#[cfg(unix)]
pub fn fake_aws(dir: &std::path::Path) -> crate::password::PasswordEnv {
    use std::os::unix::fs::PermissionsExt as _;
    let script = dir.join("aws");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' '{DB_PASSWORD}'\n",
            dir.join("aws-args").display()
        ),
    )
    .expect("write the fake aws");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
        .expect("make the fake aws executable");
    crate::password::PasswordEnv {
        path: vec![dir.to_path_buf()],
        home: Some(dir.to_path_buf()),
        pgpassfile: None,
    }
}

/// The arguments the fake `aws` of [`fake_aws`] was last run with.
pub fn fake_aws_args(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("aws-args"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Writes a password file (`.pgpass`, `.my.cnf`) in `dir` with `mode`.
#[cfg(unix)]
pub fn password_file(dir: &std::path::Path, name: &str, text: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    std::fs::write(&path, text).expect("write the password file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
        .expect("set the password file's permissions");
}

/// Whether this is the Windows runner of the K67 probe workflow
/// (`.github/workflows/windows-probe.yml` on the branch `windows`), which
/// sets `KERNO_K67_RUNNER` after starting its own PostgreSQL and MySQL on the
/// test containers' ports. Elsewhere, prints why and answers `false`: the
/// tests that need it never run on a developer's Windows by accident.
pub fn k67_runner() -> bool {
    let runner = cfg!(windows) && std::env::var_os("KERNO_K67_RUNNER").is_some();
    if !runner {
        eprintln!("SKIPPED: runs only on the Windows runner of the K67 probe (KERNO_K67_RUNNER)");
    }
    runner
}

/// This process's resident memory in KiB: `ps -o rss=` on Unix, the
/// working set `tasklist` gives on Windows ("123,456 K", K68).
pub fn resident_kb() -> u64 {
    let pid = std::process::id().to_string();
    let output = if cfg!(windows) {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
    } else {
        std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid])
            .output()
    }
    .expect("read this process's memory");
    let text = String::from_utf8_lossy(&output.stdout);
    // tasklist: the last quoted field; ps: the number alone.
    let size = text.rsplit(",\"").next().unwrap_or_default();
    let digits: String = size.chars().filter(char::is_ascii_digit).collect();
    digits
        .parse()
        .unwrap_or_else(|_| panic!("no resident size in {text:?}"))
}

/// Windows: how many processes run the program `image` (`pg_dump.exe`),
/// as `tasklist` lists them.
pub fn processes_named(image: &str) -> usize {
    let filter = format!("IMAGENAME eq {image}");
    let output = std::process::Command::new("tasklist")
        .args(["/FI", filter.as_str(), "/NH", "/FO", "CSV"])
        .output()
        .expect("run tasklist");
    let quoted = format!("\"{}\"", image.to_ascii_lowercase());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.to_ascii_lowercase().starts_with(&quoted))
        .count()
}

/// The servers a throwaway Windows runner starts for the tunnel through
/// `ssh.exe` (K69, `.github/workflows/windows-ssh-probe.yml`): Windows' own
/// OpenSSH server for the runner's user, keys in the profile's `.ssh`, and
/// a PostgreSQL server of its own.
#[cfg(windows)]
pub mod k69 {
    use std::path::{Path, PathBuf};

    use crate::config::{ConnectionConfig, Engine, SshAuth, SshClient, SshConfig, TlsMode};

    /// The SSH server's port (127.0.0.1).
    pub const SSH_PORT: u16 = 52290;
    /// The same server's second port, as a jump host: OpenSSH refuses a
    /// jump host that is the target itself ("jumphost loop").
    pub const JUMP_PORT: u16 = 52291;
    /// The PostgreSQL server's port (127.0.0.1).
    pub const PG_PORT: u16 = 55433;
    /// Its database, owned by the role `kerno` ([`super::DB_PASSWORD`]).
    pub const DATABASE: &str = "kerno_test_k69";
    /// The passphrase of `kerno_k69_enc`.
    pub const PASSPHRASE: &str = "k69 passphrase";

    /// Whether this runs on that runner (`KERNO_K69_RUNNER`); otherwise the
    /// K69 tests print why and pass.
    pub fn runner() -> bool {
        let on = std::env::var_os("KERNO_K69_RUNNER").is_some();
        if !on {
            eprintln!("SKIPPED: not the K69 Windows runner (KERNO_K69_RUNNER is not set)");
        }
        on
    }

    /// The runner's user, whom the SSH server lets in.
    pub fn user() -> String {
        std::env::var("USERNAME").expect("USERNAME")
    }

    /// A key the workflow made in the profile's `.ssh`.
    pub fn key(name: &str) -> PathBuf {
        crate::home::dir()
            .expect("USERPROFILE")
            .join(".ssh")
            .join(name)
    }

    /// The OpenSSH settings of a test: the SSH server by address, `auth`.
    pub fn ssh(auth: SshAuth, jump: Option<String>) -> SshConfig {
        SshConfig {
            host: "127.0.0.1".into(),
            port: SSH_PORT,
            user: user(),
            auth,
            keepalive_secs: 30,
            client: SshClient::OpenSsh,
            jump,
        }
    }

    /// The runner's PostgreSQL as the SSH server sees it, through `ssh`.
    pub fn config(ssh: SshConfig) -> ConnectionConfig {
        let mut config = super::config(Engine::Postgres);
        config.port = PG_PORT;
        config.database = Some(DATABASE.into());
        config.tls.mode = TlsMode::Disable;
        config.ssh = Some(ssh);
        config
    }

    /// Where the tunnels' private folders are made when the settings name
    /// none: the app's local data folder's `ssh` (K69, R5).
    pub fn tunnels() -> PathBuf {
        crate::storage::AppPaths::local_data_dir()
            .expect("the local data folder")
            .join("ssh")
    }

    /// The jump host's alias in [`openssh`]'s file: the server's second
    /// port, with the plain key.
    pub const JUMP: &str = "k69-jump";

    /// A second hop for a chain of jump hosts (`k69-jump,k69-hop2`): the
    /// SSH server's first port under another name, so that OpenSSH sees no
    /// loop, with the plain key.
    pub const HOP2: &str = "k69-hop2";

    /// A third hop (`k69-jump,k69-hop2,k69-hop3`): the jump host's port
    /// under another name.
    pub const HOP3: &str = "k69-hop3";

    /// The jump host [`JUMP`]'s port with the PEM key (its passphrase is
    /// [`PASSPHRASE`]), written with `/` where `-i` has `\`.
    pub const JUMP_PEM: &str = "k69-jump-pem";

    /// A configuration file of the test's own in `dir`, with its own
    /// known_hosts (filled with the server's keys when `known`): the jump
    /// hosts [`JUMP`] and [`HOP2`] take the plain key, [`JUMP_PEM`] the PEM
    /// one, the target only the key a test gives (`-i`); no agent.
    pub fn openssh(dir: &Path, askpass: PathBuf, known: bool) -> super::OpenSshFixture {
        let slashes = |path: &Path| path.display().to_string().replace('\\', "/");
        let known_hosts = dir.join("known_hosts");
        let config = dir.join("ssh_config");
        let plain = slashes(&key("kerno_k69"));
        let text = format!(
            "Host {JUMP}\n  HostName 127.0.0.1\n  Port {JUMP_PORT}\n  IdentityFile \"{plain}\"\n\
             Host {HOP2}\n  HostName 127.0.0.1\n  Port {SSH_PORT}\n  IdentityFile \"{plain}\"\n\
             Host {HOP3}\n  HostName 127.0.0.1\n  Port {JUMP_PORT}\n  IdentityFile \"{plain}\"\n\
             Host {JUMP_PEM}\n  HostName 127.0.0.1\n  Port {JUMP_PORT}\n  IdentityFile \"{}\"\n\
             Host *\n  IdentitiesOnly yes\n  IdentityAgent none\n  \
             UserKnownHostsFile \"{}\"\n  UpdateHostKeys no\n  ConnectTimeout 10\n",
            slashes(&key("kerno_k69_pem")),
            slashes(&known_hosts),
        );
        std::fs::write(&config, text).expect("write the test ssh_config");
        let scan = |port: u16| {
            let scan = std::process::Command::new(system32("ssh-keyscan.exe"))
                .args(["-p", &port.to_string(), "127.0.0.1"])
                .output()
                .expect("run ssh-keyscan");
            String::from_utf8_lossy(&scan.stdout).into_owned()
        };
        let lines = match known {
            true => scan(SSH_PORT) + &scan(JUMP_PORT),
            false => String::new(),
        };
        std::fs::write(&known_hosts, lines).expect("write the test known_hosts");
        let env = crate::openssh::OpenSshEnv {
            ssh: crate::openssh::find_ssh(std::env::var_os("PATH").as_deref())
                .expect("Windows' ssh.exe"),
            config: Some(config.clone()),
            aliases_from: Some(config.clone()),
            askpass,
            tunnels: None,
        };
        super::OpenSshFixture {
            config,
            known_hosts,
            env,
        }
    }

    /// A program of Windows' OpenSSH.
    pub fn system32(program: &str) -> PathBuf {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot");
        PathBuf::from(root)
            .join("System32")
            .join("OpenSSH")
            .join(program)
    }

    /// The command lines of the running `ssh.exe` processes.
    pub fn ssh_processes() -> Vec<String> {
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_Process -Filter \"Name='ssh.exe'\" | \
                 ForEach-Object { $_.CommandLine }",
            ])
            .output()
            .expect("list the processes");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// The running `ssh.exe` processes whose command line holds `marker`:
    /// each one's executable (`Win32_Process.ExecutablePath`, the file
    /// Windows started, whatever its command line says) and command line.
    pub fn ssh_executables(marker: &str) -> Vec<(PathBuf, String)> {
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Get-CimInstance Win32_Process -Filter \"Name='ssh.exe'\" | \
                 ForEach-Object { \"$($_.ExecutablePath)`t$($_.CommandLine)\" }",
            ])
            .output()
            .expect("list the processes");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim_end().split_once('\t'))
            .filter(|(_, command)| command.contains(marker))
            .map(|(path, command)| (PathBuf::from(path), command.to_string()))
            .collect()
    }

    /// Waits up to 15 s until no `ssh.exe` whose command line holds
    /// `marker` runs; panics naming the ones left. Async: the tasks that
    /// stop a stream's `ssh.exe` run on the test's runtime meanwhile.
    pub async fn wait_until_gone(marker: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let left: Vec<String> = ssh_processes()
                .into_iter()
                .filter(|line| line.contains(marker))
                .collect();
            if left.is_empty() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "ssh.exe left running: {left:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

/// What a [`Proxy`] does with the traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    /// Passes it both ways.
    Forward,
    /// Closes every connection and every new one at once: the server went
    /// away (a crash, a restart).
    Drop,
    /// Holds every byte, closing nothing, new connections included: the
    /// server or the route stopped answering. Back to `Forward`, what was
    /// held goes on.
    BlackHole,
}

/// A TCP proxy on `127.0.0.1` in front of a test server, which a test
/// breaks and mends ([`ProxyMode`]) without touching the shared container.
/// It runs on a thread and a runtime of its own, so a test of any kind
/// uses it; dropped, it closes everything.
pub struct Proxy {
    port: u16,
    mode: tokio::sync::watch::Sender<ProxyMode>,
}

impl Proxy {
    /// A proxy forwarding to `127.0.0.1:target`.
    pub fn start(target: u16) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the proxy");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let port = listener.local_addr().expect("the proxy's port").port();
        let (mode, watching) = tokio::sync::watch::channel(ProxyMode::Forward);
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the proxy's runtime");
            runtime.block_on(proxy_accept(listener, target, watching));
        });
        Self { port, mode }
    }

    /// The port to connect to instead of the server's.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Breaks or mends the traffic.
    pub fn set(&self, mode: ProxyMode) {
        self.mode.send_replace(mode);
    }
}

async fn proxy_accept(
    listener: std::net::TcpListener,
    target: u16,
    mut mode: tokio::sync::watch::Receiver<ProxyMode>,
) {
    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
        return;
    };
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((client, _)) = accepted else { continue };
                if *mode.borrow() == ProxyMode::Drop {
                    continue; // closed at once
                }
                let watching = mode.clone();
                tokio::spawn(async move {
                    let Ok(server) = tokio::net::TcpStream::connect(("127.0.0.1", target)).await
                    else {
                        return;
                    };
                    let (client_read, client_write) = client.into_split();
                    let (server_read, server_write) = server.into_split();
                    tokio::join!(
                        proxy_pump(client_read, server_write, watching.clone()),
                        proxy_pump(server_read, client_write, watching),
                    );
                });
            }
            // The proxy was dropped: everything closes with the runtime.
            changed = mode.changed() => if changed.is_err() { return },
        }
    }
}

/// Waits while black-holed; `false` once the traffic must be dropped.
async fn proxy_open(mode: &mut tokio::sync::watch::Receiver<ProxyMode>) -> bool {
    loop {
        match *mode.borrow_and_update() {
            ProxyMode::Forward => return true,
            ProxyMode::Drop => return false,
            ProxyMode::BlackHole => {}
        }
        if mode.changed().await.is_err() {
            return false;
        }
    }
}

async fn proxy_pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    mut mode: tokio::sync::watch::Receiver<ProxyMode>,
) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut buffer = vec![0; 16 * 1024];
    loop {
        if !proxy_open(&mut mode).await {
            return;
        }
        tokio::select! {
            read = from.read(&mut buffer) => {
                let read = match read {
                    Ok(0) | Err(_) => return,
                    Ok(read) => read,
                };
                // Held, not lost, if the hole opened meanwhile.
                if !proxy_open(&mut mode).await
                    || to.write_all(&buffer[..read]).await.is_err()
                {
                    return;
                }
            }
            changed = mode.changed() => if changed.is_err() { return },
        }
    }
}

/// One request the fake AI endpoint received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiRequest {
    /// `POST /v1/chat/completions`.
    pub line: String,
    /// Header names in lower case, with their values.
    pub headers: Vec<(String, String)>,
    /// The body, byte for byte.
    pub body: Vec<u8>,
}

impl AiRequest {
    /// The value of header `name` (any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(found, _)| *found == name)
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Default)]
struct AiLog {
    connections: usize,
    requests: Vec<AiRequest>,
    /// The answers written back, once each was.
    answered: usize,
    /// The requests whose client closed the connection before their answer.
    closed: usize,
}

/// A fake OpenAI-compatible endpoint on `127.0.0.1` (J17): a `tokio` listener on a thread and
/// a runtime of its own that counts every connection it accepts and records every request.
/// `POST …/chat/completions` answers `answer` as the model's message, `GET …/models` lists
/// `fake-model` and `other-model`, after `delay`. Dropped, it stops.
pub struct FakeAi {
    port: u16,
    log: std::sync::Arc<std::sync::Mutex<AiLog>>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl FakeAi {
    /// A fake answering `answer` at once.
    pub fn start(answer: &str) -> Self {
        Self::start_with(answer, Duration::ZERO)
    }

    /// A fake answering `answer` after `delay`.
    pub fn start_with(answer: &str, delay: Duration) -> Self {
        Self::scripted(vec![Reply::Whole(answer.to_string())], delay)
    }

    /// A fake answering its chat requests with `answers` in turn (the last one again after).
    pub fn start_answers(answers: &[&str]) -> Self {
        let replies = answers
            .iter()
            .map(|a| Reply::Whole(a.to_string()))
            .collect();
        Self::scripted(replies, Duration::ZERO)
    }

    /// A fake that writes its answer as server-sent events, one per piece of `pieces`, `gap`
    /// apart, then `[DONE]` (K51).
    pub fn streaming(pieces: &[&str], gap: Duration) -> Self {
        let pieces = pieces.iter().map(|p| p.to_string()).collect();
        Self::scripted(vec![Reply::Stream(pieces, gap)], Duration::ZERO)
    }

    fn scripted(replies: Vec<Reply>, delay: Duration) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the fake AI");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let port = listener.local_addr().expect("the fake's port").port();
        let log = std::sync::Arc::new(std::sync::Mutex::new(AiLog::default()));
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (shared, replies) = (log.clone(), std::sync::Arc::new(replies));
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the fake's runtime");
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                    return;
                };
                let serve = async {
                    loop {
                        let Ok((stream, _)) = listener.accept().await else {
                            continue;
                        };
                        shared.lock().unwrap().connections += 1;
                        let (log, replies) = (shared.clone(), replies.clone());
                        tokio::spawn(fake_ai_serve(stream, log, replies, delay));
                    }
                };
                tokio::select! {
                    () = serve => {}
                    _ = stopped => {}
                }
            });
        });
        Self {
            port,
            log,
            _stop: stop,
        }
    }

    /// Its base address: `http://127.0.0.1:<port>/v1`.
    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    /// The connections it accepted, whether a request came or not.
    pub fn connections(&self) -> usize {
        self.log.lock().unwrap().connections
    }

    /// The requests it read, in order.
    pub fn requests(&self) -> Vec<AiRequest> {
        self.log.lock().unwrap().requests.clone()
    }

    /// The answers it wrote back (after its delay).
    pub fn answered(&self) -> usize {
        self.log.lock().unwrap().answered
    }

    /// The requests whose client closed the connection while it held the answer.
    pub fn closed(&self) -> usize {
        self.log.lock().unwrap().closed
    }
}

/// What the fake answers a chat request.
#[derive(Clone)]
enum Reply {
    Whole(String),
    /// Server-sent events, one per piece, this long apart.
    Stream(Vec<String>, Duration),
}

/// Waits `delay` unless the client closes the connection first (then `true`).
async fn held_until_gone(stream: &mut tokio::net::TcpStream, delay: Duration) -> bool {
    use tokio::io::AsyncReadExt as _;
    let mut buffer = [0; 1024];
    let held = tokio::time::sleep(delay);
    tokio::pin!(held);
    loop {
        tokio::select! {
            () = &mut held => return false,
            read = stream.read(&mut buffer) => match read {
                Ok(0) | Err(_) => return true,
                Ok(_) => {}
            },
        }
    }
}

async fn fake_ai_serve(
    mut stream: tokio::net::TcpStream,
    log: std::sync::Arc<std::sync::Mutex<AiLog>>,
    replies: std::sync::Arc<Vec<Reply>>,
    delay: Duration,
) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    let head_end = loop {
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
        }
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let line = lines.next().unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    let length: usize = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    let mut body = bytes[head_end + 4..].to_vec();
    while body.len() < length {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => body.extend_from_slice(&buffer[..read]),
        }
    }
    let models = line.contains("/models");
    let reply = {
        let mut log = log.lock().unwrap();
        let asked = (log.requests.iter())
            .filter(|request| !request.line.contains("/models"))
            .count();
        log.requests.push(AiRequest {
            line,
            headers,
            body,
        });
        replies[asked.min(replies.len() - 1)].clone()
    };
    // The delay, unless the client closes the connection first.
    if held_until_gone(&mut stream, delay).await {
        log.lock().unwrap().closed += 1;
        return;
    }
    let answer = match reply {
        Reply::Stream(pieces, gap) if !models => {
            let head =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
            if stream.write_all(head.as_bytes()).await.is_err() {
                return;
            }
            for (index, piece) in pieces.iter().enumerate() {
                if index > 0 && held_until_gone(&mut stream, gap).await {
                    log.lock().unwrap().closed += 1;
                    return;
                }
                let event =
                    serde_json::json!({"choices": [{"index": 0, "delta": {"content": piece}}]});
                if stream
                    .write_all(format!("data: {event}\n\n").as_bytes())
                    .await
                    .is_err()
                {
                    log.lock().unwrap().closed += 1;
                    return;
                }
                let _ = stream.flush().await;
            }
            let _ = stream.write_all(b"data: [DONE]\n\n").await;
            let _ = stream.shutdown().await;
            log.lock().unwrap().answered += 1;
            return;
        }
        Reply::Stream(pieces, _) => pieces.concat(),
        Reply::Whole(answer) => answer,
    };
    let reply = if models {
        serde_json::json!({"object": "list", "data": [{"id": "other-model"}, {"id": "fake-model"}]})
    } else {
        serde_json::json!({"choices": [{"index": 0, "message": {"role": "assistant", "content": answer}}]})
    }
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
    log.lock().unwrap().answered += 1;
}

#[cfg(test)]
mod proxy_tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    /// An echo server, the proxy in front, each mode in turn.
    #[test]
    fn the_proxy_forwards_drops_and_holds() {
        let echo = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let target = echo.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in echo.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut stream = stream;
                    let mut buffer = [0; 64];
                    while let Ok(read) = stream.read(&mut buffer) {
                        if read == 0 || stream.write_all(&buffer[..read]).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let proxy = Proxy::start(target);
        let connect = || {
            let stream = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            stream
        };
        let echoes = |stream: &mut TcpStream, text: &[u8]| -> std::io::Result<Vec<u8>> {
            stream.write_all(text)?;
            let mut buffer = vec![0; text.len()];
            stream.read_exact(&mut buffer)?;
            Ok(buffer)
        };
        let mut first = connect();
        assert_eq!(echoes(&mut first, b"one").unwrap(), b"one");

        proxy.set(ProxyMode::BlackHole);
        std::thread::sleep(Duration::from_millis(50));
        assert!(echoes(&mut first, b"two").is_err(), "held");
        proxy.set(ProxyMode::Forward);
        let mut late = vec![0; 3];
        first
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        first.read_exact(&mut late).unwrap();
        assert_eq!(late, b"two", "what was held goes on");

        proxy.set(ProxyMode::Drop);
        std::thread::sleep(Duration::from_millis(50));
        let mut rest = [0; 8];
        assert!(matches!(first.read(&mut rest), Ok(0) | Err(_)), "closed");
        let mut refused = connect();
        assert!(echoes(&mut refused, b"x").is_err(), "a new one too");

        proxy.set(ProxyMode::Forward);
        let mut again = connect();
        assert_eq!(echoes(&mut again, b"back").unwrap(), b"back");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_list_of_required_servers_names_whole_container_names() {
        let list = "kerno-postgres, kerno-mysql";
        assert!(super::names(list, "kerno-postgres"));
        assert!(super::names(list, "kerno-mysql"));
        assert!(!super::names(list, "kerno-mysql57"));
        assert!(!super::names(list, "kerno-mariadb"));
        assert!(!super::names("", "kerno-postgres"));
    }
}
