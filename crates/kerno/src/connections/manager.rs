//! The saved connections and their live state: saving, connecting,
//! disconnecting, reconnecting, and asking the user for what a connection
//! attempt turns out to need (a password, trust in an SSH host key).
//!
//! All network, storage and secret-store work runs off the UI thread; this
//! entity only holds state and reacts to results.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{App, Context, EventEmitter, Task};
use kerno_core::config::{ConnectionConfig, Engine, Secret, Secrets, SshAuth};
use kerno_core::connector::{Connector, HostKeyStore};
use kerno_core::secrets::{self, SecretStore};
use kerno_core::ssh::SshError;
use kerno_core::storage::{SavedConnection, Storage};
use kerno_core::{Driver, Error, Session};

use crate::bridge;
use crate::state::AppState;

/// How often an open connection is checked.
const KEEP_ALIVE: Duration = Duration::from_secs(30);

/// The drivers the app was built with, by engine.
#[derive(Clone, Default)]
pub struct Drivers(Vec<Arc<dyn Driver>>);

impl Drivers {
    /// Registers a driver for the engines it speaks.
    pub fn with(mut self, driver: impl Driver) -> Self {
        self.0.push(Arc::new(driver));
        self
    }

    fn get(&self, engine: Engine) -> kerno_core::Result<Arc<dyn Driver>> {
        self.0
            .iter()
            .find(|driver| driver.engines().contains(&engine))
            .cloned()
            .ok_or_else(|| {
                Error::Config(format!("this build has no driver for {}", engine.label()))
            })
    }
}

/// Where a connection stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Not connected.
    Disconnected,
    /// A connection attempt is running.
    Connecting,
    /// Waiting for the user to answer a [`InputRequest`].
    WaitingForInput,
    /// Connected. Carries the server's version string.
    Connected { version: String },
    /// The last attempt failed, or the connection was lost and could not
    /// be re-established.
    Failed { message: String },
}

/// Something a connection attempt needs from the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputRequest {
    /// The database password (not saved, by the user's choice).
    DatabasePassword,
    /// The password of the SSH user.
    SshPassword,
    /// The passphrase of the SSH private key. `retry` is true when one was
    /// given and did not open the key.
    SshKeyPassphrase { path: String, retry: bool },
    /// The SSH server is not known: show its fingerprint and ask.
    TrustHostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
    },
}

/// The user's answer to an [`InputRequest`].
#[derive(Debug, Clone)]
pub enum InputAnswer {
    DatabasePassword(Secret),
    SshPassword(Secret),
    SshKeyPassphrase(Secret),
    /// The fingerprint the user agreed to trust.
    TrustHostKey(String),
}

/// What observers of [`Connections`] are told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionsEvent {
    /// The list of saved connections changed.
    ListChanged,
    /// A connection's status changed.
    StatusChanged { id: String },
    /// A connection attempt cannot go on without the user.
    NeedsInput { id: String, request: InputRequest },
    /// Saving, deleting or importing failed.
    Failed { message: String },
}

/// The outcome of "Test connection".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestReport {
    /// The server's version string.
    pub version: String,
    /// Round-trip time of one ping on the established connection.
    pub latency: Duration,
}

/// The live half of a connection.
#[derive(Default)]
struct Runtime {
    status: Option<Status>,
    /// Secrets for this run: loaded from the secret store, or typed.
    secrets: Option<Secrets>,
    /// An SSH host key fingerprint the user agreed to trust.
    trusted_fingerprint: Option<String>,
    /// A secret was typed at a prompt during this attempt.
    typed_secret: bool,
    connector: Option<Arc<Connector>>,
    /// The session used for the navigator and for keep-alive.
    meta: Option<Arc<dyn Session>>,
    attempt: Option<Task<()>>,
    keep_alive: Option<Task<()>>,
}

/// The saved connections and their live state.
pub struct Connections {
    drivers: Drivers,
    saved: Vec<SavedConnection>,
    runtime: HashMap<String, Runtime>,
    /// The user's own SSH known-hosts file, read but never written.
    user_known_hosts: Option<PathBuf>,
}

impl EventEmitter<ConnectionsEvent> for Connections {}

/// What one connection attempt produced.
type Attempt = kerno_core::Result<(Arc<Connector>, Arc<dyn Session>)>;

