//! The tunnel through the system's `ssh.exe`, on Windows (K69, D-138).
//!
//! Windows' OpenSSH forwards no Unix socket that tokio can open, and a
//! loopback TCP port would be open to every local account while the tunnel
//! is up. So each stream to the database is its own `ssh -W host:port`,
//! the stream being that `ssh`'s standard input and output: nothing listens
//! on this computer, the same promise as the Unix socket. The price is one
//! SSH login per stream (each session, each cancel request), and the
//! questions channel and the key's passphrase live as long as the tunnel.
//!
//! `ssh -o LogLevel=VERBOSE` says `Authenticated to …` once it has logged
//! in: that line is how Kerno knows a stream is up, as the socket appearing
//! is on Unix. Jump hosts read `LogLevel INFO` first in the tunnel's own
//! configuration file, so only the target's login is ever reported.
//!
//! The questions `ssh` asks come back to Kerno over a named pipe whose two
//! ends prove they can read a token in the tunnel's private folder (the
//! running instance's handshake, `instance/windows.rs`); the environment
//! carries the path of that file, never the token. The helper names no
//! parent process (Windows' std has no parent id): each question is its
//! own, and a wrong passphrase is found before `ssh` starts, by trying it on
//! the key file ([`super::passphrase_opens`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{
    AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, BufReader,
    ReadBuf,
};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

use super::{
    ASKPASS_VAR, Answers, Messages, Observed, OpenSshRequest, START_TIMEOUT, TOO_OLD, Via, decide,
    failure, guard_config, lock, passphrase_opens, private_dir, refused_path, safe_ssh_word,
    ssh_version, too_old, tunnel_args,
};
use crate::config::SshAuth;
use crate::instance::windows::{self as named, Bytes};
use crate::ssh::SshError;

/// `CREATE_NO_WINDOW`: `ssh.exe` is a console program, and Kerno has no
/// console: without it each `ssh` would open a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What `ssh -o LogLevel=VERBOSE` prints once it has logged in.
const AUTHENTICATED: &str = "Authenticated to ";

/// How long the stream the start opened is kept for the first connection.
const SPARE_LIFE: Duration = Duration::from_secs(5);

/// The longest question or answer that crosses the askpass pipe.
const MAX_MESSAGE: usize = 8192;

/// How long one question may take on the pipe, proofs included.
const ASK_TIMEOUT: Duration = Duration::from_secs(10);

/// A stream to the database: the standard input and output of its own
/// `ssh -W`, which is stopped when the stream is dropped.
pub(crate) struct LocalStream {
    child: Tree,
    output: ChildStdout,
    input: ChildStdin,
    seen: Arc<Seen>,
}

/// A forward the server could not open, told by the stream itself: Windows'
/// OpenSSH server takes such a forward, then closes it, and `ssh -W` says
/// nothing (seen on the runner with the database's port closed). A database
/// always speaks first or answers a login, so a stream that ends before a
/// byte came is counted as a refused forward, in the tunnel's messages
/// ([`Forward::forward_failure`]), the moment its reader sees the end.
#[derive(Debug)]
struct Seen {
    /// A byte came from the database.
    received: AtomicBool,
    /// The tunnel's messages, and the database it leads to.
    messages: Arc<Mutex<Messages>>,
    target: String,
}

/// An `ssh.exe` and the processes it started (a jump host's `ssh.exe`):
/// dropped, all of them are stopped. Windows does not stop a process's
/// children with it, and a jump host's `ssh.exe` was seen to outlive its
/// parent; `taskkill /T` (System32's) stops the tree, then the child itself
/// goes (`kill_on_drop`).
/// The pid is taken at the start: `Child::id` says nothing once the exit
/// was seen.
struct Tree(Option<Child>, Option<u32>);

impl Tree {
    fn new(child: Child) -> Self {
        let pid = child.id();
        Self(Some(child), pid)
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match &mut self.0 {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let (Some(child), Some(pid)) = (self.0.take(), self.1) else {
            return;
        };
        use std::os::windows::process::CommandExt as _;
        let root = std::env::var_os("SystemRoot")
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        let mut taskkill = std::process::Command::new(root.join("System32").join("taskkill.exe"));
        taskkill
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        // The tree is walked from the living parent: it goes last.
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                let mut taskkill = Command::from(taskkill);
                runtime.spawn(async move {
                    let _ = taskkill.status().await;
                    drop(child);
                });
            }
            Err(_) => {
                let _ = taskkill.status();
                drop(child);
            }
        }
    }
}

