//! SSH tunnels through the system's OpenSSH client (`ssh`), for what the
//! built-in client cannot do: `~/.ssh/config` aliases, ProxyJump and jump
//! hosts, IdentityAgent (1Password, Secretive), RSA and every other key type,
//! and OpenSSH's own known_hosts.
//!
//! One `ssh -N` runs per tunnel. It forwards a Unix socket in a private
//! (0700) directory to the database, so no TCP port is opened for other
//! local processes (the same promise as the built-in client, see
//! [`crate::ssh`]); the socket appears once `ssh` has authenticated, which is
//! how the tunnel knows it is up.
//!
//! `ssh` never asks anything on a terminal: `SSH_ASKPASS` points at Kerno
//! itself (forced), and [`answer_askpass`] passes the question to the tunnel
//! over a second socket in that directory. The tunnel answers the passphrase
//! of its own key file (from the secret store), answers "yes" to a host-key
//! question only when the fingerprint shown is the one the user chose to
//! trust, and refuses everything else, passwords included. No secret is ever
//! in an argument or in the environment of `ssh`.
//!
//! The running tunnel is Unix's (`openssh/unix.rs`) or Windows'
//! (`openssh/windows.rs`, K69, D-138). Windows has no Unix sockets in std
//! or tokio: there each stream to the database is its own `ssh -W`, and the
//! questions come back over a named pipe whose ends prove themselves.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::config::{Secret, SshAuth, SshConfig};
use crate::ssh::SshError;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
pub(crate) use unix::LocalStream;
#[cfg(unix)]
pub use unix::{Forward, answer_askpass};
#[cfg(windows)]
pub(crate) use windows::LocalStream;
#[cfg(windows)]
pub use windows::{Forward, answer_askpass, stop_all};

/// Variable that tells a child `ssh`'s askpass program (Kerno) which socket
/// to ask.
pub const ASKPASS_VAR: &str = "KERNO_ASKPASS";

/// Said when no `ssh` program can be used.
#[cfg(not(windows))]
pub const NOT_FOUND: &str = "OpenSSH was not found on this computer; use the built-in client";
/// Said when no `ssh.exe` can be used: Windows' own is an optional feature
/// (K69, D-138), and no other is ever looked for on `PATH`.
#[cfg(windows)]
pub const NOT_FOUND: &str = "Windows' OpenSSH client is not installed: add it in Settings ▸ System ▸ \
     Optional features, or use the built-in client";

/// Said when Windows' `ssh.exe` cannot ask Kerno its questions: before
/// OpenSSH 8.4 it ignores `SSH_ASKPASS_REQUIRE` (D-138).
pub const TOO_OLD: &str = "This OpenSSH is too old to ask Kerno for a passphrase: load the key \
     in the agent, or use the built-in client";

/// How the failure of a tunnel begins when Windows' `ssh.exe` could not
/// start Kerno's helper ([`askpass_spelling`] found no ASCII spelling of its
/// path); the line goes on with the letter that stopped it.
pub const ASKPASS_UNREACHABLE: &str = "Windows' OpenSSH cannot start Kerno's helper";

/// Where the system's OpenSSH is and how Kerno runs it. The app builds it
/// once; tests pass their own configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenSshEnv {
    /// The `ssh` program.
    pub ssh: PathBuf,
    /// A configuration file passed as `-F` (`KERNO_SSH_CONFIG`, or a test's
    /// file). `None` lets `ssh` read its own (`~/.ssh/config`, then the
    /// system's).
    pub config: Option<PathBuf>,
    /// The file the dialog lists `Host` aliases from: `config`, else
    /// `~/.ssh/config`.
    pub aliases_from: Option<PathBuf>,
    /// The program `ssh` runs to ask a question: Kerno itself, which
    /// answers through [`answer_askpass`].
    pub askpass: PathBuf,
    /// Windows: the folder the tunnels' private folders are made in (the
    /// app gives `AppPaths::local_data_dir()`'s `ssh`, in the profile);
    /// `None` takes that same folder. Unix keeps the temporary folder (a
    /// Unix socket's path must stay short), whatever this says.
    pub tunnels: Option<PathBuf>,
}

impl OpenSshEnv {
    /// Finds `ssh` (see [`find_ssh`]); `None` when there is none.
    pub fn detect(
        path: Option<&OsStr>,
        home: Option<&Path>,
        config: Option<PathBuf>,
        askpass: PathBuf,
    ) -> Option<Self> {
        let ssh = find_ssh(path)?;
        let aliases_from = config
            .clone()
            .or_else(|| home.map(|home| home.join(".ssh").join("config")));
        Some(Self {
            ssh,
            config,
            aliases_from,
            askpass,
            tunnels: None,
        })
    }
}

/// The `ssh` program: the first on `path`, then `/usr/bin`,
/// `/opt/homebrew/bin` and `/usr/local/bin`.
#[cfg(unix)]
pub fn find_ssh(path: Option<&OsStr>) -> Option<PathBuf> {
    let listed = path.map(std::env::split_paths).into_iter().flatten();
    let usual = ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    listed
        .chain(usual)
        .map(|dir| dir.join("ssh"))
        .find(|candidate| candidate.is_file())
}

/// The first `ssh.exe` of [`windows_ssh_candidates`] that exists; `path`
/// (`PATH`) is never searched.
#[cfg(windows)]
pub fn find_ssh(path: Option<&OsStr>) -> Option<PathBuf> {
    let var = |name| std::env::var(name).ok();
    windows_ssh_candidates(
        var("SystemRoot").as_deref(),
        var("ProgramFiles").as_deref(),
        path,
    )
    .into_iter()
    .find(|candidate| candidate.is_file())
}

/// Where Kerno takes `ssh.exe` on Windows, in order (K69, D-138): the one
/// Windows ships, `C:\Windows\System32\OpenSSH\ssh.exe` (the optional
/// feature "OpenSSH Client"), then the one Win32-OpenSSH's installer puts in
/// `C:\Program Files\OpenSSH\ssh.exe`: folders only administrators can
/// change. Each only when `SystemRoot`, `ProgramFiles` is exactly that folder
/// (any case, a trailing separator ignored): a process of the user's could
/// set either before the user starts Kerno as administrator, and plant an
/// `ssh.exe` there. A Windows installed elsewhere than `C:\Windows` gets no
/// system OpenSSH. Never `PATH` (taken and ignored here, so that a test
/// proves it): an `ssh.exe` a program or a user put on it (Git's, a download
/// folder's) is never run.
#[cfg(any(windows, test))]
fn windows_ssh_candidates(
    system_root: Option<&str>,
    program_files: Option<&str>,
    _path: Option<&OsStr>,
) -> Vec<PathBuf> {
    let system = crate::tools::system_program(system_root, r"OpenSSH\ssh.exe");
    system
        .into_iter()
        .chain(installed_ssh(program_files))
        .collect()
}

/// Win32-OpenSSH's installer's `ssh.exe`, `C:\Program Files\OpenSSH\ssh.exe`,
/// only when `program_files` is exactly `C:\Program Files` (ASCII case
/// ignored, a trailing `\` or `/` trimmed): the rule of
/// [`crate::tools::system_program`].
#[cfg(any(windows, test))]
fn installed_ssh(program_files: Option<&str>) -> Option<PathBuf> {
    let folder = program_files?.trim_end_matches(['\\', '/']);
    folder
        .eq_ignore_ascii_case(r"C:\Program Files")
        .then(|| PathBuf::from(r"C:\Program Files\OpenSSH\ssh.exe"))
}

