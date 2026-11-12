//! SSH tunnels: a local port that forwards to the database through an SSH
//! server.
//!
//! Host keys are checked against known-hosts files (the user's
//! `~/.ssh/known_hosts`, read-only, and Kerno's own). An unknown host fails
//! with [`SshError::UnknownHostKey`], which carries the fingerprint to show
//! the user; connecting again with that fingerprint in
//! [`TunnelRequest::trusted_fingerprint`] accepts and records the key. A
//! host whose key changed is always refused.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, AuthResult, Handle};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::known_hosts::learn_known_hosts_path;
use russh::keys::ssh_key::{HashAlg, PublicKey};
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate, check_known_hosts_path};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::config::{Secret, SshAuth, SshConfig};

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
    /// Database host, as the SSH server resolves it.
    pub target_host: &'a str,
    /// Database port.
    pub target_port: u16,
    /// Known-hosts files to check, in order. Missing files are skipped.
    pub known_hosts: &'a [PathBuf],
    /// File that newly trusted keys are appended to (Kerno's own).
    pub learn_into: Option<&'a Path>,
    /// Fingerprint the user agreed to trust for this host, from a previous
    /// [`SshError::UnknownHostKey`].
    pub trusted_fingerprint: Option<&'a str>,
}

/// An open tunnel. Connections to [`Tunnel::local_addr`] reach the database.
/// Dropping the tunnel closes the listener and the SSH connection.
pub struct Tunnel {
    local_addr: SocketAddr,
    handle: Arc<Handle<HostKeyCheck>>,
    accept_task: JoinHandle<()>,
}

impl std::fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tunnel")
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}

impl Tunnel {
    /// Connects to the SSH server, authenticates, and starts forwarding a
    /// local port to the database.
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
        let config = Arc::new(client::Config {
            keepalive_interval: (ssh.keepalive_secs > 0)
                .then(|| Duration::from_secs(ssh.keepalive_secs.into())),
            // Three missed keep-alives and the tunnel is declared dead.
            keepalive_max: 3,
            ..client::Config::default()
        });

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

        // Check the forward works before reporting the tunnel as open, so a
        // wrong database host fails here with a clear message.
        let probe = handle
            .channel_open_direct_tcpip(
                request.target_host,
                request.target_port.into(),
                "127.0.0.1",
                0,
            )
            .await
            .map_err(|error| SshError::Forward {
                host: request.target_host.to_string(),
                port: request.target_port,
                reason: forward_reason(&error),
            })?;
        let _ = probe.close().await;

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|error| SshError::Other(format!("could not open a local port: {error}")))?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| SshError::Other(error.to_string()))?;

        let handle = Arc::new(handle);
        let accept_task = tokio::spawn(forward_connections(
            listener,
            handle.clone(),
            request.target_host.to_string(),
            request.target_port,
        ));

        Ok(Self {
            local_addr,
            handle,
            accept_task,
        })
    }

    /// The local address that forwards to the database.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Whether the SSH connection is gone.
    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.accept_task.abort();
        let handle = self.handle.clone();
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

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

fn forward_reason(error: &russh::Error) -> String {
    match error {
        russh::Error::ChannelOpenFailure(reason) => format!("{reason:?}"),
        other => other.to_string(),
    }
}

async fn forward_connections(
    listener: TcpListener,
    handle: Arc<Handle<HostKeyCheck>>,
    target_host: String,
    target_port: u16,
) {
    loop {
        let (mut local, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, "SSH tunnel listener stopped");
                return;
            }
        };
        let handle = handle.clone();
        let target_host = target_host.clone();
        tokio::spawn(async move {
            let channel = match handle
                .channel_open_direct_tcpip(
                    target_host,
                    target_port.into(),
                    peer.ip().to_string(),
                    peer.port().into(),
                )
                .await
            {
                Ok(channel) => channel,
                Err(error) => {
                    tracing::warn!(%error, "SSH tunnel could not open a forward channel");
                    return;
                }
            };
            let mut remote = channel.into_stream();
            // Ends when either side closes; errors here are ordinary
            // disconnects, which the database driver reports itself.
            let _ = tokio::io::copy_bidirectional(&mut local, &mut remote).await;
        });
    }
}

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
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(other)?
                .flatten();
            let result = handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(other)?;
            accept(result).ok_or_else(|| rejected("private key"))
        }
        SshAuth::Agent => {
            let mut agent = AgentClient::connect_env().await.map_err(|error| {
                SshError::Agent(format!(
                    "could not reach the agent ({error}); is SSH_AUTH_SOCK set?"
                ))
            })?;
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

fn accept(result: AuthResult) -> Option<()> {
    result.success().then_some(())
}

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
        russh::keys::load_secret_key(owned_path, passphrase.as_ref().map(Secret::expose))
    })
    .await
    .map_err(|error| SshError::Other(error.to_string()))?;

    loaded.map_err(|error| match error {
        russh::keys::Error::KeyIsEncrypted => SshError::Passphrase {
            path: display,
            reason: "a passphrase is required",
        },
        // ssh-key reports a wrong passphrase as a decryption/crypto failure.
        other if had_passphrase && !matches!(other, russh::keys::Error::IO(_)) => {
            SshError::Passphrase {
                path: display,
                reason: "the passphrase is wrong",
            }
        }
        other => SshError::Key {
            path: display,
            reason: other.to_string(),
        },
    })
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