impl AsyncRead for LocalStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.output).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = polled {
            let seen = &self.seen;
            if buf.filled().len() > before {
                seen.received.store(true, Ordering::SeqCst);
            } else if buf.remaining() > 0 && !seen.received.swap(true, Ordering::SeqCst) {
                // The end, before any byte: counted once.
                lock(&seen.messages).push(&format!(
                    "channel 0: open failed: the SSH server closed the way to {} before the \
                     database said anything",
                    seen.target
                ));
            }
        }
        polled
    }
}

impl AsyncWrite for LocalStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.input).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.input).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.input).poll_shutdown(cx)
    }
}

/// The tunnel: how to start an `ssh -W`, its private folder and its askpass
/// pipe. Dropping it stops the pipe, the `ssh` of the stream not taken yet,
/// and removes the folder; each stream stops its own `ssh`.
pub struct Forward {
    ssh: PathBuf,
    args: Vec<OsString>,
    askpass: PathBuf,
    ask_file: PathBuf,
    dir: PathBuf,
    target: (String, u16),
    key: Option<String>,
    /// `ssh -V` is older than the askpass needs.
    too_old: bool,
    /// Lines `ssh` printed once logged in: refused forwards.
    messages: Arc<Mutex<Messages>>,
    observed: Arc<Mutex<Observed>>,
    /// Questions that reached Kerno.
    questions: Arc<AtomicU64>,
    /// The stream [`Forward::start`] opened, for the first connection, when,
    /// and the forward failures counted then.
    spare: Mutex<Option<(LocalStream, tokio::time::Instant, u64)>>,
    responder: JoinHandle<()>,
}

impl std::fmt::Debug for Forward {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forward")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Forward {
    /// Starts a first `ssh -W` and waits until it has logged in, or says
    /// why it could not.
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
        if let Some(refusal) = refused_path(&ssh.auth) {
            return Err(refusal);
        }
        // A wrong passphrase is told before `ssh` runs (see the module).
        if let (SshAuth::KeyFile { path }, Some(passphrase)) =
            (&ssh.auth, request.key_passphrase.filter(|p| !p.is_empty()))
        {
            let (path, passphrase) = (path.clone(), passphrase.clone());
            let opens =
                tokio::task::spawn_blocking(move || passphrase_opens(&path, &passphrase)).await;
            if let Ok(Some(false)) = opens {
                return Err(SshError::Passphrase {
                    path: ssh_key_text(&ssh.auth).unwrap_or_default(),
                    reason: "the passphrase is wrong",
                });
            }
        }
        let too_old = version(&request.env.ssh).await.is_some_and(too_old);

