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
