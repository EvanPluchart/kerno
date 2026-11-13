//! Errors shared by the drivers and the app.

use std::fmt;

/// Result alias used across Kerno's library crates.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// An error reported by the database server for a statement.
///
/// Carries everything the UI shows: the message, the engine's error code,
/// the optional detail and hint, and the position in the SQL text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DbError {
    /// Primary, human-readable message.
    pub message: String,
    /// SQLSTATE (PostgreSQL) or `errno (sqlstate)` (MySQL), when known.
    pub code: Option<String>,
    /// Severity as reported by the server (`ERROR`, `FATAL`, ...).
    pub severity: Option<String>,
    /// Secondary message with more detail.
    pub detail: Option<String>,
    /// Suggestion from the server on how to fix the problem.
    pub hint: Option<String>,
    /// 1-based character offset of the error in the statement text.
    pub position: Option<u32>,
}

impl DbError {
    /// An error with only a message.
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(code) = &self.code {
            write!(f, " [{code}]")?;
        }
        Ok(())
    }
}

/// Everything that can go wrong between Kerno and a database.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server rejected a statement. Boxed to keep `Error` small.
    #[error("{0}")]
    Db(Box<DbError>),
    /// The connection could not be established (network, authentication).
    #[error("could not connect: {0}")]
    Connect(String),
    /// The server asks for a password and none was given.
    #[error("the server asks for a password and none was given")]
    PasswordRequired,
    /// TLS setup or handshake failed.
    #[error("TLS: {0}")]
    Tls(String),
    /// The SSH tunnel failed.
    #[error("SSH tunnel: {0}")]
    Ssh(Box<crate::ssh::SshError>),
    /// The connection was lost or closed while in use.
    #[error("connection closed: {0}")]
    Closed(String),
    /// The running statement was cancelled.
    #[error("query cancelled")]
    Cancelled,
    /// The connection settings are incomplete or inconsistent.
    #[error("invalid connection settings: {0}")]
    Config(String),
    /// The engine does not have this feature.
    #[error("not supported by this database: {0}")]
    Unsupported(String),
    /// The connection is flagged read-only and the statement would write.
    #[error("this connection is read-only: {0}")]
    ReadOnly(String),
    /// Local I/O failed (reading a certificate or key file, for example).
    #[error("{context}: {source}")]
    Io {
        /// What Kerno was doing, for example the file being read.
        context: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The secret store failed.
    #[error("secrets: {0}")]
    Secrets(String),
    /// Kerno's own storage failed.
    #[error("storage: {0}")]
    Storage(String),
    /// A result did not have the shape a driver expected.
    #[error("unexpected response from the server: {0}")]
    Protocol(String),
}

impl Error {
    /// Wraps an I/O error with what was being done.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// The server error, when this is one.
    pub fn db(&self) -> Option<&DbError> {
        match self {
            Self::Db(error) => Some(error),
            _ => None,
        }
    }

    /// Whether this error means the connection is gone and a reconnect may help.
    pub fn is_disconnect(&self) -> bool {
        matches!(self, Self::Closed(_))
    }
}

impl From<DbError> for Error {
    fn from(error: DbError) -> Self {
        Self::Db(Box::new(error))
    }
}

impl From<crate::ssh::SshError> for Error {
    fn from(error: crate::ssh::SshError) -> Self {
        Self::Ssh(Box::new(error))
    }
}

// Connection failures in plain words ------------------------------------------

/// What kind of thing went wrong when a connection could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The host name does not resolve.
    HostNotFound,
    /// Nothing listens on the host and port.
    Refused,
    /// The host did not answer in time.
    TimedOut,
    /// The server refused the user or the password.
    Authentication,
    /// The TLS handshake or the certificate failed.
    Tls,
    /// The database named in the settings does not exist.
    NoDatabase,
    /// The SSH server could not be reached.
    SshUnreachable,
    /// The SSH server refused the login.
    SshAuthentication,
    /// The SSH server is not known yet; the app asks whether to trust it.
    SshHostKeyUnknown,
    /// The SSH server's key differs from the recorded one.
    SshHostKeyChanged,
    /// The SSH server could not open the way to the database.
    SshForward,
    /// Anything else: only the raw message says what happened.
    Other,
}