/// The version `ssh -V` printed (`OpenSSH_for_Windows_9.5p1, …`,
/// `OpenSSH_9.6p1 …`): major and minor.
#[cfg(any(windows, test))]
fn ssh_version(text: &str) -> Option<(u32, u32)> {
    let (_, rest) = text.split_once("OpenSSH_")?;
    let rest = rest.strip_prefix("for_Windows_").unwrap_or(rest);
    let (major, rest) = rest.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Whether an `ssh` of `version` ignores `SSH_ASKPASS_REQUIRE` (8.4).
#[cfg(any(windows, test))]
fn too_old(version: (u32, u32)) -> bool {
    version < (8, 4)
}

/// The SSH settings `ssh.exe` is given, on Windows (K69, D-138): the key
/// file with `~\` put under `home` and refused when it is not a file of this
/// computer ([`crate::local_path::resolve_with`]: a share is opened over SMB
/// with the user's credentials), the agent refused when it is not a pipe of
/// this machine ([`crate::ssh::is_local_pipe`]). These are the only paths a
/// saved connection (maybe from an imported file) puts on `ssh.exe`'s
/// command line (`-i`, `-o IdentityAgent=`); `-F` is the tunnel's own file.
/// Checked before anything opens the key or `ssh.exe` starts.
#[cfg(any(windows, test))]
fn resolve_ssh(ssh: &SshConfig, home: Option<&Path>) -> Result<SshConfig, SshError> {
    let mut resolved = ssh.clone();
    match &mut resolved.auth {
        SshAuth::KeyFile { path } => {
            *path =
                crate::local_path::resolve_with(path, "key", home).map_err(|_| SshError::Key {
                    path: path.display().to_string(),
                    reason: format!(
                        "Kerno opens no file of another computer: use {}",
                        crate::local_path::LOCAL_FILE
                    ),
                })?;
        }
        SshAuth::Agent {
            socket: Some(socket),
        } => {
            let text = socket.to_string_lossy();
            if !crate::ssh::is_local_pipe(&text) {
                return Err(SshError::Agent(format!(
                    "The SSH agent's address “{text}” is not a local pipe: on Windows it looks \
                     like \\\\.\\pipe\\openssh-ssh-agent. Clear it in the connection's SSH tab \
                     to use the system's agent."
                )));
            }
        }
        SshAuth::Agent { socket: None } | SshAuth::Password => {}
    }
    Ok(resolved)
}

/// Whether `passphrase` opens the key file at `path`: `Some(false)` only for
/// an encrypted key in OpenSSH's format that it does not decrypt; `None`
/// when that cannot be told here (unreadable, another format, not
/// encrypted), and `ssh` finds out itself.
#[cfg(any(windows, test))]
fn passphrase_opens(path: &Path, passphrase: &Secret) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    let key = russh::keys::ssh_key::PrivateKey::from_openssh(text.trim()).ok()?;
    key.is_encrypted()
        .then(|| key.decrypt(passphrase.expose()).is_ok())
}

// Arguments --------------------------------------------------------------------

/// How the database reaches Kerno through `ssh`.
#[derive(Debug, Clone, Copy)]
pub enum Via<'a> {
    /// `ssh -N -L <socket>:…`: one `ssh` forwards this Unix socket (Unix).
    Socket(&'a Path),
    /// `ssh -W …`: one `ssh` per stream, the stream being its standard input
    /// and output (Windows, D-138). `LogLevel VERBOSE` makes it say when it
    /// has logged in (`Authenticated to …`), which is how Kerno knows.
    Pipes,
}

/// The arguments of the `ssh` that leads to `db_host:db_port` as the SSH
/// server sees it, the way `via` says. No secret is ever in them.
pub fn tunnel_args(
    ssh: &SshConfig,
    config: Option<&Path>,
    via: Via<'_>,
    db_host: &str,
    db_port: u16,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |arg: &str| args.push(arg.into());
    if let Via::Socket(_) = via {
        push("-N");
    }
    for option in [
        "ExitOnForwardFailure=yes",
        // Questions go to Kerno (askpass), which refuses what it cannot
        // answer: a BatchMode set in the user's file would hide the key's
        // passphrase question.
        "BatchMode=no",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
        // A ControlMaster of the user's would make this tunnel a client of
        // another process, or leave one behind.
        "ControlPath=none",
    ] {
        push("-o");
        push(option);
    }
    if let Via::Pipes = via {
        push("-o");
        push("LogLevel=VERBOSE");
    }
    if ssh.keepalive_secs > 0 {
        push("-o");
        push(&format!("ServerAliveInterval={}", ssh.keepalive_secs));
        push("-o");
        push("ServerAliveCountMax=3");
    }
    if let Some(jump) = ssh.jump.as_deref().map(str::trim).filter(|j| !j.is_empty()) {
        push("-J");
        push(jump);
    }
    // Left out when empty: the SSH configuration provides them, and a
    // value on the command line would override it.
    if ssh.port > 0 {
        push("-p");
        push(&ssh.port.to_string());
    }
    if !ssh.user.trim().is_empty() {
        push("-l");
        push(ssh.user.trim());
    }
    let bracketed = if db_host.contains(':') {
        format!("[{db_host}]")
    } else {
        db_host.to_string()
    };
    if let Some(config) = config {
        args.push("-F".into());
        args.push(config.into());
    }
    match &ssh.auth {
        SshAuth::KeyFile { path } => {
            args.extend(["-i".into(), path.into()]);
            args.extend(["-o".into(), "IdentitiesOnly=yes".into()]);
        }
        SshAuth::Agent {
            socket: Some(agent),
        } => {
            let mut option = OsString::from("IdentityAgent=");
            option.push(agent);
            args.extend(["-o".into(), option]);
        }
        // From the SSH configuration and SSH_AUTH_SOCK; a password is
        // refused by `ConnectionConfig::validate`.
        SshAuth::Agent { socket: None } | SshAuth::Password => {}
    }
    match via {
        Via::Socket(socket) => {
            let forward = format!("{}:{bracketed}:{db_port}", socket.display());
            args.extend(["-L".into(), forward.into()]);
        }
        Via::Pipes => args.extend(["-W".into(), format!("{bracketed}:{db_port}").into()]),
    }
    // `--`: a host field starting with `-` is a host, never an option.
    args.extend(["--".into(), ssh.host.trim().into()]);
    args
}

/// Whether `text` is safe as a host, user or jump-host list for `ssh`:
/// OpenSSH puts jump hosts in a shell command (ProxyJump), so nothing that a
/// shell would read as more than a word, and nothing `ssh` would read as an
/// option: neither a word nor the host after its `user@` starts with `-`, and
/// in a list that holds for each jump host, not only the first.
pub fn safe_ssh_word(text: &str, list: bool) -> bool {
    let word = |word: &str| {
        let host = word.rsplit('@').next().unwrap_or(word);
        !word.is_empty()
            && !word.starts_with('-')
            && !host.trim_start_matches('[').starts_with('-')
            && word.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | ':' | '[' | ']')
            })
    };
    if list {
        text.split(',').all(word)
    } else {
        word(text)
    }
}

// The tunnel ---------------------------------------------------------------------

