//! The running instance's channel on Windows (K64, D-131): a named pipe.
//!
//! Pipe names are one namespace for the whole machine, and Kerno cannot ask
//! Windows who made a pipe (that needs a system call the workspace's no-new-
//! `unsafe` rule leaves out). What proves the other end is a Kerno of this
//! user's is the data directory: the listening Kerno writes [`PIPE_FILE`]
//! there (its pipe's random name and a random token), and both ends prove
//! they can read the token before a line crosses ([`super::proof`]). A
//! client that gets no valid proof sends nothing and opens the URL itself;
//! the line holds no password all the same ([`super::without_password`]),
//! since a data directory outside the profile may be readable by others.
//!
//! The pipe takes no peer from another machine. Its default access list
//! lets other users open it for reading only: they cannot send a nonce, and
//! are answered nothing. The file is removed when the listener goes and
//! when Kerno quits ([`forget`]); one left by a crash is replaced at the
//! next start.

use std::io::{self, ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use tokio::io::{
    AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, BufReader,
};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tokio::runtime::Runtime;

pub(crate) use super::proof::Bytes;
use super::proof::{self, SIZE};
use super::{MAX_LINE, READ_TIMEOUT};

/// The file of the data directory that names the running instance's pipe
/// and holds its token: two lines of hex.
pub const PIPE_FILE: &str = "instance.pipe";

/// Every pipe name of Kerno's: this, then 32 hex digits.
const PREFIX: &str = r"\\.\pipe\kerno-";

/// How many peers the instance serves at once (K69, R4).
const PEERS: usize = 8;

/// How long a client waits for the instance's proof.
const PROOF_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the listening end waits for a peer's first bytes, its nonce: a
/// real client writes it the moment it opens the pipe, so a peer that says
/// nothing holds its place no longer than this (K69, S2).
const NONCE_TIMEOUT: Duration = Duration::from_secs(1);

/// Windows' `ERROR_PIPE_BUSY`: every instance of the pipe is taken for the
/// moment (the instance makes the next one at once).
const ERROR_PIPE_BUSY: i32 = 231;

/// The file and pipe this process listens with, for [`forget`].
static LISTENING: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);

/// The running instance's end: its pipe, its token, its file, and the small
/// runtime its waits need (it is made on a thread of its own, before
/// Kerno's runtime).
pub struct Listener {
    // Before the runtime: dropped first.
    pipe: NamedPipeServer,
    name: String,
    token: Bytes,
    file: NameFile,
    runtime: Runtime,
}

/// [`PIPE_FILE`] while it names this listener's pipe: removed when dropped.
struct NameFile {
    path: PathBuf,
    id: String,
}

impl Drop for NameFile {
    fn drop(&mut self) {
        remove_if_named(&self.path, &self.id);
    }
}

/// Removes the file at `path` when it still names the pipe `id` (a newer
/// instance's is left alone).
fn remove_if_named(path: &Path, id: &str) {
    if std::fs::read_to_string(path).is_ok_and(|text| text.lines().next() == Some(id)) {
        let _ = std::fs::remove_file(path);
    }
}

/// Removes this process's [`PIPE_FILE`], at a clean exit: the serving
/// thread does not end before the process does.
pub fn forget() {
    let listening = LISTENING.lock().ok().and_then(|mut slot| slot.take());
    if let Some((path, id)) = listening {
        remove_if_named(&path, &id);
    }
}

/// The file in `data_dir` that names the pipe of the instance using it.
pub fn socket_path(data_dir: &Path) -> io::Result<PathBuf> {
    Ok(data_dir.join(PIPE_FILE))
}

/// The pipe and the token the file at `path` holds. A file that does not
/// hold them in Kerno's form is refused: nothing is opened from it.
pub(crate) fn read_name_file(path: &Path) -> io::Result<(String, Bytes)> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines().map(str::trim);
    let (id, token) = (lines.next().unwrap_or_default(), lines.next());
    let id_ok = id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit());
    match token.and_then(crate::update::parse_hex) {
        Some(token) if id_ok => Ok((format!("{PREFIX}{id}"), token)),
        _ => Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("{} does not name a pipe of Kerno's", path.display()),
        )),
    }
}