/// A connection failure told in plain words: what happened, what to do
/// next, and the raw message of the driver or server, kept as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explained {
    /// Kind of failure.
    pub kind: FailureKind,
    /// What happened, in one sentence.
    pub headline: String,
    /// What to do next; empty when there is nothing better than the raw text.
    pub next_step: String,
    /// The message the failure came with.
    pub raw: String,
}

/// Texts the operating system gives for a host name that does not resolve
/// (macOS, then Linux).
const HOST_NOT_FOUND: [&str; 5] = [
    "failed to lookup address information",
    "nodename nor servname",
    "Name or service not known",
    "Temporary failure in name resolution",
    "No address associated with hostname",
];

const REFUSED: [&str; 3] = ["Connection refused", "os error 61", "os error 111"];

/// Kerno's own "no answer from" (both drivers' connect timeout and the SSH
/// tunnel's), tokio-postgres' "connection timed out", mysql_async's wait,
/// and the OS's timeouts and unreachable routes (macOS 60, Linux 110).
const TIMED_OUT: [&str; 8] = [
    "no answer from",
    "no answer after",
    "connection timed out",
    "timeout waiting for server",
    "os error 60",
    "os error 110",
    "No route to host",
    "Network is unreachable",
];

const AUTHENTICATION: [&str; 5] = [
    "[28P01]",
    "[28000]",
    "1045 (28000)",
    "1698 (28000)",
    "the server asks for a password and none was given",
];

const NO_DATABASE: [&str; 2] = ["[3D000]", "1049 (42000)"];

fn any(raw: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| raw.contains(marker))
}

/// Tells a connection failure in plain words. `raw` is the failure as the
/// app shows it today (an [`Error`]'s `Display`), `config` the settings it
/// was tried with. The words rely on stable markers in the drivers' and the
/// OS's English texts; anything unrecognised is [`FailureKind::Other`].
pub fn explain(raw: &str, config: &crate::config::ConnectionConfig) -> Explained {
    let (host, port, user) = (&config.host, config.port, &config.user);
    let ssh = config.ssh.as_ref();
    let ssh_host = ssh.map(|ssh| ssh.host.as_str()).unwrap_or_default();
    let ssh_port = ssh.map(|ssh| ssh.port).unwrap_or(22);
    let ssh_user = ssh.map(|ssh| ssh.user.as_str()).unwrap_or_default();

    let (kind, headline, next_step) = if let Some(ssh_text) = raw.split_once("SSH tunnel: ") {
        let ssh_text = ssh_text.1;
        if ssh_text.contains("is not known yet") {
            (
                FailureKind::SshHostKeyUnknown,
                format!("The SSH server {ssh_host}:{ssh_port} is not known yet."),
                "Check its key fingerprint and trust it if it is the right server.".to_string(),
            )
        } else if ssh_text.contains("could not reach the SSH server") {
            if any(ssh_text, &HOST_NOT_FOUND) {
                (
                    FailureKind::SshUnreachable,
                    format!("SSH host “{ssh_host}” was not found."),
                    "Check the SSH host name.".to_string(),
                )
            } else {
                (
                    FailureKind::SshUnreachable,
                    format!("The SSH server {ssh_host}:{ssh_port} could not be reached."),
                    "Check the SSH host and port, and that the SSH server is running.".to_string(),
                )
            }
        } else if ssh_text.contains("refused the")
            || ssh_text.contains("password is missing")
            || ssh_text.contains("is encrypted")
            || ssh_text.starts_with("SSH agent:")
        {
            (
                FailureKind::SshAuthentication,
                format!("The SSH server refused the login of user “{ssh_user}”."),
                "Check the SSH user and its password, key file and passphrase, or the keys \
                 loaded in your SSH agent."
                    .to_string(),
            )
        } else if ssh_text.contains("changed (now") {
            (
                FailureKind::SshHostKeyChanged,
                format!("The key of SSH server {ssh_host}:{ssh_port} has changed."),
                "This can mean someone is intercepting the connection. If the server was \
                 reinstalled, remove its old key from the known-hosts file named below."
                    .to_string(),
            )
        } else if ssh_text.contains("could not connect to") {
            (
                FailureKind::SshForward,
                format!("The SSH server could not reach the database at {host}:{port}."),
                "Check the database host and port as the SSH server sees them (often localhost)."
                    .to_string(),
            )
        } else {
            other()
        }
    } else if raw.starts_with("TLS: ") {
        (
            FailureKind::Tls,
            "The secure (TLS) connection could not be set up.".to_string(),
            "Check the SSL mode and the CA, certificate and key files on the SSL tab.".to_string(),
        )
    } else if any(raw, &AUTHENTICATION) {
        (
            FailureKind::Authentication,
            format!("The server refused the login of user “{user}”."),
            "Check the user name and the password.".to_string(),
        )
    } else if any(raw, &NO_DATABASE) {
        let headline = match config.database.as_deref().filter(|name| !name.is_empty()) {
            Some(database) => format!("Database “{database}” does not exist on this server."),
            None => "The database does not exist on this server.".to_string(),
        };
        (
            FailureKind::NoDatabase,
            headline,
            "Check the database name, or leave it empty to open the server's default.".to_string(),
        )
    } else if any(raw, &HOST_NOT_FOUND) {
        (
            FailureKind::HostNotFound,
            format!("Host “{host}” was not found."),
            "Check the host name; for a server on this computer, use localhost.".to_string(),
        )
    } else if any(raw, &REFUSED) {
        (
            FailureKind::Refused,
            format!("Nothing answers on {host}:{port}."),
            "Check that the server is running and listens on this port.".to_string(),
        )
    } else if any(raw, &TIMED_OUT) {
        (
            FailureKind::TimedOut,
            format!("{host}:{port} did not answer in time."),
            "Check the host and port, and that no firewall or VPN blocks the way.".to_string(),
        )
    } else {
        other()
    };
    Explained {
        kind,
        headline,
        next_step,
        raw: raw.to_string(),
    }
}

