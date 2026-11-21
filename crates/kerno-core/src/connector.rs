//! Opening sessions for a saved connection: brings up the SSH tunnel when
//! one is configured, then asks the driver for sessions that go through it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::config::{ConnectParams, ConnectionConfig, Secret, Secrets, SshClient};
use crate::driver::{Driver, Session};
use crate::error::{Error, Result};
use crate::exec;
use crate::openssh::{self, OpenSshEnv, OpenSshRequest};
use crate::password::{self, PasswordEnv, PasswordOrigin};
use crate::ssh::{SshError, Tunnel, TunnelRequest};

/// Where SSH host keys are looked up and recorded.
#[derive(Debug, Clone, Default)]
pub struct HostKeyStore {
    /// Known-hosts files to check, in order. Missing files are skipped.
    pub known_hosts: Vec<PathBuf>,
    /// File newly trusted keys are appended to.
    pub learn_into: Option<PathBuf>,
    /// Fingerprint the user just agreed to trust, from a previous
    /// [`crate::ssh::SshError::UnknownHostKey`].
    pub trusted_fingerprint: Option<String>,
    /// The system's OpenSSH, for connections that use it (`None`: not
    /// found). OpenSSH keeps its own known_hosts: the files above are the
    /// built-in client's.
    pub openssh: Option<OpenSshEnv>,
    /// Where password files and commands are looked for (J3). `None`
    /// reads no file and runs no command: only the given password is used.
    pub password: Option<PasswordEnv>,
}

/// How long a password a command printed is used for new sessions before
/// the command runs again (an RDS IAM token lasts 15 minutes).
const COMMAND_PASSWORD_REUSE: Duration = Duration::from_secs(10 * 60);

/// Opens the SSH tunnel of `config`, if it has one, with the client it
/// names. Every tunnel of the app is opened here.
pub async fn open_tunnel(
    config: &ConnectionConfig,
    secrets: &Secrets,
    host_keys: &HostKeyStore,
) -> Result<Option<Arc<Tunnel>>> {
    let Some(ssh) = &config.ssh else {
        return Ok(None);
    };
    let tunnel = match ssh.client {
        SshClient::Builtin => {
            Tunnel::open(TunnelRequest {
                ssh,
                password: secrets.ssh_password.as_ref(),
                key_passphrase: secrets.ssh_key_passphrase.as_ref(),
                known_hosts: &host_keys.known_hosts,
                learn_into: host_keys.learn_into.as_deref(),
                trusted_fingerprint: host_keys.trusted_fingerprint.as_deref(),
            })
            .await?
        }
        SshClient::OpenSsh => {
            let env = host_keys
                .openssh
                .as_ref()
                .ok_or_else(|| SshError::Other(openssh::NOT_FOUND.into()))?;
            Tunnel::open_openssh(OpenSshRequest {
                ssh,
                env,
                db_host: &config.host,
                db_port: config.port,
                key_passphrase: secrets.ssh_key_passphrase.as_ref(),
                trusted_fingerprint: host_keys.trusted_fingerprint.as_deref(),
            })
            .await?
        }
    };
    Ok(Some(Arc::new(tunnel)))
}

/// A live route to one server: the driver, the settings, the secrets (in
/// memory only) and the tunnel. Each [`Connector::session`] is a new server
/// connection over that route.
pub struct Connector {
    driver: Arc<dyn Driver>,
    config: ConnectionConfig,
    secrets: Secrets,
    tunnel: Option<Arc<Tunnel>>,
    password_env: Option<PasswordEnv>,
    /// What the password command last printed, and when (memory only).
    command_password: Mutex<Option<(Secret, Instant)>>,
    /// Where the last session's password came from, and a password file
    /// that was ignored.
    last_password: Mutex<(Option<PasswordOrigin>, Option<String>)>,
}

impl std::fmt::Debug for Connector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connector")
            .field("config", &self.config)
            .field("tunnel", &self.tunnel)
            .finish_non_exhaustive()
    }
}

impl Connector {
    /// Validates the settings and opens the SSH tunnel if there is one. No
    /// database connection is made yet.
    pub async fn open(
        driver: Arc<dyn Driver>,
        config: ConnectionConfig,
        secrets: Secrets,
        host_keys: &HostKeyStore,
    ) -> Result<Self> {
        config.validate()?;
        if !driver.engines().contains(&config.engine) {
            return Err(Error::Config(format!(
                "no driver for {}",
                config.engine.label()
            )));
        }
        let tunnel = open_tunnel(&config, &secrets, host_keys).await?;
        Ok(Self {
            driver,
            config,
            secrets,
            tunnel,
            password_env: host_keys.password.clone(),
            command_password: Mutex::new(None),
            last_password: Mutex::new((None, None)),
        })
    }

