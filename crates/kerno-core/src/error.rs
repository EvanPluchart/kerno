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

/// An error as Kerno shows it (C14): the message as [`Error`]'s `Display`
/// gives it (for the server's errors, its code in brackets after it), then
/// the server's detail and hint when it gave them.
///
/// Shown to the user only: its `Display` can quote row values from the
/// detail ("Key (email)=(a@b.c) already exists."), so it is never logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    /// What went wrong, with the server's code when there is one.
    pub message: String,
    /// The server's detail; `None` when empty or the message again.
    pub detail: Option<String>,
    /// The server's hint; `None` when empty or the message again.
    pub hint: Option<String>,
}

impl Described {
    /// A description from its parts; a detail or a hint that is empty or
    /// says the message again is left out.
    pub fn new(message: String, detail: Option<&str>, hint: Option<&str>) -> Self {
        let keep = |extra: Option<&str>| {
            extra
                .map(str::trim)
                .filter(|extra| !extra.is_empty() && *extra != message.trim())
                .map(str::to_string)
        };
        Self {
            detail: keep(detail),
            hint: keep(hint),
            message,
        }
    }

    /// The lines Kerno shows, in order: the message, "Detail: …", "Hint: …".
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![self.message.clone()];
        lines.extend(self.detail.iter().map(|detail| format!("Detail: {detail}")));
        lines.extend(self.hint.iter().map(|hint| format!("Hint: {hint}")));
        lines
    }
}

/// The lines of [`Described::lines`], one per line of text.
impl fmt::Display for Described {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.lines().join("\n"))
    }
}