fn other() -> (FailureKind, String, String) {
    (
        FailureKind::Other,
        "The connection failed.".to_string(),
        String::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConnectionConfig, Engine, SshConfig};

    fn config() -> ConnectionConfig {
        ConnectionConfig {
            host: "studio".into(),
            port: 5432,
            user: "evan".into(),
            database: Some("shop".into()),
            ssh: Some(SshConfig {
                host: "bastion".into(),
                port: 2222,
                user: "deploy".into(),
                auth: Default::default(),
                keepalive_secs: 30,
            }),
            ..ConnectionConfig::new(Engine::Postgres)
        }
    }

    /// Raw texts as the app shows them, captured from the `kerno-*`
    /// containers on macOS (PostgreSQL 17, MySQL 8.4, MariaDB 11) and taken
    /// from the drivers and the OS for the Linux variants and SSH.
    #[test]
    fn each_failure_is_told_in_plain_words() {
        use FailureKind::*;
        let cases: &[(&str, FailureKind, &str)] = &[
            // Host not found: macOS, then Linux (both drivers word it alike).
            (
                "could not connect: error connecting to server: failed to lookup address \
                 information: nodename nor servname provided, or not known",
                HostNotFound,
                "Host “studio” was not found.",
            ),
            (
                "could not connect: failed to lookup address information: nodename nor \
                 servname provided, or not known",
                HostNotFound,
                "Host “studio” was not found.",
            ),
            (
                "could not connect: error connecting to server: failed to lookup address \
                 information: Name or service not known",
                HostNotFound,
                "Host “studio” was not found.",
            ),
            (
                "could not connect: failed to lookup address information: Temporary failure \
                 in name resolution",
                HostNotFound,
                "Host “studio” was not found.",
            ),
            // Refused.
            (
                "could not connect: error connecting to server: Connection refused (os error 61)",
                Refused,
                "Nothing answers on studio:5432.",
            ),
            (
                "could not connect: Connection refused (os error 111)",
                Refused,
                "Nothing answers on studio:5432.",
            ),
            // Timed out.
            (
                "could not connect: error connecting to server: connection timed out",
                TimedOut,
                "studio:5432 did not answer in time.",
            ),
            (
                "could not connect: no answer from 10.255.255.1:53306 after 1 s",
                TimedOut,
                "studio:5432 did not answer in time.",
            ),
            (
                "could not connect: error connecting to server: No route to host (os error 65)",
                TimedOut,
                "studio:5432 did not answer in time.",
            ),
            // Authentication.
            (
                "could not connect: password authentication failed for user \"kerno\" [28P01]",
                Authentication,
                "The server refused the login of user “evan”.",
            ),
            (
                "could not connect: role \"nobody\" does not exist [28000]",
                Authentication,
                "The server refused the login of user “evan”.",
            ),
            (
                "could not connect: Access denied for user 'kerno'@'192.168.107.1' (using \
                 password: YES) [1045 (28000)]",
                Authentication,
                "The server refused the login of user “evan”.",
            ),
            (
                "could not connect: Access denied for user 'root'@'localhost' [1698 (28000)]",
                Authentication,
                "The server refused the login of user “evan”.",
            ),
            (
                "the server asks for a password and none was given",
                Authentication,
                "The server refused the login of user “evan”.",
            ),
            // No such database.
            (
                "could not connect: database \"no_such_db\" does not exist [3D000]",
                NoDatabase,
                "Database “shop” does not exist on this server.",
            ),
            (
                "could not connect: Unknown database 'no_such_db' [1049 (42000)]",
                NoDatabase,
                "Database “shop” does not exist on this server.",
            ),
            // TLS.
            (
                "TLS: error performing TLS handshake: invalid peer certificate: UnknownIssuer",
                Tls,
                "The secure (TLS) connection could not be set up.",
            ),
            (
                "TLS: the server does not offer TLS",
                Tls,
                "The secure (TLS) connection could not be set up.",
            ),
            // SSH: a refused port inside the reason is still the SSH server's.
            (
                "SSH tunnel: could not reach the SSH server bastion:2222: Connection refused \
                 (os error 61)",
                SshUnreachable,
                "The SSH server bastion:2222 could not be reached.",
            ),
            (
                "SSH tunnel: could not reach the SSH server bastion:2222: failed to lookup \
                 address information: nodename nor servname provided, or not known",
                SshUnreachable,
                "SSH host “bastion” was not found.",
            ),
            (
                "SSH tunnel: the SSH server refused the password login for user deploy",
                SshAuthentication,
                "The SSH server refused the login of user “deploy”.",
            ),
            (
                "SSH tunnel: the SSH password is missing",
                SshAuthentication,
                "The SSH server refused the login of user “deploy”.",
            ),
            (
                "SSH tunnel: the private key /k/id is encrypted: the passphrase is wrong",
                SshAuthentication,
                "The SSH server refused the login of user “deploy”.",
            ),
            (
                "SSH tunnel: SSH agent: no identities",
                SshAuthentication,
                "The SSH server refused the login of user “deploy”.",
            ),
            (
                "SSH tunnel: the SSH server bastion:2222 is not known yet; its ssh-ed25519 key \
                 fingerprint is SHA256:abc",
                SshHostKeyUnknown,
                "The SSH server bastion:2222 is not known yet.",
            ),
            (
                "SSH tunnel: the key of SSH server bastion:2222 changed (now SHA256:abc, \
                 recorded in /h/known_hosts line 3). This can mean ...",
                SshHostKeyChanged,
                "The key of SSH server bastion:2222 has changed.",
            ),
            (
                "SSH tunnel: the SSH server could not connect to studio:5432: Connect failed",
                SshForward,
                "The SSH server could not reach the database at studio:5432.",
            ),
        ];
        for (raw, kind, headline) in cases {
            let explained = explain(raw, &config());
            assert_eq!(explained.kind, *kind, "{raw}");
            assert_eq!(explained.headline, *headline, "{raw}");
            assert!(!explained.next_step.is_empty(), "{raw}");
        }
    }

    #[test]
    fn the_owners_example_is_word_for_word() {
        let explained = explain(
            "could not connect: error connecting to server: failed to lookup address \
             information: nodename nor servname provided, or not known",
            &config(),
        );
        assert_eq!(
            format!("{} {}", explained.headline, explained.next_step),
            "Host “studio” was not found. Check the host name; for a server on this \
             computer, use localhost."
        );
    }

    #[test]
    fn the_raw_message_is_kept() {
        let raw = "could not connect: Connection refused (os error 61)";
        assert_eq!(explain(raw, &config()).raw, raw);
    }

    #[test]
    fn an_unknown_failure_is_other() {
        let explained = explain("something odd happened", &config());
        assert_eq!(explained.kind, FailureKind::Other);
        assert_eq!(explained.headline, "The connection failed.");
        assert_eq!(explained.next_step, "");
        assert_eq!(explained.raw, "something odd happened");
    }
}
