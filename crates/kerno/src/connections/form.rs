//! The connection dialog's data: what its fields hold as text, how a pasted
//! URL fills them, and how they become validated settings and secrets.
//! No UI here, so every rule is unit-tested.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kerno_core::config::{
    ConnectionConfig, Engine, Environment, Secret, Secrets, SshAuth, SshConfig, TlsConfig, TlsMode,
    is_secret_option, parse_connection_url,
};

/// A field of the dialog, to attach an error to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Url,
    Host,
    Port,
    User,
    Options,
    CaFile,
    CertFile,
    KeyFile,
    SshHost,
    SshPort,
    SshUser,
    SshKeyFile,
    SshAgentSocket,
    SshKeepalive,
}

/// What is wrong with one field, in words the user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: Field,
    pub message: String,
}

fn error(field: Field, message: impl Into<String>) -> FieldError {
    FieldError {
        field,
        message: message.into(),
    }
}

/// How the SSH tunnel authenticates, as chosen in the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SshAuthKind {
    #[default]
    Agent,
    Password,
    KeyFile,
}

impl SshAuthKind {
    pub const ALL: [SshAuthKind; 3] = [Self::Agent, Self::Password, Self::KeyFile];

    pub fn label(self) -> &'static str {
        match self {
            Self::Agent => "SSH agent",
            Self::Password => "Password",
            Self::KeyFile => "Key file",
        }
    }
}

/// The dialog's fields, as the user typed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionForm {
    pub engine: Engine,
    pub name: String,
    pub group: String,
    pub color: Option<String>,
    pub environment: Environment,
    pub host: String,
    pub port: String,
    pub user: String,
    pub password: String,
    pub save_password: bool,
    pub database: String,
    /// Extra driver options, one `key=value` per line.
    pub options: String,
    pub read_only: bool,
    pub tls_mode: TlsMode,
    pub ca_file: String,
    pub cert_file: String,
    pub key_file: String,
    pub ssh_enabled: bool,
    pub ssh_host: String,
    pub ssh_port: String,
    pub ssh_user: String,
    pub ssh_auth: SshAuthKind,
    pub ssh_password: String,
    pub ssh_key_file: String,
    pub ssh_key_passphrase: String,
    pub ssh_agent_socket: String,
    pub ssh_keepalive: String,
}

impl ConnectionForm {
    /// An empty form for a new connection to `engine`.
    pub fn new(engine: Engine) -> Self {
        Self::from_saved(&ConnectionConfig::new(engine), &Secrets::default())
    }

    /// The form of an existing connection.
    pub fn from_saved(config: &ConnectionConfig, secrets: &Secrets) -> Self {
        let text = |path: &Option<PathBuf>| {
            path.as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        };
        let secret = |secret: &Option<Secret>| {
            secret
                .as_ref()
                .map(|s| s.expose().to_string())
                .unwrap_or_default()
        };
        let ssh = config.ssh.as_ref();
        let (ssh_auth, ssh_key_file, ssh_agent_socket) = match ssh.map(|ssh| &ssh.auth) {
            Some(SshAuth::Password) => (SshAuthKind::Password, String::new(), String::new()),
            Some(SshAuth::KeyFile { path }) => (
                SshAuthKind::KeyFile,
                path.display().to_string(),
                String::new(),
            ),
            Some(SshAuth::Agent { socket }) => (SshAuthKind::Agent, String::new(), text(socket)),
            None => (SshAuthKind::default(), String::new(), String::new()),
        };
        Self {
            engine: config.engine,
            name: config.name.clone(),
            group: config.group.clone().unwrap_or_default(),
            color: config.color.clone(),
            environment: config.environment,
            host: config.host.clone(),
            port: config.port.to_string(),
            user: config.user.clone(),
            password: secret(&secrets.password),
            save_password: config.save_password,
            database: config.database.clone().unwrap_or_default(),
            options: config
                .options
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("\n"),
            read_only: config.read_only,
            tls_mode: config.tls.mode,
            ca_file: text(&config.tls.ca_file),
            cert_file: text(&config.tls.cert_file),
            key_file: text(&config.tls.key_file),
            ssh_enabled: ssh.is_some(),
            ssh_host: ssh.map(|ssh| ssh.host.clone()).unwrap_or_default(),
            ssh_port: ssh.map_or(22, |ssh| ssh.port).to_string(),
            ssh_user: ssh.map(|ssh| ssh.user.clone()).unwrap_or_default(),
            ssh_auth,
            ssh_password: secret(&secrets.ssh_password),
            ssh_key_file,
            ssh_key_passphrase: secret(&secrets.ssh_key_passphrase),
            ssh_agent_socket,
            ssh_keepalive: ssh.map_or(30, |ssh| ssh.keepalive_secs).to_string(),
        }
    }

