//! SSH tunnels: reaching a database through an SSH server.
//!
//! A [`Tunnel`] is an authenticated SSH connection, made by the built-in
//! client (russh, this module) or by the system's `ssh` ([`crate::openssh`]).
//! Drivers use it in one of two ways, and neither leaves a port open for
//! other local processes:
//!
//! - [`Tunnel::open_stream`] returns a stream that *is* the forwarded
//!   connection. A driver that can run its protocol over any stream
//!   (PostgreSQL) uses this; nothing listens locally.
//! - [`Tunnel::forward_once`] opens a loopback port for a driver that can
//!   only dial an address (MySQL). The port admits exactly one connection and
//!   closes; see that method for what this does and does not guarantee.
//!
//! The built-in client checks host keys against known-hosts files (the user's
//! `~/.ssh/known_hosts`, read-only, and Kerno's own). An unknown host fails
//! with [`SshError::UnknownHostKey`], which carries the fingerprint to show
//! the user; connecting again with that fingerprint in
//! [`TunnelRequest::trusted_fingerprint`] accepts and records the key. A
//! host whose key changed is always refused.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::FutureExt;
use russh::client::{self, AuthResult, Handle};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::known_hosts::learn_known_hosts_path;
use russh::keys::ssh_key::{HashAlg, PrivateKey, PublicKey};
use russh::keys::{
    Algorithm, EcdsaCurve, PrivateKeyWithHashAlg, PublicKeyOrCertificate, check_known_hosts_path,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;

use crate::config::{Secret, SshAuth, SshConfig};
use crate::openssh::{Forward, OpenSshRequest};

/// Why a tunnel could not be opened or stopped working.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SshError {
    /// The SSH server could not be reached.
    #[error("could not reach the SSH server {host}:{port}: {reason}")]
    Connect {
        /// SSH host.
        host: String,
        /// SSH port.
        port: u16,
        /// What failed.
        reason: String,
    },
    /// The server's key is in no known-hosts file. Show the fingerprint and
    /// ask the user whether to trust it.
    #[error(
        "the SSH server {host}:{port} is not known yet; its {algorithm} key fingerprint is {fingerprint}"
    )]
    UnknownHostKey {
        /// SSH host.
        host: String,
        /// SSH port.
        port: u16,
        /// Key algorithm (`ssh-ed25519`, ...).
        algorithm: String,
        /// `SHA256:...` fingerprint.
        fingerprint: String,
    },
    /// The server's key differs from the recorded one.
    #[error(
        "the key of SSH server {host}:{port} changed (now {fingerprint}, recorded in {file} line {line}). \
         This can mean someone is intercepting the connection. If the server was reinstalled, remove \
         that line and connect again"
    )]
    HostKeyChanged {
        /// SSH host.
        host: String,
        /// SSH port.
        port: u16,
        /// Fingerprint presented now.
        fingerprint: String,
        /// Known-hosts file holding the old key.
        file: String,
        /// Line of the old key.
        line: usize,
    },
    /// The server refused the credentials.
    #[error("the SSH server refused the {method} login for user {user}")]
    AuthRejected {
        /// SSH user.
        user: String,
        /// `password`, `private key` or `agent`.
        method: &'static str,
    },
    /// No password was supplied for password authentication.
    #[error("the SSH password is missing")]
    PasswordMissing,
    /// The private key file is encrypted and no (or a wrong) passphrase was given.
    #[error("the private key {path} is encrypted: {reason}")]
    Passphrase {
        /// Key file.
        path: String,
        /// `a passphrase is required` or `the passphrase is wrong`.
        reason: &'static str,
    },
    /// The private key file could not be used.
    #[error("could not read the private key {path}: {reason}")]
    Key {
        /// Key file.
        path: String,
        /// What failed.
        reason: String,
    },
    /// The key file holds an RSA key. Signing with it would need the `rsa`
    /// crate, left out of this build for its timing side channel
    /// (RUSTSEC-2023-0071); an agent signs RSA keys itself.
    #[error(
        "the private key {path} is an RSA key, which the built-in SSH client cannot use: \
         choose the OpenSSH client on the SSH tab, or load the key in your SSH agent"
    )]
    RsaKeyFile {
        /// Key file.
        path: String,
    },
    /// The SSH agent is unreachable or has no usable key.
    #[error("SSH agent: {0}")]
    Agent(String),
    /// The SSH server would not open the forward to the database.
    #[error("the SSH server could not connect to {host}:{port}: {reason}")]
    Forward {
        /// Database host as seen from the SSH server.
        host: String,
        /// Database port.
        port: u16,
        /// What failed.
        reason: String,
    },
    /// The system's `ssh` failed; `stderr` is what it printed, which
    /// [`crate::error::explain`] tells in plain words.
    #[error("OpenSSH: {stderr}")]
    OpenSsh {
        /// `ssh`'s messages.
        stderr: String,
    },
    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// What is needed to open a tunnel.
