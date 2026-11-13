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

/// An SSH tunnel through which the database is reached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshConfig {
    /// SSH server host.
    pub host: String,
    /// SSH server port.
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// SSH user.
    pub user: String,
    /// Authentication method.
    #[serde(default)]
    pub auth: SshAuth,
    /// Seconds between keep-alive messages; 0 turns them off.
    #[serde(default = "default_keepalive")]
    pub keepalive_secs: u16,
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
            if ssh.user.trim().is_empty() {
                return missing("the SSH user");
            }
            if let SshAuth::KeyFile { path } = &ssh.auth
                && path.as_os_str().is_empty()
            {
                return missing("the SSH private key file");
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

/// The file written by "export connections": settings only, no secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionsFile {
    /// Format marker, so another JSON file is not mistaken for this one.
    pub kerno_connections: u32,
    /// The connections.
    pub connections: Vec<ConnectionConfig>,
    /// The looks of their folders, by path: colour and glyph, never a
    /// custom image (a file of this computer). Older files have none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub folders: BTreeMap<String, FolderLook>,
}

/// Version written in [`ConnectionsFile::kerno_connections`].
const CONNECTIONS_FILE_VERSION: u32 = 1;

/// Serializes connections for export. Passwords and passphrases are not
/// part of [`ConnectionConfig`], so the result cannot contain any.
pub fn export_connections(connections: &[ConnectionConfig]) -> Result<String> {
    export_connections_with_folders(connections, &BTreeMap::new())
}

/// [`export_connections`] with the looks of the folders (see
/// [`ConnectionsFile::folders`]).
pub fn export_connections_with_folders(
    connections: &[ConnectionConfig],
    folders: &BTreeMap<String, FolderLook>,
) -> Result<String> {
    // The one free-form part of a connection is its options: refuse to
    // write a file if any of them is named like a secret.
    for connection in connections {
        connection.validate().map_err(|error| {
            Error::Config(format!("connection \"{}\": {error}", connection.name))
        })?;
    }
    // The client folder is this computer's choice: never exported.
    let connections = connections
        .iter()
        .cloned()
        .map(|mut connection| {
            connection.client_dir = None;
            connection
        })
        .collect();
    serde_json::to_string_pretty(&ConnectionsFile {
        kerno_connections: CONNECTIONS_FILE_VERSION,
        connections,
        folders: shared_looks(folders),
    })
    .map_err(|error| Error::Config(format!("could not encode the connections: {error}")))
}

/// Reads an exported connections file. Every connection is validated; one
/// bad entry rejects the file, naming it, rather than importing part of it.
pub fn import_connections(json: &str) -> Result<Vec<ConnectionConfig>> {
    Ok(import_connections_file(json)?.connections)
}

/// [`import_connections`] with the looks of the folders the file carries.
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
        connection.validate().map_err(|error| {
            Error::Config(format!(
                "connection {} (\"{}\"): {error}",
                index + 1,
                connection.name
            ))
        })?;
    }
    // A file could otherwise point backups at programs someone else chose:
    // the client folder is always this computer's own choice.
    let connections = file
        .connections
        .into_iter()
        .map(|mut connection| {
            connection.client_dir = None;
            connection
        })
        .collect();
    Ok(ConnectionsFile {
        connections,
        folders: shared_looks(&file.folders),
        ..file
    })
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
        });
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("SSH host")
        );
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
        });
        first.auto_commit = Some(true);
        first.default_schema = Some("shop".into());
        first.init_sql = vec!["SET TIME ZONE 'UTC'".into(), "SET work_mem = '64MB'".into()];
        first.ignore_init_errors = true;
        let mut second = ConnectionConfig::new(Engine::MySql);
        second.name = "Billing".into();
        let exported = export_connections(&[first.clone(), second.clone()]).unwrap();
        assert!(exported.contains("\"kerno_connections\": 1"), "{exported}");
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
        let exported =
            export_connections_with_folders(std::slice::from_ref(&connection), &folders).unwrap();
        assert!(!exported.contains(".png"), "{exported}");
        let file = import_connections_file(&exported).unwrap();
        assert_eq!(file.connections, [connection.clone()]);
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
