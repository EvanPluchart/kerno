//! Connection settings. Nothing in [`ConnectionConfig`] is secret: passwords
//! and passphrases travel separately in [`Secrets`] and are never serialized.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum SshAuth {
    /// Password, from [`Secrets::ssh_password`].
    Password,
    /// Private key file, decrypted with [`Secrets::ssh_key_passphrase`] when encrypted.
    KeyFile {
        /// Path of the private key.
        path: PathBuf,
    },
    /// Keys held by the running SSH agent (`SSH_AUTH_SOCK`).
    #[default]
    Agent,
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

/// What kind of system a connection points at. `Production` makes every
/// write ask for a second confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    /// A developer machine.
    #[default]
    Local,
    /// A shared pre-production system.
    Staging,
    /// Live data.
    Production,
}

impl Environment {
    /// Every environment, safest first.
    pub const ALL: [Environment; 3] = [Self::Local, Self::Staging, Self::Production];

    /// Name shown to the user.
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Staging => "Staging",
            Self::Production => "Production",
        }
    }
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
    /// Environment tag.
    #[serde(default)]
    pub environment: Environment,
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
}

fn default_true() -> bool {
    true
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
            environment: Environment::default(),
            color: None,
            group: None,
            save_password: true,
            options: BTreeMap::new(),
        }
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
            auth: SshAuth::Agent,
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
    fn tls_mode_names_round_trip() {
        for mode in TlsMode::ALL {
            assert_eq!(TlsMode::parse(mode.as_str()), Some(mode));
        }
    }
}