/// What one OpenSSH tunnel needs.
#[derive(Debug, Clone)]
pub struct OpenSshRequest<'a> {
    /// The SSH settings.
    pub ssh: &'a SshConfig,
    /// The system's OpenSSH.
    pub env: &'a OpenSshEnv,
    /// The database, as the SSH server sees it.
    pub db_host: &'a str,
    /// The database port.
    pub db_port: u16,
    /// The passphrase of the key file, answered when `ssh` asks for it.
    pub key_passphrase: Option<&'a Secret>,
    /// A host-key fingerprint the user chose to trust (`SHA256:…`).
    pub trusted_fingerprint: Option<&'a str>,
}

// What ssh says and asks ------------------------------------------------------

/// How long `ssh` may take to authenticate (jump hosts included).
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// Lines of `ssh`'s messages kept for the explanation of a failure.
const TAIL_LINES: usize = 64;

/// What `ssh` printed: the last lines, and how many forwards failed.
#[derive(Debug, Default)]
struct Messages {
    lines: VecDeque<String>,
    forward_failures: u64,
    last_forward_failure: Option<String>,
}

impl Messages {
    fn push(&mut self, line: &str) {
        let line = line.trim_end_matches('\r').to_string();
        if line.contains("open failed") || line.contains("forwarding failed") {
            self.forward_failures += 1;
            self.last_forward_failure = Some(line.clone());
        }
        if self.lines.len() == TAIL_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    fn text(&self) -> String {
        self.lines
            .iter()
            .map(String::as_str)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A new directory only this user can enter, with a short path: a Unix
/// socket path is limited to about 100 bytes.
#[cfg(unix)]
fn private_dir() -> std::io::Result<PathBuf> {
    private_dir_in(&std::env::temp_dir())
}

/// A new private directory in `base`, named `kerno-ssh-` and 12 hex digits:
/// no space, whatever `base` holds.
fn private_dir_in(base: &Path) -> std::io::Result<PathBuf> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let dir = base.join(format!("kerno-ssh-{}", &id[..12]));
    crate::private::create_dir(&dir)?;
    Ok(dir)
}

/// Whether `line` is the one `ssh -o LogLevel=VERBOSE` prints once it has
/// logged in, in OpenSSH's exact shape (`sshconnect2.c`):
/// `Authenticated to <host> ([<address>]:<port>) using "<method>".`, or
/// `(via proxy)` in place of the address behind a jump host. A server's
/// banner can still print that very text: see D-138 for what it can do then.
#[cfg(any(windows, test))]
fn is_login_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("Authenticated to ") else {
        return false;
    };
    let Some((host, rest)) = rest.split_once(" (") else {
        return false;
    };
    let Some((place, rest)) = rest.split_once(") using \"") else {
        return false;
    };
    let Some(method) = rest.strip_suffix("\".") else {
        return false;
    };
    let word = |text: &str| !text.is_empty() && !text.contains(char::is_whitespace);
    let address = place
        .strip_prefix('[')
        .and_then(|place| place.rsplit_once("]:"))
        .is_some_and(|(address, port)| {
            word(address) && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
        });
    word(host)
        && (place == "via proxy" || address)
        && !method.is_empty()
        && method
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-@.".contains(&b))
}

/// The configuration every tunnel's `ssh` reads (`-F`), jump hosts
/// included: no password and no keyboard-interactive login, so no server
/// can put a question of its own in front of the askpass (one worded like
/// the key's passphrase question would be answered); then the user's file
/// and the system's (`system`), which `-F` would otherwise leave out. The
/// first value of an option wins, so these three cannot be overridden below.
fn guard_config(user: Option<&Path>, system: &str) -> String {
    let mut text =
        String::from("KbdInteractiveAuthentication no\nPasswordAuthentication no\nBatchMode no\n");
    // A path that cannot be quoted is left out rather than half-read.
    if let Some(user) = user.and_then(Path::to_str)
        && !user.contains(['"', '\n'])
    {
        text.push_str(&format!("Include \"{user}\"\n"));
    }
    text.push_str(&format!("Include {system}\n"));
    text
}

/// What the tunnel may answer.
#[derive(Debug, Clone, Default)]
struct Answers {
    /// The key file of the connection, as given to `ssh -i`.
    key: Option<String>,
    passphrase: Option<Secret>,
    trusted: Option<String>,
}

/// A host key `ssh` asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostKeyQuestion {
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
}

/// What the questions told about a failure.
#[derive(Debug, Default)]
struct Observed {
    /// The `ssh` processes the passphrase was given to (a jump host's
    /// `ssh` asks for the same key on its own).
    passphrase_given: Vec<u32>,
    /// `a passphrase is required` / `the passphrase is wrong`, for the key.
    passphrase: Option<&'static str>,
    /// Another key file (from the SSH configuration) asked for one.
    other_key: Option<String>,
    /// A host key that was not trusted.
    host_key: Option<HostKeyQuestion>,
}

/// The answer to one question of the `ssh` process `asker`, or `None` to
/// refuse it.
fn decide(prompt: &str, asker: u32, answers: &Answers, observed: &mut Observed) -> Option<Secret> {
    if prompt.contains("authenticity of host") {
        let question = host_key_question(prompt)?;
        if answers.trusted.as_deref() == Some(question.fingerprint.as_str()) {
            return Some(Secret::new("yes"));
        }
        observed.host_key = Some(question);
        return None;
    }
    if prompt.to_ascii_lowercase().contains("passphrase") {
        // Exactly OpenSSH's own question for exactly this key (it cuts the
        // path at 100 characters): nothing a server could word differently.
        let named = prompt
            .strip_prefix("Enter passphrase for key '")
            .and_then(|rest| rest.strip_suffix("': "))
            .unwrap_or_default();
        let ours = answers.key.as_deref().is_some_and(|key| {
            let (named, key) = (key_spelling(named), key_spelling(key));
            !named.is_empty() && (named == key || (named.len() == 100 && key.starts_with(&named)))
        });
        if !ours {
            // A question in other words names no key worth reporting.
            if !named.is_empty() {
                observed.other_key = Some(named.to_string());
            }
            return None;
        }
        // Asked twice by the same `ssh`: the first answer did not open it.
        let asked_before = observed.passphrase_given.contains(&asker);
        return match (&answers.passphrase, asked_before) {
            (None, _) => {
                observed.passphrase = Some("a passphrase is required");
                None
            }
            (Some(_), true) => {
                observed.passphrase = Some("the passphrase is wrong");
                None
            }
            (Some(passphrase), false) => {
                observed.passphrase_given.push(asker);
                Some(passphrase.clone())
            }
        };
    }
    // Passwords, keyboard-interactive, anything else: refused.
    None
}

/// A key file's path as compared with the one `ssh` names in its question:
/// as is on Unix; on Windows with `/` for `\` and ASCII case folded, the way
/// Windows reads a path (`~/.ssh/id` expands to `C:\Users\x/.ssh/id`, and a
/// jump host's `IdentityFile` may be written either way: K69, S6).
fn key_spelling(path: &str) -> String {
    if cfg!(windows) {
        path.replace('\\', "/").to_ascii_lowercase()
    } else {
        path.to_string()
    }
}

/// The first `'…'` in `text`.
fn quoted(text: &str) -> Option<&str> {
    let (_, rest) = text.split_once('\'')?;
    rest.split_once('\'').map(|(inside, _)| inside)
}

