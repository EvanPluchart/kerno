//! Connection settings. Nothing in [`ConnectionConfig`] is secret: passwords
//! and passphrases travel separately in [`Secrets`] and are never serialized.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::environment::{self, EnvironmentDef};
use crate::error::{Error, Result};

/// A database engine Kerno can talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// PostgreSQL.
    Postgres,
    /// MySQL.
    #[serde(rename = "mysql")]
    MySql,
    /// MariaDB.
    #[serde(rename = "mariadb")]
    MariaDb,
}

impl Engine {
    /// Every engine, in the order the engine picker shows them.
    pub const ALL: [Engine; 3] = [Engine::Postgres, Engine::MySql, Engine::MariaDb];

    /// Name shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            Self::Postgres => "PostgreSQL",
            Self::MySql => "MySQL",
            Self::MariaDb => "MariaDB",
        }
    }

    /// The engine's standard port.
    pub fn default_port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::MySql | Self::MariaDb => 3306,
        }
    }

    /// The user name the engine's own tools default to.
    pub fn default_user(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::MySql | Self::MariaDb => "root",
        }
    }

    fn from_url_scheme(scheme: &str) -> Option<Self> {
        match scheme {
            "postgres" | "postgresql" => Some(Self::Postgres),
            "mysql" => Some(Self::MySql),
            "mariadb" => Some(Self::MariaDb),
            _ => None,
        }
    }
}

/// How much TLS a connection asks for. The names follow PostgreSQL's
/// `sslmode`; MySQL's `ssl-mode` values map onto the same five levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    /// Never use TLS.
    Disable,
    /// Use TLS when the server offers it, without checking the certificate.
    #[default]
    Prefer,
    /// Require TLS, without checking the certificate.
    Require,
    /// Require TLS and a certificate signed by a trusted authority.
    VerifyCa,
    /// As `VerifyCa`, and the certificate must match the host name.
    VerifyFull,
}

impl TlsMode {
    /// Every mode, weakest first.
    pub const ALL: [TlsMode; 5] = [
        Self::Disable,
        Self::Prefer,
        Self::Require,
        Self::VerifyCa,
        Self::VerifyFull,
    ];

    /// The mode's name as written in settings and connection URLs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Prefer => "prefer",
            Self::Require => "require",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }

    /// Parses PostgreSQL `sslmode` and MySQL `ssl-mode` spellings.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().replace('_', "-").as_str() {
            "disable" | "disabled" => Some(Self::Disable),
            // PostgreSQL's "allow" (plain first, TLS on demand) is closest to prefer.
            "allow" | "prefer" | "preferred" => Some(Self::Prefer),
            "require" | "required" => Some(Self::Require),
            "verify-ca" => Some(Self::VerifyCa),
            "verify-full" | "verify-identity" => Some(Self::VerifyFull),
            _ => None,
        }
    }

    /// Whether the server certificate chain is checked.
    pub fn verifies_certificate(self) -> bool {
        matches!(self, Self::VerifyCa | Self::VerifyFull)
    }
}

/// TLS settings of a connection.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsConfig {
    /// Required level of TLS.
    pub mode: TlsMode,
    /// PEM file of the certificate authority to trust. Without it the
    /// verifying modes use the system's trust store.
    pub ca_file: Option<PathBuf>,
    /// PEM file of the client certificate.
    pub cert_file: Option<PathBuf>,
    /// PEM file of the client private key.
    pub key_file: Option<PathBuf>,
}

/// How Kerno authenticates to the SSH server of a tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum SshAuth {
    /// Password, from [`Secrets::ssh_password`].
    Password,
    /// Private key file, decrypted with [`Secrets::ssh_key_passphrase`] when encrypted.
    KeyFile {
        /// Path of the private key.
        path: PathBuf,
    },
    /// Keys held by an SSH agent.
    Agent {
        /// The agent's socket. `None` uses the one named by `SSH_AUTH_SOCK`;
        /// set it for agents that do not export that variable to GUI apps
        /// (password managers' agents, for example).
        #[serde(default)]
        socket: Option<PathBuf>,
    },
}

impl Default for SshAuth {
    fn default() -> Self {
        Self::Agent { socket: None }
    }
}

/// Which SSH client carries a tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SshClient {
    /// Kerno's own client (russh): password, key file without RSA, agent.
    #[default]
    Builtin,
    /// The system's `ssh`, with everything it reads from `~/.ssh/config`
    /// (aliases, ProxyJump, IdentityAgent, RSA keys, known_hosts).
    #[serde(rename = "openssh")]
    OpenSsh,
}

/// An SSH tunnel through which the database is reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshConfig {
    /// SSH server host. With [`SshClient::OpenSsh`], also a `Host` alias of
    /// the SSH configuration.
    pub host: String,
    /// SSH server port. With [`SshClient::OpenSsh`], 0 leaves it to the
    /// SSH configuration.
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// SSH user. With [`SshClient::OpenSsh`], empty leaves it to the SSH
    /// configuration.
    pub user: String,
    /// Authentication method.
    #[serde(default)]
    pub auth: SshAuth,
    /// Seconds between keep-alive messages; 0 turns them off.
    #[serde(default = "default_keepalive")]
    pub keepalive_secs: u16,
    /// The client that opens the tunnel.
    #[serde(default)]
    pub client: SshClient,
    /// Jump hosts, as `ssh -J` takes them (`user@host:port`, several
    /// separated by commas). OpenSSH only.
    #[serde(default)]
    pub jump: Option<String>,
}

fn default_ssh_port() -> u16 {
    22
}

fn default_keepalive() -> u16 {
    30
}

/// A saved connection, without its secrets. This is what is stored on disk
/// and what import/export writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// Name shown in the sidebar.
    pub name: String,
    /// Database engine.
    pub engine: Engine,
    /// Server host name or address.
    pub host: String,
    /// Server port.
    pub port: u16,
    /// Database user.
    pub user: String,
    /// Database opened at connect; the engine's default when empty.
    #[serde(default)]
    pub database: Option<String>,
    /// TLS settings.
    #[serde(default)]
    pub tls: TlsConfig,
    /// SSH tunnel, when the server is not reachable directly.
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    /// Block writes in the client and open sessions read-only on the server.
    #[serde(default)]
    pub read_only: bool,
    /// Id of the connection's environment (see [`crate::environment`]):
    /// `local`, `staging`, `production` or one the user added.
    #[serde(default = "default_environment")]
    pub environment: String,
    /// Colour tag: a key of the theme's connection tag colours.
    #[serde(default)]
    pub color: Option<String>,
    /// Folder the connection is listed under.
    #[serde(default)]
    pub group: Option<String>,
    /// Keep the password in the OS secret store; otherwise ask at connect.
    #[serde(default = "default_true")]
    pub save_password: bool,
    /// Extra driver options (`application_name`, `connect_timeout`, ...).
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    /// The commit mode query tabs start in: `Some(true)` auto-commit,
    /// `Some(false)` manual, `None` from the environment (production starts
    /// in manual commit, as DBeaver's connection types do).
    #[serde(default)]
    pub auto_commit: Option<bool>,
    /// PostgreSQL: schema made current in every session (`search_path`).
    /// MySQL and MariaDB use [`ConnectionConfig::database`] instead.
    #[serde(default)]
    pub default_schema: Option<String>,
    /// Statements run, in order, right after each session opens (DBeaver's
    /// "bootstrap queries"). Stored and exported in plain text.
    #[serde(default)]
    pub init_sql: Vec<String>,
    /// A failing [`ConnectionConfig::init_sql`] statement is logged and
    /// skipped instead of failing the connection.
    #[serde(default)]
    pub ignore_init_errors: bool,
    /// What the schema tree hides, per kind of object (see [`ObjectFilter`]):
    /// keys are `databases`, `schemas` and the tree's folder names
    /// (`tables`, `views`, `functions`, …). Exported with the connection.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub navigator_filters: BTreeMap<String, ObjectFilter>,
    /// The folder of the client tools (`pg_dump`, `mysqldump`…) this
    /// connection uses; Settings ▸ Local clients' default when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_dir: Option<std::path::PathBuf>,
    /// Where the database password comes from (J3).
    #[serde(default, skip_serializing_if = "PasswordSource::is_stored")]
    pub password_source: PasswordSource,
    /// The password command came from an imported file and the user has not
    /// confirmed it yet: it does not run until they do (see
    /// [`crate::password`]). Set by every import, never exported.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub password_command_unconfirmed: bool,
}

/// Where a connection's database password comes from (J3). The command
/// sources run a program without a shell; its output is the password of
/// that connect only, never stored (see [`crate::password`]).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PasswordSource {
    /// The OS secret store, or asked at connect ([`ConnectionConfig::save_password`]);
    /// with none, `~/.pgpass` (PostgreSQL) or `~/.my.cnf` (MySQL, MariaDB).
    #[default]
    Stored,
    /// A program and its arguments; `${host}`, `${port}`, `${user}` and
    /// `${database}` in an argument are replaced by the connection's values.
    Command {
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default = "default_command_timeout")]
        timeout_secs: u16,
    },
    /// An AWS RDS IAM token (`aws rds generate-db-auth-token`). Needs TLS.
    RdsIam {
        #[serde(default)]
        profile: Option<String>,
        region: String,
    },
    /// A 1Password secret reference (`op read op://vault/item/field`).
    OnePassword { reference: String },
}

/// Seconds a password command may take.
pub const DEFAULT_COMMAND_TIMEOUT: u16 = 10;

fn default_command_timeout() -> u16 {
    DEFAULT_COMMAND_TIMEOUT
}

impl PasswordSource {
    /// Whether the password is the stored or asked one (no program runs).
    pub fn is_stored(&self) -> bool {
        *self == Self::Stored
    }
}

fn default_true() -> bool {
    true
}

fn default_environment() -> String {
    environment::LOCAL.to_string()
}

/// The objects of one kind the schema tree shows: DBeaver's object filter
/// (`DBSObjectFilter`, ported from
/// `org.jkiss.dbeaver.model/.../struct/DBSObjectFilter.java`). A name is
/// shown when it matches one of `include` (or `include` is empty) and none
/// of `exclude`. Masks compare without regard to case; `*` stands for any
/// run of characters, `?` for one, `\` takes the next character as it is.
/// Unlike DBeaver, `%` and `_` are plain characters: names hold `_` often,
/// and hiding `order_items` must not hide `orderXitems`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectFilter {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Off: kept, not applied (DBeaver's "Toggle filter").
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for ObjectFilter {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            exclude: Vec::new(),
            enabled: true,
        }
    }
}

impl ObjectFilter {
    /// Whether the filter hides nothing: off, or without masks.
    pub fn is_inactive(&self) -> bool {
        !self.enabled || (self.include.is_empty() && self.exclude.is_empty())
    }

