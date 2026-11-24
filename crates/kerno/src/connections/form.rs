//! The connection dialog's data: what its fields hold as text, how a pasted
//! URL fills them, and how they become validated settings and secrets.
//! No UI here, so every rule is unit-tested.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kerno_core::config::{
    ConnectionConfig, DEFAULT_COMMAND_TIMEOUT, DatabasesShown, Engine, ObjectFilter,
    PasswordSource, Secret, Secrets, SshAuth, SshClient, SshConfig, TlsConfig, TlsMode,
    bootstrap_refusal, is_secret_option, parse_connection_url,
};
use kerno_core::sql::{LexRules, split_statements};

/// A field of the dialog, to attach an error to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    Url,
    File,
    Host,
    Port,
    User,
    PasswordSource,
    PasswordProgram,
    PasswordTimeout,
    RdsRegion,
    OpReference,
    Options,
    InitSql,
    CaFile,
    CertFile,
    KeyFile,
    SshHost,
    SshPort,
    SshUser,
    SshJump,
    SshAuth,
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
    /// OpenSSH asks no password: Kerno would have to type it.
    pub const OPENSSH: [SshAuthKind; 2] = [Self::Agent, Self::KeyFile];

    pub fn label(self) -> &'static str {
        match self {
            Self::Agent => "SSH agent",
            Self::Password => "Password",
            Self::KeyFile => "Key file",
        }
    }

    /// The methods `client` offers, in the dialog's order.
    pub fn choices(client: SshClient) -> &'static [SshAuthKind] {
        match client {
            SshClient::Builtin => &Self::ALL,
            SshClient::OpenSsh => &Self::OPENSSH,
        }
    }

    /// The label of the method with `client`.
    pub fn label_for(self, client: SshClient) -> &'static str {
        match (self, client) {
            (Self::Agent, SshClient::OpenSsh) => "SSH config or agent",
            _ => self.label(),
        }
    }
}

/// Where the database password comes from, as chosen in the dialog (J3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PasswordChoice {
    /// Saved in the secret store, Kerno's file or the keychain (or
    /// `~/.pgpass` / `~/.my.cnf` when empty).
    #[default]
    Keychain,
    /// Asked at each connect.
    Ask,
    /// A command of the user's.
    Command,
    /// `aws rds generate-db-auth-token`.
    RdsIam,
    /// `op read`.
    OnePassword,
}

impl PasswordChoice {
    pub const ALL: [PasswordChoice; 5] = [
        Self::Keychain,
        Self::Ask,
        Self::Command,
        Self::RdsIam,
        Self::OnePassword,
    ];

    /// The label; `saved_in` names where saved secrets are ("Kerno's
    /// file", "macOS Keychain"), `None` when they are in memory only.
    pub fn label(self, saved_in: Option<&'static str>) -> &'static str {
        match self {
            Self::Keychain => saved_in.unwrap_or("Until Kerno quits"),
            Self::Ask => "Ask each time",
            Self::Command => "Command",
            Self::RdsIam => "AWS RDS IAM",
            Self::OnePassword => "1Password",
        }
    }

    /// Whether a program prints the password.
    pub fn runs_a_command(self) -> bool {
        matches!(self, Self::Command | Self::RdsIam | Self::OnePassword)
    }
}

/// The dialog's fields, as the user typed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionForm {
    pub engine: Engine,
    pub name: String,
    pub group: String,
    pub color: Option<String>,
    /// Id of the environment.
    pub environment: String,
    pub host: String,
    pub port: String,
    pub user: String,
    pub password: String,
    pub password_choice: PasswordChoice,
    /// The command's program.
    pub command_program: String,
    /// Its arguments, one per line (no shell: a line is one argument).
    pub command_args: String,
    pub command_timeout: String,
    pub rds_profile: String,
    pub rds_region: String,
    pub op_reference: String,
    /// The command came from an imported file and is not confirmed yet.
    pub command_unconfirmed: bool,
    pub database: String,
    /// PostgreSQL: which databases the tree shows (K62).
    pub databases_shown: DatabasesShown,
    /// SQLite: the database file.
    pub file: String,
    /// Extra driver options, one `key=value` per line.
    pub options: String,
    pub read_only: bool,
    pub tls_mode: TlsMode,
    pub ca_file: String,
    pub cert_file: String,
    pub key_file: String,
    pub ssh_enabled: bool,
    pub ssh_client: SshClient,
    pub ssh_host: String,
    pub ssh_port: String,
    pub ssh_user: String,
    pub ssh_auth: SshAuthKind,
    pub ssh_password: String,
    pub ssh_key_file: String,
    pub ssh_key_passphrase: String,
    pub ssh_agent_socket: String,
    pub ssh_keepalive: String,
    /// Jump hosts (`ssh -J`), OpenSSH only.
    pub ssh_jump: String,
    /// The commit mode new tabs start in; `None` follows the environment.
    pub auto_commit: Option<bool>,
    /// PostgreSQL only.
    pub default_schema: String,
    /// Bootstrap statements, separated by `;`.
    pub init_sql: String,
    pub ignore_init_errors: bool,
    /// Not edited here (the tree's Filter ▸ menu does): kept as it was.
    pub navigator_filters: BTreeMap<String, ObjectFilter>,
    /// The folder of the client tools; the default when `None` (Advanced
    /// tab).
    pub client_dir: Option<std::path::PathBuf>,
}