pub(crate) fn random() -> io::Result<Bytes> {
    let mut bytes = [0; SIZE];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(bytes)
}

/// A runtime for the pipe's waits, on the calling thread.
pub(crate) fn runtime() -> io::Result<Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// The pipe `name`, waiting a moment while every instance of it is busy.
async fn open(name: &str) -> io::Result<NamedPipeClient> {
    let started = tokio::time::Instant::now();
    loop {
        match ClientOptions::new().open(name) {
            Err(error)
                if error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    && started.elapsed() < READ_TIMEOUT =>
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            opened => return opened,
        }
    }
}

/// The pipe `name`, once the other end has proved it holds `token` and this
/// end has proved it too. Any failure means no Kerno of this user's is
/// there, and nothing more is written.
pub(crate) async fn handshake(name: &str, token: &Bytes) -> io::Result<NamedPipeClient> {
    let not_kerno = || {
        io::Error::new(
            ErrorKind::PermissionDenied,
            "the pipe named in the data directory did not prove it is this user's Kerno",
        )
    };
    let mut pipe = open(name).await?;
    let client_nonce = random()?;
    pipe.write_all(&client_nonce).await?;
    let mut answer = [0; 2 * SIZE];
    let read = tokio::time::timeout(PROOF_TIMEOUT, pipe.read_exact(&mut answer)).await;
    if !matches!(read, Ok(Ok(_))) {
        return Err(not_kerno());
    }
    let server_nonce = proof::check_server(token, &client_nonce, &answer).ok_or_else(not_kerno)?;
    pipe.write_all(&proof::client_proof(token, &server_nonce))
        .await?;
    Ok(pipe)
}

/// Hands `line`, a connection URL, to the running instance, without its
/// password, once the instance has proved it is this user's Kerno. Fails
/// when none listens or none proves it: then nothing was sent.
pub fn send(path: &Path, line: &str) -> io::Result<()> {
    let line = super::without_password(line)
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "not a connection URL"))?;
    let (name, token) = read_name_file(path)?;
    runtime()?.block_on(async {
        let mut pipe = handshake(&name, &token).await?;
        pipe.write_all(format!("{line}\n").as_bytes()).await?;
        pipe.flush().await
    })
}

/// Whether a Kerno of this user's listens on the pipe `path` names (it
/// proves it, and is sent no line).
pub fn running(path: &Path) -> bool {
    read_name_file(path)
        .and_then(|(name, token)| {
            runtime()?.block_on(async { handshake(&name, &token).await.map(drop) })
        })
        .is_ok()
}

/// A pipe [`new_pipe`] made.
pub(crate) struct NewPipe {
    /// Its first instance.
    pub(crate) pipe: NamedPipeServer,
    /// Its name (`\\.\pipe\kerno-…`).
    pub(crate) name: String,
    /// The 32 hex digits of its name.
    pub(crate) id: String,
    /// The token its peers prove they hold.
    pub(crate) token: Bytes,
}

/// A new pipe under a random name, local peers only, and the file at `path`
/// (a private file) naming it with a new random token, replacing one a crash
/// left. Called inside a Tokio runtime. The running instance's pipe and each
/// OpenSSH tunnel's askpass pipe (K69) are made here.
pub(crate) fn new_pipe(path: &Path) -> io::Result<NewPipe> {
    let id = crate::dialect::hex(&random()?[..16]);
    let token = random()?;
    let name = format!("{PREFIX}{id}");
    let pipe = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(&name)?;
    // Written once the pipe exists.
    crate::private::file_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?
        .write_all(format!("{id}\n{}\n", crate::dialect::hex(&token)).as_bytes())?;
    Ok(NewPipe {
        pipe,
        name,
        id,
        token,
    })
}

/// The next instance of the pipe `name`, made while a peer holds the
/// current one: a second peer finds one free instead of a busy name.
pub(crate) fn next_instance(name: &str) -> io::Result<NamedPipeServer> {
    ServerOptions::new()
        .reject_remote_clients(true)
        .create(name)
}

