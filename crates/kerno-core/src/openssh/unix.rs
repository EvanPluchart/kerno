//! The running tunnel through the system's `ssh`, on Unix: its private
//! folder, the Unix socket it forwards, and the askpass channel, a second
//! Unix socket in that folder (see the parent module).

use std::collections::VecDeque;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;
use tokio::process::{Child, Command};

use super::{ASKPASS_VAR, OpenSshRequest, safe_ssh_word, tunnel_args};
use crate::config::{Secret, SshAuth};
use crate::ssh::SshError;

/// The stream of the socket `ssh` forwards to the database.
pub(crate) type LocalStream = tokio::net::UnixStream;

/// How long `ssh` may take to authenticate (jump hosts included).
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// Lines of `ssh`'s messages kept for the explanation of a failure.
const TAIL_LINES: usize = 64;

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

        // Kerno's own configuration file, so its rules reach the `ssh` of
        // every jump host too (`-J` hands `-F` on, not the `-o` options).
        let guard = dir.join("config");
        std::fs::write(&guard, guard_config(request.env.aliases_from.as_deref())).map_err(
            |error| SshError::Other(format!("could not write the tunnel's ssh config: {error}")),
        )?;
        let args = tunnel_args(
            request.ssh,
            Some(&guard),
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

    /// The tunnel's private directory (its socket and its `ssh` config):
    /// the command line of every `ssh` of the tunnel, a jump host's
    /// included, names it.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The socket that leads to the database.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// A new connection to the database through the tunnel's socket.
    pub(crate) async fn connect(&self) -> std::io::Result<LocalStream> {
        LocalStream::connect(&self.socket).await
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

/// The configuration every tunnel's `ssh` reads (`-F`), jump hosts
/// included: no password and no keyboard-interactive login, so no server
/// can put a question of its own in front of the askpass (one worded like
/// the key's passphrase question would be answered); then the user's file
/// and the system's, which `-F` would otherwise leave out. The first value
/// of an option wins, so these three cannot be overridden below.
fn guard_config(user: Option<&Path>) -> String {
    let mut text =
        String::from("KbdInteractiveAuthentication no\nPasswordAuthentication no\nBatchMode no\n");
    // A path that cannot be quoted is left out rather than half-read.
    if let Some(user) = user.and_then(Path::to_str)
        && !user.contains(['"', '\n'])
    {
        text.push_str(&format!("Include \"{user}\"\n"));
    }
    text.push_str("Include /etc/ssh/ssh_config\n");
    text
}

/// A new directory only this user can enter, with a short path: a Unix
/// socket path is limited to about 100 bytes.
fn private_dir() -> std::io::Result<PathBuf> {
    let id = uuid::Uuid::new_v4().simple().to_string();
    let dir = std::env::temp_dir().join(format!("kerno-ssh-{}", &id[..12]));
    crate::private::create_dir(&dir)?;
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
        // Exactly OpenSSH's own question for exactly this key (it cuts the
        // path at 100 characters): nothing a server could word differently.
        let named = prompt
            .strip_prefix("Enter passphrase for key '")
            .and_then(|rest| rest.strip_suffix("': "))
            .unwrap_or_default();
        let ours = answers.key.as_deref().is_some_and(|key| {
            !named.is_empty() && (named == key || (named.len() == 100 && key.starts_with(named)))
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let text = guard_config(Some(Path::new("/home/ada/.ssh/my config")));
        assert_eq!(
            text,
            "KbdInteractiveAuthentication no\nPasswordAuthentication no\nBatchMode no\n\
             Include \"/home/ada/.ssh/my config\"\nInclude /etc/ssh/ssh_config\n"
        );
        assert!(!guard_config(Some(Path::new("/a\"b"))).contains("/a"));
        assert!(guard_config(None).starts_with("KbdInteractiveAuthentication no\n"));
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
