//! The tunnel through the system's `ssh.exe`, on Windows (K69, D-138).
//!
//! Windows' OpenSSH forwards no Unix socket that tokio can open, and a
//! loopback TCP port would be open to every local account while the tunnel
//! is up. So each stream to the database is its own `ssh -W host:port`, the
//! stream being that `ssh`'s standard input and output: a driver that takes
//! a stream (PostgreSQL) gets one that listens nowhere. The price is one SSH
//! login per stream (each session, each cancel request), and the questions
//! channel and the key's passphrase live as long as the tunnel.
//!
//! `ssh -o LogLevel=VERBOSE` prints its login line once it has logged in
//! ([`super::is_login_line`]): that line is how Kerno knows a stream is up,
//! as the socket appearing is on Unix. Jump hosts read `LogLevel INFO` first
//! in the tunnel's own configuration file, so only the target's login is
//! ever reported.
//!
//! The tunnel's private folder is under the app's local data folder, in the
//! profile ([`crate::storage::AppPaths::local_data_dir`]), whatever `TEMP`
//! says, and `ssh.exe` runs in its parent with `-F <folder>/config`: no path
//! with a space ever reaches the command line OpenSSH writes for a jump host
//! (it does not quote `-F`'s), whatever the profile's name. Kerno keeps the
//! folder's two files open, others may read them but neither write nor
//! delete them, while the tunnel lives.
//!
//! The questions `ssh` asks come back to Kerno over a named pipe whose two
//! ends prove they can read a token in that folder (the running instance's
//! handshake, `instance/windows.rs`); the environment carries the path of
//! that file and the number of the `ssh` that asks, never the token. Each
//! peer is served by a task of its own (a silent one delays nobody), at most
//! [`PEERS`] at once. A stream's questions are answered only until it is up.
//! A passphrase Kerno could try on the key file itself
//! ([`super::passphrase_opens`]) is known right before `ssh` starts, and given
//! to every question for that key; one it could not try (a PEM key) is given
//! once per hop of a stream (its `ssh` and each jump host's, which `ssh`
//! starts itself with the same environment, so the same number), a question
//! beyond that meaning it was wrong.
//!
//! A jump host's `ssh` is started by `ssh.exe` itself (`-J`): Windows'
//! OpenSSH writes its command as `"<argv[0]>" … -F <file> -W … <jump>`,
//! `argv[0]` quoted (OpenSSH's `ssh.c`, the `WINDOWS` branch), and `argv[0]`
//! is the absolute path Kerno started, so every hop runs that same
//! `ssh.exe`, a path with a space included; OpenSSH's fall-back to a plain
//! `ssh` is left out on Windows. The tests check each process's executable.

use std::collections::HashMap;
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
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use super::{
    ASKPASS_UNREACHABLE, ASKPASS_VAR, Answers, Messages, Observed, OpenSshRequest, START_TIMEOUT,
    TOO_OLD, Via, askpass_spelling, decide, failure, guard_config, is_login_line, lock,
    passphrase_opens, private_dir_in, process_rows, resolve_ssh, safe_ssh_word, ssh_version,
    too_old, tree, tunnel_args,
};
use crate::config::SshAuth;
use crate::instance::windows::{self as named, Bytes};
use crate::ssh::SshError;

/// Variable that tells the askpass helper which `ssh` of the tunnel asks.
const SPAWN_VAR: &str = "KERNO_ASKPASS_SPAWN";

/// `CREATE_NO_WINDOW`: `ssh.exe` is a console program, and Kerno has no
/// console: without it each `ssh` would open a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `FILE_SHARE_READ`: others may read the tunnel's files, not write or
/// delete them.
const FILE_SHARE_READ: u32 = 1;

/// How long the stream the start opened is kept for the first connection.
const SPARE_LIFE: Duration = Duration::from_secs(5);

/// The longest question or answer that crosses the askpass pipe.
const MAX_MESSAGE: usize = 8192;

/// How long one peer may take on the pipe, proofs included.
const ASK_TIMEOUT: Duration = Duration::from_secs(10);

/// How many peers of the askpass pipe are served at once; more wait their
/// turn (S2), each silent one holding its place a second at most.
const PEERS: usize = 8;