/// Makes a new pipe and names it, with a new token, in `path`. `None` when
/// a Kerno of this user's already listens on the pipe `path` names: it is
/// left alone.
pub fn listen(path: &Path) -> io::Result<Option<Listener>> {
    if running(path) {
        return Ok(None);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let runtime = runtime()?;
    let NewPipe {
        pipe,
        name,
        id,
        token,
    } = {
        let _entered = runtime.enter();
        new_pipe(path)?
    };
    if let Ok(mut slot) = LISTENING.lock() {
        *slot = Some((path.to_path_buf(), id.clone()));
    }
    Ok(Some(Listener {
        pipe,
        name,
        token,
        file: NameFile {
            path: path.to_path_buf(),
            id,
        },
        runtime,
    }))
}

/// Accepts peers one after the other and hands each one's line to
/// `on_line`, until `on_line` returns false. Blocking: run it on its own
/// thread.
pub fn serve(listener: Listener, mut on_line: impl FnMut(String) -> bool) {
    let Listener {
        runtime,
        mut pipe,
        name,
        token,
        file,
    } = listener;
    runtime.block_on(async {
        // Each peer in a task of its own (K69, R4): one that connects and
        // says nothing delays nobody; at most `PEERS` at once, the next one
        // waiting its turn (S2): a peer that says nothing holds its place a
        // second at most (`NONCE_TIMEOUT`).
        let peers = std::sync::Arc::new(tokio::sync::Semaphore::new(PEERS));
        let (sent, mut lines) = tokio::sync::mpsc::channel::<String>(PEERS);
        // A place is taken before a peer is accepted, in a branch of its own:
        // the lines are read while every place is held (T5).
        let mut place = None;
        loop {
            tokio::select! {
                // The semaphore is never closed.
                taken = peers.clone().acquire_owned(), if place.is_none() => {
                    let Ok(taken) = taken else {
                        return;
                    };
                    place = Some(taken);
                }
                // An error here is a peer that wrote and closed before this
                // call (`ERROR_NO_DATA`): what it wrote is still read below.
                // Cancel safe: a peer is never lost between two turns.
                _ = pipe.connect(), if place.is_some() => {
                    // The next instance before this one is read: a second
                    // peer finds one free instead of a busy name.
                    let Ok(next) = next_instance(&name) else {
                        return;
                    };
                    let mut peer = std::mem::replace(&mut pipe, next);
                    let Some(permit) = place.take() else {
                        continue;
                    };
                    let sent = sent.clone();
                    tokio::spawn(async move {
                        let exchange = exchange(&mut peer, &token);
                        if let Ok(Some(line)) = tokio::time::timeout(READ_TIMEOUT, exchange).await {
                            let _ = sent.send(line).await;
                        }
                        drop(permit);
                    });
                }
                Some(line) = lines.recv() => {
                    if !on_line(line) {
                        return;
                    }
                }
            }
        }
    });
    // The file goes when this instance stops listening.
    drop(file);
}

/// One peer: its nonce, this instance's answer, its proof, then its line,
/// without the line break. `None` when it does not prove it holds the token
/// or sends nothing usable.
async fn exchange(peer: &mut NamedPipeServer, token: &Bytes) -> Option<String> {
    prove_peer(peer, token).await?;
    read_line(peer).await
}

/// The listening end's half of the handshake: the peer's nonce (within
/// [`NONCE_TIMEOUT`]), this end's answer, the peer's proof. `None` when the
/// peer does not prove it holds `token`: nothing more may be read from it or
/// written to it.
pub(crate) async fn prove_peer(peer: &mut NamedPipeServer, token: &Bytes) -> Option<()> {
    let mut client_nonce = [0; SIZE];
    tokio::time::timeout(NONCE_TIMEOUT, peer.read_exact(&mut client_nonce))
        .await
        .ok()?
        .ok()?;
    let server_nonce = random().ok()?;
    peer.write_all(&proof::server_answer(token, &client_nonce, &server_nonce))
        .await
        .ok()?;
    let mut client_proof = [0; SIZE];
    peer.read_exact(&mut client_proof).await.ok()?;
    proof::check_client(token, &server_nonce, &client_proof).then_some(())
}

/// One line, without its line break; `None` when there is none.
async fn read_line(peer: impl AsyncRead + Unpin) -> Option<String> {
    let mut line = String::new();
    BufReader::new(peer.take(MAX_LINE))
        .read_line(&mut line)
        .await
        .ok()?;
    let line = line.trim();
    (!line.is_empty()).then(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "0123456789abcdef0123456789abcdef";

    fn token_hex(byte: u8) -> String {
        crate::dialect::hex(&[byte; SIZE])
    }

    /// A file that does not hold one of Kerno's names and a token opens
    /// nothing: no line is written to a path someone put there.
    #[test]
    fn only_a_name_kerno_makes_is_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path(dir.path()).unwrap();
        assert_eq!(path, dir.path().join(PIPE_FILE));
        for text in [
            format!("..\\..\\Users\\Public\\x\n{}", token_hex(1)),
            format!("\\\\.\\pipe\\other\n{}", token_hex(1)),
            format!("0123456789abcdef0123456789abcdeg\n{}", token_hex(1)),
            format!("{ID}\n"),
            format!("{ID}\n{}", &token_hex(1)[1..]),
            ID.to_string(),
        ] {
            std::fs::write(&path, &text).unwrap();
            let error = send(&path, "postgres://h").unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidData, "{text}");
            assert!(!running(&path));
        }
        std::fs::write(&path, format!(" {ID}\n{}\n", token_hex(7))).unwrap();
        assert_eq!(
            read_name_file(&path).unwrap(),
            (format!("{PREFIX}{ID}"), [7; SIZE])
        );
    }

    /// The whole hand-off through a real pipe: the line arrives without its
    /// password; the file goes with the listener; a file left by a crash is
    /// replaced by the next instance.
    #[test]
    fn a_url_crosses_once_both_ends_proved_themselves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("first-run").join(PIPE_FILE);
        assert_eq!(
            send(&path, "postgres://h").unwrap_err().kind(),
            ErrorKind::NotFound
        );
        let listener = listen(&path).unwrap().expect("nobody listens yet");
        let written = std::fs::read_to_string(&path).unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            serve(listener, |line| {
                sent.send(line).unwrap();
                false
            })
        });
        // Probes prove themselves and send no line.
        assert!(running(&path));
        assert!(
            listen(&path).unwrap().is_none(),
            "a live instance is left alone"
        );
        send(&path, "postgres://ada:hunter2@h/d").unwrap();
        server.join().unwrap();
        assert_eq!(
            received.try_iter().collect::<Vec<_>>(),
            ["postgres://ada@h/d"]
        );
        assert!(!path.exists(), "the file goes with the listener");

        // A crash left its file: nobody answers, the next instance replaces it.
        std::fs::write(&path, &written).unwrap();
        assert_eq!(
            send(&path, "postgres://h").unwrap_err().kind(),
            ErrorKind::NotFound
        );
        let listener = listen(&path).unwrap().expect("the stale file is replaced");
        assert_ne!(std::fs::read_to_string(&path).unwrap(), written);
        drop(listener);
        assert!(!path.exists(), "the file goes with the listener");
    }

    /// A peer that connects and says nothing delays nobody (K69, R4): a
    /// URL sent meanwhile arrives at once. Mutation seen: peers served one
    /// after the other (the URL waits for the silent peer's 2 s).
    #[test]
    fn a_silent_peer_delays_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path(dir.path()).unwrap();
        let listener = listen(&path).unwrap().expect("nobody listens yet");
        let (sent, received) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            serve(listener, |line| {
                sent.send(line).unwrap();
                false
            })
        });
        let (name, _) = read_name_file(&path).unwrap();
        let runtime = runtime().unwrap();
        let silent = {
            let _entered = runtime.enter();
            ClientOptions::new().open(&name).unwrap()
        };
        std::thread::sleep(Duration::from_millis(200));
        let started = std::time::Instant::now();
        send(&path, "postgres://h").unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "{:?}",
            started.elapsed()
        );
        server.join().unwrap();
        drop(silent);
        assert_eq!(received.try_iter().collect::<Vec<_>>(), ["postgres://h"]);
    }

    /// Eight peers that say nothing (another account opening the pipe again
    /// and again) take every place: a URL sent then waits its turn, a second
    /// at most, and arrives (K69, S2). Mutation seen: a peer beyond the
    /// places let go (the URL is lost).
    #[test]
    fn eight_silent_peers_then_a_proved_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path(dir.path()).unwrap();
        let listener = listen(&path).unwrap().expect("nobody listens yet");
        let (sent, received) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            serve(listener, |line| {
                sent.send(line).unwrap();
                false
            })
        });
        let (name, _) = read_name_file(&path).unwrap();
        let runtime = runtime().unwrap();
        let silent: Vec<NamedPipeClient> = {
            let _entered = runtime.enter();
            (0..PEERS)
                .map(|_| {
                    loop {
                        match ClientOptions::new().open(&name) {
                            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                                std::thread::sleep(Duration::from_millis(20));
                            }
                            opened => break opened.unwrap(),
                        }
                    }
                })
                .collect()
        };
        std::thread::sleep(Duration::from_millis(100));
        let started = std::time::Instant::now();
        send(&path, "postgres://h").unwrap();
        let took = started.elapsed();
        assert!(took < Duration::from_secs(3), "{took:?}");
        server.join().unwrap();
        drop(silent);
        assert_eq!(received.try_iter().collect::<Vec<_>>(), ["postgres://h"]);
    }

    /// Three times more URLs at once than there are places: every one
    /// arrives; the lines are read while every place is held (T5).
    #[test]
    fn many_urls_at_once_all_arrive() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path(dir.path()).unwrap();
        let listener = listen(&path).unwrap().expect("nobody listens yet");
        let count = 3 * PEERS;
        let (sent, received) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut seen = 0;
            serve(listener, |line| {
                sent.send(line).unwrap();
                seen += 1;
                seen < count
            })
        });
        let senders: Vec<_> = (0..count)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || send(&path, &format!("postgres://h{n}")))
            })
            .collect();
        for sender in senders {
            sender.join().unwrap().unwrap();
        }
        server.join().unwrap();
        assert_eq!(received.try_iter().count(), count);
    }

    /// Something else listens under the name the file holds (a program of
    /// another user that took the name, a "Kerno" without the token): it
    /// cannot prove itself, so the client sends it nothing more than its
    /// nonce and says so.
    #[test]
    fn a_pipe_without_the_token_gets_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path(dir.path()).unwrap();
        std::fs::write(&path, format!("{ID}\n{}\n", token_hex(1))).unwrap();
        for answers in [false, true] {
            let runtime = runtime().unwrap();
            let squatter = {
                let _entered = runtime.enter();
                ServerOptions::new()
                    .first_pipe_instance(true)
                    .create(format!("{PREFIX}{ID}"))
                    .unwrap()
            };
            let heard = std::thread::spawn(move || {
                runtime.block_on(async {
                    let mut squatter = squatter;
                    let _ = squatter.connect().await;
                    let mut nonce = [0; SIZE];
                    let _ = squatter.read_exact(&mut nonce).await;
                    if answers {
                        // An answer made with another token.
                        let answer = proof::server_answer(&[2; SIZE], &nonce, &[3; SIZE]);
                        let _ = squatter.write_all(&answer).await;
                    }
                    let mut rest = Vec::new();
                    let read = squatter.read_to_end(&mut rest);
                    let _ = tokio::time::timeout(Duration::from_secs(8), read).await;
                    rest
                })
            });
            let error = send(&path, "postgres://ada:hunter2@h/d").unwrap_err();
            assert_eq!(
                error.kind(),
                ErrorKind::PermissionDenied,
                "answers: {answers}"
            );
            let rest = heard.join().unwrap();
            assert!(rest.is_empty(), "answers: {answers}: {rest:?}");
        }
    }
}
