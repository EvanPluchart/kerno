//! Opening sessions for a saved connection: brings up the SSH tunnel when
//! one is configured, then asks the driver for sessions that go through it.

use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{ConnectParams, ConnectionConfig, Secrets};
use crate::driver::{Driver, Session};
use crate::error::{Error, Result};
use crate::exec;
use crate::ssh::{Tunnel, TunnelRequest};

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
}

/// A live route to one server: the driver, the settings, the secrets (in
/// memory only) and the tunnel. Each [`Connector::session`] is a new server
/// connection over that route.
pub struct Connector {
    driver: Arc<dyn Driver>,
    config: ConnectionConfig,
    secrets: Secrets,
    tunnel: Option<Arc<Tunnel>>,
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
        let tunnel = match &config.ssh {
            None => None,
            Some(ssh) => Some(Arc::new(
                Tunnel::open(TunnelRequest {
                    ssh,
                    password: secrets.ssh_password.as_ref(),
                    key_passphrase: secrets.ssh_key_passphrase.as_ref(),
                    known_hosts: &host_keys.known_hosts,
                    learn_into: host_keys.learn_into.as_deref(),
                    trusted_fingerprint: host_keys.trusted_fingerprint.as_deref(),
                })
                .await?,
            )),
        };
        Ok(Self {
            driver,
            config,
            secrets,
            tunnel,
        })
    }

    /// Opens a session in `database`, or in the connection's default
    /// database when `None`.
    pub async fn session(&self, database: Option<&str>) -> Result<Arc<dyn Session>> {
        let params = ConnectParams {
            engine: self.config.engine,
            host: self.config.host.clone(),
            port: self.config.port,
            tunnel: self.tunnel.clone(),
            user: self.config.user.clone(),
            password: self.secrets.password.clone(),
            database: database
                .map(str::to_string)
                .or_else(|| self.config.database.clone())
                .filter(|name| !name.is_empty()),
            tls: self.config.tls.clone(),
            read_only: self.config.read_only,
            options: self.config.options.clone(),
        };
        let session = self.driver.connect(&params).await?;
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
            tracing::debug!(statement = index + 1, sql, "bootstrap statement");
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
