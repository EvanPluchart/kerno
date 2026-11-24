//! The saved connections and their live state: saving, connecting,
//! disconnecting, reconnecting, and asking the user for what a connection
//! attempt turns out to need (a password, trust in an SSH host key).
//!
//! All network, storage and secret-store work runs off the UI thread; this
//! entity only holds state and reacts to results.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui_kit::{App, Context, EventEmitter, Task};
use kerno_core::config::{
    ConnectionConfig, Engine, FolderLook, ParsedUrl, Secret, Secrets, SshAuth, all_folders,
    folder_name, folder_path, in_folder, parent_folder, rebase_folder,
};
use kerno_core::connector::{Connector, HostKeyStore};
use kerno_core::password::{PasswordEnv, PasswordOrigin};
use kerno_core::secrets::{self, SecretStore};
use kerno_core::ssh::SshError;
use kerno_core::storage::{SavedConnection, Storage};
use kerno_core::{Driver, Error, Session};

use crate::bridge;
use crate::state::AppState;

/// How often an open connection is checked.
const KEEP_ALIVE: Duration = Duration::from_secs(5);
/// How long a check may take. A check that times out is tried once more
/// at once (a long catalog read on the same session holds it up); two in
/// a row, or a check that fails, and the connection is lost: a connection
/// that stopped answering is noticed within 10 s of the next check.
const PING_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one attempt to reconnect may take before the next one.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The wait before reconnect attempt `attempt` (from 0): 0.5 s, 1 s, then
/// every 2 s for about a minute, then every 5 s.
fn retry_delay(attempt: u32) -> Duration {
    Duration::from_millis(match attempt {
        0 => 500,
        1 => 1_000,
        2..30 => 2_000,
        _ => 5_000,
    })
}

/// A failure that may pass by itself: the server or the network is away.
/// A refused login, a host key or a missing secret is not retried.
fn passing(error: &Error) -> bool {
    matches!(
        error,
        Error::Connect(_) | Error::Closed(_) | Error::Io { .. }
    )
}

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
    /// The user confirmed the deletion of a connection: the window ends
    /// what its tabs hold (asking first), then calls `delete`.
    DeleteRequested { id: String },
    /// An open connection was saved with "connect": its new settings need a
    /// new session, which the window opens once what its tabs hold is ended.
    ReconnectRequested { id: String },
    /// Something the user should know about a connection that opened.
    Warned { message: String },
}

/// The outcome of "Test connection".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestReport {
    /// The server's version string.
    pub version: String,
    /// Round-trip time of one ping on the established connection.
    pub latency: Duration,
    /// Where the password came from ("Password from ~/.pgpass"), or why a
    /// password file was not used; `None` when no password was sent.
    pub password: Option<String>,
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
    /// The other sessions opened on this connection (tabs, dialogs, the
    /// navigator's other databases): disconnecting closes those still open,
    /// whoever holds them.
    opened: Opened,
    attempt: Option<Task<()>>,
    keep_alive: Option<Task<()>>,
    /// Where the open connection's password came from, in words (J3).
    password_origin: Option<String>,
    /// Reconnecting after a loss: the attempts that failed so far.
    reconnect: Option<u32>,
}

/// The saved connections and their live state.
pub struct Connections {
    drivers: Drivers,
    saved: Vec<SavedConnection>,
    runtime: HashMap<String, Runtime>,
    /// The user's own SSH known-hosts file, read but never written by the
    /// built-in client (OpenSSH records the keys the user trusts there).
    user_known_hosts: Option<PathBuf>,
    /// The system's OpenSSH, when there is one.
    openssh: Option<kerno_core::openssh::OpenSshEnv>,
    /// Where password files and commands are looked for (J3).
    password_env: Option<PasswordEnv>,
    /// The last write of moved or renamed connections.
    writing: Option<Task<()>>,
    /// Connections opened from a URL (`kerno <url>`) and not saved: listed
    /// in `saved` for this run, never written to storage or the secret
    /// store until the user saves them.
    unsaved: HashSet<String>,
}

impl EventEmitter<ConnectionsEvent> for Connections {}