#[derive(Debug, Clone)]
pub struct TunnelRequest<'a> {
    /// SSH server and authentication method.
    pub ssh: &'a SshConfig,
    /// Password of the SSH user, for [`SshAuth::Password`].
    pub password: Option<&'a Secret>,
    /// Passphrase of the key file, for [`SshAuth::KeyFile`].
    pub key_passphrase: Option<&'a Secret>,
    /// Known-hosts files to check, in order. Missing files are skipped.
    pub known_hosts: &'a [PathBuf],
    /// File that newly trusted keys are appended to (Kerno's own).
    pub learn_into: Option<&'a Path>,
    /// Fingerprint the user agreed to trust for this host, from a previous
    /// [`SshError::UnknownHostKey`].
    pub trusted_fingerprint: Option<&'a str>,
}

/// A byte stream to the database, carried by the SSH connection.
///
/// A concrete type rather than a boxed trait object: generic driver code
/// (TLS upgrade, connection tasks) needs to prove it is `Send + 'static`.
pub struct TunnelStream(StreamKind);

enum StreamKind {
    /// A channel of the built-in client.
    Channel(russh::ChannelStream<client::Msg>),
    /// The private socket OpenSSH forwards.
    Local(crate::openssh::LocalStream),
}

impl std::fmt::Debug for TunnelStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TunnelStream")
    }
}

