//! The running tunnel through the system's `ssh`, on Unix: its private
//! folder, the Unix socket it forwards, and the askpass channel, a second
//! Unix socket in that folder (see the parent module).

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;
use tokio::process::{Child, Command};

use super::{
    ASKPASS_VAR, Answers, Messages, Observed, OpenSshRequest, START_TIMEOUT, Via, decide, failure,
    guard_config, lock, private_dir, safe_ssh_word, tunnel_args,
};
use crate::config::SshAuth;
use crate::ssh::SshError;

/// The stream of the socket `ssh` forwards to the database.
pub(crate) type LocalStream = tokio::net::UnixStream;

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
        let text = guard_config(request.env.aliases_from.as_deref(), "/etc/ssh/ssh_config");
        std::fs::write(&guard, text).map_err(|error| {
            SshError::Other(format!("could not write the tunnel's ssh config: {error}"))
        })?;
        let args = tunnel_args(
            request.ssh,
            Some(&guard),
            Via::Socket(&socket),
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
    pub(crate) async fn connect(&self) -> Result<LocalStream, SshError> {
        LocalStream::connect(&self.socket)
            .await
            .map_err(|error| SshError::Forward {
                host: self.target.0.clone(),
                port: self.target.1,
                reason: format!("the tunnel's socket did not answer: {error}"),
            })
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

// Questions from ssh ----------------------------------------------------------

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