    /// The password of a new session in `database`: the stored one, a
    /// password file's, or the command's (run again once
    /// [`COMMAND_PASSWORD_REUSE`] has passed).
    async fn password(&self, database: Option<&str>) -> Result<Option<Secret>> {
        let command = !self.config.password_source.is_stored();
        let recent = self
            .command_password
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .filter(|(_, at)| command && at.elapsed() < COMMAND_PASSWORD_REUSE);
        if let Some((secret, _)) = recent {
            return Ok(Some(secret));
        }
        let resolved = password::resolve(
            &self.config,
            self.secrets.password.as_ref(),
            database,
            self.password_env.as_ref(),
        )
        .await?;
        if command && let Some(secret) = &resolved.password {
            *self
                .command_password
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some((secret.clone(), Instant::now()));
        }
        *self
            .last_password
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = (resolved.origin, resolved.warning);
        Ok(resolved.password)
    }

    /// Where the password of the last session came from (`None`: no
    /// password was sent).
    pub fn password_origin(&self) -> Option<PasswordOrigin> {
        self.last_password
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0
            .clone()
    }

    /// A password file the last session could not use, and why.
    pub fn password_warning(&self) -> Option<String> {
        self.last_password
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .1
            .clone()
    }

    /// Opens a session in `database`, or in the connection's default
    /// database when `None`.
    pub async fn session(&self, database: Option<&str>) -> Result<Arc<dyn Session>> {
        let password = self.password(database).await?;
        let params = ConnectParams {
            engine: self.config.engine,
            host: self.config.host.clone(),
            port: self.config.port,
            tunnel: self.tunnel.clone(),
            user: self.config.user.clone(),
            password,
            database: database
                .map(str::to_string)
                .or_else(|| self.config.database.clone())
                .filter(|name| !name.is_empty()),
            file: self.config.file.clone(),
            tls: self.config.tls.clone(),
            read_only: self.config.read_only,
            options: self.config.options.clone(),
        };
        let mark = self.tunnel.as_ref().map(|tunnel| tunnel.forward_mark());
        let session = match self.driver.connect(&params).await {
            Ok(session) => session,
            // A password file was ignored: say so, rather than only that a
            // password is missing.
            Err(Error::PasswordRequired) if self.password_warning().is_some() => {
                let warning = self.password_warning().unwrap_or_default();
                return Err(Error::Connect(format!(
                    "{warning} The server asks for a password and none was given."
                )));
            }
            Err(error) => {
                // OpenSSH tells why a forward failed only in its messages.
                if let (Some(tunnel), Some(mark)) = (&self.tunnel, mark)
                    && let Some(refused) = tunnel
                        .forward_failure(mark, &self.config.host, self.config.port)
                        .await
                {
                    return Err(refused.into());
                }
                return Err(error);
            }
        };
        if let Err(error) = self.bootstrap(&session).await {
            session.close().await;
            return Err(error);
        }
        Ok(session)
    }

    /// Makes the default schema current, then runs the bootstrap
    /// statements, on a session that just opened. A failing statement fails
    /// the session, naming it, unless the connection ignores such errors.
    async fn bootstrap(&self, session: &Arc<dyn Session>) -> Result<()> {
        let schema = self
            .config
            .default_schema
            .as_deref()
            .map(str::trim)
            .filter(|schema| !schema.is_empty() && session.dialect().has_schemas())
            .map(|schema| {
                format!(
                    "SET search_path TO {}, public",
                    session.dialect().quote_ident(schema)
                )
            });
        // The default schema is a setting the user chose, never ignored.
        if let Some(sql) = schema {
            exec::collect(session.clone(), sql.as_str())
                .await
                .map_err(|error| {
                    Error::Connect(format!("the default schema could not be set: {error}"))
                })?;
        }
        let statements = self
            .config
            .init_sql
            .iter()
            .map(|sql| sql.trim())
            .enumerate();
        for (index, sql) in statements.filter(|(_, sql)| !sql.is_empty()) {
            // Checked where it runs too: a store edited by hand, or written
            // by an older version, does not go through the form.
            if let Some(reason) = crate::config::bootstrap_refusal(sql, self.config.engine) {
                return Err(Error::Config(format!(
                    "bootstrap statement {} {reason}",
                    index + 1
                )));
            }
            // The statement number only: Kerno never logs SQL text.
            tracing::debug!(statement = index + 1, "bootstrap statement");
            if let Err(error) = exec::collect(session.clone(), sql).await {
                if !self.config.ignore_init_errors {
                    return Err(Error::Connect(format!(
                        "bootstrap statement {} failed: {sql}: {error}",
                        index + 1
                    )));
                }
                tracing::warn!(statement = index + 1, %error, "bootstrap statement failed; ignored");
            }
        }
        // Whatever ran above, a read-only connection ends its setup read-only
        // (bootstrap statements are also refused when they name that state).
        if self.config.read_only
            && let Some(sql) = session.dialect().read_only_session_sql()
        {
            exec::collect(session.clone(), sql).await.map_err(|error| {
                Error::Connect(format!("the session could not be made read-only: {error}"))
            })?;
        }
        Ok(())
    }

    /// The connection's settings.
    pub fn config(&self) -> &ConnectionConfig {
        &self.config
    }

    /// Whether the SSH tunnel died (always `false` without a tunnel).
    pub fn tunnel_closed(&self) -> bool {
        self.tunnel
            .as_ref()
            .is_some_and(|tunnel| tunnel.is_closed())
    }
}