impl AsyncRead for TunnelStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match &mut self.0 {
            StreamKind::Channel(stream) => Pin::new(stream).poll_read(cx, buf),
            StreamKind::Local(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for TunnelStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match &mut self.0 {
            StreamKind::Channel(stream) => Pin::new(stream).poll_write(cx, buf),
            StreamKind::Local(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.0 {
            StreamKind::Channel(stream) => Pin::new(stream).poll_flush(cx),
            StreamKind::Local(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match &mut self.0 {
            StreamKind::Channel(stream) => Pin::new(stream).poll_shutdown(cx),
            StreamKind::Local(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// An authenticated SSH connection that can forward to hosts the SSH server
/// reaches. Dropping it closes the SSH connection and everything forwarded
/// through it.
pub struct Tunnel {
    route: Route,
}

/// The two clients a tunnel can go through.
#[derive(Clone)]
enum Route {
    /// The built-in client: any host the SSH server reaches.
    Builtin(Arc<Handle<HostKeyCheck>>),
    /// The system's `ssh`: the one database it forwards to.
    OpenSsh(Arc<Forward>),
}

impl Route {
    /// A new connection to `host:port` as the SSH server sees it.
    async fn connect(&self, host: &str, port: u16) -> Result<TunnelStream, SshError> {
        match self {
            Route::Builtin(handle) => {
                let channel = open_channel(handle, host, port).await?;
                Ok(TunnelStream(StreamKind::Channel(channel.into_stream())))
            }
            Route::OpenSsh(forward) => {
                if forward.target() != (host, port) {
                    return Err(SshError::Other(format!(
                        "this OpenSSH tunnel leads to {}:{}, not {host}:{port}",
                        forward.target().0,
                        forward.target().1
                    )));
                }
                let stream = forward.connect().await.map_err(|error| SshError::Forward {
                    host: host.to_string(),
                    port,
                    reason: format!("the tunnel's socket did not answer: {error}"),
                })?;
                Ok(TunnelStream(StreamKind::Local(stream)))
            }
        }
    }
}

impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel")
            .field("closed", &self.is_closed())
            .finish()
    }
}

/// The SSH client's settings for a tunnel: keep-alive and host-key offer.
fn client_config(ssh: &SshConfig) -> client::Config {
    client::Config {
        keepalive_interval: (ssh.keepalive_secs > 0)
            .then(|| Duration::from_secs(ssh.keepalive_secs.into())),
        // Three missed keep-alives and the tunnel is declared dead.
        keepalive_max: 3,
        // Without the `rsa` crate an RSA host key cannot be verified:
        // leave it out of the offer, so a server with only an RSA host
        // key fails with "no common key algorithm" rather than "wrong
        // server signature", which reads like an interception.
        preferred: russh::Preferred {
            key: Cow::Borrowed(&[
                Algorithm::Ed25519,
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP256,
                },
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP384,
                },
                Algorithm::Ecdsa {
                    curve: EcdsaCurve::NistP521,
                },
            ]),
            ..russh::Preferred::DEFAULT
        },
        ..client::Config::default()
    }
}

impl Tunnel {
    /// Connects to the SSH server and authenticates.
    pub async fn open(request: TunnelRequest<'_>) -> Result<Self, SshError> {
        let ssh = request.ssh;
        let verdict = Arc::new(Mutex::new(None));
        let handler = HostKeyCheck {
            host: ssh.host.clone(),
            port: ssh.port,
            known_hosts: request.known_hosts.to_vec(),
            learn_into: request.learn_into.map(Path::to_path_buf),
            trusted_fingerprint: request.trusted_fingerprint.map(str::to_string),
            verdict: verdict.clone(),
        };
        let config = Arc::new(client_config(ssh));

        let connect = client::connect(config, (ssh.host.as_str(), ssh.port), handler);
        let mut handle = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
            Ok(Ok(handle)) => handle,
            Ok(Err(error)) => {
                // A host-key refusal surfaces as a generic error; the handler
                // recorded the precise reason.
                let recorded = verdict.lock().ok().and_then(|mut slot| slot.take());
                return Err(recorded.unwrap_or_else(|| SshError::Connect {
                    host: ssh.host.clone(),
                    port: ssh.port,
                    reason: error.to_string(),
                }));
            }
            Err(_) => {
                return Err(SshError::Connect {
                    host: ssh.host.clone(),
                    port: ssh.port,
                    reason: format!("no answer after {} s", CONNECT_TIMEOUT.as_secs()),
                });
            }
        };

        authenticate(&mut handle, &request).await?;
        Ok(Self {
            route: Route::Builtin(Arc::new(handle)),
        })
    }

    /// Starts the system's `ssh` (see [`crate::openssh`]): the tunnel leads
    /// to `request.db_host:request.db_port` only.
    pub async fn open_openssh(request: OpenSshRequest<'_>) -> Result<Self, SshError> {
        Ok(Self {
            route: Route::OpenSsh(Arc::new(Forward::start(request).await?)),
        })
    }

    /// Opens a connection to `host:port` as seen from the SSH server and
    /// returns it as a stream. Nothing listens on the local machine.
    pub async fn open_stream(&self, host: &str, port: u16) -> Result<TunnelStream, SshError> {
        self.route.connect(host, port).await
    }

    /// Opens a loopback port that forwards one connection to `host:port`,
    /// for drivers that can only dial an address. Connect to the returned
    /// address right away.
    ///
    /// The port is not a standing door into the remote network:
    /// - it admits exactly one connection, then stops listening;
    /// - it stops listening after [`ADMISSION_TIMEOUT`] if nobody connects;
    /// - if a second connection is already waiting when the first is taken,
    ///   both are dropped, because the caller's cannot be told apart.
    ///
    /// What remains: another local process that connects in the instant
    /// between this call and the caller's own connect takes the slot. The
    /// caller's connect is then refused (so the theft is noticed, not
    /// silent), and the other process holds a raw connection to the database
    /// port, where it still has to authenticate. Loopback TCP offers no way
    /// to check which process is on the other end; drivers that can take a
    /// stream should use [`Tunnel::open_stream`] instead.
    pub async fn forward_once(&self, host: &str, port: u16) -> Result<SocketAddr, SshError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| SshError::Other(format!("could not open a local port: {error}")))?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| SshError::Other(error.to_string()))?;
        let route = self.route.clone();
        let host = host.to_string();

        tokio::spawn(async move {
            let accepted = tokio::time::timeout(ADMISSION_TIMEOUT, listener.accept()).await;
            let Ok(Ok((mut local, _))) = accepted else {
                return;
            };
            // Anything else already queued makes the first connection ambiguous.
            let contested = listener.accept().now_or_never().is_some();
            drop(listener);
            if contested {
                tracing::warn!(
                    "two local connections reached a single-use tunnel port; both were refused"
                );
                return;
            }
            let mut remote = match route.connect(&host, port).await {
                Ok(stream) => stream,
                Err(error) => {
                    tracing::warn!(%error, "SSH tunnel could not open a forward channel");
                    return;
                }
            };
            // Ends when either side closes; errors here are ordinary
            // disconnects, which the database driver reports itself.
            let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
        });

        Ok(local_addr)
    }