/// The commit modes the dialog offers, with their labels.
pub const COMMIT_MODES: [(Option<bool>, &str); 3] = [
    (None, "From the environment"),
    (Some(true), "Auto-commit"),
    (Some(false), "Manual commit"),
];

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
        let mut command = (
            String::new(),
            String::new(),
            DEFAULT_COMMAND_TIMEOUT.to_string(),
        );
        let (mut rds_profile, mut rds_region, mut op_reference) =
            (String::new(), String::new(), String::new());
        let password_choice = match &config.password_source {
            PasswordSource::Stored if config.save_password => PasswordChoice::Keychain,
            PasswordSource::Stored => PasswordChoice::Ask,
            PasswordSource::Command {
                program,
                args,
                timeout_secs,
            } => {
                command = (program.clone(), args.join("\n"), timeout_secs.to_string());
                PasswordChoice::Command
            }
            PasswordSource::RdsIam { profile, region } => {
                rds_profile = profile.clone().unwrap_or_default();
                rds_region = region.clone();
                PasswordChoice::RdsIam
            }
            PasswordSource::OnePassword { reference } => {
                op_reference = reference.clone();
                PasswordChoice::OnePassword
            }
        };
        let (command_program, command_args, command_timeout) = command;
        Self {
            engine: config.engine,
            name: config.name.clone(),
            group: config.group.clone().unwrap_or_default(),
            color: config.color.clone(),
            environment: config.environment.clone(),
            host: config.host.clone(),
            port: config.port.to_string(),
            user: config.user.clone(),
            password: secret(&secrets.password),
            password_choice,
            command_program,
            command_args,
            command_timeout,
            rds_profile,
            rds_region,
            op_reference,
            command_unconfirmed: config.password_command_unconfirmed,
            database: config.database.clone().unwrap_or_default(),
            databases_shown: config.databases_shown,
            file: text(&config.file),
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
            ssh_client: ssh.map(|ssh| ssh.client).unwrap_or_default(),
            ssh_host: ssh.map(|ssh| ssh.host.clone()).unwrap_or_default(),
            // OpenSSH's port 0: from the SSH configuration.
            ssh_port: match ssh.map_or(22, |ssh| ssh.port) {
                0 => String::new(),
                port => port.to_string(),
            },
            ssh_user: ssh.map(|ssh| ssh.user.clone()).unwrap_or_default(),
            ssh_auth,
            ssh_password: secret(&secrets.ssh_password),
            ssh_key_file,
            ssh_key_passphrase: secret(&secrets.ssh_key_passphrase),
            ssh_agent_socket,
            ssh_keepalive: ssh.map_or(30, |ssh| ssh.keepalive_secs).to_string(),
            ssh_jump: ssh.and_then(|ssh| ssh.jump.clone()).unwrap_or_default(),
            auto_commit: config.auto_commit,
            default_schema: config.default_schema.clone().unwrap_or_default(),
            init_sql: config
                .init_sql
                .iter()
                .map(|sql| format!("{sql};"))
                .collect::<Vec<_>>()
                .join("\n"),
            ignore_init_errors: config.ignore_init_errors,
            navigator_filters: config.navigator_filters.clone(),
            client_dir: config.client_dir.clone(),
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
        // From a database file to a server: the server's default host.
        if previous.is_file() && !engine.is_file() && self.host.trim().is_empty() {
            self.host = ConnectionConfig::new(engine).host;
        }
        self.engine = engine;
    }

    /// Switches the SSH client. The port follows only while it holds the
    /// other client's default (22, or empty for "from the SSH
    /// configuration"); a password, which OpenSSH cannot take, becomes the
    /// agent.
    pub fn set_ssh_client(&mut self, client: SshClient) {
        match client {
            SshClient::OpenSsh => {
                if self.ssh_port.trim() == "22" {
                    self.ssh_port.clear();
                }
                if self.ssh_auth == SshAuthKind::Password {
                    self.ssh_auth = SshAuthKind::Agent;
                }
            }
            SshClient::Builtin => {
                if self.ssh_port.trim().is_empty() {
                    self.ssh_port = "22".into();
                }
            }
        }
        self.ssh_client = client;
    }

    /// Chooses where the password comes from. AWS RDS IAM sends its token
    /// over TLS only: a mode that may go without it becomes `require`.
    pub fn set_password_choice(&mut self, choice: PasswordChoice) {
        self.password_choice = choice;
        if choice == PasswordChoice::RdsIam
            && matches!(self.tls_mode, TlsMode::Disable | TlsMode::Prefer)
        {
            self.tls_mode = TlsMode::Require;
        }
    }

    /// The password source the fields describe, unchecked.
    fn password_source(&self) -> PasswordSource {
        let trimmed = |text: &str| text.trim().to_string();
        match self.password_choice {
            PasswordChoice::Keychain | PasswordChoice::Ask => PasswordSource::Stored,
            PasswordChoice::Command => PasswordSource::Command {
                program: trimmed(&self.command_program),
                // One argument per line, kept as typed but for its line end.
                args: self
                    .command_args
                    .lines()
                    .map(|line| line.trim_end_matches('\r').to_string())
                    .filter(|line| !line.trim().is_empty())
                    .collect(),
                timeout_secs: self.command_timeout.trim().parse().unwrap_or(0),
            },
            PasswordChoice::RdsIam => PasswordSource::RdsIam {
                profile: Some(trimmed(&self.rds_profile)).filter(|p| !p.is_empty()),
                region: trimmed(&self.rds_region),
            },
            PasswordChoice::OnePassword => PasswordSource::OnePassword {
                reference: trimmed(&self.op_reference),
            },
        }
    }

    /// The command line the password choice runs, as the user will see it
    /// before confirming it; `None` when no program runs.
    pub fn command_display(&self) -> Option<String> {
        let config = ConnectionConfig {
            host: self.host.trim().to_string(),
            port: self.port.trim().parse().unwrap_or(0),
            user: self.user.trim().to_string(),
            database: Some(self.database.trim().to_string()).filter(|d| !d.is_empty()),
            password_source: self.password_source(),
            ..ConnectionConfig::new(self.engine)
        };
        kerno_core::password::command_line(&config, None).map(|line| line.display())
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
        if self.engine.is_file() {
            // The file's name without its folder: `shop.db`.
            return std::path::Path::new(self.file.trim())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.engine.label().to_string());
        }
        match (self.user.trim(), self.host.trim()) {
            ("", "") => self.engine.label().to_string(),
            ("", host) => host.to_string(),
            (user, host) => format!("{user}@{host}"),
        }
    }

    /// Validates the fields and turns them into settings and secrets.
    /// Every problem is reported, each on its field.
    pub fn build(&self) -> Result<(ConnectionConfig, Secrets), Vec<FieldError>> {
        if self.engine.is_file() {
            return self.build_file();
        }
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

        let rules = LexRules::of(self.engine);
        let mut init_sql = Vec::new();
        for statement in split_statements(&self.init_sql, rules) {
            let sql = statement.text(&self.init_sql).to_string();
            if let Some(reason) = bootstrap_refusal(&sql, self.engine) {
                errors.push(error(
                    Field::InitSql,
                    format!("Statement {} {reason}", init_sql.len() + 1),
                ));
            }
            init_sql.push(sql);
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
            let openssh = self.ssh_client == SshClient::OpenSsh;
            required(&self.ssh_host, Field::SshHost, "The SSH host", &mut errors);
            // OpenSSH: an empty port or user comes from the SSH configuration.
            if !openssh {
                required(&self.ssh_user, Field::SshUser, "The SSH user", &mut errors);
            }
            let ssh_port = if openssh && self.ssh_port.trim().is_empty() {
                0
            } else {
                port(&self.ssh_port, Field::SshPort, &mut errors)
            };
            let jump = self.ssh_jump.trim();
            let jump = (openssh && !jump.is_empty()).then(|| jump.to_string());
            if openssh {
                let words = [
                    (Field::SshHost, self.ssh_host.trim(), false, "Host"),
                    (Field::SshUser, self.ssh_user.trim(), false, "User"),
                    (
                        Field::SshJump,
                        jump.as_deref().unwrap_or_default(),
                        true,
                        "Jump host",
                    ),
                ];
                for (field, text, list, what) in words {
                    if !text.is_empty() && !kerno_core::openssh::safe_ssh_word(text, list) {
                        errors.push(error(
                            field,
                            format!(
                                "{what}: use letters, digits and . - _ @ : [ ]{}",
                                if list {
                                    ", and commas between hosts"
                                } else {
                                    ""
                                }
                            ),
                        ));
                    }
                }
                if self.ssh_auth == SshAuthKind::Password {
                    errors.push(error(
                        Field::SshAuth,
                        "OpenSSH logs in with a key or your SSH agent; for a password, choose \
                         the built-in client",
                    ));
                }
            }
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
                client: self.ssh_client,
                jump,
            })
        } else {
            None
        };

        let password_source = self.password_source();
        match &password_source {
            PasswordSource::Stored => {}
            PasswordSource::Command { program, .. } => {
                if program.is_empty() {
                    errors.push(error(Field::PasswordProgram, "Enter the program to run"));
                }
                if !(1..=120).contains(&self.command_timeout.trim().parse::<u16>().unwrap_or(0)) {
                    errors.push(error(
                        Field::PasswordTimeout,
                        "Enter a number of seconds between 1 and 120",
                    ));
                }
            }
            PasswordSource::RdsIam { region, .. } => {
                if region.is_empty() {
                    errors.push(error(
                        Field::RdsRegion,
                        "Enter the AWS region (us-east-1, …)",
                    ));
                }
                if matches!(self.tls_mode, TlsMode::Disable | TlsMode::Prefer) {
                    errors.push(error(
                        Field::PasswordSource,
                        "AWS RDS IAM sends its token over TLS only: choose require or a \
                         verifying mode on the SSL / TLS tab",
                    ));
                }
            }
            PasswordSource::OnePassword { reference } => {
                if !reference.starts_with("op://") {
                    errors.push(error(
                        Field::OpReference,
                        "A reference starts with op:// (op://vault/item/field)",
                    ));
                }
            }
        }

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
            file: None,
            tls,
            ssh,
            read_only: self.read_only,
            environment: self.environment.clone(),
            color: self.color.clone(),
            group: kerno_core::config::folder_path(&self.group),
            save_password: self.password_choice != PasswordChoice::Ask,
            options,
            auto_commit: self.auto_commit,
            // MySQL and MariaDB have no schemas: the database is the default.
            default_schema: (self.engine == Engine::Postgres)
                .then(|| non_empty(&self.default_schema))
                .flatten(),
            init_sql,
            ignore_init_errors: self.ignore_init_errors,
            navigator_filters: self.navigator_filters.clone(),
            client_dir: self.client_dir.clone(),
            password_command_unconfirmed: self.command_unconfirmed && !password_source.is_stored(),
            password_source,
            // MySQL and MariaDB list only what the account sees (K62).
            databases_shown: match self.engine {
                Engine::Postgres => self.databases_shown,
                _ => DatabasesShown::All,
            },
        };
        // Secrets are kept exactly as typed (a password may start or end
        // with a space), and only the ones the chosen methods use.
        let secret = |value: &str| (!value.is_empty()).then(|| Secret::new(value));
        let ssh_auth = self.ssh_enabled.then_some(self.ssh_auth);
        let secrets = Secrets {
            // A command prints the password: nothing typed is kept for it.
            password: (!self.password_choice.runs_a_command())
                .then(|| secret(&self.password))
                .flatten(),
            ssh_password: (ssh_auth == Some(SshAuthKind::Password))
                .then(|| secret(&self.ssh_password))
                .flatten(),
            ssh_key_passphrase: (ssh_auth == Some(SshAuthKind::KeyFile))
                .then(|| secret(&self.ssh_key_passphrase))
                .flatten(),
        };
        Ok((config, secrets))
    }

    /// [`ConnectionForm::build`] for a database file (SQLite): the file and
    /// the choices every connection has. What only a server takes (host,
    /// port, user, password, TLS, SSH, options, bootstrap statements) is
    /// not kept, and no secret is.
    fn build_file(&self) -> Result<(ConnectionConfig, Secrets), Vec<FieldError>> {
        let file = self.file.trim();
        if file.is_empty() {
            return Err(vec![error(
                Field::File,
                "Choose the database file, or make a new one with New file…",
            )]);
        }
        let name = Some(self.name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.default_name());
        let config = ConnectionConfig {
            name,
            file: Some(expand_home(file)),
            read_only: self.read_only,
            environment: self.environment.clone(),
            color: self.color.clone(),
            group: kerno_core::config::folder_path(&self.group),
            auto_commit: self.auto_commit,
            navigator_filters: self.navigator_filters.clone(),
            ..ConnectionConfig::new(self.engine)
        };
        Ok((config, Secrets::default()))
    }
}