    /// Switches engine. Port and user follow the new engine's defaults
    /// only where they still hold the old engine's defaults.
    pub fn set_engine(&mut self, engine: Engine) {
        let previous = self.engine;
        if self.port.trim() == previous.default_port().to_string() {
            self.port = engine.default_port().to_string();
        }
        if self.user.trim() == previous.default_user() {
            self.user = engine.default_user().to_string();
        }
        self.engine = engine;
    }

    /// Fills the fields a connection URL carries. Fields the URL does not
    /// mention keep their values.
    pub fn apply_url(&mut self, url: &str) -> Result<(), FieldError> {
        let parsed = parse_connection_url(url).map_err(|e| error(Field::Url, e.to_string()))?;
        self.set_engine(parsed.engine);
        // No port in the URL means the engine's standard port.
        self.port = parsed
            .port
            .unwrap_or(parsed.engine.default_port())
            .to_string();
        if let Some(host) = parsed.host {
            self.host = host;
        }
        if let Some(user) = parsed.user {
            self.user = user;
        }
        if let Some(password) = parsed.password {
            self.password = password.expose().to_string();
        }
        if let Some(database) = parsed.database {
            self.database = database;
        }
        if let Some(mode) = parsed.tls_mode {
            self.tls_mode = mode;
        }
        if !parsed.options.is_empty() {
            self.options = parsed
                .options
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("\n");
        }
        Ok(())
    }

    /// The name shown when the user gave none.
    pub fn default_name(&self) -> String {
        match (self.user.trim(), self.host.trim()) {
            ("", "") => self.engine.label().to_string(),
            ("", host) => host.to_string(),
            (user, host) => format!("{user}@{host}"),
        }
    }

    /// Validates the fields and turns them into settings and secrets.
    /// Every problem is reported, each on its field.
    pub fn build(&self) -> Result<(ConnectionConfig, Secrets), Vec<FieldError>> {
        let mut errors = Vec::new();
        let required = |value: &str, field: Field, what: &str, errors: &mut Vec<FieldError>| {
            if value.trim().is_empty() {
                errors.push(error(field, format!("{what} is required")));
            }
        };
        let port = |value: &str, field: Field, errors: &mut Vec<FieldError>| -> u16 {
            match value.trim().parse::<u16>() {
                Ok(port) if port > 0 => port,
                _ => {
                    errors.push(error(field, "Enter a port between 1 and 65535"));
                    0
                }
            }
        };
        let file = |value: &str,
                    field: Field,
                    what: &str,
                    errors: &mut Vec<FieldError>|
         -> Option<PathBuf> {
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            let path = expand_home(value);
            if !path.is_file() {
                errors.push(error(
                    field,
                    format!("{what} not found: {}", path.display()),
                ));
            }
            Some(path)
        };

        required(&self.host, Field::Host, "The host", &mut errors);
        required(&self.user, Field::User, "The user", &mut errors);
        let db_port = port(&self.port, Field::Port, &mut errors);

        let mut options = BTreeMap::new();
        for line in self.options.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match line.split_once('=') {
                // Options are stored and exported in plain text.
                Some((key, _)) if is_secret_option(key.trim()) => errors.push(error(
                    Field::Options,
                    format!(
                        "\"{}\" looks like a secret: options are stored in plain text, use the password fields",
                        key.trim()
                    ),
                )),
                Some((key, value)) if !key.trim().is_empty() => {
                    options.insert(key.trim().to_string(), value.trim().to_string());
                }
                _ => errors.push(error(
                    Field::Options,
                    format!("\"{line}\" is not an option: write one key=value per line"),
                )),
            }
        }