    /// Whether the SSH connection is gone.
    pub fn is_closed(&self) -> bool {
        match &self.route {
            Route::Builtin(handle) => handle.is_closed(),
            Route::OpenSsh(forward) => forward.is_closed(),
        }
    }

    /// The private directory of an OpenSSH tunnel, which the command line
    /// of each of its `ssh` processes names; `None` with the built-in client.
    pub fn openssh_dir(&self) -> Option<&std::path::Path> {
        match &self.route {
            Route::Builtin(_) => None,
            Route::OpenSsh(forward) => Some(forward.dir()),
        }
    }

    /// A mark to pass to [`Tunnel::forward_failure`] after a connection
    /// through the tunnel failed.
    pub fn forward_mark(&self) -> u64 {
        match &self.route {
            Route::Builtin(_) => 0,
            Route::OpenSsh(forward) => forward.forward_mark(),
        }
    }

    /// Why the SSH server refused to open the way to `host:port` since
    /// `mark`. OpenSSH accepts the local connection first and says so only
    /// in its messages; the built-in client reports it when it happens.
    pub async fn forward_failure(&self, mark: u64, host: &str, port: u16) -> Option<SshError> {
        let Route::OpenSsh(forward) = &self.route else {
            return None;
        };
        let reason = forward.forward_failure(mark).await?;
        Some(SshError::Forward {
            host: host.to_string(),
            port,
            reason,
        })
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let Route::Builtin(handle) = &self.route else {
            return;
        };
        let handle = handle.clone();
        // Say goodbye politely when a runtime is still around to do it.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = handle
                    .disconnect(russh::Disconnect::ByApplication, "", "en")
                    .await;
            });
        }
    }
}

async fn open_channel(
    handle: &Handle<HostKeyCheck>,
    host: &str,
    port: u16,
) -> Result<russh::Channel<client::Msg>, SshError> {
    handle
        .channel_open_direct_tcpip(host, port.into(), "127.0.0.1", 0)
        .await
        .map_err(|error| SshError::Forward {
            host: host.to_string(),
            port,
            reason: match &error {
                russh::Error::ChannelOpenFailure(reason) => format!("{reason:?}"),
                other => other.to_string(),
            },
        })
}

/// How long a single-use tunnel port waits for its one connection.
pub const ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

async fn authenticate(
    handle: &mut Handle<HostKeyCheck>,
    request: &TunnelRequest<'_>,
) -> Result<(), SshError> {
    let ssh = request.ssh;
    let user = ssh.user.as_str();
    let other = |error: russh::Error| SshError::Other(error.to_string());
    let rejected = |method| SshError::AuthRejected {
        user: user.to_string(),
        method,
    };

    match &ssh.auth {
        SshAuth::Password => {
            let password = request.password.ok_or(SshError::PasswordMissing)?;
            let result = handle
                .authenticate_password(user, password.expose())
                .await
                .map_err(other)?;
            accept(result).ok_or_else(|| rejected("password"))
        }
        SshAuth::KeyFile { path } => {
            let key = load_key(path, request.key_passphrase).await?;
            // Never RSA (see `load_key`), so there is no hash to negotiate.
            let result = handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), None))
                .await
                .map_err(other)?;
            accept(result).ok_or_else(|| rejected("private key"))
        }
        SshAuth::Agent { socket } => {
            let mut agent = connect_agent(socket.as_deref()).await?;
            let identities = agent
                .request_identities()
                .await
                .map_err(|error| SshError::Agent(error.to_string()))?;
            if identities.is_empty() {
                return Err(SshError::Agent(
                    "the agent holds no key; add one with ssh-add".into(),
                ));
            }
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(other)?
                .flatten();
            for identity in identities {
                let AgentIdentity::PublicKey { key, .. } = identity else {
                    continue;
                };
                // A key the agent refuses to sign with is skipped, not fatal.
                if let Ok(result) = handle
                    .authenticate_publickey_with(user, key, hash, &mut agent)
                    .await
                    && result.success()
                {
                    return Ok(());
                }
            }
            Err(rejected("agent"))
        }
    }
}

