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

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;
use tokio::process::{Child, Command};

use crate::config::{Secret, SshAuth, SshConfig};
use crate::ssh::SshError;

/// Variable that tells a child `ssh`'s askpass program (Kerno) which socket
/// to ask.
pub const ASKPASS_VAR: &str = "KERNO_ASKPASS";

/// Said when no `ssh` program was found.
pub const NOT_FOUND: &str = "OpenSSH was not found on this computer; use the built-in client";

/// How long `ssh` may take to authenticate (jump hosts included).
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// Lines of `ssh`'s messages kept for the explanation of a failure.
const TAIL_LINES: usize = 64;

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
}

impl OpenSshEnv {
    /// Finds `ssh` on `path` (the `PATH` variable) and in the usual places
    /// a GUI app's short `PATH` misses. `None` when there is none.
    pub fn detect(
        path: Option<&OsStr>,
        home: Option<&Path>,
        config: Option<PathBuf>,
        askpass: PathBuf,
    ) -> Option<Self> {
        let ssh = find_ssh(path)?;
        let aliases_from = config
            .clone()
            .or_else(|| home.map(|home| home.join(".ssh/config")));
        Some(Self {
            ssh,
            config,
            aliases_from,
            askpass,
        })
    }
}

/// The `ssh` program: the first on `path`, then `/usr/bin`,
/// `/opt/homebrew/bin` and `/usr/local/bin`.
pub fn find_ssh(path: Option<&OsStr>) -> Option<PathBuf> {
    let listed = path.map(std::env::split_paths).into_iter().flatten();
    let usual = ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    listed
        .chain(usual)
        .map(|dir| dir.join("ssh"))
        .find(|candidate| candidate.is_file())
}

// Arguments --------------------------------------------------------------------

/// The arguments of the `ssh` that forwards `socket` to `db_host:db_port`
/// as the SSH server sees it. No secret is ever in them.
pub fn tunnel_args(
    ssh: &SshConfig,
    config: Option<&Path>,
    socket: &Path,
    db_host: &str,
    db_port: u16,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut push = |arg: &str| args.push(arg.into());
    push("-N");
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
    let forward = format!("{}:{bracketed}:{db_port}", socket.display());
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
    args.extend(["-L".into(), forward.into()]);
    // `--`: a host field starting with `-` is a host, never an option.
    args.extend(["--".into(), ssh.host.trim().into()]);
    args
}

/// Whether `text` is safe as a host, user or jump-host list for `ssh`:
/// OpenSSH puts jump hosts in a shell command (ProxyJump), so nothing that a
/// shell would read as more than a word.
pub fn safe_ssh_word(text: &str, list: bool) -> bool {
    !text.is_empty()
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '.' | '-' | '_' | '@' | ':' | '[' | ']')
                || (list && c == ',')
        })
        && !text.starts_with('-')
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

/// A running `ssh -N` and its private directory. Dropping it kills `ssh`
/// and removes the directory.
pub struct Forward {
    child: Mutex<Child>,
    dir: PathBuf,
    socket: PathBuf,
    target: (String, u16),
    messages: Arc<Mutex<Messages>>,
}

impl std::fmt::Debug for Forward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forward")
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

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