/// Sessions opened through [`Connections::open_session`], not kept alive by
/// the list.
type Opened = Arc<std::sync::Mutex<Vec<std::sync::Weak<dyn Session>>>>;

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
            openssh: None,
            password_env: None,
            writing: None,
            unsaved: HashSet::new(),
        }
    }

    /// Opens what a connection URL names (`kerno <url>`): the saved
    /// connection to the same server and database if there is one, with
    /// its own secrets; otherwise an unsaved one, kept for this run only,
    /// with the URL's password in memory. Connects it; returns its id.
    pub fn open_url(&mut self, url: &ParsedUrl, cx: &mut Context<Self>) -> String {
        let (unsaved, secrets) = kerno_core::instance::unsaved_connection(url);
        let found = self
            .saved
            .iter()
            .find(|saved| kerno_core::instance::same_target(&saved.config, &unsaved.config));
        let id = match found {
            Some(saved) => saved.id.clone(),
            None => {
                let id = unsaved.id.clone();
                self.runtime.entry(id.clone()).or_default().secrets = Some(secrets);
                self.unsaved.insert(id.clone());
                self.saved.push(unsaved);
                cx.emit(ConnectionsEvent::ListChanged);
                cx.notify();
                id
            }
        };
        self.connect(&id, cx);
        id
    }

    /// Whether a connection was opened from a URL and is not saved.
    pub fn is_unsaved(&self, id: &str) -> bool {
        self.unsaved.contains(id)
    }

    /// Sets where password files and commands are looked for (J3).
    pub fn set_password_env(&mut self, env: Option<PasswordEnv>) {
        self.password_env = env;
    }

    /// Where password files and commands are looked for.
    pub fn password_env(&self) -> Option<&PasswordEnv> {
        self.password_env.as_ref()
    }

    /// Where the password of an open connection came from, in words
    /// ("Password from the command `aws`"); `None` when none was sent.
    pub fn password_origin(&self, id: &str) -> Option<String> {
        self.runtime.get(id)?.password_origin.clone()
    }

    /// Sets where the system's OpenSSH is (`None`: not found).
    pub fn set_openssh(&mut self, openssh: Option<kerno_core::openssh::OpenSshEnv>) {
        self.openssh = openssh;
    }

    /// The system's OpenSSH, when there is one.
    pub fn openssh(&self) -> Option<&kerno_core::openssh::OpenSshEnv> {
        self.openssh.as_ref()
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
            openssh: self.openssh.clone(),
            password: self.password_env.clone(),
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
        let runtime = self.runtime.get(id);
        let connector = runtime.and_then(|runtime| runtime.connector.clone());
        let opened = runtime.map(|runtime| runtime.opened.clone());
        bridge::spawn(cx, async move {
            let connector =
                connector.ok_or_else(|| Error::Closed("the connection is not open".into()))?;
            let session = connector.session(database.as_deref()).await?;
            if let Some(opened) = opened {
                let mut opened = opened
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                opened.retain(|session| session.strong_count() > 0);
                opened.push(Arc::downgrade(&session));
            }
            Ok(session)
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
        // An unsaved connection is stored under the id it has.
        let insert = id.as_deref().is_some_and(|id| self.unsaved.contains(id));
        let persisted = persist(storage, store, id, insert, config.clone(), secrets.clone());
        let saving = bridge::spawn_blocking(cx, persisted);
        cx.spawn(async move |this, cx| {
            let outcome = saving.await;
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(id) => {
                    this.unsaved.remove(&id);
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
                        // Changed settings only apply to a new connection;
                        // an open one is closed by the window, which asks
                        // about what its tabs hold first.
                        match this.status(&id) {
                            Status::Disconnected | Status::Failed { .. } => this.connect(&id, cx),
                            _ => cx.emit(ConnectionsEvent::ReconnectRequested { id }),
                        }
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
        let nothing_saved = original.config.engine.is_file() || !original.config.save_password;
        let loading = bridge::spawn_blocking(cx, move || match known {
            Some(secrets) => Ok(secrets),
            None if nothing_saved => Ok(Secrets::default()),
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
        keep_folder(self.get(id).and_then(|saved| folder_of(&saved.config)), cx);
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
                    // Its saved queries went with it; its files leave its
                    // list (K38).
                    AppState::update_settings(cx, |settings| {
                        settings.forget_connection(&deleted_id)
                    });
                    crate::library::SavedQueries::reload(cx);
                    this.saved.retain(|connection| connection.id != deleted_id);
                    this.runtime.remove(&deleted_id);
                    this.unsaved.remove(&deleted_id);
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

    /// The secrets of a connection, to edit it: the ones of this run, or
    /// the stored ones.
    pub fn secrets_of(&self, id: &str, cx: &App) -> Task<kerno_core::Result<Secrets>> {
        let known = self
            .runtime
            .get(id)
            .and_then(|runtime| runtime.secrets.clone());
        let store = AppState::get(cx).secrets.clone();
        let nothing_saved = self
            .get(id)
            .is_some_and(|saved| saved.config.engine.is_file() || !saved.config.save_password);
        let id = id.to_string();
        bridge::spawn_blocking(cx, move || match known {
            Some(secrets) => Ok(secrets),
            None if nothing_saved => Ok(Secrets::default()),
            None => secrets::load_secrets(store.as_ref(), &id),
        })
    }

    /// Reads the saved list again from storage, and checks the open
    /// connections now rather than at their next keep-alive.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.reload_then(Vec::new(), cx);
    }

    /// Connections an import replaced (J15): read again with the list, the
    /// secrets of `secrets_changed` read again at the next connect (the
    /// import changed or forgot them: a server moved elsewhere must not get
    /// the old password), and the open ones reconnected with the new
    /// settings once the window asked about what their tabs hold, as saving
    /// an edited one does.
    pub fn replaced(
        &mut self,
        ids: Vec<String>,
        secrets_changed: &[String],
        cx: &mut Context<Self>,
    ) {
        for id in secrets_changed {
            if let Some(runtime) = self.runtime.get_mut(id) {
                runtime.secrets = None;
            }
        }
        self.reload_then(ids, cx);
    }

    /// Reads the saved list again; then asks the window to reconnect the
    /// open connections of `reopen`, which now have their new settings.
    fn reload_then(&mut self, reopen: Vec<String>, cx: &mut Context<Self>) {
        let storage = AppState::get(cx).storage.clone();
        // A change still being written is read back, not undone.
        let previous = self.writing.take();
        cx.spawn(async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let reading = cx.update(|cx| bridge::spawn_blocking(cx, move || storage.connections()));
            let outcome = reading.await;
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(saved) => {
                    // The unsaved connections are not in storage: kept.
                    let unsaved: Vec<SavedConnection> = this
                        .saved
                        .drain(..)
                        .filter(|connection| this.unsaved.contains(&connection.id))
                        .collect();
                    this.saved = saved;
                    this.saved.extend(unsaved);
                    cx.emit(ConnectionsEvent::ListChanged);
                    cx.notify();
                    for id in reopen {
                        if !matches!(
                            this.status(&id),
                            Status::Disconnected | Status::Failed { .. }
                        ) {
                            cx.emit(ConnectionsEvent::ReconnectRequested { id });
                        }
                    }
                }
                Err(error) => this.fail(
                    format!("The saved connections could not be read: {error}"),
                    cx,
                ),
            });
        })
        .detach();
        let open: Vec<String> = self
            .runtime
            .iter()
            .filter(|(_, runtime)| runtime.meta.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for id in open {
            self.verify(&id, cx);
        }
    }

    /// Checks the connection now, as [`Connections::watch`] does, and
    /// treats it as lost when it does not answer. A session of a tab found
    /// closed may be the server's doing for that session alone (a timeout,
    /// a terminate): the connection, and the other tabs' sessions and
    /// transactions, stay while the connection's own session answers (C15).
    pub fn verify(&self, id: &str, cx: &mut Context<Self>) {
        let Some(meta) = self.meta_session(id) else {
            return;
        };
        let id = id.to_string();
        let ping = bridge::spawn(cx, answers(meta));
        cx.spawn(async move |this, cx| {
            if !ping.await {
                let _ = this.update(cx, |this, cx| this.connection_lost(&id, cx));
            }
        })
        .detach();
    }

    // Folders ------------------------------------------------------------------

    /// Every folder, sorted: the ones the user made (kept even when empty)
    /// and the ones connections are in, with their parents.
    pub fn folders(&self, cx: &App) -> Vec<String> {
        let kept = &AppState::get(cx).settings.connection_folders;
        all_folders(
            kept.iter()
                .map(String::as_str)
                .chain(self.saved.iter().filter_map(|c| c.config.group.as_deref())),
        )
    }

    /// How many connections `folder` holds, in its sub-folders too.
    pub fn count_in(&self, folder: &str) -> usize {
        self.saved
            .iter()
            .filter(|c| folder_of(&c.config).is_some_and(|path| in_folder(&path, folder)))
            .count()
    }

    /// Makes a folder named `name` in `parent` (the top level when `None`);
    /// returns its path.
    pub fn create_folder(
        &mut self,
        parent: Option<&str>,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        let name = folder_name(name)?;
        let path = match parent {
            Some(parent) => format!("{parent}/{name}"),
            None => name,
        };
        if self.folders(cx).contains(&path) {
            return Err(format!("There is already a folder “{path}”."));
        }
        let kept = path.clone();
        AppState::update_settings(cx, move |settings| {
            settings.connection_folders.insert(kept);
        });
        cx.emit(ConnectionsEvent::ListChanged);
        cx.notify();
        Ok(path)
    }

    /// Renames the folder at `path`; the connections and folders inside
    /// follow.
    pub fn rename_folder(
        &mut self,
        path: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let name = folder_name(name)?;
        let to = match parent_folder(path) {
            Some(parent) => format!("{parent}/{name}"),
            None => name,
        };
        if to == path {
            return Ok(());
        }
        if self.folders(cx).contains(&to) {
            return Err(format!("There is already a folder “{to}”."));
        }
        self.rebase(path, Some(&to), cx);
        Ok(())
    }

    /// Removes the folder at `path`. What it holds, connections and
    /// folders, moves up to its parent.
    pub fn delete_folder(&mut self, path: &str, cx: &mut Context<Self>) {
        // Its own look goes first: rebased, it would land on its parent.
        AppState::update_settings(cx, |settings| {
            settings.folder_looks.remove(path);
        });
        let parent = parent_folder(path).map(str::to_string);
        self.rebase(path, parent.as_deref(), cx);
        prune_folder_icons(cx);
    }

    /// Sets how the folder at `path` is drawn (a look that sets nothing
    /// makes it plain), and removes the pictures no folder uses any more.
    pub fn set_folder_look(&mut self, path: &str, look: FolderLook, cx: &mut Context<Self>) {
        let path = path.to_string();
        AppState::update_settings(cx, move |settings| {
            if look.is_empty() {
                settings.folder_looks.remove(&path);
            } else {
                settings.folder_looks.insert(path, look);
            }
        });
        prune_folder_icons(cx);
    }

    /// Gives the folders of an imported file their looks, only to the ones
    /// that have none.
    pub fn import_folder_looks(
        &mut self,
        looks: std::collections::BTreeMap<String, FolderLook>,
        cx: &mut Context<Self>,
    ) {
        if looks.is_empty() {
            return;
        }
        AppState::update_settings(cx, move |settings| {
            for (path, look) in looks {
                settings.folder_looks.entry(path).or_insert(look);
            }
        });
    }

    /// Moves everything inside `from` (included) under `to`.
    fn rebase(&mut self, from: &str, to: Option<&str>, cx: &mut Context<Self>) {
        let rebased = |set: &mut std::collections::BTreeSet<String>| {
            *set = std::mem::take(set)
                .into_iter()
                .filter_map(|path| match in_folder(&path, from) {
                    true => rebase_folder(&path, from, to),
                    false => Some(path),
                })
                .collect();
        };
        AppState::update_settings(cx, |settings| {
            rebased(&mut settings.connection_folders);
            rebased(&mut settings.collapsed_folders);
            // Looks follow their folders; a folder moved onto one that
            // exists (a deletion) leaves the existing one's look alone.
            let (moved, kept) = std::mem::take(&mut settings.folder_looks)
                .into_iter()
                .partition::<Vec<_>, _>(|(path, _)| in_folder(path, from));
            settings.folder_looks = kept.into_iter().collect();
            for (path, look) in moved {
                if let Some(path) = rebase_folder(&path, from, to) {
                    settings.folder_looks.entry(path).or_insert(look);
                }
            }
        });
        let mut changed = Vec::new();
        for connection in &mut self.saved {
            if let Some(path) = folder_of(&connection.config)
                && in_folder(&path, from)
            {
                connection.config.group = rebase_folder(&path, from, to);
                changed.push(connection.clone());
            }
        }
        self.store(changed, cx);
    }

    /// Puts a connection in `folder` (the top level when `None`).
    pub fn move_connection(&mut self, id: &str, folder: Option<&str>, cx: &mut Context<Self>) {
        let folder = folder.and_then(folder_path);
        let Some(connection) = self.saved.iter_mut().find(|c| c.id == id) else {
            return;
        };
        let left = folder_of(&connection.config);
        if left == folder {
            return;
        }
        connection.config.group = folder;
        let changed = vec![connection.clone()];
        keep_folder(left, cx);
        self.store(changed, cx);
    }

    /// Gives a connection another name.
    pub fn rename_connection(
        &mut self,
        id: &str,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("A connection needs a name.".into());
        }
        if let Some(connection) = self.saved.iter_mut().find(|c| c.id == id)
            && connection.config.name != name
        {
            connection.config.name = name.to_string();
            let changed = vec![connection.clone()];
            self.store(changed, cx);
        }
        Ok(())
    }

    /// Makes a connection read-only, or lets it write again, and saves the
    /// flag (never its secrets). An open connection is opened again, so
    /// every session starts as the flag says.
    pub fn set_read_only(&mut self, id: &str, read_only: bool, cx: &mut Context<Self>) {
        let Some(connection) = self.saved.iter_mut().find(|c| c.id == id) else {
            return;
        };
        if connection.config.read_only == read_only {
            return;
        }
        connection.config.read_only = read_only;
        let changed = vec![connection.clone()];
        self.store(changed, cx);
        if !matches!(
            self.status(id),
            Status::Disconnected | Status::Failed { .. }
        ) {
            self.disconnect(id, cx);
            self.connect(id, cx);
        }
    }

    /// Sets (or with `None` removes) the schema tree's filter of the
    /// objects of kind `key` of a connection, and saves it with the
    /// connection's settings (never its secrets).
    pub fn set_navigator_filter(
        &mut self,
        id: &str,
        key: &str,
        filter: Option<kerno_core::config::ObjectFilter>,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.saved.iter_mut().find(|c| c.id == id) else {
            return;
        };
        let filters = &mut connection.config.navigator_filters;
        if filters.get(key) == filter.as_ref() {
            return;
        }
        match filter {
            Some(filter) => filters.insert(key.to_string(), filter),
            None => filters.remove(key),
        };
        let changed = vec![connection.clone()];
        self.store(changed, cx);
    }

    /// Sets (or with `None` removes) the folder of the client tools a
    /// connection uses, and saves it with the connection's settings.
    pub fn set_client_dir(
        &mut self,
        id: &str,
        dir: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.saved.iter_mut().find(|c| c.id == id) else {
            return;
        };
        if connection.config.client_dir == dir {
            return;
        }
        connection.config.client_dir = dir;
        let changed = vec![connection.clone()];
        self.store(changed, cx);
    }

    /// Shows connections already changed in memory, and writes their
    /// settings (never their secrets) in the background. If that fails,
    /// the list is read back from storage.
    fn store(&mut self, mut changed: Vec<SavedConnection>, cx: &mut Context<Self>) {
        cx.emit(ConnectionsEvent::ListChanged);
        cx.notify();
        // An unsaved connection changes in memory only.
        changed.retain(|connection| !self.unsaved.contains(&connection.id));
        if changed.is_empty() {
            return;
        }
        let storage = AppState::get(cx).storage.clone();
        // One write after the other, so the last change is the one stored.
        let previous = self.writing.take();
        self.writing = Some(cx.spawn(async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let writing = cx.update(|cx| {
                bridge::spawn_blocking(cx, move || {
                    changed
                        .iter()
                        .try_for_each(|c| storage.save_connection(Some(&c.id), &c.config).map(drop))
                })
            });
            if let Err(error) = writing.await {
                let _ = this.update(cx, |this, cx| {
                    this.fail(format!("The change could not be saved: {error}"), cx);
                    this.reload(cx);
                });
            }
        }));
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
        let reconnecting = runtime.reconnect.is_some();
        let known_secrets = runtime.secrets.clone();
        let trusted_fingerprint = runtime.trusted_fingerprint.clone();
        let driver = self.drivers.get(saved.config.engine);
        let store = AppState::get(cx).secrets.clone();
        let host_keys = self.host_keys(trusted_fingerprint, cx);
        let config = saved.config.clone();
        let connection_id = saved.id.clone();

        let nothing_saved = config.engine.is_file() || !config.save_password;
        let loading = bridge::spawn_blocking(cx, {
            let id = connection_id.clone();
            move || match known_secrets {
                Some(secrets) => Ok(secrets),
                // A database file has no secret, and "ask each time" saves
                // none (`persist`): no store is read (K33).
                None if nothing_saved => Ok(Secrets::default()),
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
            if !config.save_password
                && config.password_source.is_stored()
                && secrets.password.is_none()
            {
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
                            let opening = async move {
                                let connector = Arc::new(
                                    Connector::open(driver, config, secrets, &host_keys).await?,
                                );
                                let meta = connector.session(None).await?;
                                Ok((connector, meta))
                            };
                            if !reconnecting {
                                return opening.await;
                            }
                            // A server that accepts but does not answer
                            // (frozen, or behind a dead route): next attempt.
                            tokio::time::timeout(RECONNECT_TIMEOUT, opening)
                                .await
                                .unwrap_or_else(|_| {
                                    Err(Error::Connect(format!(
                                        "the server did not answer within {} s",
                                        RECONNECT_TIMEOUT.as_secs()
                                    )))
                                })
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
        if let Some(runtime) = self.runtime.get_mut(id) {
            runtime.reconnect = None;
        }
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
        let runtime = self.runtime.entry(id.to_string()).or_default();
        match (&outcome, runtime.reconnect) {
            (Err(error), Some(failed)) if passing(error) => {
                return self.retry_later(id, failed, error, cx);
            }
            _ => runtime.reconnect = None,
        }
        match outcome {
            Ok((connector, meta)) => {
                let version = meta.server().named(meta.engine());
                // MySQL before 5.6.5 has no read-only sessions (K22).
                if config.read_only && meta.dialect().read_only_session_sql().is_none() {
                    cx.emit(ConnectionsEvent::Warned {
                        message: format!(
                            "{}: this server ({version}) cannot make a session read-only. Kerno \
                             still refuses the statements it knows to write, but the server \
                             would accept them; connect with an account that may only read.",
                            config.name
                        ),
                    });
                }
                let keep_alive = self.watch(id, cx);
                let saved_in = saved_in(cx);
                let runtime = self.runtime.entry(id.to_string()).or_default();
                let asked = runtime.typed_secret || !config.save_password;
                runtime.password_origin = origin_text(
                    connector.password_origin().as_ref(),
                    connector.password_warning().as_deref(),
                    asked,
                    saved_in,
                );
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
        runtime.reconnect = None;
        runtime.keep_alive = None;
        runtime.connector = None;
        runtime.password_origin = None;
        let meta = runtime.meta.take();
        close_all(meta, &runtime.opened, cx);
        self.set_status(id, Status::Disconnected, cx);
    }

    /// Whether the connection is being opened again after a loss (its
    /// status is then [`Status::Connecting`]).
    pub fn reconnecting(&self, id: &str) -> bool {
        self.runtime
            .get(id)
            .is_some_and(|runtime| runtime.reconnect.is_some())
    }

    /// A reconnect attempt failed while the server is away: the next one
    /// after [`retry_delay`], the status still "reconnecting" (Disconnect
    /// drops the wait with the attempt).
    fn retry_later(&mut self, id: &str, failed: u32, error: &Error, cx: &mut Context<Self>) {
        let delay = retry_delay(failed);
        tracing::info!(connection = id, attempt = failed + 1, ?delay, %error, "reconnect failed; retrying");
        let runtime = self.runtime.entry(id.to_string()).or_default();
        runtime.reconnect = Some(failed + 1);
        let id = id.to_string();
        runtime.attempt = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(runtime) = this.runtime.get_mut(&id) {
                    runtime.status = Some(Status::Disconnected);
                }
                this.connect(&id, cx);
            });
        }));
        cx.notify();
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
                let ping = cx.update(|cx| bridge::spawn(cx, answers(meta)));
                if !ping.await {
                    let _ = this.update(cx, |this, cx| this.connection_lost(&id, cx));
                    return;
                }
            }
        })
    }

    /// Called when a session of this connection reported that the server
    /// is gone, or a check went unanswered: reconnects with the secrets
    /// already known, again and again while the server is away (see
    /// [`retry_delay`]), until it is back or the user disconnects.
    pub fn connection_lost(&mut self, id: &str, cx: &mut Context<Self>) {
        if !matches!(self.status(id), Status::Connected { .. }) {
            return;
        }
        if let Some(runtime) = self.runtime.get_mut(id) {
            runtime.keep_alive = None;
            runtime.connector = None;
            runtime.password_origin = None;
            // Gone with the server, or left half-open: closed either way.
            close_all(runtime.meta.take(), &runtime.opened, cx);
            runtime.status = Some(Status::Disconnected);
            runtime.reconnect = Some(0);
        }
        tracing::info!(connection = id, "connection lost; reconnecting");
        self.connect(id, cx);
    }

    /// Tries a connection without saving it, and reports the server version
    /// and latency. `trusted_fingerprint` is an SSH host key the user just
    /// agreed to trust; `typed`, that the password in `secrets` was typed
    /// in the dialog, not read from where saved passwords are (K56).
    pub fn test(
        &self,
        config: ConnectionConfig,
        secrets: Secrets,
        trusted_fingerprint: Option<String>,
        typed: bool,
        cx: &App,
    ) -> Task<kerno_core::Result<TestReport>> {
        let driver = self.drivers.get(config.engine);
        let host_keys = self.host_keys(trusted_fingerprint, cx);
        let saved_in = saved_in(cx);
        bridge::spawn(cx, async move {
            let asked = !config.save_password;
            let connector = Connector::open(driver?, config, secrets, &host_keys).await?;
            let session = connector.session(None).await?;
            let started = Instant::now();
            session.ping().await?;
            let latency = started.elapsed();
            let version = session.server().named(session.engine());
            session.close().await;
            let password = match connector.password_origin() {
                Some(PasswordOrigin::Given) if typed => Some(TYPED_HERE.to_string()),
                origin => origin_text(
                    origin.as_ref(),
                    connector.password_warning().as_deref(),
                    asked,
                    saved_in,
                ),
            };
            Ok(TestReport {
                version,
                latency,
                password,
            })
        })
    }

    /// The names of the databases on the server `config` describes, for the
    /// connection dialog's Database field. The session opens without the
    /// typed database, which may be the mistyped one. PostgreSQL then opens
    /// the database named after the user; when there is none, `postgres`,
    /// as DBeaver does.
    pub fn list_databases(
        &self,
        mut config: ConnectionConfig,
        secrets: Secrets,
        trusted_fingerprint: Option<String>,
        cx: &App,
    ) -> Task<kerno_core::Result<Vec<String>>> {
        config.database = None;
        let postgres = config.engine == Engine::Postgres;
        let driver = self.drivers.get(config.engine);
        let host_keys = self.host_keys(trusted_fingerprint, cx);
        bridge::spawn(cx, async move {
            let connector = Connector::open(driver?, config, secrets, &host_keys).await?;
            let session = match connector.session(None).await {
                Err(Error::Connect(text)) if postgres && text.contains("[3D000]") => {
                    connector.session(Some("postgres")).await?
                }
                other => other?,
            };
            let databases = session.databases().await;
            session.close().await;
            Ok(databases?.into_iter().map(|info| info.name).collect())
        })
    }
}