/// Reads ssh's "The authenticity of host 'h (ip)' can't be established. /
/// ED25519 key fingerprint is SHA256:…" question.
fn host_key_question(prompt: &str) -> Option<HostKeyQuestion> {
    let named = quoted(prompt)?;
    let name = named.split(" (").next().unwrap_or(named);
    let (host, port) = host_and_port(name);
    // "ED25519 key fingerprint is SHA256:…." (OpenSSH 9), "is: SHA256:…" (10).
    let line = prompt
        .lines()
        .find(|line| line.contains(" key fingerprint is"))?;
    let (algorithm, fingerprint) = line.split_once(" key fingerprint is")?;
    let fingerprint = fingerprint
        .trim_start_matches(':')
        .split_whitespace()
        .next()?
        .trim_end_matches('.');
    Some(HostKeyQuestion {
        host,
        port,
        algorithm: algorithm.trim().to_string(),
        fingerprint: fingerprint.to_string(),
    })
}

/// `[h]:p` → (`h`, `p`); `h` → (`h`, 22), as known_hosts writes them.
fn host_and_port(name: &str) -> (String, u16) {
    if let Some(rest) = name.strip_prefix('[')
        && let Some((host, port)) = rest.split_once("]:")
        && let Ok(port) = port.parse()
    {
        return (host.to_string(), port);
    }
    (name.to_string(), 22)
}

/// Why `ssh` exited, from its questions and its messages. `key` is the
/// connection's key file.
fn failure(observed: &Observed, key: Option<&str>, stderr: &str) -> SshError {
    if let Some(question) = &observed.host_key {
        return SshError::UnknownHostKey {
            host: question.host.clone(),
            port: question.port,
            algorithm: question.algorithm.clone(),
            fingerprint: question.fingerprint.clone(),
        };
    }
    // The key could not be opened: the user can type its passphrase.
    if let (Some(reason), Some(key)) = (observed.passphrase, key) {
        return SshError::Passphrase {
            path: key.to_string(),
            reason,
        };
    }
    if let Some(changed) = host_key_changed(stderr) {
        return changed;
    }
    let stderr = match &observed.other_key {
        Some(other) => format!("the key {other} needs a passphrase, which ssh asked for\n{stderr}"),
        None => stderr.to_string(),
    };
    SshError::OpenSsh { stderr }
}

/// ssh's "REMOTE HOST IDENTIFICATION HAS CHANGED!" message, read.
fn host_key_changed(stderr: &str) -> Option<SshError> {
    if !stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        return None;
    }
    let mut lines = stderr.lines();
    let fingerprint = lines
        .by_ref()
        .find(|line| line.contains("sent by the remote host is"))
        .and_then(|_| lines.next())
        .map(|line| line.trim().trim_end_matches('.').to_string())
        .unwrap_or_default();
    let (file, line) = stderr
        .lines()
        .filter(|line| line.starts_with("Offending"))
        .find_map(|line| line.split_once(" key in ").map(|(_, at)| at.trim()))
        .and_then(|at| at.rsplit_once(':'))
        .map(|(file, line)| (file.to_string(), line.parse().unwrap_or(0)))
        .unwrap_or_default();
    let name = stderr
        .lines()
        .find_map(|line| line.split_once("ost key for ").map(|(_, rest)| rest))
        .and_then(|rest| rest.split(" has changed").next())
        .unwrap_or_default();
    let (host, port) = host_and_port(name);
    Some(SshError::HostKeyChanged {
        host,
        port,
        fingerprint,
        file,
        line,
    })
}

// ~/.ssh/config ----------------------------------------------------------------

/// The `Host` aliases of an SSH configuration file and the files it
/// `Include`s, in order and without duplicates: names only, no patterns
/// (`*`, `?`, `!`). `Match` blocks are skipped. Relative includes are read
/// from the including file's directory; `~` is `home`.
pub fn host_aliases(config: &Path, home: Option<&Path>) -> Vec<String> {
    let mut aliases = Vec::new();
    read_aliases(config, home, 0, &mut aliases);
    aliases
}

fn read_aliases(file: &Path, home: Option<&Path>, depth: usize, out: &mut Vec<String>) {
    // ssh itself stops at 16; a loop of includes stops here.
    if depth > 8 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    let dir = file.parent().unwrap_or(Path::new("/"));
    let mut in_match = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (keyword, rest) = line
            .split_once(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or((line, ""));
        let words = config_words(rest.trim_start_matches(|c: char| c.is_whitespace() || c == '='));
        match keyword.to_ascii_lowercase().as_str() {
            "host" => {
                in_match = false;
                for word in words {
                    let pattern = word.contains(['*', '?', '!']);
                    if !pattern && !out.contains(&word) {
                        out.push(word);
                    }
                }
            }
            "match" => in_match = true,
            "include" if !in_match => {
                for word in words {
                    for path in include_paths(&word, dir, home) {
                        read_aliases(&path, home, depth + 1, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The words of a configuration line, `"…"` quotes removed.
fn config_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// The files an `Include` names: `~` expanded, relative to `dir`, a glob
/// (`*`, `?`) in the file name matched against the directory, sorted. On
/// Windows Kerno opens none that is not a file of this computer
/// ([`crate::local_path::is_local_file`]): reading `\\host\share\…` would log in
/// there; `ssh.exe` itself does what the user's file says (K69, D-138).
fn include_paths(word: &str, dir: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let path = match (word.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => dir.join(word),
    };
    if cfg!(windows) && !crate::local_path::is_local_file(&path.to_string_lossy()) {
        return Vec::new();
    }
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return Vec::new();
    };
    if !name.contains(['*', '?']) {
        return vec![path];
    }
    let parent = path.parent().unwrap_or(Path::new("/"));
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|file| wildcard(name.as_bytes(), file.as_bytes()))
        })
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    found.sort();
    found
}

/// `*` and `?` matching, as ssh's globs do in a file name.
fn wildcard(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            wildcard(&pattern[1..], text) || (!text.is_empty() && wildcard(pattern, &text[1..]))
        }
        (Some(b'?'), Some(_)) => wildcard(&pattern[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => wildcard(&pattern[1..], &text[1..]),
        _ => false,
    }
}

// Windows: the processes of a tunnel, and its helper's spelling ---------------

/// One process of a listing: its id, its parent's id, when it started.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessRow {
    pid: u32,
    parent: u32,
    started: u64,
}

/// The rows of a listing of `<pid> <parent pid> <start time>` lines; other
/// lines are skipped.
#[cfg(any(windows, test))]
fn process_rows(text: &str) -> Vec<ProcessRow> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace().map(str::parse::<u64>);
            let (Some(Ok(pid)), Some(Ok(parent)), Some(Ok(started)), None) =
                (words.next(), words.next(), words.next(), words.next())
            else {
                return None;
            };
            Some(ProcessRow {
                pid: u32::try_from(pid).ok()?,
                parent: u32::try_from(parent).ok()?,
                started,
            })
        })
        .collect()
}

/// `roots` and every process under them in `rows`, the deepest last: a
/// child is one whose parent id is the process's and that started after it
/// (Windows keeps a dead parent's id, which a later process may get). Taken
/// all at once, before anything is stopped: once a middle `ssh.exe` of a
/// chain of jump hosts is gone, the `ssh.exe` it started has no parent left
/// to be found by (seen on the runner with two hops, K69).
#[cfg(any(windows, test))]
fn tree(rows: &[ProcessRow], roots: &[u32]) -> Vec<u32> {
    let mut found: Vec<ProcessRow> = rows
        .iter()
        .filter(|row| roots.contains(&row.pid))
        .copied()
        .collect();
    let mut at = 0;
    while let Some(&parent) = found.get(at) {
        for row in rows {
            let child = row.parent == parent.pid
                && row.started >= parent.started
                && row.pid != parent.pid
                && !found.iter().any(|seen| seen.pid == row.pid);
            if child {
                found.push(*row);
            }
        }
        at += 1;
    }
    // A root no listing showed is still stopped by its own id.
    let unlisted: Vec<u32> = roots
        .iter()
        .copied()
        .filter(|root| !found.iter().any(|row| row.pid == *root))
        .collect();
    found.iter().map(|row| row.pid).chain(unlisted).collect()
}