/// `error` as Kerno shows it: the server's detail and hint with the message
/// of a [`Error::Db`]; the message alone for every other error, so a cancel
/// or a lost connection keeps the words of whoever shows it.
pub fn describe(error: &Error) -> Described {
    match error {
        Error::Db(db) => {
            // Compared with the message without its code too.
            let not_the_message = |extra: &&str| extra.trim() != db.message.trim();
            Described::new(
                db.to_string(),
                db.detail.as_deref().filter(not_the_message),
                db.hint.as_deref().filter(not_the_message),
            )
        }
        other => Described::new(other.to_string(), None, None),
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
    /// The account may not connect to the database (PostgreSQL's CONNECT
    /// privilege, K62).
    NotAllowed,
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
    /// The saved password could not be read from the system secret store.
    SecretStore,
    /// An open connection, or one of its sessions, was closed or lost
    /// while in use (K75).
    Lost,
    /// A database file (SQLite) is not where the connection says.
    FileMissing,
    /// The file is not a database Kerno can open.
    NotADatabase,
    /// Another connection or program holds the file locked.
    FileLocked,
    /// The file may only be read, and the connection would write.
    FileReadOnly,
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
/// (macOS, then Linux, then Windows' WSAHOST_NOT_FOUND, WSATRY_AGAIN and
/// WSANO_DATA, which Windows words in the user's language).
const HOST_NOT_FOUND: [&str; 8] = [
    "failed to lookup address information",
    "nodename nor servname",
    "Name or service not known",
    "Temporary failure in name resolution",
    "No address associated with hostname",
    "os error 11001",
    "os error 11002",
    "os error 11004",
];

/// macOS, Linux, then Windows' WSAECONNREFUSED.
const REFUSED: [&str; 4] = [
    "Connection refused",
    "os error 61",
    "os error 111",
    "os error 10061",
];

/// Kerno's own "no answer from" (both drivers' connect timeout and the SSH
/// tunnel's), tokio-postgres' "connection timed out", mysql_async's wait,
/// and the OS's timeouts and unreachable routes (macOS 60, Linux 110,
/// Windows 10060; Windows' unreachable network and host, 10051 and 10065).
const TIMED_OUT: [&str; 11] = [
    "no answer from",
    "no answer after",
    "connection timed out",
    "timeout waiting for server",
    "os error 60",
    "os error 110",
    "os error 10060",
    "No route to host",
    "Network is unreachable",
    "os error 10051",
    "os error 10065",
];

/// Part of the driver's message when a server's TLS is older than what
/// Kerno accepts (MySQL 5.x, K22); read by [`explain`].
pub const OLD_SERVER_TLS: &str = "the server's TLS is older than Kerno accepts";

/// Part of the driver's message when an account's password has MySQL's
/// pre-4.1 hash (K22); read by [`explain`].
pub const OLD_PASSWORD_HASH: &str = "has a pre-4.1 password hash";

const AUTHENTICATION: [&str; 5] = [
    "[28P01]",
    "[28000]",
    "1045 (28000)",
    "1698 (28000)",
    "the server asks for a password and none was given",
];

const NO_DATABASE: [&str; 2] = ["[3D000]", "1049 (42000)"];

/// PostgreSQL's "permission denied for database", at connect: no CONNECT
/// privilege.
const NOT_ALLOWED: [&str; 1] = ["[42501]"];

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
    // OpenSSH's port 0 is the SSH configuration's: the host alone.
    let ssh_server = match ssh_port {
        0 => ssh_host.to_string(),
        port => format!("{ssh_host}:{port}"),
    };
    let ssh_user = ssh.map(|ssh| ssh.user.as_str()).unwrap_or_default();

    let (kind, headline, next_step) = if config.engine.is_file() {
        file_words(raw, config)
    } else if let Some(words) = password_words(raw) {
        words
    } else if let Some(ssh_text) = raw.split_once("SSH tunnel: ") {
        let ssh_text = ssh_text.1;
        if let Some(stderr) = ssh_text.strip_prefix("OpenSSH: ") {
            openssh_words(stderr, config)
        } else if ssh_text.starts_with(crate::openssh::NOT_FOUND) {
            // Windows ships OpenSSH as an optional feature (K69, D-138).
            let (headline, next_step) = match cfg!(unix) {
                true => (
                    "OpenSSH was not found on this computer.",
                    "Choose the built-in client on the SSH tab, or install OpenSSH.",
                ),
                false => (
                    "Windows' OpenSSH client is not installed.",
                    "Add “OpenSSH Client” in Settings ▸ System ▸ Optional features, or choose the \
                     built-in client on the SSH tab.",
                ),
            };
            (
                FailureKind::Other,
                headline.to_string(),
                next_step.to_string(),
            )
        } else if ssh_text.contains("is not known yet") {
            // The server named in the text: with OpenSSH, maybe a jump host.
            let server = between(ssh_text, "the SSH server ", " is not known yet")
                .map(str::to_string)
                .unwrap_or_else(|| ssh_server.clone());
            (
                FailureKind::SshHostKeyUnknown,
                format!("The SSH server {server} is not known yet."),
                "Check its key fingerprint and trust it if it is the right server.".to_string(),
            )
        } else if let Some(path) = between(
            ssh_text,
            "the private key ",
            " is encrypted: a passphrase is required",
        ) {
            (
                FailureKind::SshAuthentication,
                format!("The key in {path} needs a passphrase."),
                "Load it with ssh-add or enter it here.".to_string(),
            )
        } else if let Some(path) = between(ssh_text, "the private key ", " is an RSA key") {
            (
                FailureKind::SshAuthentication,
                format!("The built-in SSH client cannot use the RSA key {path}."),
                "Choose the OpenSSH client on the SSH tab, or load the key in your SSH agent."
                    .to_string(),
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
                    format!("The SSH server {ssh_server} could not be reached."),
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
            let server = between(ssh_text, "the key of SSH server ", " changed (now")
                .map(str::to_string)
                .unwrap_or_else(|| ssh_server.clone());
            (
                FailureKind::SshHostKeyChanged,
                format!("The key of SSH server {server} has changed."),
                "This can mean someone is intercepting the connection. If the server was \
                 reinstalled, remove its old key from the known-hosts file named below."
                    .to_string(),
            )
        } else if ssh_text.contains("could not connect to")
            && ssh_text.contains("administratively prohibited")
        {
            (
                FailureKind::SshForward,
                "The SSH server does not allow forwarding to the database.".to_string(),
                "Its AllowTcpForwarding setting (or the account's permitopen) forbids it: ask \
                 its administrator."
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
    } else if let Some(store) = raw.strip_prefix("secrets: ") {
        secret_store_words(store)
    } else if raw.contains(OLD_SERVER_TLS) {
        (
            FailureKind::Tls,
            "This server only offers an old TLS that Kerno does not accept.".to_string(),
            "Choose SSL mode “prefer” (Kerno then connects without TLS) or “disable” on the SSL \
             tab, on a network you trust or through an SSH tunnel; or update the server's TLS."
                .to_string(),
        )
    } else if raw.contains(OLD_PASSWORD_HASH) {
        (
            FailureKind::Authentication,
            format!("The account “{user}” has an old password hash that Kerno does not accept."),
            "Ask the server's administrator to set its password again with a current hash \
             (SET old_passwords = 0, then SET PASSWORD)."
                .to_string(),
        )
    } else if raw.starts_with("TLS: ") {
        (
            FailureKind::Tls,
            "The secure (TLS) connection could not be set up.".to_string(),
            "Check the SSL mode and the CA, certificate and key files on the SSL tab.".to_string(),
        )
    } else if any(raw, &AUTHENTICATION) {
        use crate::config::PasswordSource;
        let next = match &config.password_source {
            PasswordSource::Stored => "Check the user name and the password.",
            PasswordSource::RdsIam { .. } => {
                "Check that this database user may log in with IAM (rds_iam on PostgreSQL, \
                 AWSAuthenticationPlugin on MySQL) and that the AWS profile's policy allows \
                 rds-db:connect."
            }
            _ => {
                "Check the user name, and that the password command prints this user's \
                  password (Run now on the General tab)."
            }
        };
        (
            FailureKind::Authentication,
            format!("The server refused the login of user “{user}”."),
            next.to_string(),
        )
    } else if any(raw, &NOT_ALLOWED) {
        let headline = match config.database.as_deref().filter(|name| !name.is_empty()) {
            Some(database) => format!("You may not connect to database “{database}”."),
            None => "You may not connect to this database.".to_string(),
        };
        (
            FailureKind::NotAllowed,
            headline,
            "Ask the server's administrator for the CONNECT privilege on it, or choose another \
             database."
                .to_string(),
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
    } else if raw.starts_with(LOST) {
        lost_words(config)
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

/// A password command that failed, or a password file that was ignored
/// (J3), in plain words.
fn password_words(raw: &str) -> Option<(FailureKind, String, String)> {
    use crate::password::{COMMAND_MARKER, IGNORED, UNCONFIRMED};
    if raw.contains(UNCONFIRMED) {
        return Some((
            FailureKind::Other,
            "This connection's password command came from an imported file.".to_string(),
            "Open the connection's settings, check the command and confirm it: it does not run \
             before."
                .to_string(),
        ));
    }
    if let Some((_, rest)) = raw.split_once(COMMAND_MARKER) {
        let (name, what) = rest.split_once("` ")?;
        let (headline, next) = if what.starts_with("was not found") {
            (
                format!("The password command “{name}” was not found."),
                "Install it, or give the full path of the program.",
            )
        } else if what.starts_with("did not finish") {
            (
                format!("The password command “{name}” did not finish in time."),
                "Run it in a terminal: it may be waiting for input or a sign-in (op signin, aws \
                 sso login). Raise its timeout if it is only slow.",
            )
        } else if what.starts_with("printed nothing") {
            (
                format!("The password command “{name}” printed nothing."),
                "It must print the password on its output.",
            )
        } else {
            (
                format!("The password command “{name}” failed."),
                "Its message is below. Run it in a terminal to fix it (for AWS, check the \
                 profile and the region; for 1Password, sign in with op signin).",
            )
        };
        return Some((FailureKind::Authentication, headline, next.to_string()));
    }
    if !raw.contains("The server asks for a password") {
        return None;
    }
    let (file, why) = raw.split_once(IGNORED)?;
    let file = file.rsplit(": ").next().unwrap_or(file);
    let why = why.split(" The server asks").next().unwrap_or(why);
    let mut next = why.to_string();
    if let Some(first) = next.get(..1) {
        next.replace_range(..1, &first.to_uppercase());
    }
    Some((
        FailureKind::Authentication,
        format!("The server asks for a password, and {file} was ignored."),
        format!("{next} Or save the password with the connection."),
    ))
}

/// What an OpenSSH tunnel says when `ssh` neither connected nor gave up.
pub(crate) const NO_ANSWER_FROM_SSH: &str = "no answer from ssh after";

/// The text between `start` and `end`.
fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(start)?;
    rest.split_once(end).map(|(inside, _)| inside)
}

/// The jump hosts of the connection's Jump host field, as (host, port).
fn jump_hosts(config: &crate::config::ConnectionConfig) -> Vec<(String, Option<u16>)> {
    let jump = config.ssh.as_ref().and_then(|ssh| ssh.jump.as_deref());
    jump.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|hop| !hop.is_empty())
        .map(|hop| {
            let hop = hop.rsplit_once('@').map_or(hop, |(_, host)| host);
            match hop.strip_prefix('[').and_then(|rest| rest.split_once(']')) {
                Some((host, rest)) => (
                    host.to_string(),
                    rest.strip_prefix(':').and_then(|port| port.parse().ok()),
                ),
                None => match hop.split_once(':') {
                    Some((host, port)) => (host.to_string(), port.parse().ok()),
                    None => (hop.to_string(), None),
                },
            }
        })
        .collect()
}

/// What `ssh`'s messages say, in plain words. Its first recognised line
/// wins: a jump host's failure comes before the tunnel's own.
fn openssh_words(
    stderr: &str,
    config: &crate::config::ConnectionConfig,
) -> (FailureKind, String, String) {
    let jumps = jump_hosts(config);
    let is_jump = |host: &str, port: Option<u16>| {
        jumps.iter().any(|(jump, jump_port)| {
            jump == host && (jump_port.is_none() || port.is_none() || *jump_port == port)
        })
    };
    let role = |host: &str, port: Option<u16>| match is_jump(host, port) {
        true => "jump host",
        false => "SSH server",
    };
    let words = |kind, headline: String, next: &str| (kind, headline, next.to_string());
    for line in stderr.lines().map(str::trim) {
        // Windows' ssh.exe before 8.4 (K69, D-138).
        if cfg!(windows) && line.starts_with("This OpenSSH is too old to ask Kerno") {
            return words(
                FailureKind::SshAuthentication,
                "This OpenSSH is too old to ask Kerno for a passphrase or a host key.".to_string(),
                "Load the key in the SSH agent (ssh-add) and connect once with ssh in a terminal \
                 to record the server's key, or choose the built-in client on the SSH tab.",
            );
        }
        if let Some(rest) = line
            .split_once("Could not resolve hostname ")
            .map(|(_, r)| r)
        {
            let host = rest.split(':').next().unwrap_or(rest).trim();
            let who = if is_jump(host, None) {
                "Jump host"
            } else {
                "SSH host"
            };
            return words(
                FailureKind::SshUnreachable,
                format!("{who} “{host}” was not found."),
                "Check the host name, or its Host entry in your SSH configuration \
                 (~/.ssh/config).",
            );
        }
        if let Some(rest) = line.split_once("connect to host ").map(|(_, r)| r)
            && let Some((host, rest)) = rest.split_once(" port ")
            && let Some((port, reason)) = rest.split_once(": ")
        {
            let port = port.parse().ok();
            let shown = format!("{host}:{}", port.unwrap_or(22));
            let role = role(host, port);
            return if reason.contains("refused") {
                words(
                    FailureKind::SshUnreachable,
                    format!("Nothing answers on the {role} {shown}."),
                    "Check the host and port, and that the SSH server is running.",
                )
            } else {
                words(
                    FailureKind::SshUnreachable,
                    format!("The {role} {shown} did not answer in time."),
                    "Check the host and port, and that no firewall or VPN blocks the way.",
                )
            };
        }
        if let Some((who, _)) = line.split_once(": Permission denied") {
            let host = who.rsplit_once('@').map_or(who, |(_, host)| host);
            let role = role(host, None);
            return words(
                FailureKind::SshAuthentication,
                format!("The {role} refused the login of “{who}”."),
                "Check the user and the key file, or the keys in your SSH agent (ssh-add -l). \
                 An encrypted key needs its passphrase: load it with ssh-add or enter it here.",
            );
        }
        if let Some(key) = between(line, "the key ", " needs a passphrase") {
            return words(
                FailureKind::SshAuthentication,
                format!("The key in {key} needs a passphrase."),
                "Load it with ssh-add, then connect again.",
            );
        }
        if let Some(key) =
            between(line, "Permissions ", " are too open").and_then(|rest| between(rest, "'", "'"))
        {
            return words(
                FailureKind::SshAuthentication,
                format!("OpenSSH will not use the key {key}: other users can read it."),
                &format!("Make it private: chmod 600 {key}"),
            );
        }
        if let Some(file) = line
            .split_once("Bad owner or permissions on ")
            .map(|(_, f)| f)
        {
            return words(
                FailureKind::Other,
                format!("OpenSSH will not read {file}: its owner or permissions are unsafe."),
                &format!("Make it yours and private: chmod 600 {file}"),
            );
        }
        if let Some(host) = between(line, "host key is known for ", " and you have requested") {
            return words(
                FailureKind::SshHostKeyUnknown,
                format!("The SSH server {host} is not in your known_hosts file yet."),
                "Your SSH configuration asks for strict checking: connect once with ssh in a \
                 terminal to check its key, then try again.",
            );
        }
        if line.contains("administratively prohibited") {
            return words(
                FailureKind::SshForward,
                "The SSH server does not allow forwarding.".to_string(),
                "Its AllowTcpForwarding setting (or the account's permitopen) forbids it: ask \
                 its administrator.",
            );
        }
        if line.contains("stdio forwarding failed") {
            return words(
                FailureKind::SshForward,
                "A jump host could not reach the next SSH server.".to_string(),
                "Check the SSH host and port as the jump host sees them.",
            );
        }
        if line.contains("Host key verification failed") {
            return words(
                FailureKind::SshHostKeyUnknown,
                "OpenSSH could not verify an SSH server's key.".to_string(),
                "Connect once with ssh in a terminal to check and record its key, then try \
                 again.",
            );
        }
        if line.contains(NO_ANSWER_FROM_SSH) {
            let host = config.ssh.as_ref().map(|ssh| ssh.host.as_str());
            return words(
                FailureKind::SshUnreachable,
                format!(
                    "The SSH server {} did not answer in time.",
                    host.unwrap_or_default()
                ),
                "Check the host and port, the jump hosts if any, and that no firewall or VPN \
                 blocks the way.",
            );
        }
        if line.contains("Connection closed by")
            || line.contains("Connection reset by")
            || line.contains("kex_exchange_identification")
        {
            return words(
                FailureKind::SshUnreachable,
                "The SSH server closed the connection.".to_string(),
                "Check the SSH host and port, and the jump hosts if any: something that is not \
                 an SSH server answered, or the server refused this computer.",
            );
        }
    }
    words(
        FailureKind::Other,
        "OpenSSH could not open the tunnel.".to_string(),
        "Read ssh's message below.",
    )
}

/// What the system secret store is called on this platform.
const SECRET_STORE: &str = if cfg!(target_os = "macos") {
    "keychain"
} else {
    "secret store"
};

/// A read of the saved password that the secret store refused or could not
/// serve (C14), from `secrets.rs`'s `secret_error` text: what the store
/// said, never a secret. Each case names a next step.
fn secret_store_words(text: &str) -> (FailureKind, String, String) {
    // Kerno's own file (K33) words its failures itself: what happened, then
    // what to do.
    if text.starts_with(crate::secrets::FILE_PROBLEM) {
        let (headline, next) = text.split_once(". ").unwrap_or((text, ""));
        let headline = format!("{}.", headline.trim_end_matches('.'));
        return (FailureKind::SecretStore, headline, next.to_string());
    }
    let store = SECRET_STORE;
    let ask = "or set Password to “Ask each time” in the connection's settings.";
    let (headline, next) = if any(
        text,
        &[
            // The prompt answered Deny (-128), or a wrong login password (-25293).
            "User canceled",
            "error code -128",
            "passphrase you entered is not correct",
            "error code -25293",
        ],
    ) {
        (
            format!("Kerno could not read the saved password: the {store} prompt was refused."),
            format!("Connect again and choose Allow in the {store} prompt, {ask}"),
        )
    } else if any(
        text,
        &[
            // keyring's NoStorageAccess: locked, read-only or missing.
            "Couldn't access platform storage",
            "interaction is not allowed",
            "error code -25308",
        ],
    ) {
        (
            format!(
                "Kerno could not read the saved password: the {store} is locked or cannot be used."
            ),
            format!("Unlock the {store}, then connect again, {ask}"),
        )
    } else {
        (
            format!("Kerno could not read the password from the {store}."),
            format!("Allow Kerno in the {store} prompt, {ask}"),
        )
    };
    (FailureKind::SecretStore, headline, next)
}

/// A database file's failure (SQLite), from the words its driver opens
/// with; the raw message follows them.
fn file_words(
    raw: &str,
    config: &crate::config::ConnectionConfig,
) -> (FailureKind, String, String) {
    let file = config
        .file
        .as_ref()
        .map(|file| file.display().to_string())
        .unwrap_or_default();
    let (kind, headline, next) = if raw.contains("the database file does not exist") {
        (
            FailureKind::FileMissing,
            format!("The file “{file}” does not exist."),
            "Check the path, or make a new database with New file…",
        )
    } else if raw.contains("the file is not a SQLite database") {
        (
            FailureKind::NotADatabase,
            format!("“{file}” is not a SQLite database."),
            "Choose the database file itself; an encrypted or damaged file cannot be opened.",
        )
    } else if raw.contains("the database file is locked") || raw.contains("database is locked") {
        (
            FailureKind::FileLocked,
            format!("The file “{file}” is locked by another connection or program."),
            "Finish or close what holds it (an open transaction, another app), then connect again.",
        )
    } else if raw.contains("the database file can only be read") {
        (
            FailureKind::FileReadOnly,
            format!("Kerno may only read “{file}”."),
            "Turn Read-only on to open it as it is, or give yourself write permission on the file \
             and its folder.",
        )
    } else if raw.starts_with(LOST) {
        return lost_words(config);
    } else {
        return other();
    };
    (kind, headline, next.to_string())
}

/// How [`Error::Closed`] starts: a connection or a session closed in use.
const LOST: &str = "connection closed: ";

/// A connection, or one of its sessions, closed or lost while in use (K75).
fn lost_words(config: &crate::config::ConnectionConfig) -> (FailureKind, String, String) {
    let headline = match config.name.trim() {
        "" => "The connection to the server was lost.".to_string(),
        name => format!("The connection to “{name}” was lost."),
    };
    (
        FailureKind::Lost,
        headline,
        "Reconnect it (its menu in the sidebar), then try again.".to_string(),
    )
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
    use crate::config::{ConnectionConfig, Engine, SshClient, SshConfig};

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
                client: SshClient::Builtin,
                jump: None,
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
            // Windows' own words (a runner, K68).
            (
                "could not connect: error connecting to server: No connection could be made \
                 because the target machine actively refused it. (os error 10061)",
                Refused,
                "Nothing answers on studio:5432.",
            ),
            (
                "could not connect: error connecting to server: No such host is known. (os error \
                 11001)",
                HostNotFound,
                "Host “studio” was not found.",
            ),
            (
                "could not connect: error connecting to server: A connection attempt failed \
                 because the connected party did not properly respond after a period of time, or \
                 established connection failed because connected host has failed to respond. (os \
                 error 10060)",
                TimedOut,
                "studio:5432 did not answer in time.",
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
            // K62: no CONNECT privilege on the database.
            (
                "could not connect: permission denied for database \"shop\" [42501]",
                NotAllowed,
                "You may not connect to database “shop”.",
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
            // K22: MySQL 5.x servers.
            (
                "TLS: the server's TLS is older than Kerno accepts (TLS 1.0 or 1.1, or no ECDHE \
                 key exchange): peer is incompatible: ServerDoesNotSupportTls12Or13",
                Tls,
                "This server only offers an old TLS that Kerno does not accept.",
            ),
            (
                "could not connect: the account evan has a pre-4.1 password hash, which Kerno \
                 refuses",
                Authentication,
                "The account “evan” has an old password hash that Kerno does not accept.",
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

    /// J2: `ssh`'s messages (OpenSSH 10 on macOS, from the `kerno-ssh*`
    /// containers) in plain words; a jump host is named as one.
    #[test]
    fn openssh_messages_are_told_in_plain_words() {
        use FailureKind::*;
        let mut config = config();
        if let Some(ssh) = config.ssh.as_mut() {
            ssh.client = SshClient::OpenSsh;
            ssh.jump = Some("ops@jump.example.com:2200".into());
        }
        let cases: &[(&str, FailureKind, &str)] = &[
            (
                "ssh: Could not resolve hostname bastion: nodename nor servname provided, or not known",
                SshUnreachable,
                "SSH host “bastion” was not found.",
            ),
            (
                "ssh: Could not resolve hostname jump.example.com: Name or service not known\n\
                 Connection closed by UNKNOWN port 65535",
                SshUnreachable,
                "Jump host “jump.example.com” was not found.",
            ),
            (
                "ssh: connect to host jump.example.com port 2200: Connection refused\n\
                 Connection closed by UNKNOWN port 65535",
                SshUnreachable,
                "Nothing answers on the jump host jump.example.com:2200.",
            ),
            (
                "ssh: connect to host 10.0.0.9 port 22: Operation timed out",
                SshUnreachable,
                "The SSH server 10.0.0.9:22 did not answer in time.",
            ),
            (
                "deploy@bastion: Permission denied (publickey).",
                SshAuthentication,
                "The SSH server refused the login of “deploy@bastion”.",
            ),
            (
                "ops@jump.example.com: Permission denied (publickey,password).",
                SshAuthentication,
                "The jump host refused the login of “ops@jump.example.com”.",
            ),
            (
                "the key /Users/ada/.ssh/id_rsa needs a passphrase, which ssh asked for\n\
                 deploy@bastion: Permission denied (publickey).",
                SshAuthentication,
                "The key in /Users/ada/.ssh/id_rsa needs a passphrase.",
            ),
            (
                "@@@@\n@         WARNING: UNPROTECTED PRIVATE KEY FILE!          @\n\
                 Permissions 0644 for '/k/id_ed25519' are too open.",
                SshAuthentication,
                "OpenSSH will not use the key /k/id_ed25519: other users can read it.",
            ),
            (
                "Bad owner or permissions on /Users/ada/.ssh/config",
                Other,
                "OpenSSH will not read /Users/ada/.ssh/config: its owner or permissions are unsafe.",
            ),
            (
                "No ED25519 host key is known for bastion and you have requested strict checking.\n\
                 Host key verification failed.",
                SshHostKeyUnknown,
                "The SSH server bastion is not in your known_hosts file yet.",
            ),
            (
                "channel 0: open failed: administratively prohibited: open failed\n\
                 stdio forwarding failed",
                SshForward,
                "The SSH server does not allow forwarding.",
            ),
            (
                "kex_exchange_identification: Connection closed by remote host",
                SshUnreachable,
                "The SSH server closed the connection.",
            ),
            (
                "no answer from ssh after 30 s",
                SshUnreachable,
                "The SSH server bastion did not answer in time.",
            ),
            ("something new", Other, "OpenSSH could not open the tunnel."),
        ];
        for (stderr, kind, headline) in cases {
            let raw = format!("SSH tunnel: OpenSSH: {stderr}");
            let explained = explain(&raw, &config);
            assert_eq!(explained.kind, *kind, "{stderr}");
            assert_eq!(explained.headline, *headline, "{stderr}");
            assert!(!explained.next_step.is_empty(), "{stderr}");
            // ssh's own words stay below.
            assert_eq!(explained.raw, raw);
        }

        // "The key in ~/.ssh/id_rsa needs a passphrase: load it with ssh-add
        // or enter it here" (CLAUDE.md §4 J2), for either client.
        let explained = explain(
            "SSH tunnel: the private key ~/.ssh/id_rsa is encrypted: a passphrase is required",
            &config,
        );
        assert_eq!(
            format!("{} {}", explained.headline, explained.next_step),
            "The key in ~/.ssh/id_rsa needs a passphrase. Load it with ssh-add or enter it here."
        );
        let explained = explain(
            "SSH tunnel: the private key /k/id_rsa is an RSA key, which the built-in SSH \
             client cannot use: choose the OpenSSH client on the SSH tab, or load the key in \
             your SSH agent",
            &config,
        );
        assert_eq!(
            explained.headline,
            "The built-in SSH client cannot use the RSA key /k/id_rsa."
        );
        let explained = explain(
            &format!("SSH tunnel: {}", crate::openssh::NOT_FOUND),
            &config,
        );
        assert_eq!(
            explained.headline,
            match cfg!(windows) {
                true => "Windows' OpenSSH client is not installed.",
                false => "OpenSSH was not found on this computer.",
            }
        );
        // K69: Windows' ssh.exe before 8.4 cannot ask Kerno (Windows only).
        let explained = explain(
            &format!(
                "SSH tunnel: OpenSSH: {}\nkerno@bastion: Permission denied (publickey).",
                crate::openssh::TOO_OLD
            ),
            &config,
        );
        assert_eq!(explained.kind, SshAuthentication);
        assert_eq!(
            explained.headline.starts_with("This OpenSSH is too old"),
            cfg!(windows),
            "{}",
            explained.headline
        );
        let explained = explain(
            "SSH tunnel: the SSH server could not connect to studio:5432: channel 2: open \
             failed: administratively prohibited: open failed",
            &config,
        );
        assert_eq!(explained.kind, SshForward);
        assert_eq!(
            explained.headline,
            "The SSH server does not allow forwarding to the database."
        );
        // A jump host's unknown key names the jump host.
        let explained = explain(
            "SSH tunnel: the SSH server jump.example.com:2200 is not known yet; its ED25519 key \
             fingerprint is SHA256:abc",
            &config,
        );
        assert_eq!(
            explained.headline,
            "The SSH server jump.example.com:2200 is not known yet."
        );
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

    /// K75: a connection or a session closed in use, on a server or a file, says so with the
    /// connection's name and the next step; a refused connection still says it was refused.
    #[test]
    fn a_lost_connection_is_told_in_plain_words() {
        let closed = Error::Closed("the session was closed".into()).to_string();
        let named = ConnectionConfig {
            name: "shop test".into(),
            ..config()
        };
        let file = ConnectionConfig {
            name: "notes".into(),
            file: Some("/tmp/notes.db".into()),
            ..ConnectionConfig::new(Engine::Sqlite)
        };
        for config in [&named, &file] {
            let explained = explain(&closed, config);
            assert_eq!(explained.kind, FailureKind::Lost);
            assert_eq!(
                format!("{} {}", explained.headline, explained.next_step),
                format!(
                    "The connection to “{}” was lost. Reconnect it (its menu in the sidebar), \
                     then try again.",
                    config.name
                )
            );
            assert_eq!(explained.raw, closed);
        }
        assert_eq!(
            explain(&closed, &config()).headline,
            "The connection to the server was lost."
        );
        let refused = "connection closed: Connection refused (os error 61)";
        assert_eq!(explain(refused, &named).kind, FailureKind::Refused);
    }

    #[test]
    fn the_raw_message_is_kept() {
        let raw = "could not connect: Connection refused (os error 61)";
        assert_eq!(explain(raw, &config()).raw, raw);
    }

    /// C14: the server's detail and hint come with its message and code, in
    /// one order; nothing empty or said twice; other errors keep their words.
    #[test]
    fn describe_keeps_the_detail_and_the_hint() {
        let db = |detail: Option<&str>, hint: Option<&str>| {
            Error::from(DbError {
                message: "cannot drop table t because other objects depend on it".into(),
                code: Some("2BP01".into()),
                detail: detail.map(str::to_string),
                hint: hint.map(str::to_string),
                ..DbError::default()
            })
        };
        let full = describe(&db(
            Some("view v depends on table t"),
            Some("Use DROP ... CASCADE to drop the dependent objects too."),
        ));
        assert_eq!(
            full.to_string(),
            "cannot drop table t because other objects depend on it [2BP01]\n\
             Detail: view v depends on table t\n\
             Hint: Use DROP ... CASCADE to drop the dependent objects too."
        );
        assert_eq!(full.lines().len(), 3);
        // Empty, blank or the message again: not repeated.
        for extra in [
            Some(""),
            Some("  "),
            Some("cannot drop table t because other objects depend on it"),
            Some("cannot drop table t because other objects depend on it [2BP01]"),
            None,
        ] {
            let described = describe(&db(extra, extra));
            assert_eq!(
                (described.detail, described.hint),
                (None, None),
                "{extra:?}"
            );
        }
        // MySQL gives neither: its message and its code are kept.
        let mysql = describe(&Error::from(DbError {
            message: "Duplicate entry '1' for key 't.PRIMARY'".into(),
            code: Some("1062 (23000)".into()),
            ..DbError::default()
        }));
        assert_eq!(
            mysql.to_string(),
            "Duplicate entry '1' for key 't.PRIMARY' [1062 (23000)]"
        );
        // Not from a server: the message only, as Display gives it.
        for error in [
            Error::Cancelled,
            Error::Closed("broken pipe".into()),
            Error::Config("no host".into()),
            Error::Storage("disk full".into()),
            Error::io("reading ca.pem", std::io::Error::other("denied")),
        ] {
            let described = describe(&error);
            assert_eq!(described.to_string(), error.to_string());
            assert_eq!(described.lines(), vec![error.to_string()]);
        }
    }

    /// C14: a read of the saved password that the secret store refused or
    /// could not serve says what to do, from `secret_error`'s text, and
    /// never more than the store's own message.
    #[test]
    fn a_secret_store_failure_names_the_next_step() {
        let store = SECRET_STORE;
        let cases = [
            (
                "secrets: could not read from the system secret store: Platform failure: User \
                 canceled the operation.",
                format!("Kerno could not read the saved password: the {store} prompt was refused."),
                format!("Connect again and choose Allow in the {store} prompt"),
            ),
            (
                "secrets: could not read from the system secret store: Platform failure: error \
                 code -25293",
                format!("Kerno could not read the saved password: the {store} prompt was refused."),
                format!("Connect again and choose Allow in the {store} prompt"),
            ),
            (
                "secrets: could not read from the system secret store: Couldn't access platform \
                 storage: User interaction is not allowed.",
                format!(
                    "Kerno could not read the saved password: the {store} is locked or cannot be used."
                ),
                format!("Unlock the {store}, then connect again"),
            ),
            (
                "secrets: could not address the system secret store: Value of 'user' is invalid",
                format!("Kerno could not read the password from the {store}."),
                format!("Allow Kerno in the {store} prompt"),
            ),
        ];
        for (raw, headline, next) in cases {
            let explained = explain(raw, &config());
            assert_eq!(explained.kind, FailureKind::SecretStore, "{raw}");
            assert_eq!(explained.headline, headline, "{raw}");
            assert!(explained.next_step.starts_with(&next), "{raw}");
            assert!(
                explained
                    .next_step
                    .ends_with("or set Password to “Ask each time” in the connection's settings."),
                "{raw}"
            );
            assert_eq!(explained.raw, raw);
        }
    }

    /// K33: a failure of Kerno's password file is not told as a keychain
    /// prompt: its own words, what happened then what to do. Mutation
    /// seen: the `FILE_PROBLEM` branch removed (keychain advice comes back).
    #[test]
    fn a_password_file_failure_says_what_happened_and_what_to_do() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::secrets::FileSecretStore::new(dir.path());
        crate::secrets::SecretStore::set(&store, "a", &crate::config::Secret::new("pw")).unwrap();
        std::fs::remove_file(dir.path().join(crate::secrets::KEY_FILE)).unwrap();
        let raw = crate::secrets::SecretStore::get(&store, "a")
            .unwrap_err()
            .to_string();
        let explained = explain(&raw, &config());
        assert_eq!(explained.kind, FailureKind::SecretStore);
        assert_eq!(
            explained.headline,
            "Kerno's password file could not be read: its key file secrets.key is missing."
        );
        assert!(
            explained
                .next_step
                .starts_with("It was set aside as secrets.kerno.damaged"),
            "{}",
            explained.next_step
        );
        assert!(explained.next_step.contains("enter the password again"));
        assert!(!explained.next_step.contains(SECRET_STORE));
    }

    #[test]
    fn an_unknown_failure_is_other() {
        let explained = explain("something odd happened", &config());
        assert_eq!(explained.kind, FailureKind::Other);
        assert_eq!(explained.headline, "The connection failed.");
        assert_eq!(explained.next_step, "");
        assert_eq!(explained.raw, "something odd happened");
    }

    /// J3: a failing password command, an ignored password file and an
    /// imported command are told in plain words, with what to do; a refused
    /// login names the command instead of a typed password.
    #[test]
    fn password_sources_are_explained() {
        use crate::config::PasswordSource;
        let config = config();
        let failed = explain(
            "could not connect: the password command `aws` failed (exit status 255): Unable \
             to locate credentials",
            &config,
        );
        assert_eq!(failed.headline, "The password command “aws” failed.");
        assert!(failed.next_step.contains("profile"), "{failed:?}");
        assert!(failed.raw.contains("Unable to locate credentials"));
        let slow = explain(
            "could not connect: the password command `op` did not finish in 10 s",
            &config,
        );
        assert_eq!(
            slow.headline,
            "The password command “op” did not finish in time."
        );
        let missing = explain(
            "could not connect: the password command `vault` was not found (looked in /bin)",
            &config,
        );
        assert_eq!(
            missing.headline,
            "The password command “vault” was not found."
        );
        let ignored = explain(
            "could not connect: ~/.pgpass was ignored: other users can read it (permissions \
             0644); run chmod 600 ~/.pgpass. The server asks for a password and none was given.",
            &config,
        );
        assert_eq!(ignored.kind, FailureKind::Authentication);
        assert_eq!(
            ignored.headline,
            "The server asks for a password, and ~/.pgpass was ignored."
        );
        assert!(
            ignored.next_step.starts_with("Other users can read it"),
            "{ignored:?}"
        );
        let imported = explain(
            &format!(
                "invalid connection settings: {}: open the settings",
                crate::password::UNCONFIRMED
            ),
            &config,
        );
        assert!(imported.headline.contains("imported file"), "{imported:?}");
        let refused = "could not connect: password authentication failed for user \"evan\" [28P01]";
        let rds = ConnectionConfig {
            password_source: PasswordSource::RdsIam {
                profile: None,
                region: "us-east-1".into(),
            },
            ..config.clone()
        };
        assert!(explain(refused, &rds).next_step.contains("rds_iam"));
        let command = ConnectionConfig {
            password_source: PasswordSource::OnePassword {
                reference: "op://a/b/c".into(),
            },
            ..config.clone()
        };
        assert!(
            explain(refused, &command)
                .next_step
                .contains("password command")
        );
        assert_eq!(
            explain(refused, &config).next_step,
            "Check the user name and the password."
        );
    }

    /// K29: a database file's failures in plain words first, SQLite's
    /// message kept as the raw text.
    #[test]
    fn a_file_s_failures_are_told_plainly() {
        use crate::config::Engine;
        let config = ConnectionConfig {
            file: Some("/data/shop.db".into()),
            ..ConnectionConfig::new(Engine::Sqlite)
        };
        let cases = [
            (
                "could not connect: the database file does not exist: /data/shop.db",
                FailureKind::FileMissing,
                "The file “/data/shop.db” does not exist.",
            ),
            (
                "could not connect: the file is not a SQLite database: /data/shop.db (file is not a \
                 database)",
                FailureKind::NotADatabase,
                "“/data/shop.db” is not a SQLite database.",
            ),
            (
                "could not connect: the database file is locked: /data/shop.db (database is locked)",
                FailureKind::FileLocked,
                "The file “/data/shop.db” is locked by another connection or program.",
            ),
            (
                "could not connect: the database file can only be read: /data/shop.db is read-only",
                FailureKind::FileReadOnly,
                "Kerno may only read “/data/shop.db”.",
            ),
        ];
        for (raw, kind, headline) in cases {
            let explained = explain(raw, &config);
            assert_eq!(
                (explained.kind, explained.headline.as_str()),
                (kind, headline)
            );
            assert!(!explained.next_step.is_empty());
            assert_eq!(explained.raw, raw);
        }
        assert_eq!(explain("something else", &config).kind, FailureKind::Other);
    }
}