    /// Whether the tree shows an object named `name`.
    pub fn shows(&self, name: &str) -> bool {
        if self.is_inactive() {
            return true;
        }
        let any = |masks: &[String]| masks.iter().any(|mask| mask_matches(mask, name));
        (self.include.is_empty() || any(&self.include)) && !any(&self.exclude)
    }
}

/// `name` as a mask that matches it and nothing else: its wildcards and
/// backslashes escaped.
pub fn exact_mask(name: &str) -> String {
    let mut mask = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '*' | '?' | '\\') {
            mask.push('\\');
        }
        mask.push(c);
    }
    mask
}

/// Whether `name` matches `mask` (see [`ObjectFilter`]): a glob over
/// characters, backtracking on the last `*` seen.
fn mask_matches(mask: &str, name: &str) -> bool {
    #[derive(Clone, Copy, PartialEq)]
    enum Token {
        Any,
        One,
        Char(char),
    }
    let mut tokens = Vec::new();
    let mut chars = mask.trim().chars();
    while let Some(c) = chars.next() {
        tokens.push(match c {
            '*' => Token::Any,
            '?' => Token::One,
            '\\' => Token::Char(chars.next().unwrap_or('\\')),
            c => Token::Char(c),
        });
    }
    if tokens.is_empty() {
        return false;
    }
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    let name: Vec<char> = name.chars().collect();
    let (mut t, mut n) = (0, 0);
    // Where the last `*` was, and the name position it now covers up to.
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        match tokens.get(t) {
            Some(Token::Any) => {
                star = Some((t, n));
                t += 1;
            }
            Some(Token::One) => (t, n) = (t + 1, n + 1),
            Some(Token::Char(c)) if same(*c, name[n]) => (t, n) = (t + 1, n + 1),
            _ => match star {
                // The `*` takes one more character and the rest is tried again.
                Some((at, covered)) => {
                    star = Some((at, covered + 1));
                    (t, n) = (at + 1, covered + 1);
                }
                None => return false,
            },
        }
    }
    tokens[t..].iter().all(|token| *token == Token::Any)
}

impl ConnectionConfig {
    /// A connection to `engine` on localhost with the engine's defaults.
    pub fn new(engine: Engine) -> Self {
        Self {
            name: String::new(),
            engine,
            host: "localhost".into(),
            port: engine.default_port(),
            user: engine.default_user().into(),
            database: None,
            tls: TlsConfig::default(),
            ssh: None,
            read_only: false,
            environment: default_environment(),
            color: None,
            group: None,
            save_password: true,
            options: BTreeMap::new(),
            auto_commit: None,
            default_schema: None,
            init_sql: Vec::new(),
            ignore_init_errors: false,
            navigator_filters: BTreeMap::new(),
            client_dir: None,
            password_source: PasswordSource::Stored,
            password_command_unconfirmed: false,
        }
    }

    /// Whether query tabs of this connection start in manual commit: its
    /// own choice, else its environment's rule.
    pub fn starts_in_manual_commit(&self, environments: &[EnvironmentDef]) -> bool {
        let rules = &environment::environment(environments, &self.environment).rules;
        self.auto_commit.map_or(!rules.auto_commit, |auto| !auto)
    }

    /// Checks the settings are complete enough to try a connection.
    pub fn validate(&self) -> Result<()> {
        let missing = |what: &str| Err(Error::Config(format!("{what} is required")));
        if self.host.trim().is_empty() {
            return missing("the host");
        }
        if self.port == 0 {
            return missing("a port between 1 and 65535");
        }
        if self.user.trim().is_empty() {
            return missing("the user");
        }
        if self.tls.cert_file.is_some() != self.tls.key_file.is_some() {
            return Err(Error::Config(
                "a client certificate needs both the certificate file and the key file".into(),
            ));
        }
        if let Some(key) = self.options.keys().find(|key| is_secret_option(key)) {
            return Err(Error::Config(format!(
                "option \"{key}\" looks like a secret; options are stored and exported in plain text, \
                 so use the password fields instead"
            )));
        }
        for (index, sql) in self.init_sql.iter().enumerate() {
            if let Some(reason) = bootstrap_refusal(sql, self.engine) {
                return Err(Error::Config(format!(
                    "bootstrap statement {} {reason}",
                    index + 1
                )));
            }
        }
        if let Some(ssh) = &self.ssh {
            if ssh.host.trim().is_empty() {
                return missing("the SSH host");
            }
            let builtin = ssh.client == SshClient::Builtin;
            // OpenSSH takes the user and the port from its configuration.
            if builtin && ssh.user.trim().is_empty() {
                return missing("the SSH user");
            }
            if builtin && ssh.port == 0 {
                return missing("a port between 1 and 65535 for the SSH server");
            }
            if let SshAuth::KeyFile { path } = &ssh.auth
                && path.as_os_str().is_empty()
            {
                return missing("the SSH private key file");
            }
            if !builtin && ssh.auth == SshAuth::Password {
                return Err(Error::Config(
                    "SSH password login works with the built-in client only: choose it, or log \
                     in with a key or your SSH agent through OpenSSH"
                        .into(),
                ));
            }
            if builtin
                && ssh
                    .jump
                    .as_deref()
                    .is_some_and(|jump| !jump.trim().is_empty())
            {
                return Err(Error::Config(
                    "a jump host needs the OpenSSH client: choose it on the SSH tab".into(),
                ));
            }
            // OpenSSH hands jump hosts to a shell (ProxyJump): a connection
            // file from elsewhere must not run commands through them.
            let jump = ssh.jump.as_deref().map(str::trim).unwrap_or_default();
            let unsafe_word = [
                ("SSH host", ssh.host.trim(), false),
                ("SSH user", ssh.user.trim(), false),
                ("jump host", jump, true),
            ]
            .into_iter()
            .find(|(_, text, list)| {
                !builtin && !text.is_empty() && !crate::openssh::safe_ssh_word(text, *list)
            });
            if let Some((what, text, _)) = unsafe_word {
                return Err(Error::Config(format!(
                    "the {what} “{text}” has characters OpenSSH cannot take: use letters, \
                     digits and . - _ @ : [ ]{}",
                    if what == "jump host" {
                        " (and , between hosts)"
                    } else {
                        ""
                    }
                )));
            }
        }
        self.validate_password_source()
    }

    /// The password source's own rules (J3).
    fn validate_password_source(&self) -> Result<()> {
        let refuse = |message: &str| Err(Error::Config(message.into()));
        // What the user confirms must be what runs: no line breaks, control
        // characters or bidi overrides, which only a hand-made file can hold.
        let words: Vec<&str> = match &self.password_source {
            PasswordSource::Stored => Vec::new(),
            PasswordSource::Command { program, args, .. } => std::iter::once(program.as_str())
                .chain(args.iter().map(String::as_str))
                .collect(),
            PasswordSource::RdsIam { profile, region } => {
                vec![profile.as_deref().unwrap_or_default(), region.as_str()]
            }
            PasswordSource::OnePassword { reference } => vec![reference.as_str()],
        };
        let hidden = |c: char| {
            c.is_control()
                || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        };
        if words.iter().any(|word| word.chars().any(hidden)) {
            return refuse(
                "the password command holds a line break or an invisible control character: \
                 each argument must be plain text",
            );
        }
        match &self.password_source {
            PasswordSource::Stored => {}
            PasswordSource::Command {
                program,
                timeout_secs,
                ..
            } => {
                if program.trim().is_empty() {
                    return refuse("the password command needs a program");
                }
                if !(1..=120).contains(timeout_secs) {
                    return refuse("the password command's timeout is between 1 and 120 seconds");
                }
            }
            PasswordSource::RdsIam { region, .. } => {
                if region.trim().is_empty() {
                    return refuse("AWS RDS IAM needs the region of the database (us-east-1, …)");
                }
                if matches!(self.tls.mode, TlsMode::Disable | TlsMode::Prefer) {
                    return refuse(
                        "AWS RDS IAM sends its token over TLS only: choose require or a \
                         verifying SSL mode",
                    );
                }
            }
            PasswordSource::OnePassword { reference } => {
                if !reference.trim().starts_with("op://") {
                    return refuse(
                        "a 1Password reference starts with op:// (op://vault/item/field)",
                    );
                }
            }
        }
        Ok(())
    }
}

/// A secret string. Its `Debug` output never shows the value, so it cannot
/// leak through logs or panics.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret itself. Call only where it is handed to a driver.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the secret is the empty string.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// The secrets of one connection, held in memory only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Secrets {
    /// Database password.
    pub password: Option<Secret>,
    /// Password of the SSH user.
    pub ssh_password: Option<Secret>,
    /// Passphrase of the SSH private key file.
    pub ssh_key_passphrase: Option<Secret>,
}

/// What a driver needs to open one session: where the server is, how to
/// reach it, who to be, and which database.
#[derive(Debug, Clone)]
pub struct ConnectParams {
    /// Engine to speak.
    pub engine: Engine,
    /// Server host. Also the name TLS certificates are checked against.
    pub host: String,
    /// Server port.
    pub port: u16,
    /// SSH tunnel to go through. `host` and `port` are then resolved by the
    /// SSH server, and the driver must not dial them directly.
    pub tunnel: Option<std::sync::Arc<crate::ssh::Tunnel>>,
    /// Database user.
    pub user: String,
    /// Database password.
    pub password: Option<Secret>,
    /// Database to open; the engine's default when `None`.
    pub database: Option<String>,
    /// TLS settings.
    pub tls: TlsConfig,
    /// Open the session read-only on the server.
    pub read_only: bool,
    /// Extra driver options.
    pub options: BTreeMap<String, String>,
}

/// Whether an option's name says its value is a secret. Options are stored
/// and exported in plain text, so such an option is never accepted:
/// secrets go through [`Secrets`] to the OS secret store.
///
/// A name with a secret's word in it is one (`sslpassword`, `my.api_token`,
/// `password_prod`), unless its last part says the value describes the
/// secret instead of holding it: `password_encryption` (a method),
/// `token_lifetime` (a duration), `validate_password.length`. An ending
/// that is not known keeps the name refused.
pub fn is_secret_option(key: &str) -> bool {
    /// What a setting about a secret, not a secret, ends with.
    const ABOUT: [&str; 12] = [
        "encryption",
        "lifetime",
        "history",
        "interval",
        "length",
        "policy",
        "count",
        "rounds",
        "current",
        "timeout",
        "attempts",
        "time",
    ];
    let key = key.to_ascii_lowercase();
    let mut parts = key.rsplit(['_', '.', '-']);
    let last = parts.next().unwrap_or_default();
    let secret = |part: &str| {
        part == "pwd"
            || ["password", "passwd", "passphrase", "secret", "token"]
                .iter()
                .any(|word| part.contains(word))
    };
    secret(last) || (!ABOUT.contains(&last) && parts.any(secret))
}