/// Expands a leading `~/` to the home directory.
pub(crate) fn expand_home(path: &str) -> PathBuf {
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
        assert!(form.password_choice == PasswordChoice::Keychain);
        assert!(!form.ssh_enabled && !form.read_only);
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
        config.environment = kerno_core::environment::PRODUCTION.into();
        config.database = Some("billing".into());
        config.read_only = true;
        config.save_password = false;
        config.options.insert("a".into(), "1".into());
        config.options.insert("b".into(), "two words".into());
        config.auto_commit = Some(false);
        config.init_sql = vec![
            "SET time_zone = '+00:00'".into(),
            "SET @note = 'a;b'".into(),
            "SET sql_mode = 'ANSI_QUOTES'".into(),
        ];
        config.ignore_init_errors = true;
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
            client: SshClient::Builtin,
            jump: None,
        });
        let secrets = Secrets {
            password: Some(Secret::new("db")),
            ssh_password: None,
            ssh_key_passphrase: Some(Secret::new("phrase")),
        };
        let form = ConnectionForm::from_saved(&config, &secrets);
        assert_eq!(form.build().unwrap(), (config, secrets));
    }

    /// J2 (DBeaver #17976, Beekeeper #1344): an alias alone is enough with
    /// OpenSSH; a password stays the built-in client's.
    #[test]
    fn openssh_fields_build_and_round_trip() {
        let mut form = ConnectionForm::new(Engine::Postgres);
        form.ssh_enabled = true;
        assert_eq!(form.ssh_client, SshClient::Builtin);
        form.ssh_auth = SshAuthKind::Password;
        form.set_ssh_client(SshClient::OpenSsh);
        assert_eq!(form.ssh_port, "", "the default port leaves it to the file");
        assert_eq!(
            form.ssh_auth,
            SshAuthKind::Agent,
            "OpenSSH takes no password"
        );
        form.ssh_host = "prod-bastion".into();
        form.ssh_jump = " ops@jump:2222,edge ".into();
        let (config, secrets) = form.build().unwrap();
        let ssh = config.ssh.clone().unwrap();
        assert_eq!(
            (ssh.client, ssh.port, ssh.user.as_str(), ssh.jump.as_deref()),
            (SshClient::OpenSsh, 0, "", Some("ops@jump:2222,edge"))
        );
        config.validate().unwrap();
        assert_eq!(
            ConnectionForm::from_saved(&config, &secrets)
                .build()
                .unwrap(),
            (config, secrets)
        );

        // Back to the built-in client: the port and the user are needed
        // again, and the jump host is not kept.
        form.set_ssh_client(SshClient::Builtin);
        assert_eq!(form.ssh_port, "22");
        assert_eq!(fields(&form.build().unwrap_err()), [Field::SshUser]);
        form.ssh_user = "ops".into();
        assert_eq!(form.build().unwrap().0.ssh.unwrap().jump, None);

        // What OpenSSH refuses, each on its field.
        form.set_ssh_client(SshClient::OpenSsh);
        form.ssh_auth = SshAuthKind::Password;
        form.ssh_jump = "j;id".into();
        form.ssh_host = "a b".into();
        assert_eq!(
            fields(&form.build().unwrap_err()),
            [Field::SshHost, Field::SshJump, Field::SshAuth]
        );
        assert_eq!(
            SshAuthKind::choices(SshClient::OpenSsh),
            [SshAuthKind::Agent, SshAuthKind::KeyFile]
        );
    }

    #[test]
    fn bootstrap_statements_are_split_and_checked() {
        let mut form = ConnectionForm::new(Engine::Postgres);
        assert_eq!(form.auto_commit, None);
        form.init_sql =
            "SET TIME ZONE 'UTC';\n\n-- a comment\nSET application_name = 'a;b'\n".into();
        form.default_schema = " shop ".into();
        let (config, _) = form.build().unwrap();
        assert_eq!(
            config.init_sql,
            ["SET TIME ZONE 'UTC'", "SET application_name = 'a;b'"]
        );
        assert_eq!(config.default_schema.as_deref(), Some("shop"));
        assert_eq!(
            ConnectionForm::from_saved(&config, &Secrets::default())
                .build()
                .unwrap()
                .0,
            config
        );

        // No default schema on MySQL: the database field is the default.
        form.set_engine(Engine::MySql);
        assert_eq!(form.build().unwrap().0.default_schema, None);

        form.init_sql = "SET a = 1; ALTER USER u IDENTIFIED BY 'hunter2'".into();
        let errors = form.build().unwrap_err();
        assert_eq!(fields(&errors), [Field::InitSql]);
        assert!(
            errors[0].message.contains("Statement 2"),
            "{}",
            errors[0].message
        );
        assert!(
            !errors[0].message.contains("hunter2"),
            "{}",
            errors[0].message
        );

        form.init_sql = "SET a = 1; DELETE FROM t".into();
        let errors = form.build().unwrap_err();
        assert!(
            errors[0]
                .message
                .contains("Statement 2 is not a SET or RESET"),
            "{}",
            errors[0].message
        );
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

    /// J3: each password choice builds its source and round-trips; a
    /// command keeps no typed password; RDS IAM moves TLS to require; an
    /// imported command stays unconfirmed until the dialog confirms it.
    #[test]
    fn password_choices_build_their_sources() {
        let mut form = ConnectionForm::new(Engine::Postgres);
        form.password = "typed".into();
        form.set_password_choice(PasswordChoice::Ask);
        let (config, secrets) = form.build().unwrap();
        assert!(!config.save_password && config.password_source.is_stored());
        assert_eq!(secrets.password.unwrap().expose(), "typed");

        form.set_password_choice(PasswordChoice::Command);
        let errors = form.build().unwrap_err();
        assert_eq!(fields(&errors), [Field::PasswordProgram]);
        form.command_program = " vault ".into();
        form.command_args = "read\r\n\nsecret/${host} with spaces\n".into();
        form.command_timeout = "0".into();
        assert_eq!(fields(&form.build().unwrap_err()), [Field::PasswordTimeout]);
        form.command_timeout = "30".into();
        let (config, secrets) = form.build().unwrap();
        assert_eq!(
            config.password_source,
            PasswordSource::Command {
                program: "vault".into(),
                args: vec!["read".into(), "secret/${host} with spaces".into()],
                timeout_secs: 30,
            }
        );
        assert!(config.save_password, "SSH secrets are still saved");
        assert_eq!(secrets.password, None, "the typed password is not kept");
        assert_eq!(
            form.command_display().as_deref(),
            Some("vault read 'secret/localhost with spaces'")
        );
        assert_eq!(
            ConnectionForm::from_saved(&config, &secrets).command_args,
            "read\nsecret/${host} with spaces"
        );

        assert_eq!(form.tls_mode, TlsMode::Prefer);
        form.set_password_choice(PasswordChoice::RdsIam);
        assert_eq!(form.tls_mode, TlsMode::Require, "RDS IAM needs TLS");
        assert_eq!(fields(&form.build().unwrap_err()), [Field::RdsRegion]);
        form.rds_region = "eu-west-3".into();
        form.tls_mode = TlsMode::Disable;
        assert_eq!(fields(&form.build().unwrap_err()), [Field::PasswordSource]);
        form.tls_mode = TlsMode::VerifyFull;
        let (config, _) = form.build().unwrap();
        config.validate().unwrap();
        assert_eq!(
            config.password_source,
            PasswordSource::RdsIam {
                profile: None,
                region: "eu-west-3".into()
            }
        );
        assert_eq!(
            ConnectionForm::from_saved(&config, &Secrets::default()).password_choice,
            PasswordChoice::RdsIam
        );

        form.set_password_choice(PasswordChoice::OnePassword);
        form.op_reference = "Work/db".into();
        assert_eq!(fields(&form.build().unwrap_err()), [Field::OpReference]);
        form.op_reference = "op://Work/db/password".into();
        let (mut config, _) = form.build().unwrap();
        assert_eq!(
            form.command_display().as_deref(),
            Some("op read op://Work/db/password")
        );

        // Imported: kept unconfirmed through the form until confirmed.
        config.password_command_unconfirmed = true;
        let mut imported = ConnectionForm::from_saved(&config, &Secrets::default());
        assert!(imported.command_unconfirmed);
        assert!(imported.build().unwrap().0.password_command_unconfirmed);
        imported.command_unconfirmed = false;
        assert!(!imported.build().unwrap().0.password_command_unconfirmed);
    }

    /// K29: a SQLite form keeps its file and nothing a server takes, holds
    /// no secret, is named after the file, and round-trips.
    #[test]
    fn a_database_file_form_builds_from_its_file() {
        let mut form = ConnectionForm::new(Engine::Postgres);
        form.password = "typed".into();
        form.ssh_enabled = true;
        form.options = "a=1".into();
        form.set_engine(Engine::Sqlite);
        assert_eq!(fields(&form.build().unwrap_err()), [Field::File]);
        form.file = " /data/shop.db ".into();
        form.read_only = true;
        let (config, secrets) = form.build().unwrap();
        config.validate().unwrap();
        assert_eq!(config.name, "shop.db");
        assert_eq!(
            config.file.as_deref(),
            Some(std::path::Path::new("/data/shop.db"))
        );
        assert!(config.read_only);
        assert_eq!((config.ssh.clone(), config.options.len()), (None, 0));
        assert_eq!(secrets, Secrets::default(), "no password is kept");
        assert_eq!(
            ConnectionForm::from_saved(&config, &secrets)
                .build()
                .unwrap(),
            (config, secrets)
        );
        // Back to a server: its default host and port again.
        form.set_engine(Engine::MySql);
        assert_eq!(
            (form.host.as_str(), form.port.as_str()),
            ("localhost", "3306")
        );
    }
}