        let tls = TlsConfig {
            mode: self.tls_mode,
            ca_file: file(&self.ca_file, Field::CaFile, "CA certificate", &mut errors),
            cert_file: file(
                &self.cert_file,
                Field::CertFile,
                "Client certificate",
                &mut errors,
            ),
            key_file: file(&self.key_file, Field::KeyFile, "Client key", &mut errors),
        };
        match (&tls.cert_file, &tls.key_file) {
            (Some(_), None) => errors.push(error(
                Field::KeyFile,
                "A client certificate needs its key file",
            )),
            (None, Some(_)) => errors.push(error(
                Field::CertFile,
                "A client key needs its certificate file",
            )),
            _ => {}
        }

        let ssh = if self.ssh_enabled {
            required(&self.ssh_host, Field::SshHost, "The SSH host", &mut errors);
            required(&self.ssh_user, Field::SshUser, "The SSH user", &mut errors);
            let ssh_port = port(&self.ssh_port, Field::SshPort, &mut errors);
            let keepalive_secs = self
                .ssh_keepalive
                .trim()
                .parse::<u16>()
                .unwrap_or_else(|_| {
                    errors.push(error(
                        Field::SshKeepalive,
                        "Enter a number of seconds (0 turns keep-alive off)",
                    ));
                    0
                });
            let auth = match self.ssh_auth {
                SshAuthKind::Password => SshAuth::Password,
                SshAuthKind::KeyFile => {
                    if self.ssh_key_file.trim().is_empty() {
                        errors.push(error(Field::SshKeyFile, "Choose the private key file"));
                    }
                    SshAuth::KeyFile {
                        path: file(
                            &self.ssh_key_file,
                            Field::SshKeyFile,
                            "Private key",
                            &mut errors,
                        )
                        .unwrap_or_default(),
                    }
                }
                SshAuthKind::Agent => {
                    let socket = self.ssh_agent_socket.trim();
                    let socket = (!socket.is_empty()).then(|| expand_home(socket));
                    if let Some(path) = &socket
                        && !path.exists()
                    {
                        errors.push(error(
                            Field::SshAgentSocket,
                            format!("Agent socket not found: {}", path.display()),
                        ));
                    }
                    SshAuth::Agent { socket }
                }
            };
            Some(SshConfig {
                host: self.ssh_host.trim().to_string(),
                port: ssh_port,
                user: self.ssh_user.trim().to_string(),
                auth,
                keepalive_secs,
            })
        } else {
            None
        };

        if !errors.is_empty() {
            return Err(errors);
        }

        let non_empty = |value: &str| {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        };
        let name = non_empty(&self.name).unwrap_or_else(|| self.default_name());
        let config = ConnectionConfig {
            name,
            engine: self.engine,
            host: self.host.trim().to_string(),
            port: db_port,
            user: self.user.trim().to_string(),
            database: non_empty(&self.database),
            tls,
            ssh,
            read_only: self.read_only,
            environment: self.environment,
            color: self.color.clone(),
            group: kerno_core::config::folder_path(&self.group),
            save_password: self.save_password,
            options,
        };
        // Secrets are kept exactly as typed (a password may start or end
        // with a space), and only the ones the chosen methods use.
        let secret = |value: &str| (!value.is_empty()).then(|| Secret::new(value));
        let ssh_auth = self.ssh_enabled.then_some(self.ssh_auth);
        let secrets = Secrets {
            password: secret(&self.password),
            ssh_password: (ssh_auth == Some(SshAuthKind::Password))
                .then(|| secret(&self.ssh_password))
                .flatten(),
            ssh_key_passphrase: (ssh_auth == Some(SshAuthKind::KeyFile))
                .then(|| secret(&self.ssh_key_passphrase))
                .flatten(),
        };
        Ok((config, secrets))
    }
}