/// Why a bootstrap statement is refused, if it is. It is stored and
/// exported in plain text, so it may not name a secret (`ALTER ROLE …
/// PASSWORD`, `SET my.api_token = …`, MySQL's `IDENTIFIED BY`). It runs at
/// every connect without being shown — also when the connection came from
/// an imported file — so it may only set session state.
///
/// The check accepts a small grammar rather than refusing what it knows to
/// be dangerous, because the server and this code do not read SQL the same
/// way (MySQL runs what is inside `/*! … */`, `sql_mode` changes what a
/// quote means, PostgreSQL decodes `U&"…"`):
///
/// ```text
/// SET [SESSION | LOCAL] name (= | TO) value [, value]…
/// SET TIME ZONE value | SET NAMES value [COLLATE value] | SET CHARACTER SET value
/// SET @variable = value                       (MySQL user variable)
/// RESET name
/// ```
///
/// with no comment anywhere, a bare ASCII `name`, and each value a number,
/// a bare word, a `'…'` string without quote or backslash inside, or on
/// PostgreSQL a plain `"…"` name. Names that reach beyond the session,
/// change who it runs as, or touch the transaction and its read-only state
/// are refused.
pub fn bootstrap_refusal(sql: &str, engine: Engine) -> Option<&'static str> {
    use crate::sql::{LexRules, TokenKind, lex};
    const NOT_A_SETTING: &str =
        "is not a SET or RESET of a session setting: bootstrap statements only set up the session";
    const NOT_PLAIN: &str = "is not written plainly: no comment, a bare setting name, and values that \
         are numbers, words or simple '…' strings";
    const BEYOND_THE_SESSION: &str =
        "would reach beyond the session (GLOBAL, PERSIST) or change who it runs as";
    const READ_ONLY: &str =
        "would change the transaction or its read-only state, which the connection decides";

    let secret = sql
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .any(|word| is_secret_option(word) || word.eq_ignore_ascii_case("identified"));
    if secret {
        return Some(
            "looks like it holds a secret: bootstrap statements are stored and exported in plain text",
        );
    }
    let postgres = engine == Engine::Postgres;
    let rules = if postgres {
        LexRules::POSTGRES
    } else {
        LexRules::MYSQL
    };
    let all = lex(sql, rules);
    if all
        .iter()
        .any(|token| matches!(token.kind, TokenKind::LineComment | TokenKind::BlockComment))
    {
        return Some(NOT_PLAIN);
    }
    let mut tokens: Vec<(TokenKind, &str)> = all
        .iter()
        .filter(|token| token.kind != TokenKind::Whitespace)
        .map(|token| (token.kind, &sql[token.range.clone()]))
        .collect();
    // Nothing to run (a blank line of the form).
    if tokens.is_empty() {
        return None;
    }
    if tokens
        .last()
        .is_some_and(|(kind, _)| *kind == TokenKind::Semicolon)
    {
        tokens.pop();
    }
    let bare = |text: &str| {
        !text.is_empty()
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    };
    let is_word = |index: usize, expected: &str| {
        tokens.get(index).is_some_and(|(kind, text)| {
            *kind == TokenKind::Word && text.eq_ignore_ascii_case(expected)
        })
    };
    // A setting name, checked whole and by its last dotted part.
    let name_refusal = |text: &str| -> Option<&'static str> {
        if !bare(text) {
            return Some(NOT_PLAIN);
        }
        let name = text.to_ascii_lowercase();
        let last = name.rsplit('.').next().unwrap_or(&name);
        let any = |list: &[&str]| name.split('.').any(|part| list.contains(&part));
        if any(&["global", "persist", "persist_only"])
            || [
                "role",
                "session_authorization",
                "authorization",
                "session_replication_role",
                "sql_log_bin",
                "password",
            ]
            .contains(&last)
        {
            return Some(BEYOND_THE_SESSION);
        }
        [
            "transaction",
            "characteristics",
            "constraints",
            "default_transaction_read_only",
            "transaction_read_only",
            "tx_read_only",
            "read_only",
            "super_read_only",
            "autocommit",
            "all",
        ]
        .contains(&last)
        .then_some(READ_ONLY)
    };
    let plain_value = |(kind, text): &(TokenKind, &str)| match kind {
        TokenKind::Number => true,
        TokenKind::Word => bare(text),
        TokenKind::String => {
            text.len() >= 2
                && text.starts_with('\'')
                && text.ends_with('\'')
                && !text[1..text.len() - 1].contains(['\'', '\\'])
        }
        // A quoted name as a value (`SET search_path TO "My Schema"`), on
        // PostgreSQL only, and not the `U&"…"` form, which is decoded.
        TokenKind::QuotedIdent => {
            postgres
                && text.len() >= 2
                && text.starts_with('"')
                && text.ends_with('"')
                && !text[1..text.len() - 1].contains(['"', '\\'])
        }
        _ => false,
    };
    // `value [, value]…` up to the end, each optionally signed.
    let values = |from: usize| -> bool {
        let mut index = from;
        loop {
            if tokens
                .get(index)
                .is_some_and(|(kind, text)| *kind == TokenKind::Punct && matches!(*text, "-" | "+"))
            {
                index += 1;
            }
            if !tokens.get(index).is_some_and(plain_value) {
                return false;
            }
            index += 1;
            match tokens.get(index) {
                None => return true,
                Some((TokenKind::Punct, ",")) => index += 1,
                Some(_) => return false,
            }
        }
    };
    let plain = |ok: bool| (!ok).then_some(NOT_PLAIN);

    if is_word(0, "reset") {
        return match tokens.as_slice() {
            [_, (TokenKind::Word, name)] => name_refusal(name),
            _ => Some(NOT_PLAIN),
        };
    }
    if !is_word(0, "set") {
        return Some(NOT_A_SETTING);
    }
    let mut at = 1;
    if is_word(at, "session") || is_word(at, "local") {
        at += 1;
    }
    // The forms without an equals sign.
    if is_word(at, "time") && is_word(at + 1, "zone") {
        return plain(values(at + 2) && tokens.len() == at + 3);
    }
    if is_word(at, "names") {
        let collated = tokens.len() == at + 4 && is_word(at + 2, "collate");
        return plain(
            tokens.get(at + 1).is_some_and(plain_value)
                && (tokens.len() == at + 2
                    || (collated && tokens.get(at + 3).is_some_and(plain_value))),
        );
    }
    if is_word(at, "character") && is_word(at + 1, "set") {
        return plain(tokens.len() == at + 3 && tokens.get(at + 2).is_some_and(plain_value));
    }
    // A MySQL user variable: `@name`, never the `@@scope.name` form.
    let user_variable = !postgres
        && matches!(tokens.get(at), Some((TokenKind::Punct, "@")))
        && matches!(tokens.get(at + 1), Some((TokenKind::Word, name)) if bare(name));
    if user_variable {
        at += 2;
    } else {
        // `name` or `prefix.name` (the lexer gives the dot on its own).
        let Some((TokenKind::Word, first)) = tokens.get(at) else {
            return Some(NOT_PLAIN);
        };
        let mut name = first.to_string();
        at += 1;
        while let (Some((TokenKind::Punct, ".")), Some((TokenKind::Word, part))) =
            (tokens.get(at), tokens.get(at + 1))
        {
            name.push('.');
            name.push_str(part);
            at += 2;
        }
        if let Some(reason) = name_refusal(&name) {
            return Some(reason);
        }
    }
    let assigns = matches!(tokens.get(at), Some((TokenKind::Punct, "=")))
        || (!user_variable && is_word(at, "to"));
    plain(assigns && values(at + 1))
}

/// The file written by "export connections" (J15, DBeaver #22579):
/// connections, the looks of their folders, environments and saved tasks,
/// settings only. Never a password, a passphrase or anything else of the
/// secret store, never a folder of this computer (local client, custom
/// folder picture), never a password command's confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConnectionsFile {
    /// Format marker, so another JSON file is not mistaken for this one.
    pub kerno_connections: u32,
    /// The connections.
    pub connections: Vec<PortableConnection>,
    /// The looks of their folders, by path: colour and glyph, never a
    /// custom image (a file of this computer). Older files have none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub folders: BTreeMap<String, FolderLook>,
    /// The user's environments and the built-ins they edited (version 2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environments: Vec<EnvironmentDef>,
    /// Saved tasks (version 2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<PortableTask>,
}

/// A connection in a [`ConnectionsFile`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortableConnection {
    /// Its id where it was exported (version 2): an import that finds the
    /// id replaces that connection, and its keychain entries are found by
    /// it. Version 1 files have none; neither has an id that is not a UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(flatten)]
    pub config: ConnectionConfig,
}

/// A saved task in a [`ConnectionsFile`]: its settings hold no secret
/// (the authentication override keeps its user only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortableTask {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The id of the connection it runs on.
    pub connection_id: String,
    /// That connection's engine, which decides what the tool's extra
    /// arguments may hold.
    pub engine: Engine,
    pub settings: crate::tools::TaskSettings,
}

/// Version written in [`ConnectionsFile::kerno_connections`]: 2 added
/// connection ids, environments and tasks. Version 1 files still import.
const CONNECTIONS_FILE_VERSION: u32 = 2;

/// Serializes connections for export, without ids. Passwords and
/// passphrases are not part of [`ConnectionConfig`], so the result cannot
/// contain any.
pub fn export_connections(connections: &[ConnectionConfig]) -> Result<String> {
    export_connections_file(ConnectionsFile {
        connections: connections
            .iter()
            .map(|config| PortableConnection {
                id: None,
                config: config.clone(),
            })
            .collect(),
        ..ConnectionsFile::default()
    })
}

/// Writes a connections file. What is this computer's only is dropped
/// (local client folders, folder pictures, password command
/// confirmations), as are the built-in environments left as shipped; a
/// connection named like a secret option, or a task whose extra arguments
/// hold a password, refuses the whole file.
pub fn export_connections_file(file: ConnectionsFile) -> Result<String> {
    // The one free-form part of a connection is its options: refuse to
    // write a file if any of them is named like a secret.
    for connection in &file.connections {
        connection.config.validate().map_err(|error| {
            Error::Config(format!(
                "connection \"{}\": {error}",
                connection.config.name
            ))
        })?;
    }
    check_tasks(&file.tasks)?;
    let builtins = environment::builtin_environments();
    let file = ConnectionsFile {
        kerno_connections: CONNECTIONS_FILE_VERSION,
        connections: file
            .connections
            .into_iter()
            .map(|mut connection| {
                // The client folder is this computer's choice: never exported.
                connection.config.client_dir = None;
                // Confirmation is this computer's: an importer confirms again.
                connection.config.password_command_unconfirmed = false;
                connection
            })
            .collect(),
        folders: shared_looks(&file.folders),
        environments: file
            .environments
            .into_iter()
            .filter(|def| !builtins.contains(def))
            .collect(),
        tasks: file.tasks,
    };
    serde_json::to_string_pretty(&file)
        .map_err(|error| Error::Config(format!("could not encode the connections: {error}")))
}