        let dir = private_dir().map_err(|error| {
            SshError::Other(format!("could not create the tunnel's directory: {error}"))
        })?;
        let (args, made) = match prepare(&dir, &request) {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(error);
            }
        };
        let answers = Answers {
            key: ssh_key_text(&ssh.auth),
            passphrase: request.key_passphrase.filter(|p| !p.is_empty()).cloned(),
            trusted: request.trusted_fingerprint.map(str::to_string),
        };
        let observed = Arc::new(Mutex::new(Observed::default()));
        let questions = Arc::new(AtomicU64::new(0));
        let responder = tokio::spawn(respond(
            made.pipe,
            made.name,
            made.token,
            answers.clone(),
            observed.clone(),
            questions.clone(),
        ));
        // From here on dropping `forward` cleans everything up.
        let forward = Self {
            ssh: request.env.ssh.clone(),
            args,
            askpass: request.env.askpass.clone(),
            ask_file: dir.join("ask"),
            dir,
            target: (request.db_host.to_string(), request.db_port),
            key: answers.key,
            too_old,
            messages: Arc::new(Mutex::new(Messages::default())),
            observed,
            questions,
            spare: Mutex::new(None),
            responder,
        };
        let first = forward.spawn().await?;
        let mark = forward.forward_mark();
        *lock(&forward.spare) = Some((first, tokio::time::Instant::now(), mark));
        Ok(forward)
    }

    /// Starts one `ssh -W` and waits until it has logged in.
    async fn spawn(&self) -> Result<LocalStream, SshError> {
        *lock(&self.observed) = Observed::default();
        let asked_before = self.questions.load(Ordering::SeqCst);
        let mut spawned = Command::new(&self.ssh)
            .args(&self.args)
            .env("SSH_ASKPASS", &self.askpass)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(ASKPASS_VAR, &self.ask_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|error| {
                SshError::Other(format!("could not start {}: {error}", self.ssh.display()))
            })?;
        let streams = (
            spawned.stdout.take(),
            spawned.stdin.take(),
            spawned.stderr.take(),
        );
        // From here on every return stops `ssh` and its jump hosts' `ssh`.
        let mut child = Tree::new(spawned);
        let (Some(output), Some(input), stderr) = streams else {
            return Err(SshError::Other("ssh's standard streams are missing".into()));
        };

        // Read continuously: a pipe nobody reads fills up and stops `ssh`.
        let own = Arc::new(Mutex::new(Messages::default()));
        let up = Arc::new(AtomicBool::new(false));
        let reader = stderr.map(|stderr| {
            let (own, up, shared) = (own.clone(), up.clone(), self.messages.clone());
            tokio::spawn(async move {
                let mut stderr = BufReader::new(stderr);
                let mut line = Vec::new();
                while let Ok(read) = stderr.read_until(b'\n', &mut line).await
                    && read > 0
                {
                    let text = String::from_utf8_lossy(&line);
                    let text = text.trim_end();
                    if text.starts_with(AUTHENTICATED) {
                        up.store(true, Ordering::SeqCst);
                    } else if up.load(Ordering::SeqCst) {
                        // Once logged in, only the target's forward can fail
                        // (see `Seen` for when ssh says nothing).
                        lock(&shared).push(text);
                    } else {
                        lock(&own).push(text);
                    }
                    line.clear();
                }
            })
        });

        let started = tokio::time::Instant::now();
        loop {
            if up.load(Ordering::SeqCst) {
                break;
            }
            match child.try_wait() {
                Ok(Some(_)) => {
                    // Everything `ssh` said, before reading it.
                    if let Some(reader) = reader {
                        let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
                    }
                    let text = lock(&own).text();
                    let asked = self.questions.load(Ordering::SeqCst) > asked_before;
                    return Err(self.failure(&text, asked));
                }
                Ok(None) => {}
                Err(error) => return Err(SshError::Other(error.to_string())),
            }
            if started.elapsed() >= START_TIMEOUT {
                let said = lock(&own).text();
                let stderr = format!(
                    "{said}\n{} {} s",
                    crate::error::NO_ANSWER_FROM_SSH,
                    START_TIMEOUT.as_secs()
                );
                return Err(SshError::OpenSsh {
                    stderr: stderr.trim_start().to_string(),
                });
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Ok(LocalStream {
            child,
            output,
            input,
            seen: Arc::new(Seen {
                received: AtomicBool::new(false),
                messages: self.messages.clone(),
                target: format!("{}:{}", self.target.0, self.target.1),
            }),
        })
    }

    /// Why an `ssh` exited before it logged in. `asked`: a question of its
    /// reached Kerno.
    fn failure(&self, stderr: &str, asked: bool) -> SshError {
        let error = failure(&lock(&self.observed), self.key.as_deref(), stderr);
        // An `ssh` that cannot ask Kerno asks nobody, and fails.
        match error {
            SshError::OpenSsh { stderr } if self.too_old && !asked => SshError::OpenSsh {
                stderr: format!("{TOO_OLD}\n{stderr}"),
            },
            other => other,
        }
    }

    /// The tunnel's private folder (its askpass file and its `ssh`
    /// config): the command line of every `ssh` of the tunnel, a jump
    /// host's included, names it.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A new stream to the database: the one [`Forward::start`] opened
    /// while it is fresh and its `ssh` runs (a server closes a connection
    /// that says nothing: MySQL after 10 s), else a new `ssh -W`.
    pub(crate) async fn connect(&self) -> Result<LocalStream, SshError> {
        let spare = lock(&self.spare).take();
        // A spare whose forward the server refused meanwhile is not handed
        // out: its failure would come before the caller's mark.
        if let Some((mut stream, at, mark)) = spare
            && at.elapsed() < SPARE_LIFE
            && self.forward_mark() == mark
            && matches!(stream.child.try_wait(), Ok(None))
        {
            return Ok(stream);
        }
        self.spawn().await
    }

    /// The database host and port this tunnel leads to.
    pub fn target(&self) -> (&str, u16) {
        (&self.target.0, self.target.1)
    }

    /// Whether the tunnel is gone: never, as no `ssh` stands for it (each
    /// stream has its own, and a new one logs in again).
    pub fn is_closed(&self) -> bool {
        false
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
        // The pipe goes with the task; the spare stream stops its `ssh`.
        self.responder.abort();
        drop(lock(&self.spare).take());
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Writes the tunnel's `ssh` config into `dir` and makes its askpass pipe
/// (named by the file `ask` there): the arguments of every `ssh -W`, and
/// the pipe.
fn prepare(
    dir: &Path,
    request: &OpenSshRequest<'_>,
) -> Result<(Vec<OsString>, named::NewPipe), SshError> {
    // Kerno's own configuration file, so its rules reach the `ssh` of every
    // jump host too (`-J` hands `-F` on, not the `-o` options); `LogLevel
    // INFO` first keeps a jump host's `ssh` from saying it logged in.
    let guard = dir.join("config");
    let system = std::env::var_os("ProgramData")
        .map(|data| PathBuf::from(data).join("ssh").join("ssh_config"))
        .and_then(|path| config_path(&path))
        .unwrap_or_else(|| "C:/ProgramData/ssh/ssh_config".into());
    let user = request.env.aliases_from.as_deref().and_then(config_path);
    let text = format!(
        "LogLevel INFO\n{}",
        guard_config(user.as_deref().map(Path::new), &system)
    );
    std::fs::write(&guard, text).map_err(|error| {
        SshError::Other(format!("could not write the tunnel's ssh config: {error}"))
    })?;
    let args = tunnel_args(
        request.ssh,
        Some(&guard),
        Via::Pipes,
        request.db_host,
        request.db_port,
    );
    let made = named::new_pipe(&dir.join("ask"))
        .map_err(|error| SshError::Other(format!("could not open the askpass pipe: {error}")))?;
    Ok((args, made))
}

/// The key file as `ssh` names it in its question.
fn ssh_key_text(auth: &SshAuth) -> Option<String> {
    match auth {
        SshAuth::KeyFile { path } => Some(path.display().to_string()),
        _ => None,
    }
}

/// `path` as a configuration file can take it: forward slashes (OpenSSH
/// reads a backslash as an escape), `None` for a path that is not text.
fn config_path(path: &Path) -> Option<String> {
    path.to_str().map(|text| text.replace('\\', "/"))
}

/// The version `ssh -V` prints, `None` when it says nothing usable.
async fn version(ssh: &Path) -> Option<(u32, u32)> {
    let output = Command::new(ssh)
        .arg("-V")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(5), output)
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&output.stderr).into_owned()
        + &String::from_utf8_lossy(&output.stdout);
    ssh_version(&text)
}

// Questions from ssh ----------------------------------------------------------

/// Answers the questions of the tunnel's `ssh` processes, one peer after the
/// other, each once it proved it holds the token.
async fn respond(
    mut pipe: NamedPipeServer,
    name: String,
    token: Bytes,
    answers: Answers,
    observed: Arc<Mutex<Observed>>,
    questions: Arc<AtomicU64>,
) {
    loop {
        // An error here is a peer that wrote and closed before this call:
        // what it wrote is still read below.
        let _ = pipe.connect().await;
        let Ok(next) = named::next_instance(&name) else {
            return;
        };
        let mut peer = std::mem::replace(&mut pipe, next);
        let exchange = answer(&mut peer, &token, &answers, &observed, &questions);
        let _ = tokio::time::timeout(ASK_TIMEOUT, exchange).await;
    }
}

/// One question: the peer's proof, then "<id>\n<question>", length first;
/// the answer, length first (0: refused). Waits for the peer to close, so
/// that the answer is read before this end goes.
async fn answer(
    peer: &mut NamedPipeServer,
    token: &Bytes,
    answers: &Answers,
    observed: &Mutex<Observed>,
    questions: &AtomicU64,
) -> Option<()> {
    named::prove_peer(peer, token).await?;
    let question = read_message(peer).await?;
    questions.fetch_add(1, Ordering::SeqCst);
    let text = String::from_utf8_lossy(&question);
    let (asker, prompt) = text.split_once('\n').unwrap_or(("0", &text));
    let asker = asker.trim().parse().unwrap_or(0);
    let reply = decide(prompt, asker, answers, &mut lock(observed));
    let reply = reply.as_ref().map(|reply| reply.expose().as_bytes());
    write_message(peer, reply.unwrap_or_default()).await?;
    let mut rest = [0; 1];
    let _ = peer.read(&mut rest).await;
    Some(())
}

async fn read_message(stream: &mut (impl AsyncRead + Unpin)) -> Option<Vec<u8>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await.ok()?;
    let length = usize::try_from(u32::from_le_bytes(length)).ok()?;
    if length > MAX_MESSAGE {
        return None;
    }
    let mut message = vec![0; length];
    stream.read_exact(&mut message).await.ok()?;
    Some(message)
}