/// The SSH agent: at `socket`, else where `SSH_AUTH_SOCK` says (a Unix
/// socket).
#[cfg(unix)]
async fn connect_agent(socket: Option<&Path>) -> Result<DynamicAgent, SshError> {
    match socket {
        Some(path) => AgentClient::connect_uds(path)
            .await
            .map(AgentClient::dynamic)
            .map_err(|error| {
                SshError::Agent(format!(
                    "could not reach the agent at {}: {error}",
                    path.display()
                ))
            }),
        None => AgentClient::connect_env()
            .await
            .map(AgentClient::dynamic)
            .map_err(|error| {
                SshError::Agent(format!(
                    "could not reach the agent ({error}); is SSH_AUTH_SOCK set?"
                ))
            }),
    }
}

/// The SSH agent on Windows (K64): a named pipe of this machine (see
/// [`agent_pipe`]).
#[cfg(windows)]
async fn connect_agent(socket: Option<&Path>) -> Result<DynamicAgent, SshError> {
    let env = std::env::var_os("SSH_AUTH_SOCK");
    let pipe = agent_pipe(socket.map(Path::as_os_str), env.as_deref())?;
    AgentClient::connect_named_pipe(&pipe)
        .await
        .map(AgentClient::dynamic)
        .map_err(|error| SshError::Agent(format!("could not reach the agent at {pipe}: {error}")))
}

/// The pipe of Windows' OpenSSH agent, which 1Password's takes over when it
/// is on: where Windows' `ssh` looks when nothing else is said.
#[cfg(any(windows, test))]
const DEFAULT_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// The agent's pipe on Windows: the connection's own, else the one
/// `SSH_AUTH_SOCK` names, else [`DEFAULT_AGENT_PIPE`]. A saved name may come
/// from an imported file, and a pipe of another machine (`\\host\pipe\x`)
/// would make Windows log in to that host with the user's credentials: a
/// name that is not a pipe of this machine ([`is_local_pipe`]) is refused
/// before anything is opened.
#[cfg(any(windows, test))]
fn agent_pipe(
    saved: Option<&std::ffi::OsStr>,
    env: Option<&std::ffi::OsStr>,
) -> Result<String, SshError> {
    let saved = saved.filter(|saved| !saved.is_empty());
    let env = env.filter(|env| !env.is_empty());
    let (name, next_step) = match (saved, env) {
        (Some(saved), _) => (
            saved,
            "Clear it in the connection's SSH tab to use the system's agent.",
        ),
        (None, Some(env)) => (env, "Unset SSH_AUTH_SOCK to use the system's agent."),
        (None, None) => return Ok(DEFAULT_AGENT_PIPE.to_string()),
    };
    match name.to_str().filter(|name| is_local_pipe(name)) {
        Some(name) => Ok(name.to_string()),
        None => Err(SshError::Agent(format!(
            "The SSH agent's address “{}” is not a local pipe: on Windows it looks like \
             {DEFAULT_AGENT_PIPE}. {next_step}",
            name.to_string_lossy()
        ))),
    }
}

/// Whether `name` is a named pipe of this machine: `\\.\pipe\` (any case),
/// then a name with no further `\` or `/` (nothing that would make it name
/// something else), not `.` or `..`. A pipe of another host, `\\?\UNC\…`, a
/// drive path, a Unix socket's path and the forward-slash spelling are not.
#[cfg(any(windows, test))]
fn is_local_pipe(name: &str) -> bool {
    const PREFIX: &str = r"\\.\pipe\";
    match (name.get(..PREFIX.len()), name.get(PREFIX.len()..)) {
        (Some(head), Some(rest)) => {
            head.eq_ignore_ascii_case(PREFIX)
                && !rest.is_empty()
                && !matches!(rest, "." | "..")
                && !rest.contains(['\\', '/', '\0'])
        }
        _ => false,
    }
}