/// The process ids of every `ssh.exe` tree Kerno started and has not stopped
/// yet, for [`stop_all`]. A pid is listed only while Kerno holds its
/// process's handle, so Windows cannot give it to another process meanwhile.
static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Stops every `ssh.exe` Kerno started and its jump hosts' `ssh.exe`, now,
/// waiting for `taskkill`: what the app does when it quits, when the runtime
/// that would stop them one by one is going away.
/// The list stays locked across the kills (S3): a stream dropped meanwhile
/// waits in [`release`] with its process's handle still held, so no pid is
/// given to another process before its `taskkill` ran.
pub fn stop_all() {
    let mut live = lock(&LIVE);
    if live.is_empty() {
        return;
    }
    let listed = listing().and_then(bounded);
    let pids = whole_tree(listed.as_deref(), &live);
    let _ = bounded(taskkill(&pids));
    live.clear();
}

/// How long the listing and each `taskkill` may take (T2): a hung WMI
/// service then costs the quit 5 s, and the roots are stopped without it.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// Runs `command` for [`STOP_WAIT`] at most, stopping it past that; its
/// standard output when it ended well. Blocking: [`stop_all`] and a stream
/// dropped outside the runtime.
fn bounded(mut command: std::process::Command) -> Option<Vec<u8>> {
    let mut child = command.spawn().ok()?;
    let reader = child.stdout.take().map(|mut output| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = std::io::Read::read_to_end(&mut output, &mut bytes);
            bytes
        })
    });
    let deadline = std::time::Instant::now() + STOP_WAIT;
    let ended = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let bytes = reader.and_then(|reader| reader.join().ok());
    ended.then(|| bytes.unwrap_or_default())
}

/// [`bounded`] on the runtime.
async fn bounded_async(command: std::process::Command) -> Option<Vec<u8>> {
    let mut command = Command::from(command);
    command.kill_on_drop(true);
    let output = tokio::time::timeout(STOP_WAIT, command.output())
        .await
        .ok()?
        .ok()?;
    output.status.success().then_some(output.stdout)
}

/// The `ssh.exe` processes running now, as `<pid> <parent pid> <start>`
/// lines (System32's PowerShell): the trees are read whole before anything
/// is stopped, since `taskkill /T` walks them as it kills, and an `ssh.exe`
/// started by a middle hop that died first is no longer under anything.
/// `None` when System32's PowerShell is not where Windows puts it.
fn listing() -> Option<std::process::Command> {
    use std::os::windows::process::CommandExt as _;
    let program = crate::tools::windows_program(r"WindowsPowerShell\v1.0\powershell.exe")?;
    let mut command = std::process::Command::new(program);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_Process -Filter \"Name='ssh.exe'\" | ForEach-Object { \
             if ($_.CreationDate) { '{0} {1} {2}' -f $_.ProcessId, $_.ParentProcessId, \
             $_.CreationDate.ToFileTimeUtc() } }",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    Some(command)
}

/// `roots` and every `ssh.exe` under them in `listed` ([`listing`]'s
/// output), deepest last; `roots` alone without a listing.
fn whole_tree(listed: Option<&[u8]>, roots: &[u32]) -> Vec<u32> {
    let rows = listed
        .map(|bytes| process_rows(&String::from_utf8_lossy(bytes)))
        .unwrap_or_default();
    tree(&rows, roots)
}