/// A task whose extra arguments the tool refuses (a password among them)
/// refuses the file.
fn check_tasks(tasks: &[PortableTask]) -> Result<()> {
    for task in tasks {
        task.settings
            .check(task.engine)
            .map_err(|error| Error::Config(format!("task \"{}\": {error}", task.name)))?;
    }
    Ok(())
}

/// Reads an exported connections file. Every connection is validated; one
/// bad entry rejects the file, naming it, rather than importing part of it.
pub fn import_connections(json: &str) -> Result<Vec<ConnectionConfig>> {
    Ok(import_connections_file(json)?
        .connections
        .into_iter()
        .map(|connection| connection.config)
        .collect())
}

/// [`import_connections`] with everything else the file carries: ids,
/// folder looks, environments and tasks. Version 1 and 2 files.
pub fn import_connections_file(json: &str) -> Result<ConnectionsFile> {
    let file: ConnectionsFile = serde_json::from_str(json)
        .map_err(|error| Error::Config(format!("this is not a Kerno connections file: {error}")))?;
    if file.kerno_connections > CONNECTIONS_FILE_VERSION {
        return Err(Error::Config(format!(
            "this file was written by a newer version of Kerno (format {}, this version reads {})",
            file.kerno_connections, CONNECTIONS_FILE_VERSION
        )));
    }
    for (index, connection) in file.connections.iter().enumerate() {
        connection.config.validate().map_err(|error| {
            Error::Config(format!(
                "connection {} (\"{}\"): {error}",
                index + 1,
                connection.config.name
            ))
        })?;
    }
    for (index, def) in file.environments.iter().enumerate() {
        if def.id.trim().is_empty() || def.name.trim().is_empty() {
            return Err(Error::Config(format!(
                "environment {} needs an id and a name",
                index + 1
            )));
        }
    }
    check_tasks(&file.tasks)?;
    let mut ids = std::collections::HashSet::new();
    let connections = file
        .connections
        .into_iter()
        .map(|mut connection| {
            // Only a UUID can name a connection, once.
            connection.id = connection
                .id
                .filter(|id| uuid::Uuid::parse_str(id).is_ok() && ids.insert(id.clone()));
            // A file could otherwise point backups at programs someone
            // else chose: the client folder is this computer's own choice.
            connection.config.client_dir = None;
            // A password command from a file runs only once the user has
            // seen it and confirmed it, whatever the file says.
            connection.config.password_command_unconfirmed =
                !connection.config.password_source.is_stored();
            connection
        })
        .collect();
    let mut seen = std::collections::HashSet::new();
    Ok(ConnectionsFile {
        connections,
        folders: shared_looks(&file.folders),
        environments: file
            .environments
            .into_iter()
            .filter(|def| seen.insert(def.id.clone()))
            .collect(),
        ..file
    })
}

/// What an import does with one item of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportAction {
    /// Nothing has its id (its name, for a task): it is added.
    Add,
    /// It replaces the item with the same id (name).
    Replace,
    /// The same item is there already: nothing changes.
    Unchanged,
}

/// What importing a file changes, item by item, in the order of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPlan {
    pub connections: Vec<ImportAction>,
    pub environments: Vec<ImportAction>,
    pub tasks: Vec<ImportAction>,
}

/// Compares a file with what this computer has.
pub fn plan_import(
    file: &ConnectionsFile,
    connections: &[crate::storage::SavedConnection],
    environments: &[EnvironmentDef],
    tasks: &[crate::storage::SavedTask],
) -> ImportPlan {
    fn action<T: PartialEq>(existing: Option<T>, imported: T) -> ImportAction {
        match existing {
            None => ImportAction::Add,
            Some(existing) if existing == imported => ImportAction::Unchanged,
            Some(_) => ImportAction::Replace,
        }
    }
    ImportPlan {
        connections: file
            .connections
            .iter()
            .map(|imported| {
                let existing = imported.id.as_deref().and_then(|id| {
                    connections
                        .iter()
                        .find(|saved| saved.id == id)
                        .map(|saved| &saved.config)
                });
                // What is this computer's alone is not a change.
                let existing = existing.map(|config| ConnectionConfig {
                    client_dir: None,
                    password_command_unconfirmed: imported.config.password_command_unconfirmed,
                    ..config.clone()
                });
                action(existing.as_ref(), &imported.config)
            })
            .collect(),
        environments: file
            .environments
            .iter()
            .map(|imported| {
                action(
                    environments.iter().find(|def| def.id == imported.id),
                    imported,
                )
            })
            .collect(),
        tasks: file
            .tasks
            .iter()
            .map(|imported| {
                let existing = tasks.iter().find(|task| task.name == imported.name);
                action(
                    existing.map(|task| (&task.description, &task.connection_id, &task.settings)),
                    (
                        &imported.description,
                        &imported.connection_id,
                        &imported.settings,
                    ),
                )
            })
            .collect(),
    }
}

/// Whether `imported`, replacing `existing`, would send its password
/// somewhere else: another engine, host, port, user or SSH tunnel. A
/// keychain entry is then not linked without asking (a file could reuse a
/// known id to collect a password).
pub fn reaches_another_server(existing: &ConnectionConfig, imported: &ConnectionConfig) -> bool {
    (
        existing.engine,
        &existing.host,
        existing.port,
        &existing.user,
        &existing.ssh,
    ) != (
        imported.engine,
        &imported.host,
        imported.port,
        &imported.user,
        &imported.ssh,
    )
}

/// The looks of folders as a connections file carries them: paths tidied,
/// no image (a file name on one computer only, never taken from a file),
/// and no look that sets nothing.
fn shared_looks(folders: &BTreeMap<String, FolderLook>) -> BTreeMap<String, FolderLook> {
    folders
        .iter()
        .filter_map(|(path, look)| {
            let look = FolderLook {
                image: None,
                ..look.clone()
            };
            Some((folder_path(path)?, look)).filter(|(_, look)| !look.is_empty())
        })
        .collect()
}

// Folders ---------------------------------------------------------------------
//
// A connection's folder is its `group`: folder names joined by `/`, from the
// top ("Work/Clients"). A folder that holds no connection is kept by the app
// in its settings.

/// A folder path as typed or stored, tidied: each name trimmed, empty names
/// dropped. `None` is the top level.
pub fn folder_path(text: &str) -> Option<String> {
    let names: Vec<&str> = text
        .split('/')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    (!names.is_empty()).then(|| names.join("/"))
}

/// Checks a name typed for one folder; returns it trimmed.
pub fn folder_name(text: &str) -> std::result::Result<String, String> {
    let name = text.trim();
    if name.is_empty() {
        return Err("A folder needs a name.".into());
    }
    if name.contains('/') {
        return Err("A folder name cannot contain “/”.".into());
    }
    Ok(name.to_string())
}

/// The folder holding `path`; `None` when it is at the top level.
pub fn parent_folder(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

/// The last name of a folder path.
pub fn last_folder_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Whether `path` is `folder` or lies inside it (`Work` does not hold
/// `Workshop`).
pub fn in_folder(path: &str, folder: &str) -> bool {
    path.strip_prefix(folder)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// `path`, which lies in `from` (see [`in_folder`]), with `from` replaced
/// by `to`: what a folder's content becomes when the folder is renamed or
/// removed. `None` is the top level.
pub fn rebase_folder(path: &str, from: &str, to: Option<&str>) -> Option<String> {
    let rest = path.strip_prefix(from).unwrap_or_default();
    folder_path(&format!("{}/{rest}", to.unwrap_or_default()))
}

/// How a folder is drawn in the sidebar: a colour of the connection palette,
/// a built-in glyph, or a picture the user chose. The app keeps the looks in
/// its settings, by folder path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderLook {
    /// A key of the connection tag colours (`blue`, `red`…).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// A built-in glyph (`briefcase`, `server`…); the folder glyph when
    /// `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub glyph: Option<String>,
    /// A picture, by the name Kerno gave its copy in the folder icons
    /// directory; drawn instead of the glyph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

impl FolderLook {
    /// Whether the look sets nothing: the plain folder.
    pub fn is_empty(&self) -> bool {
        self.color.is_none() && self.glyph.is_none() && self.image.is_none()
    }
}

/// Every folder of `paths`, their parents included, sorted, once each.
pub fn all_folders<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut folders = std::collections::BTreeSet::new();
    for path in paths {
        let mut current = folder_path(path);
        while let Some(folder) = current {
            current = parent_folder(&folder).map(str::to_string);
            folders.insert(folder);
        }
    }
    folders.into_iter().collect()
}

/// The connection as a URL (`postgresql://user@host:port/database`), for
/// the clipboard. It never holds a password: [`ConnectionConfig`] has none.
pub fn connection_url(config: &ConnectionConfig) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
    const PART: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    let scheme = match config.engine {
        Engine::Postgres => "postgresql",
        Engine::MySql => "mysql",
        Engine::MariaDb => "mariadb",
    };
    let host = if config.host.contains(':') {
        format!("[{}]", config.host)
    } else {
        config.host.clone()
    };
    let mut url = format!(
        "{scheme}://{}@{host}:{}/{}",
        utf8_percent_encode(&config.user, PART),
        config.port,
        utf8_percent_encode(config.database.as_deref().unwrap_or_default(), PART),
    );
    if config.tls.mode != TlsMode::default() {
        url.push_str("?sslmode=");
        url.push_str(config.tls.mode.as_str());
    }
    url
}

/// The fields a pasted connection URL fills in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    /// Engine named by the URL scheme.
    pub engine: Engine,
    /// Host, when present.
    pub host: Option<String>,
    /// Port, when present.
    pub port: Option<u16>,
    /// User, when present.
    pub user: Option<String>,
    /// Password, when present.
    pub password: Option<Secret>,
    /// Database, when present.
    pub database: Option<String>,
    /// TLS mode from `sslmode` / `ssl-mode`, when present.
    pub tls_mode: Option<TlsMode>,
    /// Remaining query parameters, kept as driver options.
    pub options: BTreeMap<String, String>,
}