/// An agent reached over any stream.
type DynamicAgent = AgentClient<Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin>>;

fn accept(result: AuthResult) -> Option<()> {
    result.success().then_some(())
}

/// OID of `rsaEncryption`, the algorithm of a PKCS#8 RSA key.
const RSA_ENCRYPTION_OID: &str = "1.2.840.113549.1.1.1";

/// Reads a private key file. RSA keys are refused with
/// [`SshError::RsaKeyFile`] in every format (OpenSSH, PEM PKCS#1, PKCS#8,
/// any other that loads), before any passphrase is asked for when the format
/// allows it.
async fn load_key(
    path: &Path,
    passphrase: Option<&Secret>,
) -> Result<russh::keys::PrivateKey, SshError> {
    let display = path.display().to_string();
    let owned_path = path.to_path_buf();
    let passphrase = passphrase.filter(|p| !p.is_empty()).cloned();
    let had_passphrase = passphrase.is_some();
    // Reading the file and deriving the key from a passphrase both block.
    let loaded = tokio::task::spawn_blocking(move || {
        let text = std::fs::read_to_string(owned_path)?;
        // The public half of an OpenSSH key is readable without the
        // passphrase: refuse an RSA key before asking for it, the way russh
        // refuses a PEM one.
        if PrivateKey::from_openssh(&text).is_ok_and(|key| key.algorithm().is_rsa()) {
            return Err(russh::keys::Error::UnsupportedKeyType {
                key_type_string: "RSA".into(),
                key_type_raw: Vec::new(),
            });
        }
        russh::keys::decode_secret_key(&text, passphrase.as_ref().map(Secret::expose))
    })
    .await
    .map_err(|error| SshError::Other(error.to_string()))?;

    let rsa = match &loaded {
        // Any other format that loads (PuTTY, ...).
        Ok(key) => key.algorithm().is_rsa(),
        // OpenSSH (above) and PEM PKCS#1 (`BEGIN RSA PRIVATE KEY`).
        Err(russh::keys::Error::UnsupportedKeyType {
            key_type_string, ..
        }) => key_type_string == "RSA",
        // PKCS#8 (`BEGIN PRIVATE KEY`).
        Err(russh::keys::Error::UnknownAlgorithm(oid)) => oid.to_string() == RSA_ENCRYPTION_OID,
        Err(_) => false,
    };
    if rsa {
        return Err(SshError::RsaKeyFile { path: display });
    }

    match loaded {
        Ok(key) => Ok(key),
        Err(russh::keys::Error::KeyIsEncrypted) => Err(SshError::Passphrase {
            path: display,
            reason: "a passphrase is required",
        }),
        // ssh-key reports a wrong passphrase as a decryption/crypto failure.
        Err(other) if had_passphrase && !matches!(other, russh::keys::Error::IO(_)) => {
            Err(SshError::Passphrase {
                path: display,
                reason: "the passphrase is wrong",
            })
        }
        Err(other) => Err(SshError::Key {
            path: display,
            reason: other.to_string(),
        }),
    }
}

/// Checks the server's key against the known-hosts files.
struct HostKeyCheck {
    host: String,
    port: u16,
    known_hosts: Vec<PathBuf>,
    learn_into: Option<PathBuf>,
    trusted_fingerprint: Option<String>,
    /// Why the key was refused, for [`Tunnel::open`] to report.
    verdict: Arc<Mutex<Option<SshError>>>,
}

impl HostKeyCheck {
    fn check(&self, key: &PublicKey) -> Result<(), SshError> {
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        for file in &self.known_hosts {
            if !file.exists() {
                continue;
            }
            match check_known_hosts_path(&self.host, self.port, key, file) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(russh::keys::Error::KeyChanged { line }) => {
                    return Err(SshError::HostKeyChanged {
                        host: self.host.clone(),
                        port: self.port,
                        fingerprint,
                        file: file.display().to_string(),
                        line,
                    });
                }
                Err(error) => {
                    // An unreadable or malformed file must not grant trust,
                    // and must not block the other files either.
                    tracing::warn!(file = %file.display(), %error, "could not read known hosts file");
                }
            }
        }
        if self.trusted_fingerprint.as_deref() == Some(fingerprint.as_str()) {
            if let Some(file) = &self.learn_into
                && let Err(error) = learn_known_hosts_path(&self.host, self.port, key, file)
            {
                tracing::warn!(file = %file.display(), %error, "could not record the trusted host key");
            }
            return Ok(());
        }
        Err(SshError::UnknownHostKey {
            host: self.host.clone(),
            port: self.port,
            algorithm: key.algorithm().to_string(),
            fingerprint,
        })
    }
}