/// Expands a leading `~/` to the home directory.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(errors: &[FieldError]) -> Vec<Field> {
        errors.iter().map(|e| e.field).collect()
    }

    #[test]
    fn a_new_form_has_the_engine_defaults_and_builds() {
        let form = ConnectionForm::new(Engine::Postgres);
        assert_eq!(
            (form.host.as_str(), form.port.as_str(), form.user.as_str()),
            ("localhost", "5432", "postgres")
        );
        assert!(form.save_password && !form.ssh_enabled && !form.read_only);
        assert_eq!(form.tls_mode, TlsMode::Prefer);

        let (config, secrets) = form.build().unwrap();
        assert_eq!(
            config.name, "postgres@localhost",
            "a missing name is derived"
        );
        assert_eq!(config.port, 5432);
        assert_eq!(config.database, None);
        assert_eq!(secrets, Secrets::default());
        config.validate().unwrap();
    }

    #[test]
    fn switching_engine_moves_defaults_but_not_what_was_typed() {
        let mut form = ConnectionForm::new(Engine::Postgres);
        form.set_engine(Engine::MySql);
        assert_eq!((form.port.as_str(), form.user.as_str()), ("3306", "root"));
        form.set_engine(Engine::MariaDb);
        assert_eq!((form.port.as_str(), form.user.as_str()), ("3306", "root"));

        form.port = "13306".into();
        form.user = "app".into();
        form.set_engine(Engine::Postgres);
        assert_eq!((form.port.as_str(), form.user.as_str()), ("13306", "app"));
    }

    #[test]
    fn a_pasted_url_fills_the_fields_it_carries() {
        let mut form = ConnectionForm::new(Engine::MySql);
        form.name = "Kept".into();
        form.database = "kept_db".into();
        form.apply_url("postgresql://ada:p%40ss@db.example.com:6543/shop?sslmode=verify-full&application_name=kerno")
            .unwrap();
        assert_eq!(form.engine, Engine::Postgres);
        assert_eq!(form.host, "db.example.com");
        assert_eq!(form.port, "6543");
        assert_eq!(form.user, "ada");
        assert_eq!(form.password, "p@ss");
        assert_eq!(form.database, "shop");
        assert_eq!(form.tls_mode, TlsMode::VerifyFull);
        assert_eq!(form.options, "application_name=kerno");
        assert_eq!(form.name, "Kept");

        // A URL without port, password or database: standard port, the rest kept.
        form.apply_url("mysql://root@127.0.0.1").unwrap();
        assert_eq!(form.engine, Engine::MySql);
        assert_eq!(form.port, "3306");
        assert_eq!(form.user, "root");
        assert_eq!(form.password, "p@ss");
        assert_eq!(form.database, "shop");

        let error = form.apply_url("http://example.com").unwrap_err();
        assert_eq!(error.field, Field::Url);
        assert!(error.message.contains("http"), "{}", error.message);
    }

    #[test]
    fn every_problem_is_reported_on_its_field() {
        let mut form = ConnectionForm::new(Engine::Postgres);
        form.host = "  ".into();
        form.user = String::new();
        form.port = "70000".into();
        form.options = "application_name=kerno\n# a comment\n\nnot an option\n=novalue".into();
        form.ca_file = "/nonexistent/ca.pem".into();
        form.cert_file = "/nonexistent/client.crt".into();
        form.ssh_enabled = true;
        form.ssh_port = "0".into();
        form.ssh_keepalive = "often".into();
        form.ssh_auth = SshAuthKind::KeyFile;
        let errors = form.build().unwrap_err();
        assert_eq!(
            fields(&errors),
            [
                Field::Host,
                Field::User,
                Field::Port,
                Field::Options,
                Field::Options,
                Field::CaFile,
                Field::CertFile,
                Field::KeyFile,
                Field::SshHost,
                Field::SshUser,
                Field::SshPort,
                Field::SshKeepalive,
                Field::SshKeyFile,
            ]
        );
        assert!(
            errors[3].message.contains("\"not an option\""),
            "{}",
            errors[3].message
        );
        assert!(
            errors[5].message.contains("/nonexistent/ca.pem"),
            "{}",
            errors[5].message
        );

        // SSH problems are not reported when the tunnel is off.
        form.ssh_enabled = false;
        assert!(!fields(&form.build().unwrap_err()).contains(&Field::SshHost));
    }

    #[test]
    fn a_secret_typed_as_an_option_is_refused_without_echoing_it() {
        let mut form = ConnectionForm::new(Engine::Postgres);
        form.options = "application_name=kerno\nsslpassword=hunter2".into();
        let errors = form.build().unwrap_err();
        assert_eq!(fields(&errors), [Field::Options]);
        assert!(
            errors[0].message.contains("\"sslpassword\""),
            "{}",
            errors[0].message
        );
        assert!(
            !errors[0].message.contains("hunter2"),
            "{}",
            errors[0].message
        );

        // A URL with the password as a parameter fills the password field.
        let mut form = ConnectionForm::new(Engine::Postgres);
        form.apply_url("postgres://h/d?user=ada&password=hunter2&application_name=kerno")
            .unwrap();
        assert_eq!(
            (form.user.as_str(), form.password.as_str()),
            ("ada", "hunter2")
        );
        assert_eq!(form.options, "application_name=kerno");
        let (config, secrets) = form.build().unwrap();
        assert!(!serde_json::to_string(&config).unwrap().contains("hunter2"));
        assert_eq!(
            secrets.password.as_ref().map(Secret::expose),
            Some("hunter2")
        );
    }

    #[test]
    fn secrets_follow_the_chosen_methods_and_are_kept_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id_test");
        std::fs::write(&key, "key").unwrap();

        let mut form = ConnectionForm::new(Engine::Postgres);
        form.password = " spaced pw ".into();
        form.ssh_enabled = true;
        form.ssh_host = "bastion".into();
        form.ssh_user = "ops".into();
        form.ssh_password = "ssh-pw".into();
        form.ssh_key_passphrase = "phrase".into();
        form.ssh_key_file = key.display().to_string();

        form.ssh_auth = SshAuthKind::Password;
        let (config, secrets) = form.build().unwrap();
        assert_eq!(
            secrets.password.as_ref().map(Secret::expose),
            Some(" spaced pw ")
        );
        assert_eq!(
            secrets.ssh_password.as_ref().map(Secret::expose),
            Some("ssh-pw")
        );
        assert_eq!(
            secrets.ssh_key_passphrase, None,
            "a passphrase for a method not in use is not kept"
        );
        assert_eq!(config.ssh.as_ref().unwrap().auth, SshAuth::Password);

        form.ssh_auth = SshAuthKind::KeyFile;
        let (config, secrets) = form.build().unwrap();
        assert_eq!(secrets.ssh_password, None);
        assert_eq!(
            secrets.ssh_key_passphrase.as_ref().map(Secret::expose),
            Some("phrase")
        );
        assert_eq!(
            config.ssh.as_ref().unwrap().auth,
            SshAuth::KeyFile { path: key.clone() }
        );

        form.ssh_auth = SshAuthKind::Agent;
        let (config, secrets) = form.build().unwrap();
        assert_eq!(
            (secrets.ssh_password, secrets.ssh_key_passphrase),
            (None, None)
        );
        assert_eq!(
            config.ssh.as_ref().unwrap().auth,
            SshAuth::Agent { socket: None }
        );

        // With the tunnel off, no SSH secret is kept at all.
        form.ssh_enabled = false;
        form.ssh_auth = SshAuthKind::Password;
        let (config, secrets) = form.build().unwrap();
        assert_eq!(config.ssh, None);
        assert_eq!(secrets.ssh_password, None);
    }

    #[test]
    fn a_saved_connection_round_trips_through_the_form() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, "x").unwrap();
            path
        };
        let mut config = ConnectionConfig::new(Engine::MariaDb);
        config.name = "Billing".into();
        config.group = Some("Work".into());
        config.color = Some("purple".into());
        config.environment = Environment::Production;
        config.database = Some("billing".into());
        config.read_only = true;
        config.save_password = false;
        config.options.insert("a".into(), "1".into());
        config.options.insert("b".into(), "two words".into());
        config.tls = TlsConfig {
            mode: TlsMode::VerifyCa,
            ca_file: Some(file("ca.pem")),
            cert_file: Some(file("client.crt")),
            key_file: Some(file("client.key")),
        };
        config.ssh = Some(SshConfig {
            host: "bastion".into(),
            port: 2222,
            user: "ops".into(),
            auth: SshAuth::KeyFile {
                path: file("id_ed25519"),
            },
            keepalive_secs: 0,
        });
        let secrets = Secrets {
            password: Some(Secret::new("db")),
            ssh_password: None,
            ssh_key_passphrase: Some(Secret::new("phrase")),
        };
        let form = ConnectionForm::from_saved(&config, &secrets);
        assert_eq!(form.build().unwrap(), (config, secrets));
    }

    #[test]
    fn the_folder_is_stored_as_a_tidy_path() {
        let mut form = ConnectionForm::new(Engine::Postgres);
        form.group = " Work / Clients /".into();
        assert_eq!(
            form.build().unwrap().0.group.as_deref(),
            Some("Work/Clients")
        );
        form.group = " / ".into();
        assert_eq!(form.build().unwrap().0.group, None);
    }
}