/// A test's password, typed in the connection dialog (K56).
pub const TYPED_HERE: &str = "Password typed here";

/// Where a connection's password came from, in the words of the dialog and
/// the navigator (J3): `asked` when the user typed it for this run, a
/// password file that was ignored when no password was sent; `saved_in`
/// names where saved secrets are, `None` when in memory only.
pub fn origin_text(
    origin: Option<&PasswordOrigin>,
    ignored: Option<&str>,
    asked: bool,
    saved_in: Option<&str>,
) -> Option<String> {
    Some(match (origin, saved_in) {
        (None, _) => ignored?.to_string(),
        (Some(PasswordOrigin::Given), _) if asked => "Password asked".to_string(),
        (Some(PasswordOrigin::Given), Some(store)) => format!("Password from {store}"),
        (Some(PasswordOrigin::Given), None) => "Password remembered until Kerno quits".to_string(),
        (Some(PasswordOrigin::File(file)), _) => format!("Password from {file}"),
        (Some(PasswordOrigin::Command(name)), _) => format!("Password from the command `{name}`"),
    })
}

/// Where the store in use keeps saved secrets, `None` when in memory only.
fn saved_in(cx: &App) -> Option<&'static str> {
    let store = &AppState::get(cx).secrets;
    store.is_persistent().then(|| store.name())
}