impl client::Handler for HostKeyCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            // A host certificate is trusted through its public key, like a
            // plain host key: Kerno does not evaluate certificate authorities.
            PublicKeyOrCertificate::Certificate(certificate) => {
                PublicKey::from(certificate.public_key().clone())
            }
        };
        match self.check(&key) {
            Ok(()) => Ok(true),
            Err(error) => {
                if let Ok(mut slot) = self.verdict.lock() {
                    *slot = Some(error);
                }
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SshClient;

    #[test]
    fn ssh_keepalive_is_passed_to_the_client() {
        let mut ssh = SshConfig {
            host: "bastion".into(),
            port: 22,
            user: "kerno".into(),
            auth: SshAuth::default(),
            keepalive_secs: 45,
            client: SshClient::Builtin,
            jump: None,
        };
        let config = client_config(&ssh);
        assert_eq!(config.keepalive_interval, Some(Duration::from_secs(45)));
        assert_eq!(config.keepalive_max, 3);

        ssh.keepalive_secs = 0;
        assert_eq!(client_config(&ssh).keepalive_interval, None);
    }

    /// K64: on Windows the agent is a pipe of this machine only: the
    /// connection's own, then `SSH_AUTH_SOCK`'s, then Windows' OpenSSH
    /// agent's; a name from an imported file that leads anywhere else is
    /// refused, saying what a valid one looks like. Strings in, a verdict
    /// out: run on every system. Mutation seen: the prefix check dropped.
    #[test]
    fn the_windows_agent_is_a_pipe_of_this_machine_only() {
        use std::ffi::OsStr;
        for local in [
            DEFAULT_AGENT_PIPE,
            r"\\.\pipe\mine",
            r"\\.\PIPE\1password-agent",
        ] {
            assert!(is_local_pipe(local), "{local}");
        }
        for other in [
            r"\\some-host\pipe\x",
            r"\\?\UNC\host\pipe\x",
            r"\\.\pipe\..",
            r"\\.\pipe\.",
            r"\\.\pipe\..\..\C:\x",
            r"\\.\pipe\a\b",
            r"\\.\pipe\",
            r"\\.\pipe",
            "//./pipe/openssh-ssh-agent",
            r"C:\Users\ada\agent.sock",
            "/Users/ada/Library/Containers/agent.sock",
            "",
        ] {
            assert!(!is_local_pipe(other), "{other}");
        }

        let mine = OsStr::new(r"\\.\pipe\mine");
        let other = OsStr::new("other");
        assert_eq!(
            agent_pipe(Some(mine), Some(other)).unwrap(),
            r"\\.\pipe\mine"
        );
        assert_eq!(agent_pipe(None, Some(mine)).unwrap(), r"\\.\pipe\mine");
        assert_eq!(agent_pipe(None, None).unwrap(), DEFAULT_AGENT_PIPE);
        let empty = OsStr::new("");
        assert_eq!(
            agent_pipe(Some(empty), Some(empty)).unwrap(),
            DEFAULT_AGENT_PIPE
        );
        match agent_pipe(Some(OsStr::new(r"\\evil\pipe\x")), Some(mine)) {
            Err(SshError::Agent(message)) => {
                assert!(
                    message.contains(r"“\\evil\pipe\x” is not a local pipe"),
                    "{message}"
                );
                assert!(message.contains(DEFAULT_AGENT_PIPE), "{message}");
                assert!(message.contains("connection's SSH tab"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        match agent_pipe(None, Some(OsStr::new("/tmp/ssh-x/agent.1"))) {
            Err(SshError::Agent(message)) => {
                assert!(message.contains("SSH_AUTH_SOCK"), "{message}")
            }
            other => panic!("{other:?}"),
        }
    }
}