/// System32's `taskkill /T /F` on each of `pids`: those processes, and any
/// process one of them started since they were listed.
fn taskkill(pids: &[u32]) -> std::process::Command {
    use std::os::windows::process::CommandExt as _;
    // System32's, by its full path (no `ssh.exe` is found on a Windows
    // installed elsewhere than `C:\Windows` either).
    let program = crate::tools::windows_program("taskkill.exe")
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\taskkill.exe"));
    let mut command = std::process::Command::new(program);
    command.args(["/T", "/F"]);
    for pid in pids {
        command.args(["/PID", &pid.to_string()]);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    command
}

/// A stream to the database: the standard input and output of its own
/// `ssh -W` (held by its [`Tree`]), which is stopped when the stream is
/// dropped.
pub(crate) struct LocalStream {
    child: Tree,
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
/// goes (`kill_on_drop`). The pid is taken at the start (`Child::id` says
/// nothing once the exit was seen) and listed in [`LIVE`] until the child's
/// handle goes.
struct Tree {
    child: Option<Child>,
    pid: Option<u32>,
    /// The `ssh.exe`'s standard output and input, the stream itself, closed
    /// only once the tree is stopped: an `ssh.exe` whose input ends leaves at
    /// once and stops only its own jump host's `ssh.exe` (`TerminateProcess`),
    /// whose own `ssh.exe` then outlives everything, out of any tree (seen on
    /// the runner with two and three hops).
    io: Option<(ChildStdout, ChildStdin)>,
}

impl Tree {
    fn new(child: Child, io: Option<(ChildStdout, ChildStdin)>) -> Self {
        let pid = child.id();
        if let Some(pid) = pid {
            lock(&LIVE).push(pid);
        }
        Self {
            child: Some(child),
            pid,
            io,
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match &mut self.child {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }
}

/// The end of a stream whose pipes are gone (never, while it lives).
fn closed() -> std::io::Error {
    std::io::Error::from(std::io::ErrorKind::BrokenPipe)
}

/// Takes `pid` off [`LIVE`] and drops `child`, its handle, under the list's
/// lock: [`stop_all`] never kills a pid whose handle went (S3).
fn release(child: Child, pid: u32) {
    let mut live = lock(&LIVE);
    live.retain(|listed| *listed != pid);
    drop(child);
    drop(live);
}

impl Drop for Tree {
    fn drop(&mut self) {
        let io = self.io.take();
        let (Some(child), Some(pid)) = (self.child.take(), self.pid) else {
            return;
        };
        // The whole tree is listed while every `ssh.exe` of it lives (its
        // pipes still open), then stopped; the pipes close last.
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    let listed = match listing() {
                        Some(listing) => bounded_async(listing).await,
                        None => None,
                    };
                    let pids = whole_tree(listed.as_deref(), &[pid]);
                    let _ = bounded_async(taskkill(&pids)).await;
                    release(child, pid);
                    drop(io);
                });
            }
            Err(_) => {
                let listed = listing().and_then(bounded);
                let pids = whole_tree(listed.as_deref(), &[pid]);
                let _ = bounded(taskkill(&pids));
                release(child, pid);
                drop(io);
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
        let this = &mut *self;
        let Some((output, _)) = this.child.io.as_mut() else {
            return Poll::Ready(Err(closed()));
        };
        let polled = Pin::new(output).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = polled {
            let seen = &this.seen;
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
        match self.child.io.as_mut() {
            Some((_, input)) => Pin::new(input).poll_write(cx, buf),
            None => Poll::Ready(Err(closed())),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.child.io.as_mut() {
            Some((_, input)) => Pin::new(input).poll_flush(cx),
            None => Poll::Ready(Err(closed())),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.child.io.as_mut() {
            Some((_, input)) => Pin::new(input).poll_shutdown(cx),
            None => Poll::Ready(Err(closed())),
        }
    }
}

/// What the tunnel knows of each `ssh` still logging in, by its number.
#[derive(Debug, Default)]
struct Spawns {
    next: AtomicU64,
    asking: Mutex<HashMap<u64, Asking>>,
}

/// One `ssh` still logging in: what its questions told, how many reached
/// Kerno. Gone once it is up (its later questions are refused) or ended.
#[derive(Debug, Default)]
struct Asking {
    observed: Observed,
    questions: u64,
}

/// Takes a spawn off [`Spawns`] when dropped: once it is up, ended, or its
/// start was cancelled.
struct Listed {
    spawns: Arc<Spawns>,
    id: u64,
}

impl Listed {
    fn new(spawns: &Arc<Spawns>) -> Self {
        let id = spawns.next.fetch_add(1, Ordering::SeqCst) + 1;
        lock(&spawns.asking).insert(id, Asking::default());
        Self {
            spawns: spawns.clone(),
            id,
        }
    }

    /// What its questions told, taken off the list.
    fn take(&self) -> Asking {
        lock(&self.spawns.asking)
            .remove(&self.id)
            .unwrap_or_default()
    }
}

impl Drop for Listed {
    fn drop(&mut self) {
        lock(&self.spawns.asking).remove(&self.id);
    }
}

/// The tunnel: how to start an `ssh -W`, its private folder and its askpass
/// pipe. Dropping it stops the pipe, the `ssh` of the stream not taken yet,
/// and removes the folder; each stream stops its own `ssh`.
pub struct Forward {
    ssh: PathBuf,
    args: Vec<OsString>,
    /// Where `ssh.exe` runs: the folder's parent (`-F` is relative to it).
    base: PathBuf,
    /// `SSH_ASKPASS`: the helper's path as `ssh.exe` can start it
    /// ([`askpass_spelling`]: ASCII, maybe relative to `base`).
    askpass: OsString,
    /// The letter no spelling of the helper's path could avoid: `ssh.exe`
    /// cannot start it, and a failure says so first.
    askpass_letter: Option<char>,
    ask_file: PathBuf,
    dir: PathBuf,
    target: (String, u16),
    key: Option<String>,
    /// `ssh -V` is older than the askpass needs.
    too_old: bool,
    /// Lines `ssh` printed once logged in: refused forwards.
    messages: Arc<Mutex<Messages>>,
    spawns: Arc<Spawns>,
    /// The stream [`Forward::start`] opened, for the first connection, and
    /// the forward failures counted then; retired after [`SPARE_LIFE`].
    spare: Arc<Mutex<Option<(LocalStream, u64)>>>,
    /// The folder's `config` and `ask`, open for as long as the tunnel.
    held: Vec<std::fs::File>,
    tasks: Vec<JoinHandle<()>>,
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
        let jump = request
            .ssh
            .jump
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        let words = [
            (request.ssh.host.trim(), false),
            (request.ssh.user.trim(), false),
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
        // `~\` expanded, nothing of another machine (`-i`, the agent).
        let ssh = resolve_ssh(request.ssh, crate::home::dir().as_deref())?;
        let passphrase = request.key_passphrase.filter(|p| !p.is_empty()).cloned();
        // A passphrase tried on the key file: wrong, it is told before
        // `ssh` runs; right, it answers every question for that key.
        let mut verified = false;
        if let (SshAuth::KeyFile { path }, Some(passphrase)) = (&ssh.auth, &passphrase) {
            let (path, passphrase) = (path.clone(), passphrase.clone());
            let opens =
                tokio::task::spawn_blocking(move || passphrase_opens(&path, &passphrase)).await;
            match opens {
                Ok(Some(false)) => {
                    return Err(SshError::Passphrase {
                        path: ssh_key_text(&ssh.auth).unwrap_or_default(),
                        reason: "the passphrase is wrong",
                    });
                }
                Ok(Some(true)) => verified = true,
                _ => {}
            }
        }
        let too_old = version(&request.env.ssh).await.is_some_and(too_old);

        let base = match &request.env.tunnels {
            Some(base) => base.clone(),
            None => crate::storage::AppPaths::local_data_dir()
                .map(|dir| dir.join("ssh"))
                .ok_or_else(|| {
                    SshError::Other("could not find the local data folder for the tunnel".into())
                })?,
        };
        let dir = std::fs::create_dir_all(&base)
            .and_then(|()| private_dir_in(&base))
            .map_err(|error| {
                SshError::Other(format!("could not create the tunnel's directory: {error}"))
            })?;
        let prepared = prepare(&dir, &ssh, &request);
        let (args, made, held) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(error);
            }
        };
        let answers = Answers {
            key: ssh_key_text(&ssh.auth),
            passphrase,
            trusted: request.trusted_fingerprint.map(str::to_string),
        };
        let spawns = Arc::new(Spawns::default());
        let responder = tokio::spawn(respond(
            made.pipe,
            made.name,
            made.token,
            Arc::new(answers.clone()),
            Giving {
                verified,
                hops: 1 + jump.split(',').filter(|hop| !hop.trim().is_empty()).count(),
            },
            spawns.clone(),
        ));
        // A path `ssh.exe` can start: else it still runs (an agent and a
        // known host ask nothing), and a failure names the letter first.
        let (askpass, askpass_letter) = match askpass_spelling(&request.env.askpass, &base) {
            Ok(spelling) => (spelling, None),
            Err(letter) => (request.env.askpass.clone().into_os_string(), Some(letter)),
        };
        // From here on dropping `forward` cleans everything up.
        let mut forward = Self {
            ssh: request.env.ssh.clone(),
            args,
            base,
            askpass,
            askpass_letter,
            ask_file: dir.join("ask"),
            dir,
            target: (request.db_host.to_string(), request.db_port),
            key: answers.key,
            too_old,
            messages: Arc::new(Mutex::new(Messages::default())),
            spawns,
            spare: Arc::new(Mutex::new(None)),
            held,
            tasks: vec![responder],
        };
        let first = forward.spawn().await?;
        let mark = forward.forward_mark();
        *lock(&forward.spare) = Some((first, mark));
        // A spare nobody took stops logged in no longer than its life.
        let spare = forward.spare.clone();
        forward.tasks.push(tokio::spawn(async move {
            tokio::time::sleep(SPARE_LIFE).await;
            drop(lock(&spare).take());
        }));
        Ok(forward)
    }

    /// Starts one `ssh -W` and waits until it has logged in.
    async fn spawn(&self) -> Result<LocalStream, SshError> {
        let listed = Listed::new(&self.spawns);
        let mut spawned = Command::new(&self.ssh)
            .args(&self.args)
            .current_dir(&self.base)
            .env("SSH_ASKPASS", &self.askpass)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(ASKPASS_VAR, &self.ask_file)
            .env(SPAWN_VAR, listed.id.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|error| {
                SshError::Other(format!("could not start {}: {error}", self.ssh.display()))
            })?;
        let io = spawned.stdout.take().zip(spawned.stdin.take());
        let stderr = spawned.stderr.take();
        // From here on every return stops `ssh` and its jump hosts' `ssh`,
        // its pipes closed after them.
        let mut child = Tree::new(spawned, io);
        if child.io.is_none() {
            return Err(SshError::Other("ssh's standard streams are missing".into()));
        }

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
                    if !up.load(Ordering::SeqCst) && is_login_line(text) {
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
                // Its questions are over: later ones are refused.
                listed.take();
                break;
            }
            match child.try_wait() {
                Ok(Some(_)) => {
                    // Everything `ssh` said, before reading it.
                    if let Some(reader) = reader {
                        let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
                    }
                    let text = lock(&own).text();
                    return Err(self.failure(listed.take(), &text));
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
            seen: Arc::new(Seen {
                received: AtomicBool::new(false),
                messages: self.messages.clone(),
                target: format!("{}:{}", self.target.0, self.target.1),
            }),
        })
    }

    /// Why an `ssh` exited before it logged in, from its own questions.
    fn failure(&self, asking: Asking, stderr: &str) -> SshError {
        let error = failure(&asking.observed, self.key.as_deref(), stderr);
        // An `ssh` that cannot ask Kerno asks nobody, and fails.
        match (error, self.askpass_letter) {
            // `ssh.exe` says only that it could not start "ssh_askpass".
            (SshError::OpenSsh { stderr }, Some(letter))
                if stderr.contains("ssh_askpass: posix_spawnp") =>
            {
                SshError::OpenSsh {
                    stderr: format!(
                        "{ASKPASS_UNREACHABLE} from a folder whose name has the letter \
                         “{letter}”.\n{stderr}"
                    ),
                }
            }
            (SshError::OpenSsh { stderr }, _) if self.too_old && asking.questions == 0 => {
                SshError::OpenSsh {
                    stderr: format!("{TOO_OLD}\n{stderr}"),
                }
            }
            (other, _) => other,
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
        if let Some((mut stream, mark)) = spare
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
        // The pipe goes with its task; the spare stream stops its `ssh`;
        // the files are let go before the folder is removed.
        for task in &self.tasks {
            task.abort();
        }
        drop(lock(&self.spare).take());
        self.held.clear();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Writes the tunnel's `ssh` config into `dir` and makes its askpass pipe
/// (named by the file `ask` there): the arguments of every `ssh -W`, the
/// pipe, and the two files held open (others may only read them).
fn prepare(
    dir: &Path,
    ssh: &crate::config::SshConfig,
    request: &OpenSshRequest<'_>,
) -> Result<(Vec<OsString>, named::NewPipe, Vec<std::fs::File>), SshError> {
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
    let made = named::new_pipe(&dir.join("ask"))
        .map_err(|error| SshError::Other(format!("could not open the askpass pipe: {error}")))?;
    let hold = |path: &Path| {
        use std::os::windows::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)
            .map_err(|error| SshError::Other(format!("could not hold the tunnel's files: {error}")))
    };
    let held = vec![hold(&guard)?, hold(&dir.join("ask"))?];
    // Relative to the folder's parent, where `ssh.exe` runs: OpenSSH puts
    // `-F` unquoted in a jump host's command, and the folder's own name has
    // no space.
    let name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let relative = PathBuf::from(format!("{name}/config"));
    let args = tunnel_args(
        ssh,
        Some(&relative),
        Via::Pipes,
        request.db_host,
        request.db_port,
    );
    Ok((args, made, held))
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

/// How the tunnel answers a passphrase: `verified` when Kerno opened the key
/// with it; `hops`, the `ssh` processes of one stream that may ask for it
/// (the stream's own and one per jump host of the field).
#[derive(Debug, Clone, Copy)]
struct Giving {
    verified: bool,
    hops: usize,
}

/// Answers the questions of the tunnel's `ssh` processes: each peer in a
/// task of its own, at most [`PEERS`] at once, the next one waiting its
/// turn (S2); each once it proved it holds the token.
async fn respond(
    mut pipe: NamedPipeServer,
    name: String,
    token: Bytes,
    answers: Arc<Answers>,
    giving: Giving,
    spawns: Arc<Spawns>,
) {
    let peers = Arc::new(Semaphore::new(PEERS));
    // A distinct asker for each answer given.
    let fresh = Arc::new(AtomicU64::new(1 << 31));
    loop {
        // An error here is a peer that wrote and closed before this call:
        // what it wrote is still read below.
        let _ = pipe.connect().await;
        let Ok(next) = named::next_instance(&name) else {
            return;
        };
        let mut peer = std::mem::replace(&mut pipe, next);
        // The semaphore is never closed: the wait always ends.
        let Ok(permit) = peers.clone().acquire_owned().await else {
            return;
        };
        let (answers, spawns, fresh) = (answers.clone(), spawns.clone(), fresh.clone());
        tokio::spawn(async move {
            let exchange = answer(&mut peer, &token, &answers, giving, &spawns, &fresh);
            let _ = tokio::time::timeout(ASK_TIMEOUT, exchange).await;
            drop(permit);
        });
    }
}

/// One question: the peer's proof, then "<spawn>\n<question>", length
/// first; the answer, length first (0: refused). A question of an `ssh`
/// that is up, ended or unknown is refused. Waits for the peer to close, so
/// that the answer is read before this end goes.
async fn answer(
    peer: &mut NamedPipeServer,
    token: &Bytes,
    answers: &Answers,
    giving: Giving,
    spawns: &Spawns,
    fresh: &AtomicU64,
) -> Option<()> {
    named::prove_peer(peer, token).await?;
    let question = read_message(peer).await?;
    let text = String::from_utf8_lossy(&question);
    let (spawn, prompt) = text.split_once('\n').unwrap_or(("0", &text));
    let spawn: u64 = spawn.trim().parse().unwrap_or(0);
    let reply = {
        let mut asking = lock(&spawns.asking);
        asking.get_mut(&spawn).and_then(|asking| {
            asking.questions += 1;
            // A verified passphrase answers every question for its key (a
            // jump host's `ssh` asks for it too); one Kerno could not try,
            // once per hop of this stream (S5): past that, the asker of the
            // last answer asks again, which `decide` takes as a wrong one.
            let given = &asking.observed.passphrase_given;
            let asker = match given.last() {
                Some(&last) if !giving.verified && given.len() >= giving.hops => last,
                _ => u32::try_from(fresh.fetch_add(1, Ordering::SeqCst)).unwrap_or(u32::MAX),
            };
            decide(prompt, asker, answers, &mut asking.observed)
        })
    };
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
/// argument) and the number of the `ssh` that asks to the tunnel whose file
/// [`ASKPASS_VAR`] names, once the tunnel proved itself, and prints the
/// answer. Returns the exit code when this process is such a run, `None`
/// otherwise; `main` calls it first thing.
pub fn answer_askpass() -> Option<i32> {
    let file = std::env::var_os(ASKPASS_VAR)?;
    let prompt = std::env::args_os().nth(1).unwrap_or_default();
    let spawn = std::env::var(SPAWN_VAR).unwrap_or_default();
    let mut question = format!("{}\n", spawn.trim()).into_bytes();
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

    /// A pipe served by [`respond`] in `dir`, with a passphrase for `key`.
    fn serve(
        runtime: &tokio::runtime::Runtime,
        dir: &Path,
        key: &str,
        verified: bool,
    ) -> (PathBuf, Arc<Spawns>, JoinHandle<()>) {
        serve_hops(runtime, dir, key, verified, 1)
    }

    fn serve_hops(
        runtime: &tokio::runtime::Runtime,
        dir: &Path,
        key: &str,
        verified: bool,
        hops: usize,
    ) -> (PathBuf, Arc<Spawns>, JoinHandle<()>) {
        let file = dir.join("ask");
        let made = {
            let _entered = runtime.enter();
            named::new_pipe(&file).unwrap()
        };
        let answers = Answers {
            key: Some(key.into()),
            passphrase: Some(crate::config::Secret::new("s3cret")),
            trusted: None,
        };
        let spawns = Arc::new(Spawns::default());
        let responder = runtime.spawn(respond(
            made.pipe,
            made.name,
            made.token,
            Arc::new(answers),
            Giving { verified, hops },
            spawns.clone(),
        ));
        (file, spawns, responder)
    }

    /// A passphrase Kerno could not try, asked by a stream behind one jump
    /// host (its `ssh` and the jump host's, one number): given once to each,
    /// a third question meaning it was wrong (S5). Mutation seen: once per
    /// stream (the jump host's answer used the target's).
    #[test]
    fn an_unverified_passphrase_is_given_once_per_hop() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let key = r"C:\Users\ada\.ssh\id_pem";
        let (file, spawns, responder) = serve_hops(&runtime, dir.path(), key, false, 2);
        let listed = Listed::new(&spawns);
        let prompt = format!("{}\nEnter passphrase for key '{key}': ", listed.id);
        for _ in 0..2 {
            assert_eq!(
                ask(&file, prompt.as_bytes()).as_deref(),
                Some(&b"s3cret"[..])
            );
        }
        assert_eq!(ask(&file, prompt.as_bytes()).as_deref(), Some(&b""[..]));
        assert_eq!(
            listed.take().observed.passphrase,
            Some("the passphrase is wrong")
        );
        responder.abort();
    }

    /// Eight peers that say nothing take every place: a proved question
    /// waits its turn, a second at most, and is answered (S2). Mutation
    /// seen: a peer beyond the places let go.
    #[test]
    fn eight_silent_peers_then_a_proved_one() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let key = r"C:\k";
        let (file, spawns, responder) = serve(&runtime, dir.path(), key, true);
        let (name, _) = named::read_name_file(&file).unwrap();
        let silent: Vec<_> = {
            let _entered = runtime.enter();
            (0..PEERS)
                .map(|_| {
                    loop {
                        match tokio::net::windows::named_pipe::ClientOptions::new().open(&name) {
                            // ERROR_PIPE_BUSY: the next instance is on its way.
                            Err(error) if error.raw_os_error() == Some(231) => {
                                std::thread::sleep(Duration::from_millis(20));
                            }
                            opened => break opened.unwrap(),
                        }
                    }
                })
                .collect()
        };
        std::thread::sleep(Duration::from_millis(100));
        let listed = Listed::new(&spawns);
        let prompt = format!("{}\nEnter passphrase for key '{key}': ", listed.id);
        let started = std::time::Instant::now();
        assert_eq!(
            ask(&file, prompt.as_bytes()).as_deref(),
            Some(&b"s3cret"[..])
        );
        let took = started.elapsed();
        assert!(took < Duration::from_secs(3), "{took:?}");
        drop(silent);
        responder.abort();
    }

    /// The whole question through a real pipe: answered once both ends
    /// proved themselves, for an `ssh` still logging in only; a passphrase
    /// Kerno could not try is given once per `ssh` (R8, R12). Mutations
    /// seen: the spawn's entry not required (a finished `ssh` answered);
    /// the asker made unique for an unverified passphrase (asked twice,
    /// answered twice).
    #[test]
    fn a_question_crosses_the_proved_pipe() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let key = r"C:\Users\ada\.ssh\id_pem";
        let (file, spawns, responder) = serve(&runtime, dir.path(), key, false);
        let listed = Listed::new(&spawns);
        let prompt = format!("{}\nEnter passphrase for key '{key}': ", listed.id);
        assert_eq!(
            ask(&file, prompt.as_bytes()).as_deref(),
            Some(&b"s3cret"[..])
        );
        // Asked again by the same `ssh`: the passphrase was wrong.
        assert_eq!(ask(&file, prompt.as_bytes()).as_deref(), Some(&b""[..]));
        let asking = listed.take();
        assert_eq!(asking.questions, 2);
        assert_eq!(asking.observed.passphrase, Some("the passphrase is wrong"));
        // That `ssh` is up or gone: its questions are refused.
        assert_eq!(ask(&file, prompt.as_bytes()).as_deref(), Some(&b""[..]));
        // Another key's question: refused.
        let other = Listed::new(&spawns);
        let question = format!("{}\nEnter passphrase for key 'C:\\other': ", other.id);
        assert_eq!(ask(&file, question.as_bytes()).as_deref(), Some(&b""[..]));
        // A file with another token: the tunnel does not prove itself to
        // it, and it gets no answer.
        let text = std::fs::read_to_string(&file).unwrap();
        let id = text.lines().next().unwrap();
        let forged = dir.path().join("forged");
        std::fs::write(&forged, format!("{id}\n{}\n", "ab".repeat(32))).unwrap();
        let question = format!("{}\nEnter passphrase for key '{key}': ", other.id);
        assert_eq!(ask(&forged, question.as_bytes()), None);
        assert_eq!(other.take().questions, 1);
        responder.abort();
    }

    /// A verified passphrase is given to every question for its key, a jump
    /// host's `ssh` asking for it on its own.
    #[test]
    fn a_verified_passphrase_answers_each_question() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let key = r"C:\Users\ada\.ssh\id_ed25519";
        let (file, spawns, responder) = serve(&runtime, dir.path(), key, true);
        let listed = Listed::new(&spawns);
        let prompt = format!("{}\nEnter passphrase for key '{key}': ", listed.id);
        for _ in 0..2 {
            assert_eq!(
                ask(&file, prompt.as_bytes()).as_deref(),
                Some(&b"s3cret"[..])
            );
        }
        responder.abort();
    }

    /// The server side never answers a peer that did not prove itself: a
    /// wrong proof, then a question, gets nothing. Mutation seen: the
    /// proof's result ignored in `answer`.
    #[test]
    fn a_peer_without_the_proof_gets_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let key = r"C:\k";
        let (file, spawns, responder) = serve(&runtime, dir.path(), key, true);
        let listed = Listed::new(&spawns);
        let (name, _) = named::read_name_file(&file).unwrap();
        let answer = runtime.block_on(async {
            let mut pipe = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&name)
                .unwrap();
            pipe.write_all(&[7; 32]).await.unwrap();
            let mut server = [0; 64];
            pipe.read_exact(&mut server).await.unwrap();
            // A proof made with no token at all.
            pipe.write_all(&[0; 32]).await.unwrap();
            let question = format!("{}\nEnter passphrase for key '{key}': ", listed.id);
            let _ = write_message(&mut pipe, question.as_bytes()).await;
            tokio::time::timeout(Duration::from_secs(3), read_message(&mut pipe)).await
        });
        assert!(matches!(answer, Ok(None) | Err(_)), "{answer:?}");
        assert_eq!(listed.take().questions, 0);
        responder.abort();
    }

    /// A peer that connects and says nothing delays nobody (R4): a proved
    /// question is answered while it holds its instance. Mutation seen:
    /// peers served one after the other.
    #[test]
    fn a_silent_peer_delays_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let key = r"C:\k";
        let (file, spawns, responder) = serve(&runtime, dir.path(), key, true);
        let (name, _) = named::read_name_file(&file).unwrap();
        let silent = {
            let _entered = runtime.enter();
            tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&name)
                .unwrap()
        };
        std::thread::sleep(Duration::from_millis(200));
        let listed = Listed::new(&spawns);
        let prompt = format!("{}\nEnter passphrase for key '{key}': ", listed.id);
        let started = std::time::Instant::now();
        assert_eq!(
            ask(&file, prompt.as_bytes()).as_deref(),
            Some(&b"s3cret"[..])
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        drop(silent);
        responder.abort();
    }
}