impl Connections {
    /// The manager for `saved`, with the drivers the app was built with.
    /// `user_known_hosts` is the user's own SSH known-hosts file
    /// (`~/.ssh/known_hosts`): hosts trusted there are trusted here.
    pub fn new(
        saved: Vec<SavedConnection>,
        drivers: Drivers,
        user_known_hosts: Option<PathBuf>,
    ) -> Self {
        Self {
            drivers,
            saved,
            runtime: HashMap::new(),
            user_known_hosts,
        }
    }

    /// SSH host keys are looked up in the user's own known-hosts file and
    /// in Kerno's; keys the user accepts are recorded in Kerno's only.
    fn host_keys(&self, trusted_fingerprint: Option<String>, cx: &App) -> HostKeyStore {
        let kerno_file = AppState::get(cx).paths.known_hosts();
        HostKeyStore {
            known_hosts: self
                .user_known_hosts
                .iter()
                .cloned()
                .chain([kerno_file.clone()])
                .collect(),
            learn_into: Some(kerno_file),
            trusted_fingerprint,
        }
    }

    /// Saved connections, in the user's order.
    pub fn saved(&self) -> &[SavedConnection] {
        &self.saved
    }

    /// A saved connection by id.
    pub fn get(&self, id: &str) -> Option<&SavedConnection> {
        self.saved.iter().find(|connection| connection.id == id)
    }

    /// Where a connection stands.
    pub fn status(&self, id: &str) -> Status {
        self.runtime
            .get(id)
            .and_then(|runtime| runtime.status.clone())
            .unwrap_or(Status::Disconnected)
    }

    /// The navigator's session of a connected connection.
    pub fn meta_session(&self, id: &str) -> Option<Arc<dyn Session>> {
        self.runtime.get(id)?.meta.clone()
    }

    /// Opens another session on a connected connection, in `database` or
    /// the connection's default one. Query tabs each use their own.
    pub fn open_session(
        &self,
        id: &str,
        database: Option<String>,
        cx: &App,
    ) -> Task<kerno_core::Result<Arc<dyn Session>>> {
        let connector = self
            .runtime
            .get(id)
            .and_then(|runtime| runtime.connector.clone());
        bridge::spawn(cx, async move {
            let connector =
                connector.ok_or_else(|| Error::Closed("the connection is not open".into()))?;
            connector.session(database.as_deref()).await
        })
    }

    fn set_status(&mut self, id: &str, status: Status, cx: &mut Context<Self>) {
        let runtime = self.runtime.entry(id.to_string()).or_default();
        if matches!(status, Status::Failed { .. }) && std::mem::take(&mut runtime.typed_secret) {
            // A typed secret that did not work is asked for again.
            runtime.secrets = None;
        }
        runtime.status = Some(status);
        cx.emit(ConnectionsEvent::StatusChanged { id: id.to_string() });
        cx.notify();
    }

    fn fail(&mut self, error: impl std::fmt::Display, cx: &mut Context<Self>) {
        cx.emit(ConnectionsEvent::Failed {
            message: error.to_string(),
        });
    }

    // Saved list -------------------------------------------------------------