async fn write_message(stream: &mut (impl AsyncWrite + Unpin), message: &[u8]) -> Option<()> {
    let length = u32::try_from(message.len()).ok()?.to_le_bytes();
    stream.write_all(&length).await.ok()?;
    stream.write_all(message).await.ok()?;
    stream.flush().await.ok()
}

// Askpass -----------------------------------------------------------------------

/// Kerno run by `ssh` as its askpass program: passes the question (the first
/// argument) to the tunnel whose file [`ASKPASS_VAR`] names, once the tunnel
/// proved itself, and prints the answer. Returns the exit code when this
/// process is such a run, `None` otherwise; `main` calls it first thing.
pub fn answer_askpass() -> Option<i32> {
    let file = std::env::var_os(ASKPASS_VAR)?;
    let prompt = std::env::args_os().nth(1).unwrap_or_default();
    // No parent id on Windows: each question is its own (see the module).
    let mut question = format!("{}\n", std::process::id()).into_bytes();
    question.extend_from_slice(prompt.to_string_lossy().as_bytes());
    let answer = ask(Path::new(&file), &question);
    Some(match answer {
        Some(answer) if !answer.is_empty() => {
            use std::io::Write as _;
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

fn ask(file: &Path, question: &[u8]) -> Option<Vec<u8>> {
    let (name, token) = named::read_name_file(file).ok()?;
    named::runtime().ok()?.block_on(async {
        let exchange = async {
            let mut pipe = named::handshake(&name, &token).await.ok()?;
            write_message(&mut pipe, question).await?;
            read_message(&mut pipe).await
        };
        tokio::time::timeout(ASK_TIMEOUT, exchange).await.ok()?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn askpass_is_silent_outside_a_tunnel() {
        if std::env::var_os(ASKPASS_VAR).is_none() {
            assert_eq!(answer_askpass(), None);
        }
        // A tunnel that is gone answers nothing (ssh takes it as "no").
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(ask(&dir.path().join("ask"), b"1\nPassword: "), None);
    }

    /// The whole question through a real pipe: answered once both ends
    /// proved themselves; a peer without the token gets nothing.
    #[test]
    fn a_question_crosses_the_proved_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ask");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let key = r"C:\Users\ada\.ssh\id_ed25519";
        let made = {
            let _entered = runtime.enter();
            named::new_pipe(&file).unwrap()
        };
        let answers = Answers {
            key: Some(key.into()),
            passphrase: Some(crate::config::Secret::new("s3cret")),
            trusted: None,
        };
        let questions = Arc::new(AtomicU64::new(0));
        let observed = Arc::new(Mutex::new(Observed::default()));
        let responder = runtime.spawn(respond(
            made.pipe,
            made.name,
            made.token,
            answers,
            observed.clone(),
            questions.clone(),
        ));
        let prompt = format!("12\nEnter passphrase for key '{key}': ");
        assert_eq!(
            ask(&file, prompt.as_bytes()).as_deref(),
            Some(&b"s3cret"[..])
        );
        // Another key's question: refused (an empty answer).
        let other = b"13\nEnter passphrase for key 'C:\\other': ";
        assert_eq!(ask(&file, other).as_deref(), Some(&b""[..]));
        assert_eq!(questions.load(Ordering::SeqCst), 2);
        // A file with another token: the tunnel does not prove itself to
        // it, and it gets no answer.
        let text = std::fs::read_to_string(&file).unwrap();
        let id = text.lines().next().unwrap();
        let forged = dir.path().join("forged");
        std::fs::write(&forged, format!("{id}\n{}\n", "ab".repeat(32))).unwrap();
        assert_eq!(ask(&forged, prompt.as_bytes()), None);
        assert_eq!(questions.load(Ordering::SeqCst), 2);
        responder.abort();
    }
}