/// Parses `postgres://`, `postgresql://`, `mysql://` and `mariadb://` URLs.
pub fn parse_connection_url(input: &str) -> Result<ParsedUrl> {
    let bad = |why: &str| Error::Config(format!("not a connection URL: {why}"));
    let url = url::Url::parse(input.trim()).map_err(|e| bad(&e.to_string()))?;
    let engine = Engine::from_url_scheme(url.scheme()).ok_or_else(|| {
        bad(&format!(
            "unknown scheme \"{}\" (expected postgres, postgresql, mysql or mariadb)",
            url.scheme()
        ))
    })?;
    let decode = |text: &str| -> Result<String> {
        percent_encoding::percent_decode_str(text)
            .decode_utf8()
            .map(|s| s.into_owned())
            .map_err(|_| bad("it contains bytes that are not UTF-8"))
    };
    let non_empty = |text: String| if text.is_empty() { None } else { Some(text) };

    // `url` keeps IPv6 hosts in brackets; drivers want the bare address.
    let host = url
        .host_str()
        .map(|h| h.trim_start_matches('[').trim_end_matches(']').to_string())
        .and_then(non_empty);
    let user = non_empty(decode(url.username())?);
    let password = url.password().map(decode).transpose()?.map(Secret::new);
    let database = non_empty(decode(url.path().trim_start_matches('/'))?);

    let mut user = user;
    let mut password = password;
    let mut tls_mode = None;
    let mut options = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        match key.to_ascii_lowercase().as_str() {
            "sslmode" | "ssl-mode" | "ssl_mode" => {
                tls_mode = Some(
                    TlsMode::parse(&value)
                        .ok_or_else(|| bad(&format!("unknown TLS mode \"{value}\"")))?,
                );
            }
            // libpq-style URLs may carry the credentials as parameters.
            // They are credentials, not options: options are stored in
            // plain text and exported.
            "user" => user = non_empty(value.into_owned()),
            "password" => password = Some(Secret::new(value.into_owned())),
            // Any other secret (a key passphrase, a token) has no field to
            // go to and is dropped rather than kept as an option.
            _ if is_secret_option(&key) => {}
            _ => {
                options.insert(key.into_owned(), value.into_owned());
            }
        }
    }

    Ok(ParsedUrl {
        engine,
        host,
        port: url.port(),
        user,
        password,
        database,
        tls_mode,
        options,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_postgres_url() {
        let parsed =
            parse_connection_url("postgresql://ada:s3cr%40t%2F@db.example.com:6543/shop?sslmode=verify-full&application_name=kerno")
                .unwrap();
        assert_eq!(parsed.engine, Engine::Postgres);
        assert_eq!(parsed.host.as_deref(), Some("db.example.com"));
        assert_eq!(parsed.port, Some(6543));
        assert_eq!(parsed.user.as_deref(), Some("ada"));
        assert_eq!(
            parsed.password.as_ref().map(Secret::expose),
            Some("s3cr@t/")
        );
        assert_eq!(parsed.database.as_deref(), Some("shop"));
        assert_eq!(parsed.tls_mode, Some(TlsMode::VerifyFull));
        assert_eq!(
            parsed.options.get("application_name").map(String::as_str),
            Some("kerno")
        );
    }

    #[test]
    fn parses_minimal_and_mysql_urls() {
        let parsed = parse_connection_url("  postgres://localhost  ").unwrap();
        assert_eq!(parsed.engine, Engine::Postgres);
        assert_eq!(parsed.host.as_deref(), Some("localhost"));
        assert_eq!(parsed.port, None);
        assert_eq!(parsed.user, None);
        assert_eq!(parsed.password, None);
        assert_eq!(parsed.database, None);
        assert_eq!(parsed.tls_mode, None);

        let parsed =
            parse_connection_url("mysql://root@127.0.0.1:3307/app?ssl-mode=REQUIRED").unwrap();
        assert_eq!(parsed.engine, Engine::MySql);
        assert_eq!(parsed.port, Some(3307));
        assert_eq!(parsed.tls_mode, Some(TlsMode::Require));

        let parsed = parse_connection_url("mariadb://u:@h/d?ssl_mode=VERIFY_IDENTITY").unwrap();
        assert_eq!(parsed.engine, Engine::MariaDb);
        // An empty password in a URL is the same as no password.
        assert_eq!(parsed.password, None);
        assert_eq!(parsed.tls_mode, Some(TlsMode::VerifyFull));
    }

    #[test]
    fn parses_ipv6_hosts_without_brackets() {
        let parsed = parse_connection_url("postgres://u@[::1]:5433/d").unwrap();
        assert_eq!(parsed.host.as_deref(), Some("::1"));
        assert_eq!(parsed.port, Some(5433));
    }

    #[test]
    fn rejects_other_schemes_and_bad_tls_modes() {
        for input in ["http://example.com", "sqlite:///tmp/x.db", "not a url", ""] {
            let error = parse_connection_url(input).unwrap_err();
            assert!(matches!(error, Error::Config(_)), "{input}: {error}");
        }
        let error = parse_connection_url("postgres://h/d?sslmode=sometimes").unwrap_err();
        assert!(error.to_string().contains("sometimes"), "{error}");
    }

    #[test]
    fn secrets_never_appear_in_debug_output() {
        let secrets = Secrets {
            password: Some(Secret::new("hunter2-db")),
            ssh_password: Some(Secret::new("hunter2-ssh")),
            ssh_key_passphrase: Some(Secret::new("hunter2-key")),
        };
        let parsed = parse_connection_url("postgres://u:hunter2-url@h/d").unwrap();
        let debug = format!("{secrets:?} {parsed:?}");
        assert!(!debug.contains("hunter2"), "{debug}");
    }

    #[test]
    fn config_serialization_has_no_secret_fields_and_round_trips() {
        let mut config = ConnectionConfig::new(Engine::MariaDb);
        config.name = "Shop".into();
        config.tls.mode = TlsMode::VerifyCa;
        config.ssh = Some(SshConfig {
            host: "bastion".into(),
            port: 2222,
            user: "ops".into(),
            auth: SshAuth::KeyFile {
                path: "/home/ops/.ssh/id_ed25519".into(),
            },
            keepalive_secs: 15,
            client: SshClient::Builtin,
            jump: None,
        });
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(
            serde_json::from_str::<ConnectionConfig>(&json).unwrap(),
            config
        );

        // The only field whose name mentions a secret is the boolean that
        // says whether to keep the password in the OS secret store.
        fn keys(value: &serde_json::Value, out: &mut Vec<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    for (key, value) in map {
                        out.push(key.clone());
                        keys(value, out);
                    }
                }
                serde_json::Value::Array(items) => items.iter().for_each(|v| keys(v, out)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        keys(&serde_json::from_str(&json).unwrap(), &mut all);
        let secretish: Vec<_> = all
            .iter()
            .filter(|key| {
                ["password", "passphrase", "secret"]
                    .iter()
                    .any(|s| key.contains(s))
            })
            .collect();
        assert_eq!(secretish, ["save_password"], "{json}");
        assert_eq!(
            config_value(&json, "save_password"),
            serde_json::Value::Bool(true)
        );
    }

    fn config_value(json: &str, key: &str) -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(json).unwrap()[key].clone()
    }

    #[test]
    fn config_loads_with_only_the_required_fields() {
        let config: ConnectionConfig = serde_json::from_str(
            r#"{"name":"x","engine":"postgres","host":"h","port":5432,"user":"u"}"#,
        )
        .unwrap();
        assert_eq!(config.tls.mode, TlsMode::Prefer);
        assert!(config.save_password);
        assert!(config.ssh.is_none());
        // Stores and exports written before the connection settings of G13.
        assert_eq!(config.auto_commit, None);
        assert_eq!(config.default_schema, None);
        assert!(config.init_sql.is_empty() && !config.ignore_init_errors);
        assert!(!config.starts_in_manual_commit(&environment::builtin_environments()));
    }

    #[test]
    fn the_commit_mode_follows_the_environment_unless_chosen() {
        let mut environments = environment::builtin_environments();
        environments.push(environment::EnvironmentDef {
            id: "qa".into(),
            name: "QA".into(),
            color_light: None,
            color_dark: None,
            tint: environment::Tint::None,
            rules: environment::EnvironmentRules {
                auto_commit: false,
                ..Default::default()
            },
        });
        let mut config = ConnectionConfig::new(Engine::Postgres);
        for (environment, manual) in [
            ("local", false),
            ("staging", false),
            ("production", true),
            ("qa", true),
            // An environment that no longer exists: local's rules.
            ("gone", false),
        ] {
            config.environment = environment.into();
            config.auto_commit = None;
            assert_eq!(
                config.starts_in_manual_commit(&environments),
                manual,
                "{environment}"
            );
            config.auto_commit = Some(true);
            assert!(
                !config.starts_in_manual_commit(&environments),
                "{environment}"
            );
            config.auto_commit = Some(false);
            assert!(
                config.starts_in_manual_commit(&environments),
                "{environment}"
            );
        }
    }

    /// Stores and exports written when the environment was a fixed list
    /// hold its lowercase name, which is the built-in's id.
    #[test]
    fn an_old_connection_keeps_its_environment_by_id() {
        let mut stored = serde_json::to_value(ConnectionConfig::new(Engine::MySql)).unwrap();
        stored["environment"] = "production".into();
        let config: ConnectionConfig = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(config.environment, environment::PRODUCTION);
        assert!(config.starts_in_manual_commit(&environment::builtin_environments()));
        stored.as_object_mut().unwrap().remove("environment");
        let config: ConnectionConfig = serde_json::from_value(stored).unwrap();
        assert_eq!(config.environment, environment::LOCAL);
    }

    #[test]
    fn a_bootstrap_statement_with_a_secret_is_refused_without_echoing_it() {
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.name = "Boot".into();
        config.init_sql = vec![
            "SET search_path TO shop, public".into(),
            "SET time_zone = '+00:00'".into(),
        ];
        config.validate().unwrap();
        for leaky in [
            "ALTER ROLE app PASSWORD 'hunter2'",
            "SET my.api_token = 'hunter2'",
            "CREATE USER x IDENTIFIED BY 'hunter2'",
            "SET my.client_secret = 'hunter2'",
            "SET password = 'hunter2'",
        ] {
            config.init_sql.push(leaky.into());
            let error = config.validate().unwrap_err().to_string();
            assert!(error.contains("bootstrap statement 3"), "{error}");
            assert!(!error.contains("hunter2"), "{error}");
            let error = export_connections(std::slice::from_ref(&config))
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("Boot") && !error.contains("hunter2"),
                "{error}"
            );
            config.init_sql.pop();
        }
        // Only a SET or RESET of a session setting, written plainly.
        for (refused, why) in [
            ("INSERT INTO audit VALUES (1)", "is not a SET or RESET"),
            ("DROP TABLE t", "is not a SET or RESET"),
            ("BEGIN", "is not a SET or RESET"),
            ("SELECT audit.note_login()", "is not a SET or RESET"),
            ("CALL sp_on_login()", "is not a SET or RESET"),
            ("DO $$ BEGIN PERFORM 1; END $$", "is not a SET or RESET"),
            ("SET a = 1; DROP TABLE t", "not written plainly"),
            ("SET GLOBAL max_connections = 10", "beyond the session"),
            ("SET PERSIST max_connections = 10", "beyond the session"),
            ("SET global.max_connections = 10", "beyond the session"),
            ("SET ROLE admin", "beyond the session"),
            ("SET SESSION AUTHORIZATION postgres", "beyond the session"),
            (
                "SET session_replication_role = replica",
                "beyond the session",
            ),
            ("SET my.sql_log_bin = 0", "beyond the session"),
            ("SET session.sql_log_bin = 0", "not written plainly"),
            ("SET default_transaction_read_only = off", "read-only state"),
            ("SET transaction_read_only = off", "read-only state"),
            (
                "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
                "read-only state",
            ),
            ("SET SESSION TRANSACTION READ WRITE", "read-only state"),
            ("SET my.tx_read_only = 0", "read-only state"),
            ("SET session.tx_read_only = 0", "not written plainly"),
            ("SET autocommit = 0", "read-only state"),
            ("RESET ALL", "read-only state"),
            ("RESET default_transaction_read_only", "read-only state"),
            // The server and this check must read the statement the same way.
            ("SET @@session.tx_read_only = 0", "not written plainly"),
            ("SET @@global.max_connections = 10", "not written plainly"),
            ("SET @x = (SELECT f())", "not written plainly"),
            (
                "SET sql_mode = CONCAT(@@sql_mode, ',ANSI')",
                "not written plainly",
            ),
            (
                "SET a = 1 /*! , GLOBAL max_connections = 10 */",
                "not written plainly",
            ),
            (
                "SET /*!50000 GLOBAL */ max_connections = 10",
                "not written plainly",
            ),
            ("SET a = 1 -- note", "not written plainly"),
            ("SET a = 'it''s'", "not written plainly"),
            ("SET a = 'back\\slash'", "not written plainly"),
            ("SET a = E'x'", "not written plainly"),
            ("SET a = $$x$$", "not written plainly"),
            (
                "SET \"default_transaction_read_only\" = off",
                "not written plainly",
            ),
            (
                "SET U&\"default\\005ftransaction_read_only\" = off",
                "not written plainly",
            ),
            ("SET `autocommit` = 0", "not written plainly"),
            ("SET a = 1, b = 2", "not written plainly"),
            ("SET a", "not written plainly"),
            ("RESET a, b", "not written plainly"),
        ] {
            config.init_sql.push(refused.into());
            let error = config.validate().unwrap_err().to_string();
            assert!(
                error.contains("bootstrap statement 3") && error.contains(why),
                "{refused}: {error}"
            );
            config.init_sql.pop();
        }
        // What bootstrap statements are for.
        for engine in [Engine::Postgres, Engine::MySql] {
            for allowed in [
                "",
                "SET search_path TO shop, public",
                "SET time_zone = '+05:30'",
                "set statement_timeout = 30000;",
                "SET SESSION sql_mode = 'ANSI_QUOTES,STRICT_ALL_TABLES'",
                "SET default_transaction_isolation = 'repeatable read'",
                "SET NAMES utf8mb4",
                "SET NAMES utf8mb4 COLLATE utf8mb4_unicode_ci",
                "SET TIME ZONE 'Pacific/Auckland'",
                "SET lock_timeout = -1",
                "SET my.setting = on",
                // Named after a secret, holds none: the hashing method.
                "SET password_encryption = 'scram-sha-256'",
                "RESET statement_timeout",
            ] {
                assert_eq!(bootstrap_refusal(allowed, engine), None, "{allowed}");
            }
        }
        // A quoted name is a value on PostgreSQL only; a user variable exists on MySQL only.
        let quoted = "SET search_path TO \"My Schema\", public";
        assert_eq!(bootstrap_refusal(quoted, Engine::Postgres), None);
        assert!(bootstrap_refusal(quoted, Engine::MySql).is_some());
        assert!(bootstrap_refusal("SET a = \"x\"", Engine::MariaDb).is_some());
        assert_eq!(bootstrap_refusal("SET @boot = 'yes'", Engine::MySql), None);
        assert!(bootstrap_refusal("SET @boot = 'yes'", Engine::Postgres).is_some());
        let file = r#"{"kerno_connections": 1, "connections": [{"name":"x","engine":"postgres","host":"h","port":5432,"user":"u","init_sql":["SET a = 1","ALTER USER u PASSWORD 'hunter2'"]}]}"#;
        let error = import_connections(file).unwrap_err().to_string();
        assert!(
            error.contains("bootstrap statement 2") && !error.contains("hunter2"),
            "{error}"
        );
    }

    #[test]
    fn validation_names_what_is_missing() {
        let mut config = ConnectionConfig::new(Engine::Postgres);
        assert!(config.validate().is_ok());

        config.host = "  ".into();
        assert!(config.validate().unwrap_err().to_string().contains("host"));
        config.host = "h".into();

        config.tls.cert_file = Some("/c.pem".into());
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("key file")
        );
        config.tls.key_file = Some("/k.pem".into());
        assert!(config.validate().is_ok());

        config.ssh = Some(SshConfig {
            host: String::new(),
            port: 22,
            user: "u".into(),
            auth: SshAuth::default(),
            keepalive_secs: 30,
            client: SshClient::Builtin,
            jump: None,
        });
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("SSH host")
        );
    }

    /// J2 (DBeaver #5845, #17976): OpenSSH takes an alias with nothing else;
    /// a password needs the built-in client, a jump host OpenSSH.
    #[test]
    fn openssh_settings_are_checked_in_plain_words() {
        let mut config = ConnectionConfig::new(Engine::Postgres);
        let mut ssh = SshConfig {
            host: "prod-bastion".into(),
            port: 0,
            user: String::new(),
            auth: SshAuth::Agent { socket: None },
            keepalive_secs: 30,
            client: SshClient::OpenSsh,
            jump: Some("ops@jump1:2222,jump2".into()),
        };
        config.ssh = Some(ssh.clone());
        config.validate().unwrap();
        let error = |config: &ConnectionConfig| config.validate().unwrap_err().to_string();

        ssh.auth = SshAuth::Password;
        config.ssh = Some(ssh.clone());
        assert!(
            error(&config).contains("built-in client only"),
            "{}",
            error(&config)
        );

        ssh.auth = SshAuth::Agent { socket: None };
        ssh.client = SshClient::Builtin;
        config.ssh = Some(ssh.clone());
        assert!(error(&config).contains("SSH user"), "{}", error(&config));
        ssh.user = "ops".into();
        ssh.port = 22;
        config.ssh = Some(ssh.clone());
        assert!(error(&config).contains("jump host needs the OpenSSH client"));
        ssh.jump = None;
        config.ssh = Some(ssh.clone());
        config.validate().unwrap();

        // An imported file cannot slip a command into ProxyJump.
        ssh.client = SshClient::OpenSsh;
        for (host, jump) in [
            ("bastion", "j;touch /tmp/x"),
            ("bastion", "$(id)"),
            ("-oProxyCommand=id", ""),
            ("a b", ""),
        ] {
            ssh.host = host.into();
            ssh.jump = Some(jump.into());
            config.ssh = Some(ssh.clone());
            assert!(
                error(&config).contains("characters OpenSSH cannot take"),
                "{host} {jump}"
            );
        }

        // Stored before J2: the built-in client, no jump host.
        let stored: SshConfig =
            serde_json::from_str(r#"{"host":"b","port":22,"user":"u"}"#).unwrap();
        assert_eq!((stored.client, &stored.jump), (SshClient::Builtin, &None));
        let json = serde_json::to_string(&SshConfig {
            client: SshClient::OpenSsh,
            ..stored
        })
        .unwrap();
        assert!(json.contains(r#""client":"openssh""#), "{json}");
    }

    #[test]
    fn an_imported_connection_never_chooses_a_local_client() {
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.name = "Shop".into();
        config.client_dir = Some("/opt/homebrew/opt/libpq/bin".into());
        // Kept where the app stores it, on this computer...
        let stored = serde_json::to_string(&config).unwrap();
        assert!(stored.contains("client_dir"), "{stored}");
        // ...never in an exported file.
        let exported = export_connections(std::slice::from_ref(&config)).unwrap();
        assert!(!exported.contains("client_dir"), "{exported}");
        // A file that names one has it cleared.
        let hostile = r#"{"kerno_connections": 1, "connections": [{
            "name": "Shop", "engine": "postgres", "host": "db", "port": 5432,
            "user": "ada", "client_dir": "/tmp/evil"}]}"#;
        let imported = import_connections(hostile).unwrap();
        assert_eq!(imported[0].client_dir, None);
    }

    /// J3: a password source travels in a connections file (it holds no
    /// secret), but a command from a file is unconfirmed whatever the file
    /// says, and the confirmation is never exported.
    #[test]
    fn an_imported_password_command_waits_for_confirmation() {
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.name = "Shop".into();
        config.password_source = PasswordSource::Command {
            program: "vault".into(),
            args: vec!["read".into(), "${host}".into()],
            timeout_secs: 15,
        };
        // Stored as it is on this computer.
        let stored: ConnectionConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(stored, config);
        let exported = export_connections(std::slice::from_ref(&config)).unwrap();
        assert!(exported.contains("vault"), "{exported}");
        assert!(!exported.contains("unconfirmed"), "{exported}");
        let imported = import_connections(&exported).unwrap();
        assert_eq!(imported[0].password_source, config.password_source);
        assert!(imported[0].password_command_unconfirmed);
        // A file claiming it was confirmed is not believed.
        let hostile = r#"{"kerno_connections": 1, "connections": [{
            "name": "Shop", "engine": "postgres", "host": "db", "port": 5432, "user": "ada",
            "password_source": {"kind": "one-password", "reference": "op://Private/bank/pw"},
            "password_command_unconfirmed": false}]}"#;
        assert!(import_connections(hostile).unwrap()[0].password_command_unconfirmed);
        // A line break or a bidi override would show one command and run
        // another: refused, so such a file is not imported.
        for trick in ["read\nrm -rf ~", "a\u{202e}b", "\u{7}"] {
            let mut tricked = config.clone();
            tricked.password_source = PasswordSource::Command {
                program: "vault".into(),
                args: vec![trick.into()],
                timeout_secs: 10,
            };
            assert!(tricked.validate().is_err(), "{trick:?}");
            let file = serde_json::to_string(&ConnectionsFile {
                kerno_connections: 1,
                connections: vec![PortableConnection {
                    id: None,
                    config: tricked,
                }],
                ..ConnectionsFile::default()
            })
            .unwrap();
            assert!(import_connections(&file).is_err(), "{trick:?}");
        }
        // Stored passwords need nothing, and older files have no source.
        let plain = ConnectionConfig {
            name: "Plain".into(),
            ..ConnectionConfig::new(Engine::MySql)
        };
        let exported = export_connections(std::slice::from_ref(&plain)).unwrap();
        assert!(!exported.contains("password_source"), "{exported}");
        let imported = import_connections(&exported).unwrap();
        assert_eq!(imported[0].password_source, PasswordSource::Stored);
        assert!(!imported[0].password_command_unconfirmed);
    }

    #[test]
    fn connections_export_and_import_without_secrets() {
        let mut first = ConnectionConfig::new(Engine::Postgres);
        first.name = "Shop".into();
        first.environment = environment::PRODUCTION.into();
        first.color = Some("red".into());
        // A nested folder travels as it is stored.
        first.group = Some("Work/Clients".into());
        first.read_only = true;
        first
            .options
            .insert("application_name".into(), "kerno".into());
        first.ssh = Some(SshConfig {
            host: "bastion".into(),
            port: 22,
            user: "ops".into(),
            auth: SshAuth::Agent { socket: None },
            keepalive_secs: 30,
            client: SshClient::Builtin,
            jump: None,
        });
        first.auto_commit = Some(true);
        first.default_schema = Some("shop".into());
        first.init_sql = vec!["SET TIME ZONE 'UTC'".into(), "SET work_mem = '64MB'".into()];
        first.ignore_init_errors = true;
        let mut second = ConnectionConfig::new(Engine::MySql);
        second.name = "Billing".into();
        let exported = export_connections(&[first.clone(), second.clone()]).unwrap();
        assert!(exported.contains("\"kerno_connections\": 2"), "{exported}");
        for key in [
            "auto_commit",
            "default_schema",
            "init_sql",
            "ignore_init_errors",
        ] {
            assert!(
                exported.contains(&format!("\"{key}\"")),
                "{key}: {exported}"
            );
        }
        assert_eq!(import_connections(&exported).unwrap(), [first, second]);
        assert_eq!(
            import_connections(&export_connections(&[]).unwrap()).unwrap(),
            []
        );
    }

    /// A task of the tool wizards, for the connections file tests.
    fn backup_task(name: &str, connection_id: &str) -> PortableTask {
        PortableTask {
            name: name.into(),
            description: String::new(),
            connection_id: connection_id.into(),
            engine: Engine::Postgres,
            settings: crate::tools::TaskSettings::Backup(crate::tools::BackupOptions::new(
                Engine::Postgres,
                "shop",
            )),
        }
    }

    const ID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

    /// J15 (DBeaver #22579): every field that could hold something of this
    /// computer or a confirmation is filled, and none of it reaches the
    /// file. Passwords and passphrases are not part of a connection at all
    /// (the UI test checks the secret store's values end to end).
    #[test]
    fn nothing_secret_or_local_is_in_an_exported_file() {
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.name = "Shop".into();
        config.client_dir = Some("/Users/ada/secret-client-dir".into());
        config.password_source = PasswordSource::Command {
            program: "vault".into(),
            args: vec!["read".into()],
            timeout_secs: 10,
        };
        // Confirmed on this computer.
        config.password_command_unconfirmed = false;
        let mut task = backup_task("nightly", ID);
        if let crate::tools::TaskSettings::Backup(options) = &mut task.settings {
            options.auth_user = Some("backup_user".into());
        }
        let exported = export_connections_file(ConnectionsFile {
            connections: vec![PortableConnection {
                id: Some(ID.into()),
                config,
            }],
            folders: BTreeMap::from([(
                "Work".to_string(),
                FolderLook {
                    color: Some("blue".into()),
                    glyph: None,
                    image: Some("secret-picture.png".into()),
                },
            )]),
            tasks: vec![task],
            ..ConnectionsFile::default()
        })
        .unwrap();
        for leak in [
            "secret-client-dir",
            "client_dir",
            "secret-picture.png",
            "image",
            "password_command_unconfirmed",
            "\"password\":",
            "passphrase",
        ] {
            assert!(!exported.contains(leak), "{leak}: {exported}");
        }
        // What is kept: the command (not a secret), the task's user.
        assert!(exported.contains("vault") && exported.contains("backup_user"));
        // A file that brings any of it back has it dropped.
        let file = import_connections_file(&exported.replace(
            "\"name\": \"Shop\"",
            "\"name\": \"Shop\", \"client_dir\": \"/tmp/evil\", \"password_command_unconfirmed\": false",
        ))
        .unwrap();
        assert_eq!(file.connections[0].config.client_dir, None);
        assert!(file.connections[0].config.password_command_unconfirmed);

        // A task holding a password in its extra arguments is refused both ways.
        let mut leaky = backup_task("leaky", ID);
        if let crate::tools::TaskSettings::Backup(options) = &mut leaky.settings {
            options.extra_args = "--password=hunter2".into();
        }
        let error = export_connections_file(ConnectionsFile {
            tasks: vec![leaky.clone()],
            ..ConnectionsFile::default()
        })
        .unwrap_err();
        assert!(error.to_string().contains("task \"leaky\""), "{error}");
        let hostile = serde_json::to_string(&ConnectionsFile {
            kerno_connections: 2,
            tasks: vec![leaky],
            ..ConnectionsFile::default()
        })
        .unwrap();
        assert!(import_connections_file(&hostile).is_err());
    }

    /// J15: version 2 carries ids, environments and tasks; version 1 files
    /// still import, with no id.
    #[test]
    fn version_two_round_trips_and_version_one_still_imports() {
        let mut config = ConnectionConfig::new(Engine::MySql);
        config.name = "Billing".into();
        config.environment = "qa".into();
        let mut production = environment::builtin_environments()[2].clone();
        production.rules.confirm_execute = false;
        let qa = EnvironmentDef {
            id: "qa".into(),
            name: "QA".into(),
            color_light: Some("#336699".into()),
            color_dark: None,
            tint: environment::Tint::Soft,
            rules: environment::EnvironmentRules::default(),
        };
        let mut environments = environment::builtin_environments();
        environments[2] = production.clone();
        environments.push(qa.clone());
        let file = ConnectionsFile {
            connections: vec![PortableConnection {
                id: Some(ID.into()),
                config: config.clone(),
            }],
            environments,
            tasks: vec![backup_task("nightly", ID)],
            ..ConnectionsFile::default()
        };
        let exported = export_connections_file(file.clone()).unwrap();
        let imported = import_connections_file(&exported).unwrap();
        assert_eq!(imported.kerno_connections, 2);
        assert_eq!(imported.connections, file.connections);
        // The built-ins left as shipped stay out.
        assert_eq!(imported.environments, [production, qa]);
        assert_eq!(imported.tasks, file.tasks);

        // Version 1: no id, no environment, no task.
        let old = r#"{"kerno_connections": 1, "connections": [{
            "name": "Old", "engine": "postgres", "host": "db", "port": 5432, "user": "ada"}]}"#;
        let imported = import_connections_file(old).unwrap();
        assert_eq!(imported.connections[0].id, None);
        assert_eq!(imported.connections[0].config.name, "Old");
        assert!(imported.environments.is_empty() && imported.tasks.is_empty());

        // An id that is not a UUID, or that comes twice, names nothing.
        let twice = format!(
            r#"{{"kerno_connections": 2, "connections": [
            {{"id": "{ID}", "name": "A", "engine": "postgres", "host": "db", "port": 5432, "user": "u"}},
            {{"id": "{ID}", "name": "B", "engine": "postgres", "host": "db", "port": 5432, "user": "u"}},
            {{"id": "../../x", "name": "C", "engine": "postgres", "host": "db", "port": 5432, "user": "u"}}]}}"#
        );
        let ids: Vec<Option<String>> = import_connections_file(&twice)
            .unwrap()
            .connections
            .into_iter()
            .map(|connection| connection.id)
            .collect();
        assert_eq!(ids, [Some(ID.to_string()), None, None]);
        // An environment without an id is refused.
        let nameless = r#"{"kerno_connections": 2, "connections": [],
            "environments": [{"id": " ", "name": "x"}]}"#;
        assert!(import_connections_file(nameless).is_err());
    }

    #[test]
    fn the_import_plan_says_what_is_added_replaced_or_unchanged() {
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.name = "Shop".into();
        let other = "0f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
        let saved = [crate::storage::SavedConnection {
            id: ID.into(),
            config: ConnectionConfig {
                // Only this computer's: not a change.
                client_dir: Some("/opt/x".into()),
                ..config.clone()
            },
        }];
        let renamed = ConnectionConfig {
            name: "Shop (prod)".into(),
            ..config.clone()
        };
        let connection = |id: Option<&str>, config: &ConnectionConfig| PortableConnection {
            id: id.map(str::to_string),
            config: config.clone(),
        };
        let mut edited_local = environment::builtin_environments()[0].clone();
        edited_local.name = "Laptop".into();
        let mut task = backup_task("nightly", ID);
        let stored = crate::storage::SavedTask::new("nightly", ID, task.settings.clone());
        let file = ConnectionsFile {
            connections: vec![
                connection(Some(ID), &config),
                connection(Some(ID), &renamed),
                connection(Some(other), &config),
                connection(None, &config),
            ],
            environments: vec![environment::builtin_environments()[1].clone(), edited_local],
            tasks: vec![task.clone(), backup_task("weekly", ID)],
            ..ConnectionsFile::default()
        };
        let plan = plan_import(
            &file,
            &saved,
            &environment::builtin_environments(),
            std::slice::from_ref(&stored),
        );
        use ImportAction::*;
        assert_eq!(plan.connections, [Unchanged, Replace, Add, Add]);
        assert_eq!(plan.environments, [Unchanged, Replace]);
        assert_eq!(plan.tasks, [Unchanged, Add]);
        task.description = "changed".into();
        let file = ConnectionsFile {
            tasks: vec![task],
            ..file
        };
        assert_eq!(plan_import(&file, &saved, &[], &[stored]).tasks, [Replace]);

        // A password goes only where it went.
        assert!(!reaches_another_server(&config, &renamed));
        for changed in [
            ConnectionConfig {
                host: "attacker.example".into(),
                ..config.clone()
            },
            ConnectionConfig {
                port: 6543,
                ..config.clone()
            },
            ConnectionConfig {
                user: "root".into(),
                ..config.clone()
            },
            ConnectionConfig {
                ssh: Some(SshConfig {
                    host: "bastion".into(),
                    port: 22,
                    user: "ops".into(),
                    auth: SshAuth::default(),
                    keepalive_secs: 30,
                    client: SshClient::Builtin,
                    jump: None,
                }),
                ..config.clone()
            },
        ] {
            assert!(reaches_another_server(&config, &changed), "{changed:?}");
        }
    }

    #[test]
    fn exported_folders_carry_colour_and_glyph_but_no_image() {
        let mut connection = ConnectionConfig::new(Engine::Postgres);
        connection.name = "Shop".into();
        connection.group = Some("Work/Clients".into());
        let look = |color: Option<&str>, glyph: Option<&str>, image: Option<&str>| FolderLook {
            color: color.map(str::to_string),
            glyph: glyph.map(str::to_string),
            image: image.map(str::to_string),
        };
        let folders = BTreeMap::from([
            (
                "Work".to_string(),
                look(
                    Some("blue"),
                    Some("briefcase"),
                    Some("0123456789abcdef.png"),
                ),
            ),
            // Only an image: nothing of it travels.
            (
                "Work/Clients".to_string(),
                look(None, None, Some("fedcba9876543210.png")),
            ),
        ]);
        let exported = export_connections_file(ConnectionsFile {
            connections: vec![PortableConnection {
                id: None,
                config: connection.clone(),
            }],
            folders: folders.clone(),
            ..ConnectionsFile::default()
        })
        .unwrap();
        assert!(!exported.contains(".png"), "{exported}");
        let file = import_connections_file(&exported).unwrap();
        assert_eq!(file.connections[0].config, connection);
        assert_eq!(
            file.folders,
            BTreeMap::from([(
                "Work".to_string(),
                look(Some("blue"), Some("briefcase"), None)
            )])
        );

        // A file that names an image file has it dropped, and its paths are
        // tidied.
        let hostile = r#"{"kerno_connections": 1, "connections": [], "folders": {
            " Work / x ": {"color": "red", "image": "../../secret.png"},
            "Empty": {}}}"#;
        assert_eq!(
            import_connections_file(hostile).unwrap().folders,
            BTreeMap::from([("Work/x".to_string(), look(Some("red"), None, None))])
        );
        // Older files have no looks; without any, none is written.
        let plain = export_connections(std::slice::from_ref(&connection)).unwrap();
        assert!(!plain.contains("\"folders\""), "{plain}");
        assert_eq!(
            import_connections_file(&plain).unwrap().folders,
            BTreeMap::new()
        );
        // A look without anything set is the default one.
        assert!(FolderLook::default().is_empty());
        assert!(!look(None, Some("star"), None).is_empty());
    }

    #[test]
    fn folder_paths_are_tidied_checked_and_moved() {
        assert_eq!(
            folder_path(" Work / Clients/ "),
            Some("Work/Clients".into())
        );
        assert_eq!(folder_path("//"), None);
        assert_eq!(folder_path(""), None);

        assert_eq!(folder_name("  Clients "), Ok("Clients".into()));
        assert!(folder_name("   ").unwrap_err().contains("needs a name"));
        assert!(folder_name("a/b").unwrap_err().contains("“/”"));

        assert_eq!(parent_folder("Work/Clients/Acme"), Some("Work/Clients"));
        assert_eq!(parent_folder("Work"), None);
        assert_eq!(last_folder_name("Work/Clients"), "Clients");
        assert_eq!(last_folder_name("Work"), "Work");

        assert!(in_folder("Work", "Work"));
        assert!(in_folder("Work/Clients", "Work"));
        assert!(!in_folder("Workshop", "Work"));
        assert!(!in_folder("Workshop/x", "Work"));
        assert!(!in_folder("Work", "Work/Clients"));

        // Renaming Work/Clients to Work/Customers.
        let to = Some("Work/Customers");
        assert_eq!(
            rebase_folder("Work/Clients", "Work/Clients", to),
            Some("Work/Customers".into())
        );
        assert_eq!(
            rebase_folder("Work/Clients/Acme", "Work/Clients", to),
            Some("Work/Customers/Acme".into())
        );
        // Removing Work: what it held moves up one level, intact.
        assert_eq!(rebase_folder("Work", "Work", None), None);
        assert_eq!(
            rebase_folder("Work/Clients/Acme", "Work", None),
            Some("Clients/Acme".into())
        );

        assert_eq!(
            all_folders(["Work/Clients/Acme", "Home", "Work", " Lab / "]),
            ["Home", "Lab", "Work", "Work/Clients", "Work/Clients/Acme"]
        );
    }

    #[test]
    fn the_connection_url_has_no_password_and_reads_back() {
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.host = "db.example.com".into();
        config.port = 6543;
        config.user = "ada lovelace".into();
        config.database = Some("shop/eu".into());
        config.tls.mode = TlsMode::VerifyFull;
        let url = connection_url(&config);
        assert_eq!(
            url,
            "postgresql://ada%20lovelace@db.example.com:6543/shop%2Feu?sslmode=verify-full"
        );
        let parsed = parse_connection_url(&url).unwrap();
        assert_eq!(parsed.engine, Engine::Postgres);
        assert_eq!(parsed.host.as_deref(), Some("db.example.com"));
        assert_eq!(parsed.port, Some(6543));
        assert_eq!(parsed.user.as_deref(), Some("ada lovelace"));
        assert_eq!(parsed.database.as_deref(), Some("shop/eu"));
        assert_eq!(parsed.tls_mode, Some(TlsMode::VerifyFull));
        assert!(parsed.password.is_none());

        let mut config = ConnectionConfig::new(Engine::MariaDb);
        config.host = "::1".into();
        assert_eq!(connection_url(&config), "mariadb://root@[::1]:3306/");
        let parsed = parse_connection_url(&connection_url(&config)).unwrap();
        assert_eq!(parsed.host.as_deref(), Some("::1"));
        assert_eq!(parsed.tls_mode, None);
    }

    #[test]
    fn import_rejects_what_is_not_a_valid_connections_file() {
        for (json, expected) in [
            ("not json", "not a Kerno connections file"),
            ("[]", "not a Kerno connections file"),
            (r#"{"connections": []}"#, "not a Kerno connections file"),
            (
                r#"{"kerno_connections": 99, "connections": []}"#,
                "newer version",
            ),
            (
                r#"{"kerno_connections": 1, "connections": [{"name":"ok","engine":"postgres","host":"h","port":5432,"user":"u"},{"name":"broken","engine":"postgres","host":"","port":5432,"user":"u"}]}"#,
                "connection 2 (\"broken\")",
            ),
            (
                r#"{"kerno_connections": 1, "connections": [{"name":"x","engine":"oracle","host":"h","port":1,"user":"u"}]}"#,
                "not a Kerno connections file",
            ),
        ] {
            let error = import_connections(json).unwrap_err().to_string();
            assert!(error.contains(expected), "{json}: {error}");
        }
    }

    #[test]
    fn secrets_cannot_hide_in_options() {
        for key in [
            "password",
            "Password",
            "sslpassword",
            "ssl_key_passphrase",
            "PWD",
            "auth_token",
            "client_secret",
            "passwd",
        ] {
            assert!(is_secret_option(key), "{key}");
        }
        for key in [
            "sslkey",
            "application_name",
            "search_path",
            "key_buffer_size",
            "connect_timeout",
            "sql_mode",
            // About a secret, not one: a method, a duration, a policy.
            "password_encryption",
            "token_lifetime",
            "default_password_lifetime",
            "validate_password.length",
            "password_reuse_interval",
        ] {
            assert!(!is_secret_option(key), "{key}");
        }
        for key in [
            "my.api_token",
            "db_pwd",
            "private_key_passphrase",
            "secret_key",
            "password_hash",
            "token_value",
            // An ending that is not known to describe: still refused.
            "password_prod",
            "my.password.app",
        ] {
            assert!(is_secret_option(key), "{key}");
        }

        // A URL that carries credentials as parameters: they become the
        // credentials, and a secret with no field to go to is dropped.
        let parsed = parse_connection_url(
            "postgresql://db.example.com/shop?user=ada&password=hunter2&sslpassword=keypass&application_name=kerno",
        )
        .unwrap();
        assert_eq!(parsed.user.as_deref(), Some("ada"));
        assert_eq!(
            parsed.password.as_ref().map(Secret::expose),
            Some("hunter2")
        );
        assert_eq!(
            parsed.options.keys().collect::<Vec<_>>(),
            ["application_name"]
        );

        // Typed or imported as an option: refused everywhere.
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.name = "Leaky".into();
        config.options.insert("password".into(), "hunter2".into());
        let error = config.validate().unwrap_err().to_string();
        assert!(error.contains("looks like a secret"), "{error}");
        assert!(
            !error.contains("hunter2"),
            "the value must not be echoed: {error}"
        );
        let error = export_connections(std::slice::from_ref(&config))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Leaky") && !error.contains("hunter2"),
            "{error}"
        );
        let file = r#"{"kerno_connections": 1, "connections": [{"name":"x","engine":"postgres","host":"h","port":5432,"user":"u","options":{"sslpassword":"hunter2"}}]}"#;
        let error = import_connections(file).unwrap_err().to_string();
        assert!(
            error.contains("looks like a secret") && !error.contains("hunter2"),
            "{error}"
        );
    }

    #[test]
    fn tls_mode_names_round_trip() {
        for mode in TlsMode::ALL {
            assert_eq!(TlsMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn object_filters_match_as_dbeaver_masks_do() {
        let masks = |include: &[&str], exclude: &[&str]| ObjectFilter {
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            enabled: true,
        };
        // No mask, or switched off: everything is shown.
        assert!(ObjectFilter::default().shows("anything"));
        let mut off = masks(&["a*"], &[]);
        off.enabled = false;
        assert!(off.is_inactive() && off.shows("zzz"));

        // Exact names compare without regard to case.
        let only = masks(&["Orders"], &[]);
        assert!(only.shows("orders") && only.shows("ORDERS") && !only.shows("orders2"));
        // `*` is any run, `?` one character; `_` and `%` are plain.
        let wild = masks(&["ord*", "t?p"], &[]);
        assert!(wild.shows("ord") && wild.shows("orders") && wild.shows("tip"));
        assert!(!wild.shows("tp") && !wild.shows("tiip") && !wild.shows("word"));
        let plain = masks(&["order_items"], &[]);
        assert!(plain.shows("order_items") && !plain.shows("orderXitems"));
        assert!(masks(&["*_log"], &[]).shows("audit_log"));
        assert!(masks(&["a*b*c"], &[]).shows("aXXbYYbZc") && !masks(&["a*b*c"], &[]).shows("acb"));
        // Excludes win over includes.
        let both = masks(&["shop*"], &["shop_old"]);
        assert!(both.shows("shop") && !both.shows("SHOP_OLD") && !both.shows("other"));
        assert!(!masks(&[], &["pg_*"]).shows("pg_toast") && masks(&[], &["pg_*"]).shows("public"));

        // A name added as it is matches only itself, wildcards included.
        let odd = "we*rd?\\name";
        let exact = masks(&[&exact_mask(odd)], &[]);
        assert!(exact.shows(odd) && !exact.shows("weXrdY\\name"));
        assert_eq!(exact_mask("plain_name"), "plain_name");
    }

    #[test]
    fn filters_are_kept_with_the_connection_and_old_files_still_read() {
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.name = "f".into();
        let json = export_connections(std::slice::from_ref(&config)).unwrap();
        // Nothing written when there is no filter.
        assert!(!json.contains("navigator_filters"), "{json}");
        config.navigator_filters.insert(
            "tables".into(),
            ObjectFilter {
                exclude: vec!["tmp_*".into()],
                ..ObjectFilter::default()
            },
        );
        let json = export_connections(std::slice::from_ref(&config)).unwrap();
        assert_eq!(import_connections(&json).unwrap(), [config]);
        // A filter written without `enabled` is on.
        let filter: ObjectFilter = serde_json::from_str(r#"{"include":["a"]}"#).unwrap();
        assert!(filter.enabled && !filter.shows("b"));
    }
}