    /// Saves a connection (a new one when `id` is `None`) and its secrets.
    /// With "save password" off, the secrets are kept for this run only and
    /// removed from the secret store.
    ///
    /// `trusted_fingerprint` is an SSH host key the user accepted while
    /// testing the connection. With `connect`, the connection is opened
    /// (or re-opened with its new settings) once saved.
    pub fn save(
        &mut self,
        id: Option<String>,
        config: ConnectionConfig,
        secrets: Secrets,
        trusted_fingerprint: Option<String>,
        connect: bool,
        cx: &mut Context<Self>,
    ) {
        let state = AppState::get(cx);
        let (storage, store) = (state.storage.clone(), state.secrets.clone());
        let persisted = persist(storage, store, id, config.clone(), secrets.clone());
        let saving = bridge::spawn_blocking(cx, persisted);
        cx.spawn(async move |this, cx| {
            let outcome = saving.await;
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(id) => {
                    match this.saved.iter_mut().find(|connection| connection.id == id) {
                        Some(connection) => connection.config = config,
                        None => this.saved.push(SavedConnection {
                            id: id.clone(),
                            config,
                        }),
                    }
                    // What was typed is what the next connect uses.
                    let runtime = this.runtime.entry(id.clone()).or_default();
                    runtime.secrets = Some(secrets);
                    if trusted_fingerprint.is_some() {
                        runtime.trusted_fingerprint = trusted_fingerprint;
                    }
                    cx.emit(ConnectionsEvent::ListChanged);
                    cx.notify();
                    if connect {
                        // Changed settings only apply to a new connection.
                        this.disconnect(&id, cx);
                        this.connect(&id, cx);
                    }
                }
                Err(error) => this.fail(format!("The connection could not be saved: {error}"), cx),
            });
        })
        .detach();
    }

    /// Saves a copy of a connection, with its secrets, named "… copy".
    pub fn duplicate(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(original) = self.get(id).cloned() else {
            return;
        };
        let known = self
            .runtime
            .get(id)
            .and_then(|runtime| runtime.secrets.clone());
        let store = AppState::get(cx).secrets.clone();
        let original_id = original.id.clone();
        let loading = bridge::spawn_blocking(cx, move || match known {
            Some(secrets) => Ok(secrets),
            None => secrets::load_secrets(store.as_ref(), &original_id),
        });
        cx.spawn(async move |this, cx| {
            let secrets = loading.await;
            let _ = this.update(cx, |this, cx| match secrets {
                Ok(secrets) => {
                    let mut config = original.config;
                    config.name = format!("{} copy", config.name);
                    this.save(None, config, secrets, None, false, cx);
                }
                Err(error) => this.fail(
                    format!("The connection could not be duplicated: {error}"),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Disconnects and deletes a connection, its history, saved queries and
    /// secrets.
    pub fn delete(&mut self, id: &str, cx: &mut Context<Self>) {
        self.disconnect(id, cx);
        let state = AppState::get(cx);
        let (storage, store) = (state.storage.clone(), state.secrets.clone());
        let deleted_id = id.to_string();
        let deleting = bridge::spawn_blocking(cx, {
            let id = deleted_id.clone();
            move || {
                storage.delete_connection(&id)?;
                secrets::delete_secrets(store.as_ref(), &id)
            }
        });
        cx.spawn(async move |this, cx| {
            let outcome = deleting.await;
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(()) => {
                    this.saved.retain(|connection| connection.id != deleted_id);
                    this.runtime.remove(&deleted_id);
                    cx.emit(ConnectionsEvent::ListChanged);
                    cx.notify();
                }
                Err(error) => {
                    this.fail(format!("The connection could not be deleted: {error}"), cx)
                }
            });
        })
        .detach();
    }

    /// Adds imported connections. They come without secrets.
    pub fn import(&mut self, configs: Vec<ConnectionConfig>, cx: &mut Context<Self>) {
        for config in configs {
            self.save(None, config, Secrets::default(), None, false, cx);
        }
    }

    /// The secrets of a connection, to edit it: the ones of this run, or
    /// the stored ones.
    pub fn secrets_of(&self, id: &str, cx: &App) -> Task<kerno_core::Result<Secrets>> {
        let known = self
            .runtime
            .get(id)
            .and_then(|runtime| runtime.secrets.clone());
        let store = AppState::get(cx).secrets.clone();
        let id = id.to_string();
        bridge::spawn_blocking(cx, move || match known {
            Some(secrets) => Ok(secrets),
            None => secrets::load_secrets(store.as_ref(), &id),
        })
    }

    /// How many connections are open.
    pub fn connected_count(&self) -> usize {
        self.runtime
            .values()
            .filter(|runtime| matches!(runtime.status, Some(Status::Connected { .. })))
            .count()
    }

    // Connecting ---------------------------------------------------------------

    /// Starts connecting. Progress arrives as [`ConnectionsEvent`]s.
    pub fn connect(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(saved) = self.get(id).cloned() else {
            return;
        };
        if matches!(
            self.status(id),
            Status::Connecting | Status::Connected { .. }
        ) {
            return;
        }
        self.set_status(id, Status::Connecting, cx);

        let runtime = self.runtime.entry(id.to_string()).or_default();
        let known_secrets = runtime.secrets.clone();
        let trusted_fingerprint = runtime.trusted_fingerprint.clone();
        let driver = self.drivers.get(saved.config.engine);
        let store = AppState::get(cx).secrets.clone();
        let host_keys = self.host_keys(trusted_fingerprint, cx);
        let config = saved.config.clone();
        let connection_id = saved.id.clone();

        let loading = bridge::spawn_blocking(cx, {
            let id = connection_id.clone();
            move || match known_secrets {
                Some(secrets) => Ok(secrets),
                None => secrets::load_secrets(store.as_ref(), &id),
            }
        });
        let attempt = cx.spawn(async move |this, cx| {
            let secrets = match loading.await {
                Ok(secrets) => secrets,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.set_status(
                            &connection_id,
                            Status::Failed {
                                message: error.to_string(),
                            },
                            cx,
                        )
                    });
                    return;
                }
            };
            // A password the user chose not to save is asked for up front
            // rather than discovered through a failed login.
            if !config.save_password && secrets.password.is_none() {
                let _ = this.update(cx, |this, cx| {
                    this.runtime
                        .entry(connection_id.clone())
                        .or_default()
                        .secrets = Some(secrets);
                    this.ask(&connection_id, InputRequest::DatabasePassword, cx);
                });
                return;
            }
            let outcome: Attempt = match driver {
                Err(error) => Err(error),
                Ok(driver) => {
                    let (config, secrets) = (config.clone(), secrets.clone());
                    let opening = cx.update(|cx| {
                        bridge::spawn(cx, async move {
                            let connector = Arc::new(
                                Connector::open(driver, config, secrets, &host_keys).await?,
                            );
                            let meta = connector.session(None).await?;
                            Ok((connector, meta))
                        })
                    });
                    opening.await
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.runtime
                    .entry(connection_id.clone())
                    .or_default()
                    .secrets = Some(secrets);
                this.finish_attempt(&connection_id, &config, outcome, cx);
            });
        });
        self.runtime.entry(id.to_string()).or_default().attempt = Some(attempt);
    }

    fn ask(&mut self, id: &str, request: InputRequest, cx: &mut Context<Self>) {
        self.set_status(id, Status::WaitingForInput, cx);
        cx.emit(ConnectionsEvent::NeedsInput {
            id: id.to_string(),
            request,
        });
    }

    fn finish_attempt(
        &mut self,
        id: &str,
        config: &ConnectionConfig,
        outcome: Attempt,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Ok((connector, meta)) => {
                let version = meta.server().version.clone();
                let keep_alive = self.watch(id, cx);
                let runtime = self.runtime.entry(id.to_string()).or_default();
                runtime.connector = Some(connector);
                runtime.meta = Some(meta);
                runtime.keep_alive = Some(keep_alive);
                // A secret typed at a prompt worked: keep it, if this
                // connection saves its secrets (an imported connection, or
                // one whose secret-store entry is gone).
                if std::mem::take(&mut runtime.typed_secret)
                    && config.save_password
                    && let Some(secrets) = runtime.secrets.clone()
                {
                    let (store, id) = (AppState::get(cx).secrets.clone(), id.to_string());
                    let saving = bridge::spawn_blocking(cx, move || {
                        secrets::save_secrets(store.as_ref(), &id, &secrets)
                    });
                    cx.spawn(async move |this, cx| {
                        if let Err(error) = saving.await {
                            let _ = this.update(cx, |this, cx| {
                                this.fail(format!("The password could not be saved: {error}"), cx)
                            });
                        }
                    })
                    .detach();
                }
                self.set_status(id, Status::Connected { version }, cx);
            }
            // Saved without a password (imported, or removed from the
            // secret store) and the server wants one.
            Err(Error::PasswordRequired) => self.ask(id, InputRequest::DatabasePassword, cx),
            Err(Error::Ssh(error)) => match *error {
                SshError::UnknownHostKey {
                    host,
                    port,
                    algorithm,
                    fingerprint,
                } => self.ask(
                    id,
                    InputRequest::TrustHostKey {
                        host,
                        port,
                        algorithm,
                        fingerprint,
                    },
                    cx,
                ),
                SshError::Passphrase { path, reason } => self.ask(
                    id,
                    InputRequest::SshKeyPassphrase {
                        path,
                        retry: reason.contains("wrong"),
                    },
                    cx,
                ),
                SshError::PasswordMissing => self.ask(id, InputRequest::SshPassword, cx),
                // A refused SSH password that was never saved: ask again.
                SshError::AuthRejected {
                    method: "password", ..
                } if matches!(
                    config.ssh.as_ref().map(|ssh| &ssh.auth),
                    Some(SshAuth::Password)
                ) && !config.save_password =>
                {
                    self.ask(id, InputRequest::SshPassword, cx)
                }
                other => self.set_status(
                    id,
                    Status::Failed {
                        message: Error::from(other).to_string(),
                    },
                    cx,
                ),
            },
            Err(error) => self.set_status(
                id,
                Status::Failed {
                    message: error.to_string(),
                },
                cx,
            ),
        }
    }

    /// Continues a connection attempt with the user's answer.
    pub fn answer(&mut self, id: &str, answer: InputAnswer, cx: &mut Context<Self>) {
        let runtime = self.runtime.entry(id.to_string()).or_default();
        let secrets = runtime.secrets.get_or_insert_with(Secrets::default);
        let typed = !matches!(answer, InputAnswer::TrustHostKey(_));
        match answer {
            InputAnswer::DatabasePassword(password) => secrets.password = Some(password),
            InputAnswer::SshPassword(password) => secrets.ssh_password = Some(password),
            InputAnswer::SshKeyPassphrase(passphrase) => {
                secrets.ssh_key_passphrase = Some(passphrase)
            }
            InputAnswer::TrustHostKey(fingerprint) => {
                runtime.trusted_fingerprint = Some(fingerprint)
            }
        }
        runtime.typed_secret |= typed;
        runtime.status = Some(Status::Disconnected);
        self.connect(id, cx);
    }

    /// Gives up a connection attempt that was waiting for the user.
    pub fn cancel_input(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.status(id) == Status::WaitingForInput {
            // What was typed for this attempt is not kept for the next.
            if let Some(runtime) = self.runtime.get_mut(id) {
                let typed = std::mem::take(&mut runtime.typed_secret);
                let saves = self
                    .saved
                    .iter()
                    .any(|c| c.id == id && c.config.save_password);
                if typed || !saves {
                    runtime.secrets = None;
                }
            }
            self.set_status(id, Status::Disconnected, cx);
        }
    }

    /// Closes a connection's sessions and tunnel.
    pub fn disconnect(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.get_mut(id) else {
            return;
        };
        runtime.attempt = None;
        runtime.keep_alive = None;
        runtime.connector = None;
        if let Some(meta) = runtime.meta.take() {
            bridge::spawn(cx, async move { meta.close().await }).detach();
        }
        self.set_status(id, Status::Disconnected, cx);
    }

    /// Checks the connection every [`KEEP_ALIVE`], and reconnects when the
    /// server went away.
    fn watch(&self, id: &str, cx: &mut Context<Self>) -> Task<()> {
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(KEEP_ALIVE).await;
                let Ok(Some(meta)) = this.read_with(cx, |this, _| this.meta_session(&id)) else {
                    return;
                };
                let ping = cx.update(|cx| bridge::spawn(cx, async move { meta.ping().await }));
                if ping.await.is_err() {
                    let _ = this.update(cx, |this, cx| this.connection_lost(&id, cx));
                    return;
                }
            }
        })
    }

    /// Called when a session of this connection reported that the server
    /// is gone: reconnects once, with the secrets already known.
    pub fn connection_lost(&mut self, id: &str, cx: &mut Context<Self>) {
        if !matches!(self.status(id), Status::Connected { .. }) {
            return;
        }
        if let Some(runtime) = self.runtime.get_mut(id) {
            runtime.keep_alive = None;
            runtime.connector = None;
            runtime.meta = None;
            runtime.status = Some(Status::Disconnected);
        }
        tracing::info!(connection = id, "connection lost; reconnecting");
        self.connect(id, cx);
    }

    /// Tries a connection without saving it, and reports the server version
    /// and latency. `trusted_fingerprint` is an SSH host key the user just
    /// agreed to trust.
    pub fn test(
        &self,
        config: ConnectionConfig,
        secrets: Secrets,
        trusted_fingerprint: Option<String>,
        cx: &App,
    ) -> Task<kerno_core::Result<TestReport>> {
        let driver = self.drivers.get(config.engine);
        let host_keys = self.host_keys(trusted_fingerprint, cx);
        bridge::spawn(cx, async move {
            let connector = Connector::open(driver?, config, secrets, &host_keys).await?;
            let session = connector.session(None).await?;
            let started = Instant::now();
            session.ping().await?;
            let latency = started.elapsed();
            let version = session.server().version.clone();
            session.close().await;
            Ok(TestReport { version, latency })
        })
    }
}

/// Writes a connection and its secrets; returns its id.
fn persist(
    storage: Arc<Storage>,
    store: Arc<dyn SecretStore>,
    id: Option<String>,
    config: ConnectionConfig,
    secrets: Secrets,
) -> impl FnOnce() -> kerno_core::Result<String> {
    move || {
        let id = storage.save_connection(id.as_deref(), &config)?;
        if config.save_password {
            secrets::save_secrets(store.as_ref(), &id, &secrets)?;
        } else {
            secrets::delete_secrets(store.as_ref(), &id)?;
        }
        Ok(id)
    }
}