/// Whether the connection's own session answers a ping. A timeout once may
/// be a long read on this session; twice, the server is not answering.
async fn answers(meta: Arc<dyn Session>) -> bool {
    for _ in 0..2 {
        match tokio::time::timeout(PING_TIMEOUT, meta.ping()).await {
            Ok(outcome) => return outcome.is_ok(),
            Err(_) => continue,
        }
    }
    false
}

/// Closes `meta` and every session in `opened` still open, off the UI thread.
fn close_all(meta: Option<Arc<dyn Session>>, opened: &Opened, cx: &App) {
    let sessions: Vec<Arc<dyn Session>> = opened
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .drain(..)
        .filter_map(|session| session.upgrade())
        .chain(meta)
        .collect();
    if !sessions.is_empty() {
        bridge::spawn(cx, async move {
            for session in sessions {
                // A server that stopped answering would hold the close.
                let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
            }
        })
        .detach();
    }
}

/// The folder a connection is in, tidied; `None` at the top level.
pub fn folder_of(config: &ConnectionConfig) -> Option<String> {
    config.group.as_deref().and_then(folder_path)
}

/// Removes, off the UI thread, the folder pictures no look uses, those of
/// a dialog that was cancelled too. A picture written after this call is
/// kept: a dialog open now may be about to use it.
fn prune_folder_icons(cx: &mut App) {
    let state = AppState::get(cx);
    let dir = state.paths.folder_icons();
    let used: HashSet<String> = state
        .settings
        .folder_looks
        .values()
        .filter_map(|look| look.image.clone())
        .collect();
    let now = SystemTime::now();
    bridge::spawn_blocking(cx, move || {
        if let Err(error) = crate::folder_dialog::remove_unused_icons(&dir, &used, now) {
            tracing::warn!(%error, "unused folder pictures could not be removed");
        }
    })
    .detach();
}

/// Keeps a folder a connection leaves, so it does not vanish once empty.
fn keep_folder(folder: Option<String>, cx: &mut App) {
    if let Some(folder) = folder {
        AppState::update_settings(cx, |settings| {
            settings.connection_folders.insert(folder);
        });
    }
}

/// Writes a connection and its secrets; returns its id. With `insert`, the
/// connection is new to storage and keeps its id.
fn persist(
    storage: Arc<Storage>,
    store: Arc<dyn SecretStore>,
    id: Option<String>,
    insert: bool,
    config: ConnectionConfig,
    secrets: Secrets,
) -> impl FnOnce() -> kerno_core::Result<String> {
    move || {
        let id = match (insert, id) {
            (true, Some(id)) => storage.add_connection(&id, &config).map(|()| id)?,
            (_, id) => storage.save_connection(id.as_deref(), &config)?,
        };
        // A database file has no secret: the secret store is not touched.
        if config.engine.is_file() {
            return Ok(id);
        }
        if config.save_password {
            secrets::save_secrets(store.as_ref(), &id, &secrets)?;
        } else {
            secrets::delete_secrets(store.as_ref(), &id)?;
        }
        Ok(id)
    }
}