impl Forward {
    /// Starts `ssh` and waits until it has authenticated and listens, or
    /// says why it could not.
    pub async fn start(request: OpenSshRequest<'_>) -> Result<Self, SshError> {
        // Checked where `ssh` starts too: a store edited by hand does not
        // go through `ConnectionConfig::validate` (ProxyJump uses a shell).
        let ssh = request.ssh;
        let jump = ssh.jump.as_deref().map(str::trim).unwrap_or_default();
        let words = [
            (ssh.host.trim(), false),
            (ssh.user.trim(), false),
            (jump, true),
        ];
        if let Some((word, _)) = words
            .into_iter()
            .find(|(word, list)| !word.is_empty() && !safe_ssh_word(word, *list))
        {
            return Err(SshError::Other(format!(
                "“{word}” has characters OpenSSH cannot take as a host, user or jump host"
            )));
        }
        let dir = private_dir().map_err(|error| {
            SshError::Other(format!("could not create the tunnel's directory: {error}"))
        })?;
        // From here on `cleanup` removes the directory on every early return.
        let mut cleanup = RemoveDir(dir.clone());
        let socket = dir.join("db");
        let ask = dir.join("ask");
        let listener = UnixListener::bind(&ask).map_err(|error| {
            SshError::Other(format!("could not open the askpass socket: {error}"))
        })?;

        let args = tunnel_args(
            request.ssh,
            request.env.config.as_deref(),
            &socket,
            request.db_host,
            request.db_port,
        );
        let mut child = Command::new(&request.env.ssh)
            .args(&args)
            .env("SSH_ASKPASS", &request.env.askpass)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(ASKPASS_VAR, &ask)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                SshError::Other(format!(
                    "could not start {}: {error}",
                    request.env.ssh.display()
                ))
            })?;

        // Read continuously: a pipe nobody reads fills up and stops `ssh`.
        let messages = Arc::new(Mutex::new(Messages::default()));
        let reader = child.stderr.take().map(|stderr| {
            let messages = messages.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    lock(&messages).push(&line);
                }
            })
        });

        let answers = Answers {
            key: match &request.ssh.auth {
                SshAuth::KeyFile { path } => Some(path.display().to_string()),
                _ => None,
            },
            passphrase: request.key_passphrase.filter(|p| !p.is_empty()).cloned(),
            trusted: request.trusted_fingerprint.map(str::to_string),
        };
        let answers_key = answers.key.clone();
        let observed = Arc::new(Mutex::new(Observed::default()));
        let responder = tokio::spawn(respond(listener, answers, observed.clone()));

        let started = tokio::time::Instant::now();
        let outcome = loop {
            if socket.exists() {
                break Ok(());
            }
            match child.try_wait() {
                Ok(Some(_)) => {
                    // Everything `ssh` said, before reading it.
                    if let Some(reader) = reader {
                        let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
                    }
                    let text = lock(&messages).text();
                    break Err(failure(&lock(&observed), answers_key.as_deref(), &text));
                }
                Ok(None) => {}
                Err(error) => break Err(SshError::Other(error.to_string())),
            }
            if started.elapsed() >= START_TIMEOUT {
                let said = lock(&messages).text();
                let stderr = format!(
                    "{said}\n{} {} s",
                    crate::error::NO_ANSWER_FROM_SSH,
                    START_TIMEOUT.as_secs()
                );
                break Err(SshError::OpenSsh {
                    stderr: stderr.trim_start().to_string(),
                });
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        // The questions are over once `ssh` is up or gone.
        responder.abort();
        let _ = std::fs::remove_file(&ask);
        outcome?;
        // From here on `Forward` removes it.
        cleanup.0 = PathBuf::new();
        Ok(Self {
            child: Mutex::new(child),
            dir,
            socket,
            target: (request.db_host.to_string(), request.db_port),
            messages,
        })
    }

    /// The socket that leads to the database.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The database host and port this tunnel leads to.
    pub fn target(&self) -> (&str, u16) {
        (&self.target.0, self.target.1)
    }

    /// Whether `ssh` exited.
    pub fn is_closed(&self) -> bool {
        !matches!(lock(&self.child).try_wait(), Ok(None))
    }

    /// How many forwards failed so far; see [`Forward::forward_failure`].
    pub fn forward_mark(&self) -> u64 {
        lock(&self.messages).forward_failures
    }

    /// Why the SSH server refused a forward opened after `mark`, if it did.
    /// `ssh` reports it on its own time: waits a moment for its message.
    pub async fn forward_failure(&self, mark: u64) -> Option<String> {
        for _ in 0..12 {
            {
                let messages = lock(&self.messages);
                if messages.forward_failures > mark {
                    return messages.last_forward_failure.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        None
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        // `kill_on_drop` stops `ssh` (and with it a jump host's `ssh`,
        // whose pipes close).
        let _ = lock(&self.child).start_kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Removes a directory when dropped.
struct RemoveDir(PathBuf);

impl Drop for RemoveDir {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A new directory only this user can enter, with a short path: a Unix
/// socket path is limited to about 100 bytes.
fn private_dir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt as _;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let dir = std::env::temp_dir().join(format!("kerno-ssh-{}", &id[..12]));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    Ok(dir)
}

// Questions from ssh ----------------------------------------------------------

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

async fn respond(listener: UnixListener, answers: Answers, observed: Arc<Mutex<Observed>>) {
    while let Ok((mut stream, _)) = listener.accept().await {
        let mut prompt = Vec::new();
        let mut limited = (&mut stream).take(8192);
        let read = limited.read_to_end(&mut prompt);
        if tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .is_err()
        {
            continue;
        }
        // "<pid of the asking ssh>\n<question>", from `answer_askpass`.
        let text = String::from_utf8_lossy(&prompt);
        let (asker, prompt) = text.split_once('\n').unwrap_or(("0", &text));
        let asker = asker.trim().parse().unwrap_or(0);
        let reply = decide(prompt, asker, &answers, &mut lock(&observed));
        if let Some(reply) = reply {
            let _ = stream.write_all(reply.expose().as_bytes()).await;
        }
        // Dropping the stream without a reply is a refusal.
    }
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
        let named = quoted(prompt).unwrap_or_default();
        let ours = answers
            .key
            .as_deref()
            .is_some_and(|key| !named.is_empty() && key.starts_with(named));
        if !ours {
            observed.other_key = Some(named.to_string());
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

// Askpass -----------------------------------------------------------------------

/// Kerno run by `ssh` as its askpass program: passes the question (the first
/// argument) to the tunnel named by [`ASKPASS_VAR`] and prints the answer.
/// Returns the exit code when this process is such a run, `None` otherwise;
/// `main` calls it first thing.
pub fn answer_askpass() -> Option<i32> {
    let socket = std::env::var_os(ASKPASS_VAR)?;
    let prompt = std::env::args_os().nth(1).unwrap_or_default();
    // Which `ssh` asks: a jump host's `ssh` asks on its own.
    let mut question = format!("{}\n", std::os::unix::process::parent_id()).into_bytes();
    question.extend_from_slice(prompt.as_encoded_bytes());
    let answer = ask(Path::new(&socket), &question);
    Some(match answer {
        Some(answer) if !answer.is_empty() => {
            let mut out = std::io::stdout().lock();
            match out.write_all(&answer).and_then(|()| out.write_all(b"\n")) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
        // Refused, or the tunnel is gone: ssh takes it as "no".
        _ => 1,
    })
}

fn ask(socket: &Path, prompt: &[u8]) -> Option<Vec<u8>> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    stream.write_all(prompt).ok()?;
    stream.shutdown(std::net::Shutdown::Write).ok()?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).ok()?;
    Some(answer)
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
/// (`*`, `?`) in the file name matched against the directory, sorted.
fn include_paths(word: &str, dir: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let path = match (word.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => dir.join(word),
    };
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
        let socket = Path::new("/tmp/kerno-ssh-1/db");
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
    }

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

    #[test]
    fn askpass_is_silent_outside_a_tunnel() {
        // The test process is not run by ssh: no answer, no exit code.
        if std::env::var_os(ASKPASS_VAR).is_none() {
            assert_eq!(answer_askpass(), None);
        }
        // A tunnel that is gone answers nothing (ssh takes it as "no").
        assert_eq!(
            ask(Path::new("/nonexistent/kerno-ask"), b"Password: "),
            None
        );
    }
}