/// How `ssh.exe` is told where Kerno's helper is (`SSH_ASKPASS`): Windows'
/// OpenSSH reads that variable in the system's ANSI code page and starts it
/// as UTF-8, so a path with a letter beyond ASCII ("Élodie") names no file
/// (`CreateProcessW failed error:2`). The path as it is when it is ASCII;
/// else relative to `base`, where `ssh.exe` runs (its jump hosts' too),
/// which climbs out of a profile folder with `..` and is ASCII whenever the
/// helper's own folders below the common one are (an install under
/// `%LOCALAPPDATA%\Programs` beside the tunnels' `%LOCALAPPDATA%\Kerno`);
/// `Err` with the first other letter when no ASCII spelling exists.
///
/// A relative spelling always begins with `.\` or `..\` (T1): Windows
/// resolves such a name against the current folder only, while a relative
/// name like `Kerno\kerno.exe` is looked for first in the folder of the
/// program that starts it (`CreateProcessW`'s search), which another
/// account might control. The runner proves which file runs
/// (`openssh_windows::a_relative_helper_is_never_taken_from_ssh_exe_s_folder`).
#[cfg(any(windows, test))]
fn askpass_spelling(askpass: &Path, base: &Path) -> Result<OsString, char> {
    let first_other = |path: &Path| path.to_string_lossy().chars().find(|c| !c.is_ascii());
    let Some(letter) = first_other(askpass) else {
        return Ok(askpass.as_os_str().to_owned());
    };
    let same = |a: &std::path::Component<'_>, b: &std::path::Component<'_>| {
        let (a, b) = (
            a.as_os_str().to_string_lossy(),
            b.as_os_str().to_string_lossy(),
        );
        if cfg!(windows) {
            a.to_lowercase() == b.to_lowercase()
        } else {
            a == b
        }
    };
    let (to, from): (Vec<_>, Vec<_>) =
        (askpass.components().collect(), base.components().collect());
    let common = to.iter().zip(&from).take_while(|(a, b)| same(a, b)).count();
    // Another drive, or nothing in common: no relative path.
    if common == 0 || !askpass.is_absolute() || !base.is_absolute() {
        return Err(letter);
    }
    // `.` first when nothing is climbed: never a name searched elsewhere.
    let mut relative = PathBuf::from(if common == from.len() { "." } else { ".." });
    for _ in common + 1..from.len() {
        relative.push("..");
    }
    for part in &to[common..] {
        relative.push(part.as_os_str());
    }
    match first_other(&relative) {
        None => Ok(relative.into_os_string()),
        Some(letter) => Err(letter),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SshClient;

    fn ssh(auth: SshAuth) -> SshConfig {
        SshConfig {
            host: "prod".into(),
            port: 0,
            user: String::new(),
            auth,
            keepalive_secs: 0,
            client: SshClient::OpenSsh,
            jump: None,
        }
    }

    fn words(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// J2 (DBeaver #5845, #14749): every combination of fields, and never a
    /// secret on the command line.
    #[test]
    fn the_arguments_follow_the_fields() {
        let socket = Via::Socket(Path::new("/tmp/kerno-ssh-1/db"));
        let base = [
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "BatchMode=no",
            "-o",
            "PasswordAuthentication=no",
            "-o",
            "KbdInteractiveAuthentication=no",
            "-o",
            "ControlPath=none",
        ];
        // An alias alone: port, user and key come from the file.
        let alias = words(&tunnel_args(
            &ssh(SshAuth::Agent { socket: None }),
            None,
            socket,
            "db.internal",
            5432,
        ));
        let mut expected: Vec<&str> = base.to_vec();
        expected.extend(["-L", "/tmp/kerno-ssh-1/db:db.internal:5432", "--", "prod"]);
        assert_eq!(alias, expected);

        // Everything set.
        let mut full = ssh(SshAuth::KeyFile {
            path: "/home/ada/.ssh/id_rsa".into(),
        });
        full.port = 2222;
        full.user = "ada".into();
        full.keepalive_secs = 15;
        full.jump = Some(" ops@jump:22,edge ".into());
        let args = words(&tunnel_args(
            &full,
            Some(Path::new("/etc/kerno/ssh_config")),
            socket,
            "::1",
            3306,
        ));
        let mut expected: Vec<&str> = base.to_vec();
        expected.extend([
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-J",
            "ops@jump:22,edge",
            "-p",
            "2222",
            "-l",
            "ada",
            "-F",
            "/etc/kerno/ssh_config",
            "-i",
            "/home/ada/.ssh/id_rsa",
            "-o",
            "IdentitiesOnly=yes",
            "-L",
            "/tmp/kerno-ssh-1/db:[::1]:3306",
            "--",
            "prod",
        ]);
        assert_eq!(args, expected);

        // An agent socket of its own (1Password, Secretive).
        let agent = words(&tunnel_args(
            &ssh(SshAuth::Agent {
                socket: Some("/Users/ada/agent.sock".into()),
            }),
            None,
            socket,
            "db",
            5432,
        ));
        assert!(
            agent
                .windows(2)
                .any(|pair| pair == ["-o", "IdentityAgent=/Users/ada/agent.sock"]),
            "{agent:?}"
        );
        // A host that looks like an option stays a host.
        let mut sneaky = ssh(SshAuth::Agent { socket: None });
        sneaky.host = "-oProxyCommand=id".into();
        let args = words(&tunnel_args(&sneaky, None, socket, "db", 1));
        assert_eq!(args[args.len() - 2..], ["--", "-oProxyCommand=id"]);
    }

    #[test]
    fn ssh_words_are_shell_safe() {
        for word in ["bastion", "ops@10.0.0.1:2222", "[::1]:22", "a_b-c.d"] {
            assert!(safe_ssh_word(word, false), "{word}");
        }
        assert!(safe_ssh_word("a@j1:22,j2", true));
        assert!(!safe_ssh_word("a@j1:22,j2", false));
        for word in [
            "", "a b", "a;b", "$(id)", "`id`", "a|b", "-oX=y", "a'b", "a\"b",
        ] {
            assert!(!safe_ssh_word(word, true), "{word}");
        }
        // An option hidden after the first jump host, or after a user.
        for word in [
            "j1,-D8080",
            "j1,,j2",
            "j1,",
            "ops@-D8080:22",
            "ops@[-w0]:22",
        ] {
            assert!(!safe_ssh_word(word, true), "{word}");
        }
        assert!(!safe_ssh_word("ops@-D8080", false));
    }

    /// K69 (R6): only OpenSSH's own login line says a stream is up: its
    /// exact shapes, nothing around them. Mutation seen: a prefix match.
    #[test]
    fn only_the_login_line_says_up() {
        for line in [
            r#"Authenticated to 127.0.0.1 ([127.0.0.1]:52290) using "publickey"."#,
            r#"Authenticated to db.example.com ([2001:db8::1]:22) using "keyboard-interactive"."#,
            r#"Authenticated to k69-alias (via proxy) using "publickey"."#,
        ] {
            assert!(is_login_line(line), "{line}");
        }
        for line in [
            "Authenticated to ",
            "Authenticated to the bastion, welcome",
            r#"Authenticated to 127.0.0.1 ([127.0.0.1]:52290) using "publickey". more"#,
            r#"Authenticated to 127.0.0.1 (127.0.0.1:52290) using "publickey"."#,
            r#"Authenticated to a b ([1.2.3.4]:22) using "publickey"."#,
            r#"Authenticated to h ([1.2.3.4]:x) using "publickey"."#,
            r#"Authenticated to h (via proxy) using "Public Key"."#,
            r#"banner: Authenticated to h (via proxy) using "publickey"."#,
        ] {
            assert!(!is_login_line(line), "{line}");
        }
    }

    /// K69 (R3): on Windows `ssh.exe` is taken from System32's OpenSSH, then
    /// `%ProgramFiles%\OpenSSH`, never from `PATH`, whatever `PATH` holds;
    /// a `SystemRoot` or `ProgramFiles` that is not a drive's path is not
    /// taken. Strings in, paths out: run on every system. Mutation seen: a
    /// `PATH` entry chained after the two folders.
    #[test]
    fn windows_ssh_exe_is_never_taken_from_path() {
        let path = std::env::join_paths([Path::new("dl"), Path::new("git")]).unwrap();
        let system = PathBuf::from(r"C:\Windows\System32\OpenSSH\ssh.exe");
        let installed = PathBuf::from(r"C:\Program Files\OpenSSH\ssh.exe");
        assert_eq!(
            windows_ssh_candidates(Some(r"C:\Windows"), Some(r"C:\Program Files"), Some(&path)),
            [system.clone(), installed.clone()]
        );
        assert_eq!(
            windows_ssh_candidates(Some(r"c:\windows\"), Some(r"C:\PROGRAM FILES\"), None),
            [system.clone(), installed]
        );
        for root in [
            None,
            Some(r"D:\Windows"),
            Some("Windows"),
            Some(r"\\host\share\Windows"),
            Some(""),
            Some(r"C:\Windows\..\Users\ada"),
        ] {
            for files in [
                None,
                Some(r"D:\Program Files"),
                Some("Program Files"),
                Some(""),
            ] {
                assert_eq!(
                    windows_ssh_candidates(root, files, Some(&path)),
                    Vec::<PathBuf>::new(),
                    "{root:?} {files:?}"
                );
            }
        }
        assert_eq!(
            windows_ssh_candidates(Some(r"C:\Windows"), Some(r"\\host\apps"), None),
            [system]
        );
    }

    /// K69: the dialog reads the profile's `.ssh\config` on Windows.
    #[cfg(windows)]
    #[test]
    fn the_profile_ssh_config_is_listed_on_windows() {
        let home = Path::new(r"C:\Users\ada");
        if let Some(env) = OpenSshEnv::detect(None, Some(home), None, "k".into()) {
            assert_eq!(
                env.aliases_from,
                Some(PathBuf::from(r"C:\Users\ada\.ssh\config"))
            );
        }
    }

    /// K69: an `Include` of the user's file that names another machine is
    /// not opened when the aliases are listed. Mutation seen: the check
    /// dropped (the share comes back as a file to read).
    #[cfg(windows)]
    #[test]
    fn an_include_on_another_machine_is_not_read() {
        let dir = Path::new(r"C:\Users\ada\.ssh");
        for word in [r"\\k69-nowhere\share\config", "//k69-nowhere/share/config"] {
            assert!(include_paths(word, dir, None).is_empty(), "{word}");
        }
        assert_eq!(
            include_paths("extra", dir, None),
            [PathBuf::from(r"C:\Users\ada\.ssh\extra")]
        );
    }

    /// K69: one `ssh -W` per stream on Windows: no `-N`, no `-L`, and the
    /// login said at `LogLevel VERBOSE`. Mutation seen: `-N` kept.
    #[test]
    fn a_stream_of_its_own_is_ssh_w() {
        let args = words(&tunnel_args(
            &ssh(SshAuth::Agent { socket: None }),
            Some(Path::new("C:/t/config")),
            Via::Pipes,
            "::1",
            5432,
        ));
        assert_eq!(
            args,
            [
                "-o",
                "ExitOnForwardFailure=yes",
                "-o",
                "BatchMode=no",
                "-o",
                "PasswordAuthentication=no",
                "-o",
                "KbdInteractiveAuthentication=no",
                "-o",
                "ControlPath=none",
                "-o",
                "LogLevel=VERBOSE",
                "-F",
                "C:/t/config",
                "-W",
                "[::1]:5432",
                "--",
                "prod",
            ]
        );
    }

    /// K69: `ssh -V`'s words, and 8.4 as the first that asks Kerno.
    /// Mutation seen: `<=` for `<`.
    #[test]
    fn the_version_of_ssh_is_read() {
        for (text, version) in [
            ("OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2", (9, 5)),
            ("OpenSSH_for_Windows_8.1p1, LibreSSL 3.0.2\r\n", (8, 1)),
            ("OpenSSH_for_Windows_7.7p1, LibreSSL 2.6.5", (7, 7)),
            ("OpenSSH_9.6p1 Ubuntu-3ubuntu13, OpenSSL 3.0.13", (9, 6)),
            ("OpenSSH_10.0p2, LibreSSL 3.3.6", (10, 0)),
        ] {
            assert_eq!(ssh_version(text), Some(version), "{text}");
        }
        assert_eq!(ssh_version("ssh: not found"), None);
        assert!(too_old((7, 7)) && too_old((8, 1)) && too_old((8, 3)));
        assert!(!too_old((8, 4)) && !too_old((8, 6)) && !too_old((9, 5)) && !too_old((10, 0)));
    }

    /// K69: what an imported connection could make `ssh.exe` reach on
    /// another machine is refused before it starts. Mutation seen: the UNC
    /// check dropped.
    #[test]
    fn a_key_or_agent_on_another_machine_is_refused() {
        let home = Path::new(r"C:\Users\Jean Dupont");
        let with = |auth: SshAuth| {
            let mut config = ssh(auth);
            config.client = SshClient::OpenSsh;
            resolve_ssh(&config, Some(home)).map(|config| config.auth)
        };
        let key = |path: &str| SshAuth::KeyFile { path: path.into() };
        for remote in [
            r"\\host\share\id",
            r"\\?\UNC\host\s\id",
            r"\\.\C:\id",
            "//host/share/id",
            r"\/host\share\id",
            "id_ed25519",
            r"C:\keys\nul",
        ] {
            assert!(
                matches!(with(key(remote)), Err(SshError::Key { .. })),
                "{remote}"
            );
        }
        for local in [r"C:\Users\ada\.ssh\id", "D:/keys/id"] {
            assert_eq!(with(key(local)).unwrap(), key(local), "{local}");
        }
        // `~/` (`~\` on Windows too) is put under the home: the path given
        // to ssh.exe and asked about is the expanded one.
        assert_eq!(
            with(key("~/.ssh/id")).unwrap(),
            SshAuth::KeyFile {
                path: home.join(".ssh/id")
            }
        );
        let agent = |socket: &str| SshAuth::Agent {
            socket: Some(socket.into()),
        };
        assert!(with(agent(r"\\.\pipe\openssh-ssh-agent")).is_ok());
        assert!(matches!(
            with(agent(r"\\evil\pipe\x")),
            Err(SshError::Agent(message)) if message.contains("not a local pipe")
        ));
        assert!(with(SshAuth::Agent { socket: None }).is_ok());
    }

    /// K69: a wrong passphrase is told before `ssh.exe` runs, from the key
    /// file itself; what cannot be told is left to `ssh`. Mutation seen:
    /// `decrypt` not called (always `Some(true)`).
    #[test]
    fn a_passphrase_is_tried_on_the_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let make = |name: &str, passphrase: &str| {
            let path = dir.path().join(name);
            std::process::Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-C", "k69", "-N", passphrase, "-f"])
                .arg(&path)
                .status()
                .ok()
                .filter(|status| status.success())
                .map(|_| path)
        };
        let (Some(encrypted), Some(plain)) = (make("enc", "k69 passphrase"), make("plain", ""))
        else {
            eprintln!("SKIPPED: ssh-keygen is not available");
            return;
        };
        let opens =
            |path: &Path, passphrase: &str| passphrase_opens(path, &Secret::new(passphrase));
        assert_eq!(opens(&encrypted, "k69 passphrase"), Some(true));
        assert_eq!(opens(&encrypted, "not it"), Some(false));
        assert_eq!(opens(&plain, "anything"), None);
        assert_eq!(opens(&dir.path().join("missing"), "x"), None);
        std::fs::write(dir.path().join("pem"), "-----BEGIN RSA PRIVATE KEY-----\n").unwrap();
        assert_eq!(opens(&dir.path().join("pem"), "x"), None);
    }

    #[cfg(unix)]
    #[test]
    fn ssh_is_found_on_path_or_in_the_usual_places() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ssh"), "").unwrap();
        let path = std::env::join_paths([Path::new("/nonexistent"), dir.path()]).unwrap();
        assert_eq!(find_ssh(Some(&path)), Some(dir.path().join("ssh")));
        // A GUI app's short PATH: the usual places.
        if Path::new("/usr/bin/ssh").is_file() {
            assert_eq!(
                find_ssh(Some(OsStr::new("/nonexistent"))),
                Some(PathBuf::from("/usr/bin/ssh"))
            );
        }
        let env = OpenSshEnv::detect(Some(&path), Some(Path::new("/home/ada")), None, "/k".into())
            .unwrap();
        assert_eq!(
            env.aliases_from,
            Some(PathBuf::from("/home/ada/.ssh/config"))
        );
        assert_eq!(env.config, None);
    }

    /// Beekeeper #1344, DBeaver #4516: the dialog offers the user's aliases.
    #[test]
    fn host_aliases_follow_includes_and_skip_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".ssh/conf.d")).unwrap();
        std::fs::create_dir_all(dir.path().join("work")).unwrap();
        let config = home.join(".ssh/config");
        std::fs::write(
            &config,
            "# mine\nHost prod-bastion staging *.internal !evil\n  User ops\n\
             Include conf.d/*.conf ~/.ssh/extra \"work dir\"\n\
             Match host db-*\n  Include ~/.ssh/never\nHost=db-jump\nHost prod-bastion\n\
             Include config\n",
        )
        .unwrap();
        std::fs::write(home.join(".ssh/conf.d/b.conf"), "host b-two\n").unwrap();
        std::fs::write(home.join(".ssh/conf.d/a.conf"), "HOST a-one a-?\n").unwrap();
        std::fs::write(home.join(".ssh/conf.d/skip.txt"), "Host not-included\n").unwrap();
        std::fs::write(home.join(".ssh/extra"), "Host extra\n").unwrap();
        std::fs::write(home.join(".ssh/never"), "Host in-match\n").unwrap();
        assert_eq!(
            host_aliases(&config, Some(&home)),
            [
                "prod-bastion",
                "staging",
                "a-one",
                "b-two",
                "extra",
                "db-jump"
            ]
        );
        assert!(host_aliases(&dir.path().join("missing"), None).is_empty());
    }

    fn answers(key: Option<&str>, passphrase: Option<&str>, trusted: Option<&str>) -> Answers {
        Answers {
            key: key.map(str::to_string),
            passphrase: passphrase.map(Secret::new),
            trusted: trusted.map(str::to_string),
        }
    }

    const HOST_KEY_PROMPT: &str = "The authenticity of host '[10.0.0.5]:2222 ([10.0.0.5]:2222)' \
         can't be established.\nED25519 key fingerprint is: SHA256:abcDEF+/12\nThis key is not \
         known by any other names.\nAre you sure you want to continue connecting \
         (yes/no/[fingerprint])? ";

    #[test]
    fn only_the_trusted_fingerprint_is_answered_yes() {
        let mut observed = Observed::default();
        let refused = decide(
            HOST_KEY_PROMPT,
            1,
            &answers(None, None, None),
            &mut observed,
        );
        assert!(refused.is_none());
        assert_eq!(
            observed.host_key,
            Some(HostKeyQuestion {
                host: "10.0.0.5".into(),
                port: 2222,
                algorithm: "ED25519".into(),
                fingerprint: "SHA256:abcDEF+/12".into(),
            })
        );
        let wrong = answers(None, None, Some("SHA256:other"));
        assert!(decide(HOST_KEY_PROMPT, 1, &wrong, &mut Observed::default()).is_none());
        let trusted = answers(None, None, Some("SHA256:abcDEF+/12"));
        let mut observed = Observed::default();
        let yes = decide(HOST_KEY_PROMPT, 1, &trusted, &mut observed).unwrap();
        assert_eq!(yes.expose(), "yes");
        assert_eq!(observed.host_key, None);
        // OpenSSH 9 words it without the colon, the name without a port.
        let old = "The authenticity of host 'bastion (10.0.0.5)' can't be established.\n\
                   ECDSA key fingerprint is SHA256:xyz.\nAre you sure (yes/no)? ";
        let question = host_key_question(old).unwrap();
        assert_eq!(
            (
                question.host.as_str(),
                question.port,
                question.fingerprint.as_str()
            ),
            ("bastion", 22, "SHA256:xyz")
        );
    }

    #[test]
    fn the_tunnel_config_forbids_server_questions_before_any_other_file() {
        let text = guard_config(
            Some(Path::new("/home/ada/.ssh/my config")),
            "/etc/ssh/ssh_config",
        );
        assert_eq!(
            text,
            "KbdInteractiveAuthentication no\nPasswordAuthentication no\nBatchMode no\n\
             Include \"/home/ada/.ssh/my config\"\nInclude /etc/ssh/ssh_config\n"
        );
        assert!(!guard_config(Some(Path::new("/a\"b")), "/s").contains("/a"));
        assert!(guard_config(None, "/s").starts_with("KbdInteractiveAuthentication no\n"));
    }

    /// A chain of three jump hosts: every `ssh.exe` under the stream's is
    /// found from one listing, a process that only reuses a dead parent's id
    /// is not, and a root missing from the listing is still named. Mutation
    /// seen: children taken only one level down.
    #[test]
    fn a_tree_is_listed_whole_before_anything_stops() {
        let rows = process_rows(
            "10 1 100\n11 10 110\n12 11 120\n13 12 130\n14 13 140\n\
             20 12 90\n21 99 150\nnoise line\n30 10 105 extra\n",
        );
        assert_eq!(rows.len(), 7);
        // 20 claims 12 as its parent but started before it: another's child.
        assert_eq!(tree(&rows, &[10]), [10, 11, 12, 13, 14]);
        assert_eq!(tree(&rows, &[13, 77]), [13, 14, 77]);
        assert!(tree(&[], &[5]) == [5]);
    }

    /// The helper's path as `ssh.exe` can start it: ASCII as it is; else
    /// relative to where `ssh.exe` runs, when that is ASCII; else the letter
    /// that stops it (S8).
    #[test]
    fn the_helper_is_named_in_ascii_or_its_letter_is_said() {
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        let profile = Path::new(root).join("Users").join("Élodie");
        let local = profile.join("AppData").join("Local");
        let base = local.join("Kerno").join("data").join("ssh");
        let installed = local.join("Programs").join("Kerno").join("kerno.exe");
        let plain = Path::new(root)
            .join("Program Files")
            .join("Kerno")
            .join("kerno.exe");
        assert_eq!(
            askpass_spelling(&plain, &base),
            Ok(plain.clone().into_os_string())
        );
        let up = PathBuf::from("..")
            .join("..")
            .join("..")
            .join("Programs")
            .join("Kerno")
            .join("kerno.exe");
        assert_eq!(askpass_spelling(&installed, &base), Ok(up.into_os_string()));
        // Below the tunnels' folder: `.` first, never a name searched for.
        let below = base.join("Kerno").join("kerno.exe");
        let down = PathBuf::from(".").join("Kerno").join("kerno.exe");
        assert_eq!(askpass_spelling(&below, &base), Ok(down.into_os_string()));
        let elsewhere = Path::new(root).join("tunnels");
        assert_eq!(askpass_spelling(&installed, &elsewhere), Err('É'));
        let greek = Path::new(root).join("Ωmega").join("kerno.exe");
        assert_eq!(askpass_spelling(&greek, &base), Err('Ω'));
    }

    /// Windows: the key named with `/` or `\`, in any ASCII case, is the
    /// same key (S6); on Unix, only the exact path is. Mutation seen: the
    /// exact comparison on Windows.
    #[test]
    fn the_key_is_known_in_either_spelling_on_windows() {
        let with = answers(Some(r"C:\Users\Ada\.ssh/id_pem"), Some("s3cret"), None);
        for named in [
            r"C:\Users\Ada\.ssh/id_pem",
            "C:/Users/Ada/.ssh/id_pem",
            r"c:\users\ada\.SSH\id_pem",
        ] {
            let prompt = format!("Enter passphrase for key '{named}': ");
            let given = decide(&prompt, 1, &with, &mut Observed::default());
            assert_eq!(
                given.is_some(),
                cfg!(windows) || named.starts_with(r"C:\Users\Ada\.ssh/")
            );
        }
        let other = "Enter passphrase for key 'C:/Users/Ada/.ssh/id_other': ";
        assert!(decide(other, 1, &with, &mut Observed::default()).is_none());
    }

    #[test]
    fn the_passphrase_goes_to_its_own_key_once() {
        let key = "/a/very/long/path/to/a/key/file/that/ssh/cuts/after/a/hundred/characters/when/it/asks/for/id_ed25519_enc";
        let prompt = format!("Enter passphrase for key '{}': ", &key[..100]);
        let with = answers(Some(key), Some("s3cret"), None);
        let mut observed = Observed::default();
        let given = |asker, observed: &mut Observed| {
            decide(&prompt, asker, &with, observed).map(|answer| answer.expose().to_string())
        };
        assert_eq!(given(7, &mut observed).as_deref(), Some("s3cret"));
        // A jump host's `ssh` asks for the same key on its own.
        assert_eq!(given(8, &mut observed).as_deref(), Some("s3cret"));
        assert_eq!(observed.passphrase, None);
        // Asked again by the same `ssh`: it was wrong.
        assert_eq!(given(7, &mut observed), None);
        assert_eq!(observed.passphrase, Some("the passphrase is wrong"));

        let mut observed = Observed::default();
        assert!(decide(&prompt, 7, &answers(Some(key), None, None), &mut observed).is_none());
        assert_eq!(observed.passphrase, Some("a passphrase is required"));

        // Another key's question (a jump host's, the config's) is refused.
        let mut observed = Observed::default();
        let other = "Enter passphrase for key '/home/ada/.ssh/id_other': ";
        assert!(decide(other, 7, &with, &mut observed).is_none());
        assert_eq!(
            observed.other_key.as_deref(),
            Some("/home/ada/.ssh/id_other")
        );
        assert_eq!(observed.passphrase, None);

        // A prefix of the key's path, or the question inside other words
        // (a server's own prompt), is not OpenSSH's question for this key.
        let short = answers(Some("/home/ada/.ssh/id_ed25519"), Some("s3cret"), None);
        for spoofed in [
            "Enter passphrase for key '/home/ada/.ssh/id': ",
            "Enter passphrase for key '/': ",
            "bastion says: Enter passphrase for key '/home/ada/.ssh/id_ed25519': ",
            "Enter passphrase for key '/home/ada/.ssh/id_ed25519': again",
        ] {
            assert!(
                decide(spoofed, 7, &short, &mut Observed::default()).is_none(),
                "{spoofed}"
            );
        }

        // Passwords and anything else: refused.
        for prompt in ["kerno@bastion's password: ", "Verification code: ", ""] {
            assert!(decide(prompt, 7, &with, &mut Observed::default()).is_none());
        }
    }

    #[test]
    fn a_failure_names_what_ssh_asked() {
        let mut observed = Observed {
            passphrase: Some("a passphrase is required"),
            ..Observed::default()
        };
        assert_eq!(
            failure(&observed, Some("/k/id"), "Permission denied (publickey)."),
            SshError::Passphrase {
                path: "/k/id".into(),
                reason: "a passphrase is required",
            }
        );
        observed.passphrase = None;
        observed.other_key = Some("/k/other".into());
        let SshError::OpenSsh { stderr } = failure(&observed, None, "Permission denied") else {
            panic!("an OpenSSH failure");
        };
        assert!(
            stderr.starts_with("the key /k/other needs a passphrase"),
            "{stderr}"
        );
        assert!(stderr.ends_with("Permission denied"), "{stderr}");
    }

    #[test]
    fn a_changed_host_key_is_read_from_the_warning() {
        let stderr = "@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n\
            The fingerprint for the ED25519 key sent by the remote host is\nSHA256:new+/key.\n\
            Add correct host key in /h/.ssh/known_hosts to get rid of this message.\n\
            Offending ED25519 key in /h/.ssh/known_hosts:7\n\
            Host key for bastion has changed and you have requested strict checking.\n\
            Host key verification failed.";
        assert_eq!(
            host_key_changed(stderr),
            Some(SshError::HostKeyChanged {
                host: "bastion".into(),
                port: 22,
                fingerprint: "SHA256:new+/key".into(),
                file: "/h/.ssh/known_hosts".into(),
                line: 7,
            })
        );
        assert_eq!(host_key_changed("Permission denied"), None);
    }

    #[test]
    fn messages_keep_a_bounded_tail_and_count_refused_forwards() {
        let mut messages = Messages::default();
        for index in 0..100 {
            messages.push(&format!("line {index}\r"));
        }
        assert_eq!(messages.lines.len(), TAIL_LINES);
        assert!(messages.text().ends_with("line 99"));
        messages.push("channel 3: open failed: administratively prohibited: open failed");
        assert_eq!(messages.forward_failures, 1);
        assert!(
            messages
                .last_forward_failure
                .as_deref()
                .is_some_and(|line| line.contains("prohibited"))
        );
    }
}
