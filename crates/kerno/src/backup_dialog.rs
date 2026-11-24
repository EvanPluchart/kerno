//! The tool wizards, as DBeaver's native-tool task wizards
//! (`PostgreBackupWizard`, `PostgreBackupAllWizard`, `PostgreRestoreWizard`,
//! `PostgreScriptExecuteWizard`, `MySQLExportWizard`,
//! `MySQLScriptExecuteWizard`), drawn with Kerno's components: the steps
//! listed on the left, Back / Next / Start, and the local client in use
//! with a "Local client…" choice.
//!
//! - Backup: the objects (schemas, then the tables of a schema; All /
//!   None; complete backup; show views), the settings (format,
//!   compression, encoding, the dump tool's options, the output folder and
//!   the file name pattern, extra arguments, another user), the progress.
//! - Global backup (`pg_dumpall`): the databases, the settings, the
//!   progress. It only reads.
//! - Restore and Execute script: the file and the options, the check of a
//!   SQL file's client commands when it holds one, then — once the command
//!   is confirmed — the progress.
//!
//! Each run shows the exact command (the password never: it goes to the
//! tool through its environment), streams the tool's output, can be
//! cancelled, and ends with what happened. A restore and a script are
//! confirmed first, twice on production, and refused on a read-only
//! connection. The settings of each wizard are `kerno_core::tools`' own
//! serialisable structs, which saved tasks keep.
//!
//! A backup or a restore of a database opens first in its simple view
//! (`backup_dialog/simple.rs`, K26): one screen on the same state, whose
//! "Advanced settings…" is the wizard.

use std::collections::{BTreeSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input as TextInput, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render,
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, UniformListScrollHandle, Window, div, px, uniform_list,
};
use kerno_core::config::{ConnectParams, ConnectionConfig, Engine, Secret, Secrets, TlsMode};
use kerno_core::connector::HostKeyStore;
use kerno_core::format::byte_size;
use kerno_core::schema::{RelationKind, TableRef};
use kerno_core::sql::LexRules;
use kerno_core::storage::{HistoryStatus, SavedTask};
use kerno_core::tools::{
    self, BackupContent, BackupFormat, BackupOptions, ClientFamily, ClientInstall, ClientRoots,
    ClientWanted, Compatibility, DumpMethod, EngineBackup, Finding, GlobalBackupOptions, NameVars,
    RefusedRole, RestoreOptions, ScriptLogLevel, ScriptOptions, Stamp, TaskSettings, Tool,
    ToolCommand,
};
use kerno_core::{Error, Session, exec};

use crate::bridge;
use crate::connections::form::expand_home;
use crate::connections::manager::{Connections, Status};
use crate::query::tab::duration_text;
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::state::AppState;
use crate::tabs::Target;
use crate::theme;

mod simple;

#[cfg(test)]
pub(crate) use simple::faster_jobs;

/// Which wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One database (`pg_dump`, `mysqldump`).
    Backup,
    /// A file into a database (`pg_restore`, `psql`, `mysql`).
    Restore,
    /// A whole PostgreSQL server (`pg_dumpall`).
    GlobalBackup,
    /// A SQL file run by `psql` / `mysql`.
    ExecuteScript,
}

impl Mode {
    /// Whether the run writes to the database (and is confirmed first);
    /// such a run reads a file the user chooses first.
    fn writes(self) -> bool {
        matches!(self, Self::Restore | Self::ExecuteScript)
    }
}

/// What the wizard is opened for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// A run; "Save as task…" keeps its settings too.
    Run,
    /// A new saved task (Tools ▸ Create new task…): "Save as task…" is the
    /// main button.
    NewTask,
    /// A saved task, on its settings: "Save task" writes them back, and
    /// each run is kept with it (DBeaver's task wizard).
    Task(Box<SavedTask>),
}

/// Called with the connection and the database once a restore or a
/// script succeeded.
type Restored = dyn Fn(&str, &str, &mut Window, &mut App);

/// Called once a task was saved or ran: the Tasks view reads them again.
type TasksChanged = dyn Fn(&mut App);

/// Lines of the tool's output kept on screen: the last ones.
const LOG_LINES: usize = 5_000;

/// The pages of the wizards, as the left column lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Objects,
    Settings,
    /// The SQL file holds client commands: the explicit opt-in.
    Check,
    Progress,
}

impl Page {
    fn label(self, mode: Mode) -> &'static str {
        match (self, mode) {
            (Self::Objects, Mode::GlobalBackup) => "Databases",
            (Self::Objects, _) => "Objects",
            (Self::Settings, _) => "Settings",
            (Self::Check, _) => "Client commands",
            (Self::Progress, _) => "Progress",
        }
    }
}

/// Which face of the dialog shows (K26, D-099): a backup or a restore of a
/// run opens on one simple screen; the wizard is its "Advanced settings…".
/// Both are drawn from the same state and build the same command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Simple,
    Advanced,
}

/// The client in use.
#[derive(Debug, Clone)]
enum ToolState {
    /// Looking for the clients and their versions.
    Probing,
    Ready {
        path: PathBuf,
        install: ClientInstall,
        compat: Compatibility,
        /// The folder asked for that is not a known client: not used.
        ignored: Option<PathBuf>,
    },
    /// Nothing can run, and why.
    Missing(String),
}

/// The file to restore or run, once read.
#[derive(Debug, Clone)]
struct Input {
    path: PathBuf,
    format: BackupFormat,
    /// The first client command a SQL file holds, which the tool would run
    /// on this computer: the run needs the explicit opt-in.
    refusal: Option<Finding>,
    /// The database a PostgreSQL archive was made from, which pg_restore
    /// `--create` creates (from `pg_restore --list`).
    archive_database: Option<String>,
    /// A PostgreSQL archive's list (`pg_restore --list`), when it could be
    /// read: what the restore needs is checked from it, and an extension
    /// is left out through it (K39).
    list: Option<Arc<str>>,
    /// The extensions that list creates, read once off the UI thread.
    extensions: Vec<String>,
}

#[derive(Debug, Clone)]
enum Step {
    /// Reading the secrets and opening the SSH tunnel, if any.
    Preparing,
    /// Nothing can run, and why.
    Blocked(String),
    Options,
    Running,
    Finished {
        success: bool,
        text: String,
    },
}

/// A failed PostgreSQL restore whose statements name roles the server
/// refused (K35, D-108): what the failure view says first, and whether it
/// offers the restore again without the backup's owners and privileges.
#[derive(Debug, Clone)]
struct RoleFailure {
    refused: Vec<RefusedRole>,
    /// The refused roles the server does not have, as it answered; `None`
    /// when it could not be asked.
    missing: Option<Vec<String>>,
    /// The file was a plain SQL script, which sets its owners in its text.
    script: bool,
    /// The failed run was all or nothing: nothing of it was kept.
    single_transaction: bool,
    /// The same restore can run again without the backup's owners and
    /// privileges: an archive, restored in one transaction (else pg_restore
    /// went on and the objects are there), which kept them.
    retry: bool,
    /// The user the restore connected as.
    user: String,
    /// The extensions whose creation failed and that the server does not
    /// offer (K39).
    extensions: Vec<String>,
    /// Those extensions can be left out: the archive's list names them.
    leavable: bool,
}

/// What a PostgreSQL archive needs that the server does not have, found
/// before the run (K39, D-112).
#[derive(Debug, Clone, Default)]
struct Preflight {
    /// Extensions the archive creates that the server does not offer.
    extensions: Vec<String>,
    /// Roles owning the archive's objects that the server does not have.
    owners: Vec<String>,
}

impl RoleFailure {
    /// A failure of a restore with `options`, its roles still to be read.
    fn new(script: bool, options: &RestoreOptions, user: String) -> Self {
        Self {
            refused: Vec::new(),
            missing: None,
            script,
            single_transaction: options.single_transaction,
            retry: !script
                && options.single_transaction
                && !(options.no_owner && options.no_privileges),
            user,
            extensions: Vec::new(),
            leavable: false,
        }
    }

    /// "Restore again" leaves out the backup's owners and privileges.
    fn retry_owners(&self) -> bool {
        self.retry && !self.refused.is_empty()
    }

    /// "Restore again" leaves out the extensions the server lacks: an
    /// archive whose list names them, restored in one transaction.
    fn retry_extensions(&self) -> bool {
        !self.script && self.single_transaction && self.leavable && !self.extensions.is_empty()
    }
}

/// The tables and views of one schema (the database on MySQL).
#[derive(Debug, Clone)]
pub(crate) struct SchemaEntry {
    pub(crate) name: String,
    pub(crate) relations: Vec<(String, RelationKind)>,
}

/// Something read from the server for a page.
#[derive(Debug, Clone)]
enum Loaded<T> {
    Loading,
    Failed(String),
    Ready(T),
}

/// The objects page: what is ticked.
#[derive(Debug, Clone, Default)]
struct Selection {
    /// DBeaver's "Complete backup": the whole database, the lists off.
    complete: bool,
    show_views: bool,
    schemas: BTreeSet<String>,
    tables: BTreeSet<TableRef>,
    /// The schema whose tables the right list shows.
    current: Option<String>,
}

fn is_view(kind: &RelationKind) -> bool {
    matches!(kind, RelationKind::View | RelationKind::MaterializedView)
}

pub struct BackupDialog {
    mode: Mode,
    view: View,
    /// The simple view's "Show command" is open.
    show_command: bool,
    connections: Entity<Connections>,
    connection: String,
    /// The connection's name, as the confirmation shows it.
    name: String,
    environment: String,
    host: String,
    engine: Engine,
    /// The engine's client tools (never SQLite's: it has none).
    family: ClientFamily,
    database: String,
    server_version: (u32, u32, u32),
    /// The connection's own client folder.
    client_dir: Option<PathBuf>,
    /// How the tool reaches the server, secrets in memory only.
    params: Option<ConnectParams>,
    tool_state: ToolState,
    /// Every client found, for "Local client…".
    detected: Vec<ClientInstall>,
    showing_clients: bool,
    client_error: Option<String>,
    page: Page,
    // The settings of each wizard, beside the text fields below.
    backup: BackupOptions,
    global: GlobalBackupOptions,
    restore: RestoreOptions,
    script: ScriptOptions,
    selection: Selection,
    objects: Loaded<Vec<SchemaEntry>>,
    databases: Loaded<Vec<String>>,
    /// Global backup: the databases the user left out (what a task keeps).
    excluded: BTreeSet<String>,
    /// Global backup: the databases this account may not open, read from the
    /// server at each opening, never kept with a task; left out of every run
    /// and said (K62).
    closed: Vec<String>,
    /// The file the macOS save panel returned when it already existed: the
    /// panel asked before replacing it, so the backup does not ask again.
    agreed_replace: Option<PathBuf>,
    folder: Entity<InputState>,
    pattern: Entity<InputState>,
    extra: Entity<InputState>,
    compression: Entity<InputState>,
    encoding: Entity<InputState>,
    jobs: Entity<InputState>,
    auth: bool,
    auth_user: Entity<InputState>,
    auth_password: Entity<InputState>,
    /// When the file names were stamped.
    stamp: Stamp,
    /// Restore / script: the file chosen (typed, pasted or picked), and
    /// what reading it found.
    file_field: Entity<InputState>,
    file: Option<PathBuf>,
    input: Option<Result<Input, String>>,
    /// The explicit opt-in to a file's client commands; reset per file.
    allow_commands: bool,
    step: Step,
    /// The roles and extensions a failed restore names, once read from its
    /// output.
    roles: Option<RoleFailure>,
    /// What the archive to restore needs that the server lacks (K39).
    preflight: Option<Preflight>,
    /// The archive's extensions the restore leaves out (`--use-list`).
    leave_out: Vec<String>,
    /// The preflight ticked "Leave out the backup's owners and privileges":
    /// the next file chosen unticks it.
    owners_ticked: bool,
    /// The command that runs or ran, as shown.
    shown_command: Option<String>,
    /// The file or folder the backup writes.
    output: Option<PathBuf>,
    started: Option<Instant>,
    elapsed: Option<Duration>,
    log: VecDeque<SharedString>,
    log_scroll: UniformListScrollHandle,
    cancel: Arc<AtomicBool>,
    /// A restore or script fed through the tool's standard input: bytes
    /// sent so far and the file's size (0 until read).
    feed: Option<(Arc<AtomicU64>, Arc<AtomicU64>)>,
    /// Close the dialog once the stopped tool has ended.
    close_when_stopped: bool,
    /// Bumped at each probe, so a late answer is dropped.
    generation: u64,
    on_restored: Rc<Restored>,
    purpose: Purpose,
    on_tasks_changed: Option<Rc<TasksChanged>>,
    /// Schemas ticked whole whose tables are ticked once the objects are
    /// read (a schema's backup, a saved task).
    pending_schemas: Vec<String>,
    probing: Option<Task<()>>,
    /// Waits for a pause in the typing of the file field.
    typing: Option<Task<()>>,
    running: Option<Task<()>>,
    ticking: Option<Task<()>>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// How long the typing of a path pauses before the path is read.
const TYPING_PAUSE: Duration = Duration::from_millis(400);

/// Opens the wizard of `mode` on the database of `target` (its
/// connection's default database when it names none), or says why it
/// cannot.
pub fn open(
    connections: Entity<Connections>,
    target: Target,
    mode: Mode,
    window: &mut Window,
    cx: &mut App,
    on_restored: impl Fn(&str, &str, &mut Window, &mut App) + 'static,
) -> Option<Entity<BackupDialog>> {
    let checked = {
        let manager = connections.read(cx);
        let connected = matches!(manager.status(&target.connection), Status::Connected { .. });
        match (
            manager.get(&target.connection),
            manager.meta_session(&target.connection),
        ) {
            (Some(saved), Some(meta)) if connected => {
                let config = &saved.config;
                let database = target
                    .database
                    .clone()
                    .or_else(|| meta.database())
                    .filter(|name| !name.is_empty());
                let family = ClientFamily::of(config.engine);
                if mode.writes() && config.read_only {
                    Err(format!(
                        "“{}” is a read-only connection: {} writes to the database.",
                        config.name,
                        if mode == Mode::Restore {
                            "a restore"
                        } else {
                            "a script"
                        }
                    ))
                } else if family.is_none() {
                    Err(format!("{}.", tools::NO_CLIENT_TOOLS))
                } else if mode == Mode::GlobalBackup && config.engine != Engine::Postgres {
                    Err("The global backup is PostgreSQL's pg_dumpall.".into())
                } else if let Some(problem) = route_problem(config) {
                    Err(problem.to_string())
                } else if let (Some(database), Some(family)) = (database, family) {
                    Ok((
                        config.clone(),
                        database,
                        meta.server().version_number,
                        meta,
                        family,
                    ))
                } else {
                    Err(
                        "Choose a database first: right-click it in the tree and choose \
                         “Back up…” or “Restore…”."
                            .into(),
                    )
                }
            }
            _ => Err(
                "Connect first: the client tool reaches the server the way the connection does."
                    .into(),
            ),
        }
    };
    let (config, database, server_version, meta, family) = match checked {
        Ok(checked) => checked,
        Err(refusal) => {
            window.push_notification(Notification::warning(SharedString::from(refusal)), cx);
            return None;
        }
    };
    let title = match mode {
        Mode::Backup => format!("Back up {database}"),
        Mode::Restore => format!("Restore into {database}"),
        Mode::GlobalBackup => format!("Global backup of {}", config.name),
        Mode::ExecuteScript => format!("Execute a script in {database}"),
    };
    let opening = Opening {
        connection: target.connection,
        config,
        database,
        server_version,
        mode,
        meta,
        family,
    };
    let on_restored: Rc<Restored> = Rc::new(on_restored);
    let view = cx.new(|cx| BackupDialog::new(connections, opening, on_restored, window, cx));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        let asked = shown.clone();
        let entered = shown.clone();
        dialog
            .title(SharedString::from(title.clone()))
            .w(px(860.))
            // Enter starts a simple backup only: never a restore, never the
            // wizard's Start.
            .on_ok(move |_, window, cx| {
                entered.update(cx, |this, cx| this.enter(window, cx));
                false
            })
            // Closing while a tool runs asks first.
            .on_cancel(move |_, window, cx| asked.update(cx, |this, cx| this.may_close(window, cx)))
            .content(move |content, _, _| content.child(view.clone()))
    });
    Some(view)
}

/// Why the tools cannot reach this connection's server, if they cannot.
fn route_problem(config: &ConnectionConfig) -> Option<&'static str> {
    let mysql = config.engine != Engine::Postgres;
    (mysql && config.ssh.is_some() && config.tls.mode == TlsMode::VerifyFull).then_some(
        "Through an SSH tunnel the MySQL and MariaDB client tools reach the server at \
         127.0.0.1, the tunnel's end on this computer, and verify-full would check the \
         server's certificate against that address and refuse it. Use verify-ca for this \
         connection, or run the tools from a machine that reaches the server directly.",
    )
}

/// What the dialog is opened on.
struct Opening {
    connection: String,
    config: ConnectionConfig,
    database: String,
    server_version: (u32, u32, u32),
    mode: Mode,
    meta: Arc<dyn Session>,
    /// The client tools of the engine: the dialog opens only for an engine
    /// that has them.
    family: ClientFamily,
}

impl BackupDialog {
    fn new(
        connections: Entity<Connections>,
        opening: Opening,
        on_restored: Rc<Restored>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Opening {
            connection,
            config,
            database,
            server_version,
            mode,
            meta,
            family,
        } = opening;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let global = GlobalBackupOptions::default();
        let pattern_default = match mode {
            Mode::GlobalBackup => global.file_pattern.clone(),
            _ => tools::DEFAULT_FILE_PATTERN.to_string(),
        };
        // A backup goes where the last one went (K26), else home.
        let last = AppState::get(cx)
            .settings
            .backup_folder
            .clone()
            .filter(|folder| mode == Mode::Backup && folder.is_absolute());
        let mut input = |value: String, placeholder: &'static str| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let folder = input(last.unwrap_or(home).display().to_string(), "Folder");
        let pattern = input(pattern_default, "${database}-${timestamp}");
        let extra = input(String::new(), "--name=value …");
        let compression = input(String::new(), "default");
        let encoding = input(String::new(), "the server's");
        let jobs = input(String::new(), "1");
        let auth_user = input(String::new(), "user");
        let file_field = input(String::new(), "Type or paste a path, or choose the file");
        let auth_password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("password, for this run only")
        });
        // A typed path is taken once typing pauses (reading it on every key
        // would look for the clients each time), or at once on Enter or when
        // the field loses the focus.
        let mut subscriptions: Vec<Subscription> =
            vec![
                cx.subscribe(&file_field, |this, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => {
                        this.typing = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(TYPING_PAUSE).await;
                            let _ = this.update(cx, |this, cx| this.take_typed(cx));
                        }));
                    }
                    InputEvent::PressEnter { .. } | InputEvent::Blur => this.take_typed(cx),
                    _ => {}
                }),
            ];
        subscriptions.extend(
            [
                &folder,
                &pattern,
                &extra,
                &compression,
                &encoding,
                &jobs,
                &auth_user,
                &auth_password,
            ]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            }),
        );
        let mut dialog = Self {
            mode,
            view: match mode {
                Mode::Backup | Mode::Restore => View::Simple,
                Mode::GlobalBackup | Mode::ExecuteScript => View::Advanced,
            },
            show_command: false,
            connections: connections.clone(),
            connection,
            name: config.name.clone(),
            environment: config.environment.clone(),
            host: config.host.clone(),
            engine: config.engine,
            family,
            database: database.clone(),
            server_version,
            client_dir: config.client_dir.clone(),
            params: None,
            tool_state: ToolState::Probing,
            detected: Vec::new(),
            showing_clients: false,
            client_error: None,
            page: match mode {
                Mode::Backup | Mode::GlobalBackup => Page::Objects,
                Mode::Restore | Mode::ExecuteScript => Page::Settings,
            },
            backup: BackupOptions::new(config.engine, database.clone()),
            global,
            restore: RestoreOptions::new(config.engine, database.clone(), "", BackupFormat::Sql),
            script: ScriptOptions::new(config.engine, database, ""),
            selection: Selection {
                complete: true,
                ..Selection::default()
            },
            objects: Loaded::Loading,
            databases: Loaded::Loading,
            excluded: BTreeSet::new(),
            closed: Vec::new(),
            folder,
            pattern,
            extra,
            compression,
            encoding,
            jobs,
            auth: false,
            auth_user,
            auth_password,
            stamp: Stamp::now(),
            file_field,
            file: None,
            input: None,
            allow_commands: false,
            step: Step::Preparing,
            roles: None,
            preflight: None,
            leave_out: Vec::new(),
            owners_ticked: false,
            agreed_replace: None,
            shown_command: None,
            output: None,
            started: None,
            elapsed: None,
            log: VecDeque::new(),
            log_scroll: UniformListScrollHandle::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            feed: None,
            close_when_stopped: false,
            generation: 0,
            on_restored,
            purpose: Purpose::Run,
            on_tasks_changed: None,
            pending_schemas: Vec::new(),
            probing: None,
            typing: None,
            running: None,
            ticking: None,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        dialog.prepare(&connections, config, cx);
        match mode {
            Mode::Backup => dialog.load_objects(meta, cx),
            Mode::GlobalBackup => dialog.load_databases(meta, cx),
            Mode::Restore | Mode::ExecuteScript => {}
        }
        dialog
    }

    /// Gets the secrets the connection uses and opens its SSH tunnel, then
    /// looks for the client.
    fn prepare(
        &mut self,
        connections: &Entity<Connections>,
        config: ConnectionConfig,
        cx: &mut Context<Self>,
    ) {
        let secrets = connections.read(cx).secrets_of(&self.connection, cx);
        // The host key was trusted when the connection was opened: in the
        // user's own known_hosts (read, never written by the built-in client;
        // OpenSSH checks its own) or in Kerno's.
        let known_hosts: Vec<PathBuf> = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".ssh/known_hosts"))
            .into_iter()
            .chain([AppState::get(cx).paths.known_hosts()])
            .collect();
        let openssh = connections.read(cx).openssh().cloned();
        let password_env = connections.read(cx).password_env().cloned();
        let database = self.database.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let prepared = match secrets.await {
                Err(error) => Err(error),
                Ok(secrets) => {
                    let opening = cx.update(|cx| {
                        let host_keys = HostKeyStore {
                            known_hosts,
                            openssh,
                            ..HostKeyStore::default()
                        };
                        bridge::spawn(
                            cx,
                            params_for(config, secrets, host_keys, database, password_env),
                        )
                    });
                    opening.await
                }
            };
            let _ = this.update(cx, |this, cx| {
                match prepared {
                    Ok(params) => {
                        this.params = Some(params);
                        this.step = Step::Options;
                        this.probe(cx);
                    }
                    Err(error) => this.step = Step::Blocked(error_text(&error)),
                }
                cx.notify();
            });
        }));
    }

    /// Lists the schemas and their tables and views, on the connection's
    /// own session when it is in the database, else on a session of its
    /// own, closed once read.
    fn load_objects(&mut self, meta: Arc<dyn Session>, cx: &mut Context<Self>) {
        let database = self.database.clone();
        let session = if meta.database().as_deref() == Some(database.as_str()) {
            None
        } else {
            Some(self.connections.read(cx).open_session(
                &self.connection,
                Some(database.clone()),
                cx,
            ))
        };
        self.tasks.push(cx.spawn(async move |this, cx| {
            let (session, own) = match session {
                None => (Ok(meta), false),
                Some(opening) => (opening.await, true),
            };
            let loaded = match session {
                Err(error) => Loaded::Failed(error_text(&error)),
                Ok(session) => {
                    let reading = cx.update(|cx| {
                        bridge::spawn(cx, async move {
                            let objects = fetch_objects(&*session, &database).await;
                            if own {
                                session.close().await;
                            }
                            objects
                        })
                    });
                    match reading.await {
                        Ok(objects) => Loaded::Ready(objects),
                        Err(error) => Loaded::Failed(error_text(&error)),
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                if let Loaded::Ready(objects) = &loaded
                    && this.selection.current.is_none()
                {
                    this.selection.current = objects.first().map(|entry| entry.name.clone());
                }
                this.objects = loaded;
                this.fill_pending();
                cx.notify();
            });
        }));
    }

    /// Lists the server's databases, for the global backup.
    fn load_databases(&mut self, meta: Arc<dyn Session>, cx: &mut Context<Self>) {
        let reading = bridge::spawn(cx, async move { meta.databases().await });
        self.tasks.push(cx.spawn(async move |this, cx| {
            // pg_dumpall would stop at a database this account may not
            // open: not offered, and left out of the backup (K62).
            let mut closed = Vec::new();
            let loaded = match reading.await {
                Ok(databases) => Loaded::Ready(
                    databases
                        .into_iter()
                        .filter(|database| !database.name.starts_with("template"))
                        .filter(|database| {
                            if !database.can_connect && database.allows_connections {
                                closed.push(database.name.clone());
                            }
                            database.can_connect
                        })
                        .map(|database| database.name)
                        .collect(),
                ),
                Err(error) => Loaded::Failed(error_text(&error)),
            };
            let _ = this.update(cx, |this, cx| {
                this.closed = closed;
                this.databases = loaded;
                cx.notify();
            });
        }));
    }

    /// The tool the wizard runs; a restore's follows its file's format.
    fn tool(&self) -> Option<Tool> {
        match self.mode {
            Mode::Backup => tools::backup_tool(self.engine),
            Mode::GlobalBackup => Some(Tool::PgDumpall),
            Mode::ExecuteScript => tools::script_tool(self.engine),
            Mode::Restore => None,
        }
    }

    /// Finds the clients, picks the connection's, and reads the file to
    /// restore or run (its format says which tool), off the UI thread.
    fn probe(&mut self, cx: &mut Context<Self>) {
        let Some(params) = self.params.clone() else {
            return;
        };
        // Without a file too: the clients are listed and the one in use is
        // shown (a script's tool is known; a restore's waits for the file).
        self.generation += 1;
        let generation = self.generation;
        self.tool_state = ToolState::Probing;
        self.input = None;
        self.preflight = None;
        self.leave_out.clear();
        if std::mem::take(&mut self.owners_ticked) {
            (self.restore.no_owner, self.restore.no_privileges) = (false, false);
        }
        self.allow_commands = false;
        let settings = &AppState::get(cx).settings;
        let family = self.family;
        let job = Job {
            mode: self.mode,
            engine: self.engine,
            server_version: self.server_version,
            tool: self.tool(),
            added: settings.client_folders.clone(),
            default_dir: settings.default_clients.get(&family).cloned(),
            connection_dir: self.client_dir.clone(),
            file: self.file.clone(),
            params,
            database: self.database.clone(),
        };
        let examining = bridge::spawn_blocking(cx, move || examine(job));
        self.probing = Some(cx.spawn(async move |this, cx| {
            let examined = examining.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.tool_state = examined.tool;
                    this.detected = examined.detected;
                    this.input = examined.input;
                    // The simple view's "Leave out the backup's owners and
                    // privileges" is for archives: a script chosen after it
                    // runs as it is (K35).
                    if this.view == View::Simple
                        && this.without_owners()
                        && matches!(&this.input, Some(Ok(input)) if input.format == BackupFormat::Sql)
                    {
                        (this.restore.no_owner, this.restore.no_privileges) = (false, false);
                    }
                    this.check_archive(cx);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// Makes `dir` this connection's client (`None`: the default), saved
    /// with the connection, and looks again.
    fn use_client(&mut self, dir: Option<PathBuf>, cx: &mut Context<Self>) {
        self.client_dir = dir.clone();
        self.client_error = None;
        let id = self.connection.clone();
        self.connections.update(cx, |connections, cx| {
            connections.set_client_dir(&id, dir, cx)
        });
        self.showing_clients = false;
        self.probe(cx);
    }

    /// Asks for a client folder; one that holds the family's tools is added
    /// to Settings ▸ Local clients and used for this connection.
    fn browse_client(&mut self, cx: &mut Context<Self>) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use this client".into()),
        });
        let family = self.family;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picking.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let probing = cx.update(|cx| {
                let dir = dir.clone();
                bridge::spawn_blocking(cx, move || tools::probe_client(&dir, family))
            });
            let found = probing.await;
            let _ = this.update(cx, |this, cx| match found {
                Some(_) => {
                    AppState::update_settings(cx, |settings| {
                        if !settings.client_folders.contains(&dir) {
                            settings.client_folders.push(dir.clone());
                        }
                    });
                    this.use_client(Some(dir), cx);
                }
                None => {
                    this.client_error = Some(format!(
                        "{} holds no {} client tool ({}).",
                        dir.display(),
                        family.label(),
                        family
                            .tools()
                            .iter()
                            .map(|tool| tool.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    cx.notify();
                }
            });
        }));
    }

    /// Asks for the file to restore or run.
    fn choose_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            // A directory archive is a folder.
            directories: self.mode == Mode::Restore && self.engine == Engine::Postgres,
            multiple: false,
            prompt: Some(
                match self.mode {
                    Mode::ExecuteScript => "Run",
                    _ => "Restore",
                }
                .into(),
            ),
        });
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picking.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, window, cx| this.load_input(path, window, cx));
            }
        }));
    }

    /// Takes `path` as the file to restore or run, shown in the file field.
    /// The opt-in to its client commands starts unchecked.
    /// The file to restore or run, once chosen; for tests.
    #[cfg(test)]
    pub fn file(&self) -> Option<&std::path::Path> {
        self.file.as_deref()
    }

    /// Takes the path typed in the file field (`~/` is the home folder)
    /// when it names another file than the one in use; the field is left as
    /// typed. An emptied field forgets the file.
    fn take_typed(&mut self, cx: &mut Context<Self>) {
        self.typing = None;
        let typed = self.file_field.read(cx).value().trim().to_string();
        let path = (!typed.is_empty()).then(|| expand_home(&typed));
        if self.file != path {
            self.file = path;
            self.probe(cx);
        }
    }

    pub fn load_input(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.typing = None;
        let shown = path.display().to_string();
        if self.file_field.read(cx).value() != shown {
            self.file_field
                .update(cx, |field, cx| field.set_value(shown, window, cx));
        }
        self.file = Some(path);
        self.page = Page::Settings;
        self.probe(cx);
    }

    /// Asks for the output folder.
    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Write here".into()),
        });
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picking.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.folder.update(cx, |input, cx| {
                        input.set_value(path.display().to_string(), window, cx)
                    });
                    cx.notify();
                });
            }
        }));
    }

    /// Backup: only `tables`, as DBeaver's Tools ▸ Backup on selected
    /// tables; an empty list is the whole database again.
    pub fn only_tables(&mut self, tables: Vec<TableRef>, cx: &mut Context<Self>) {
        if !tables.is_empty() {
            self.view = View::Advanced;
        }
        self.selection.complete = tables.is_empty();
        self.selection.schemas = tables.iter().map(|table| table.schema.clone()).collect();
        if let Some(first) = tables.first() {
            self.selection.current = Some(first.schema.clone());
            self.page = Page::Settings;
        }
        self.selection.tables = tables.into_iter().collect();
        cx.notify();
    }

    /// Backup: these schemas ticked whole (DBeaver's backup of a schema);
    /// an empty list is the whole database again.
    pub fn only_schemas(&mut self, schemas: Vec<String>, cx: &mut Context<Self>) {
        if !schemas.is_empty() {
            self.view = View::Advanced;
        }
        self.selection.complete = schemas.is_empty();
        self.selection.schemas = schemas.iter().cloned().collect();
        self.selection.tables.clear();
        if let Some(first) = schemas.first() {
            self.selection.current = Some(first.clone());
        }
        self.pending_schemas = schemas;
        self.fill_pending();
        cx.notify();
    }

    /// Ticks the tables of the schemas ticked whole, once they are read.
    fn fill_pending(&mut self) {
        if !matches!(self.objects, Loaded::Ready(_)) {
            return;
        }
        for schema in std::mem::take(&mut self.pending_schemas) {
            let tables = self
                .entry(&schema)
                .map(|entry| self.visible(entry))
                .unwrap_or_default();
            self.selection.tables.extend(tables);
        }
    }

    /// What the wizard is for; `changed` is called once a task was saved or
    /// ran.
    pub fn set_purpose(
        &mut self,
        purpose: Purpose,
        changed: impl Fn(&mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        // A task carries its settings: the wizard shows them all.
        if purpose != Purpose::Run {
            self.view = View::Advanced;
        }
        self.purpose = purpose;
        self.on_tasks_changed = Some(Rc::new(changed));
        cx.notify();
    }

    /// Takes a saved task's settings: the options, the fields, the objects
    /// ticked, the file. The settings are of this wizard's kind; an
    /// authentication override's password is left empty, to be typed.
    pub fn apply(&mut self, settings: TaskSettings, window: &mut Window, cx: &mut Context<Self>) {
        fn set(input: &Entity<InputState>, value: String, window: &mut Window, cx: &mut App) {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.view = View::Advanced;
        let folder = |path: &Path| path.display().to_string();
        let (extra, auth_user) = match &settings {
            TaskSettings::Backup(o) => (o.extra_args.clone(), o.auth_user.clone()),
            TaskSettings::GlobalBackup(o) => (o.extra_args.clone(), o.auth_user.clone()),
            TaskSettings::Restore(o) => (o.extra_args.clone(), o.auth_user.clone()),
            TaskSettings::ExecuteScript(o) => (o.extra_args.clone(), o.auth_user.clone()),
        };
        set(&self.extra, extra, window, cx);
        set(
            &self.auth_user,
            auth_user.clone().unwrap_or_default(),
            window,
            cx,
        );
        self.auth = auth_user.is_some();
        match settings {
            TaskSettings::Backup(options) => {
                set(&self.folder, folder(&options.output_folder), window, cx);
                set(&self.pattern, options.file_pattern.clone(), window, cx);
                if let EngineBackup::Postgres(pg) = &options.engine {
                    let text =
                        |value: Option<u32>| value.map(|n| n.to_string()).unwrap_or_default();
                    set(
                        &self.compression,
                        text(pg.compression.map(u32::from)),
                        window,
                        cx,
                    );
                    set(
                        &self.encoding,
                        pg.encoding.clone().unwrap_or_default(),
                        window,
                        cx,
                    );
                    set(&self.jobs, text(pg.jobs), window, cx);
                }
                let (schemas, tables) = (options.schemas.clone(), options.tables.clone());
                self.backup = options;
                if tables.is_empty() {
                    self.only_schemas(schemas, cx);
                } else {
                    self.only_tables(tables, cx);
                }
            }
            TaskSettings::GlobalBackup(options) => {
                set(&self.folder, folder(&options.output_folder), window, cx);
                set(&self.pattern, options.file_pattern.clone(), window, cx);
                set(
                    &self.encoding,
                    options.encoding.clone().unwrap_or_default(),
                    window,
                    cx,
                );
                self.excluded = options.exclude_databases.iter().cloned().collect();
                self.global = options;
            }
            TaskSettings::Restore(options) => {
                let jobs = options.jobs.map(|n| n.to_string()).unwrap_or_default();
                set(&self.jobs, jobs, window, cx);
                let input = options.input.clone();
                self.restore = options;
                self.load_input(input, window, cx);
            }
            TaskSettings::ExecuteScript(options) => {
                let input = options.input.clone();
                self.script = options;
                self.load_input(input, window, cx);
            }
        }
        cx.notify();
    }

    /// The settings a task keeps, as the pages stand, or why they cannot
    /// be kept (no password ever: [`TaskSettings::check`]).
    pub fn task_settings(&self, cx: &App) -> Result<TaskSettings, String> {
        let file = || {
            self.file
                .clone()
                .ok_or_else(|| "Choose the file first.".to_string())
        };
        let settings = match self.mode {
            Mode::Backup => {
                self.output_path(cx)?;
                TaskSettings::Backup(self.backup_options(cx)?)
            }
            Mode::GlobalBackup => {
                self.output_path(cx)?;
                TaskSettings::GlobalBackup(self.global_options(cx)?)
            }
            Mode::Restore => {
                let mut options = self.restore.clone();
                options.database = self.database.clone();
                options.input = file()?;
                if let Some(Ok(input)) = &self.input {
                    options.format = input.format;
                }
                options.jobs = Self::number(&self.jobs, "Jobs", 1..65, cx)?;
                options.extra_args = self.extra_value(cx);
                options.auth_user = self.auth_user_value(cx);
                TaskSettings::Restore(options)
            }
            Mode::ExecuteScript => {
                let mut options = self.script.clone();
                options.database = self.database.clone();
                options.input = file()?;
                options.extra_args = self.extra_value(cx);
                options.auth_user = self.auth_user_value(cx);
                TaskSettings::ExecuteScript(options)
            }
        };
        settings
            .check(self.engine)
            .map_err(|error| error_text(&error))?;
        Ok(settings)
    }

    /// Saves the settings as a new task, asking its name, or (`over`) back
    /// into the task the wizard is opened on.
    fn save_task(&mut self, over: bool, window: &mut Window, cx: &mut Context<Self>) {
        let settings = match self.task_settings(cx) {
            Ok(settings) => settings,
            Err(why) => {
                window.push_notification(
                    Notification::warning(SharedString::from(format!(
                        "The task cannot be saved: {why}"
                    ))),
                    cx,
                );
                return;
            }
        };
        if let (true, Purpose::Task(task)) = (over, &self.purpose) {
            let mut task = (**task).clone();
            task.settings = settings;
            self.store_task(task, window, cx);
            return;
        }
        let suggested = format!("{} of {}", settings.label(), self.database);
        let connection = self.connection.clone();
        let view = cx.entity();
        crate::tasks_view::ask_task_name(
            "Save as task",
            suggested,
            String::new(),
            window,
            cx,
            move |name, description, window, cx| {
                let mut task = SavedTask::new(name, connection.clone(), settings.clone());
                task.description = description;
                view.update(cx, |this, cx| this.store_task(task, window, cx));
            },
        );
    }

    /// Writes `task` and makes it the wizard's task.
    fn store_task(&mut self, task: SavedTask, window: &mut Window, cx: &mut Context<Self>) {
        let storage = AppState::get(cx).storage.clone();
        let saving = {
            let task = task.clone();
            bridge::spawn_blocking(cx, move || storage.save_task(&task))
        };
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let saved = saving.await;
            let _ = this.update_in(cx, |this, window, cx| match saved {
                Ok(id) => {
                    let name = task.name.clone();
                    this.purpose = Purpose::Task(Box::new(SavedTask { id, ..task }));
                    window.push_notification(
                        Notification::success(SharedString::from(format!(
                            "Saved the task “{name}”."
                        ))),
                        cx,
                    );
                    if let Some(changed) = this.on_tasks_changed.clone() {
                        changed(cx);
                    }
                    cx.notify();
                }
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(error.to_string())),
                    cx,
                ),
            });
        }));
    }

    /// Keeps how a task's run ended and its log with the task.
    fn record_run(&self, status: HistoryStatus, ended: &str, cx: &mut Context<Self>) {
        let Purpose::Task(task) = &self.purpose else {
            return;
        };
        let lines: Vec<String> = self
            .shown_command
            .iter()
            .map(|command| format!("$ {command}"))
            .chain(self.log.iter().map(|line| line.to_string()))
            .chain([ended.to_string()])
            .collect();
        let rules = LexRules::of(self.engine);
        let (storage, id) = (AppState::get(cx).storage.clone(), task.id);
        let recording = bridge::spawn_blocking(cx, move || {
            storage.record_task_run(id, status, &lines, rules)
        });
        let changed = self.on_tasks_changed.clone();
        cx.spawn(async move |_, cx| {
            if let Err(error) = recording.await {
                tracing::warn!(%error, "the task's run was not kept");
            }
            if let Some(changed) = changed {
                cx.update(|cx| changed(cx));
            }
        })
        .detach();
    }

    /// Whether Start can run now with nothing more to type: the client is
    /// found and suits, the file is read, the objects are listed, and no
    /// password is to be typed for an authentication override (a saved
    /// task asks it again at each run).
    pub fn ready_to_start(&self, cx: &App) -> bool {
        let typed = !self.auth || !self.auth_password.read(cx).value().is_empty();
        let listed = self.mode != Mode::Backup || matches!(self.objects, Loaded::Ready(_));
        matches!(self.step, Step::Options)
            && matches!(self.tool_state, ToolState::Ready { .. })
            && (!self.mode.writes() || matches!(self.input, Some(Ok(_))))
            && listed
            && typed
            && self.blocked(cx).is_none()
    }

    /// Shows the last settings page, where Start is.
    pub fn show_last_page(&mut self, cx: &mut Context<Self>) {
        self.page = self.last_settings_page();
        cx.notify();
    }

    /// The task the wizard is opened on, if any.
    pub fn task(&self) -> Option<&SavedTask> {
        match &self.purpose {
            Purpose::Task(task) => Some(task),
            _ => None,
        }
    }

    /// The tables (and views, when shown) of a schema.
    fn visible(&self, entry: &SchemaEntry) -> Vec<TableRef> {
        entry
            .relations
            .iter()
            .filter(|(_, kind)| self.selection.show_views || !is_view(kind))
            .map(|(name, _)| TableRef::new(entry.name.clone(), name.clone()))
            .collect()
    }

    fn entry(&self, schema: &str) -> Option<&SchemaEntry> {
        match &self.objects {
            Loaded::Ready(entries) => entries.iter().find(|entry| entry.name == schema),
            _ => None,
        }
    }

    /// What the objects page asks for: `(schemas, tables)`, both empty
    /// for the whole database. pg_dump ignores `--schema` once a `--table`
    /// is given, so a schema not wholly ticked turns every pick into
    /// tables (DBeaver's `PostgreBackupWizardPageObjects` rule).
    fn picked(&self) -> Result<(Vec<String>, Vec<TableRef>), String> {
        let selection = &self.selection;
        if selection.complete {
            return Ok((Vec::new(), Vec::new()));
        }
        if selection.schemas.is_empty() && selection.tables.is_empty() {
            return Err("Tick at least one schema or table, or choose a complete backup.".into());
        }
        let partial = selection
            .tables
            .iter()
            .any(|table| !selection.schemas.contains(&table.schema))
            || selection
                .schemas
                .iter()
                .any(|schema| match self.entry(schema) {
                    Some(entry) => self
                        .visible(entry)
                        .iter()
                        .any(|table| !selection.tables.contains(table)),
                    // Not listed yet (K65: a slower server): its ticked tables
                    // are all that is known of it, never the whole schema.
                    None => selection.tables.iter().any(|table| &table.schema == schema),
                });
        let tables = || selection.tables.iter().cloned().collect();
        Ok(match (self.family, partial) {
            (_, true) => (Vec::new(), tables()),
            (ClientFamily::Postgres, false) => {
                (selection.schemas.iter().cloned().collect(), Vec::new())
            }
            (ClientFamily::MySql, false) => (Vec::new(), Vec::new()),
        })
    }

    fn toggle_schema(&mut self, schema: String, cx: &mut Context<Self>) {
        let tables = self
            .entry(&schema)
            .map(|entry| self.visible(entry))
            .unwrap_or_default();
        let selection = &mut self.selection;
        if selection.schemas.remove(&schema) {
            selection.tables.retain(|table| table.schema != schema);
        } else {
            selection.schemas.insert(schema.clone());
            selection.tables.extend(tables);
        }
        selection.current = Some(schema);
        cx.notify();
    }

    fn toggle_table(&mut self, table: TableRef, cx: &mut Context<Self>) {
        let selection = &mut self.selection;
        if selection.tables.remove(&table) {
            if !selection.tables.iter().any(|t| t.schema == table.schema) {
                selection.schemas.remove(&table.schema);
            }
        } else {
            selection.schemas.insert(table.schema.clone());
            selection.tables.insert(table);
        }
        cx.notify();
    }

    /// All / None of the schemas (`schema` `None`) or of one schema's tables.
    fn tick_all(&mut self, schema: Option<String>, on: bool, cx: &mut Context<Self>) {
        let entries: Vec<SchemaEntry> = match (&self.objects, &schema) {
            (Loaded::Ready(entries), None) => entries.clone(),
            (Loaded::Ready(_), Some(name)) => self.entry(name).cloned().into_iter().collect(),
            _ => Vec::new(),
        };
        for entry in entries {
            let tables = self.visible(&entry);
            let selection = &mut self.selection;
            if on {
                selection.schemas.insert(entry.name.clone());
                selection.tables.extend(tables);
            } else {
                selection.schemas.remove(&entry.name);
                selection.tables.retain(|table| table.schema != entry.name);
            }
        }
        cx.notify();
    }

    fn set_complete(&mut self, complete: bool, cx: &mut Context<Self>) {
        self.selection.complete = complete;
        if !complete && self.selection.schemas.is_empty() && self.selection.tables.is_empty() {
            self.tick_all(None, true, cx);
        }
        cx.notify();
    }

    fn set_show_views(&mut self, show: bool, cx: &mut Context<Self>) {
        self.selection.show_views = show;
        if !show && let Loaded::Ready(entries) = &self.objects {
            let views: Vec<TableRef> = entries
                .iter()
                .flat_map(|entry| {
                    entry
                        .relations
                        .iter()
                        .filter(|(_, kind)| is_view(kind))
                        .map(|(name, _)| TableRef::new(entry.name.clone(), name.clone()))
                })
                .collect();
            self.selection.tables.retain(|table| !views.contains(table));
        }
        cx.notify();
    }

    fn text(input: &Entity<InputState>, cx: &App) -> String {
        input.read(cx).value().trim().to_string()
    }

    /// A small number typed in a field: `None` when empty.
    fn number(
        input: &Entity<InputState>,
        what: &str,
        range: Range<u32>,
        cx: &App,
    ) -> Result<Option<u32>, String> {
        let text = Self::text(input, cx);
        if text.is_empty() {
            return Ok(None);
        }
        text.parse::<u32>()
            .ok()
            .filter(|value| range.contains(value))
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "{what} is a number from {} to {}.",
                    range.start,
                    range.end - 1
                )
            })
    }

    fn encoding_value(&self, cx: &App) -> Result<Option<String>, String> {
        let text = Self::text(&self.encoding, cx);
        if text.is_empty() {
            Ok(None)
        } else if text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            Ok(Some(text))
        } else {
            Err(format!(
                "“{text}” is not an encoding name (UTF8, LATIN1, WIN1252…)."
            ))
        }
    }

    fn auth_user_value(&self, cx: &App) -> Option<String> {
        self.auth
            .then(|| Self::text(&self.auth_user, cx))
            .filter(|user| !user.is_empty())
    }

    fn extra_value(&self, cx: &App) -> String {
        self.extra.read(cx).value().to_string()
    }

    /// The backup settings as the pages stand.
    fn backup_options(&self, cx: &App) -> Result<BackupOptions, String> {
        let mut options = self.backup.clone();
        let (schemas, tables) = self.picked()?;
        options.schemas = schemas;
        options.tables = tables;
        if let EngineBackup::Postgres(pg) = &mut options.engine {
            pg.compression = Self::number(&self.compression, "Compression", 0..10, cx)?
                .and_then(|level| u8::try_from(level).ok());
            pg.encoding = self.encoding_value(cx)?;
            pg.jobs = if options.format == BackupFormat::Directory {
                Self::number(&self.jobs, "Jobs", 1..65, cx)?
            } else {
                None
            };
        }
        options.output_folder = PathBuf::from(Self::text(&self.folder, cx));
        options.file_pattern = Self::text(&self.pattern, cx);
        options.extra_args = self.extra_value(cx);
        options.auth_user = self.auth_user_value(cx);
        Ok(options)
    }

    /// Whether the global backup writes databases (not the globals only).
    fn dumps_databases(&self) -> bool {
        let g = &self.global;
        !(g.globals_only || g.roles_only || g.tablespaces_only)
    }

    fn global_options(&self, cx: &App) -> Result<GlobalBackupOptions, String> {
        let mut options = self.global.clone();
        options.encoding = self.encoding_value(cx)?;
        let only = options.globals_only || options.roles_only || options.tablespaces_only;
        if only {
            options.exclude_databases.clear();
        } else {
            options.exclude_databases = self.excluded.iter().cloned().collect();
            if let Loaded::Ready(all) = &self.databases
                && !all.is_empty()
                && all.iter().all(|database| self.excluded.contains(database))
            {
                return Err("Tick at least one database, or back up the globals only.".into());
            }
        }
        options.output_folder = PathBuf::from(Self::text(&self.folder, cx));
        options.file_pattern = Self::text(&self.pattern, cx);
        options.extra_args = self.extra_value(cx);
        options.auth_user = self.auth_user_value(cx);
        Ok(options)
    }

    /// The values the file name pattern's variables take.
    fn name_vars(&self, schemas: &[String], tables: &[TableRef]) -> NameVars {
        NameVars {
            host: self.host.clone(),
            database: self.database.clone(),
            schema: schemas
                .first()
                .cloned()
                .or_else(|| tables.first().map(|table| table.schema.clone())),
            table: tables.first().map(|table| table.name.clone()),
            environment: self.environment.clone(),
            now: self.stamp,
        }
    }

    /// The file a backup writes, as the settings stand.
    fn output_path(&self, cx: &App) -> Result<PathBuf, String> {
        let (folder, pattern, format, vars) = match self.mode {
            Mode::Backup => {
                let options = self.backup_options(cx)?;
                let vars = self.name_vars(&options.schemas, &options.tables);
                (
                    options.output_folder,
                    options.file_pattern,
                    options.format,
                    vars,
                )
            }
            Mode::GlobalBackup => (
                PathBuf::from(Self::text(&self.folder, cx)),
                Self::text(&self.pattern, cx),
                BackupFormat::Sql,
                self.name_vars(&[], &[]),
            ),
            Mode::Restore | Mode::ExecuteScript => {
                return Err("This wizard writes no file.".into());
            }
        };
        if !folder.is_absolute() {
            return Err("Choose the folder the backup is written to.".into());
        }
        tools::output_path(&folder, &pattern, format, &vars).map_err(|error| error_text(&error))
    }

    /// The connection's route for this run: another user's password when
    /// the override is on (the user goes in the settings).
    fn run_params(&self, cx: &App) -> Option<ConnectParams> {
        let mut params = self.params.clone()?;
        if self.auth_user_value(cx).is_some() {
            let password = self.auth_password.read(cx).value().to_string();
            params.password = (!password.is_empty()).then(|| Secret::new(password));
        }
        Some(params)
    }

    /// The command as the settings stand, or why there is none.
    fn command(&self, cx: &App) -> Result<ToolCommand, String> {
        let (ToolState::Ready { path, install, .. }, Some(params)) =
            (&self.tool_state, self.run_params(cx))
        else {
            return Err("The command appears once the client is found.".into());
        };
        let version = install.version.clone();
        let built = match self.mode {
            Mode::Backup => {
                let options = self.backup_options(cx)?;
                tools::backup_command(path, &params, &options, &self.output_path(cx)?)
            }
            Mode::GlobalBackup => {
                let mut options = self.global_options(cx)?;
                // Left out of this run, not of the task (K62).
                if self.dumps_databases() {
                    options
                        .exclude_databases
                        .extend(self.closed.iter().cloned());
                }
                tools::global_backup_command(path, &params, &options, &self.output_path(cx)?)
            }
            Mode::Restore => {
                let Some(Ok(input)) = &self.input else {
                    return Err("Choose the file to restore.".into());
                };
                let mut options = self.restore.clone();
                options.database = self.database.clone();
                options.input = input.path.clone();
                options.format = input.format;
                options.jobs = if input.format == BackupFormat::Sql {
                    None
                } else {
                    Self::number(&self.jobs, "Jobs", 1..65, cx)?
                };
                options.extra_args = self.extra_value(cx);
                options.auth_user = self.auth_user_value(cx);
                options.allow_client_commands = self.allow_commands;
                options.tool_version = version;
                options.use_list = self.use_list(cx);
                tools::restore_command(path, &params, &options)
            }
            Mode::ExecuteScript => {
                let Some(Ok(input)) = &self.input else {
                    return Err("Choose the script to run.".into());
                };
                let mut options = self.script.clone();
                options.database = self.database.clone();
                options.input = input.path.clone();
                options.extra_args = self.extra_value(cx);
                options.auth_user = self.auth_user_value(cx);
                options.allow_client_commands = self.allow_commands;
                options.tool_version = version;
                tools::script_command(path, &params, &options)
            }
        };
        built.map_err(|error| error_text(&error))
    }

    /// The pages of this wizard, in order.
    fn pages(&self) -> Vec<Page> {
        let check = matches!(
            &self.input,
            Some(Ok(Input {
                refusal: Some(_),
                ..
            }))
        );
        match self.mode {
            Mode::Backup | Mode::GlobalBackup => {
                vec![Page::Objects, Page::Settings, Page::Progress]
            }
            Mode::Restore | Mode::ExecuteScript if check => {
                vec![Page::Settings, Page::Check, Page::Progress]
            }
            Mode::Restore | Mode::ExecuteScript => vec![Page::Settings, Page::Progress],
        }
    }

    /// The page shown.
    fn current_page(&self) -> Page {
        match self.step {
            Step::Running | Step::Finished { .. } => Page::Progress,
            _ => self.page,
        }
    }

    /// The last page before the progress, where Start is.
    fn last_settings_page(&self) -> Page {
        let pages = self.pages();
        pages[pages.len() - 2]
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        let pages = self.pages();
        if let Some(at) = pages.iter().position(|page| *page == self.page)
            && let Some(next) = pages.get(at + 1).filter(|page| **page != Page::Progress)
        {
            self.page = *next;
            self.showing_clients = false;
            cx.notify();
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if matches!(self.step, Step::Finished { .. }) {
            self.step = Step::Options;
            self.page = self.last_settings_page();
            cx.notify();
            return;
        }
        let pages = self.pages();
        if let Some(at) = pages.iter().position(|page| *page == self.page)
            && at > 0
        {
            self.page = pages[at - 1];
            self.showing_clients = false;
            cx.notify();
        }
    }

    /// Why Start cannot run, if it cannot.
    fn blocked(&self, cx: &App) -> Option<String> {
        if let ToolState::Ready { compat, .. } = &self.tool_state
            && compat.blocks()
        {
            return compat.warning(self.engine);
        }
        if matches!(
            &self.input,
            Some(Ok(Input {
                refusal: Some(_),
                ..
            }))
        ) && !self.allow_commands
        {
            return Some("The file's client commands are not allowed.".into());
        }
        self.command(cx).err()
    }

    /// Start: a backup runs (asking first when its file exists); a restore
    /// or a script shows its command and asks.
    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stamp = Stamp::now();
        if self.blocked(cx).is_some() {
            return;
        }
        if self.mode.writes() {
            self.confirm_restore(window, cx);
        } else {
            self.back_up(window, cx);
        }
    }

    /// Runs the backup; a file or folder already there is replaced only
    /// once confirmed.
    pub fn back_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Ok(command), Ok(output)) = (self.command(cx), self.output_path(cx)) else {
            return;
        };
        let checking = {
            let output = output.clone();
            bridge::spawn_blocking(cx, move || output.exists())
        };
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let exists = checking.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.replace_or_run(command, output, exists, window, cx)
            });
        }));
    }

    fn replace_or_run(
        &mut self,
        command: ToolCommand,
        output: PathBuf,
        exists: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !exists {
            self.run(command, Some((output, false)), window, cx);
            return;
        }
        if self.mode == Mode::Backup && self.backup.format == BackupFormat::Directory {
            // pg_dump writes a directory archive only into a new folder.
            window.push_notification(
                Notification::warning(SharedString::from(format!(
                    "{} already exists: pg_dump writes a directory archive into a new folder. \
                     Change the file name.",
                    output.display()
                ))),
                cx,
            );
            return;
        }
        if self.agreed_replace.as_deref() == Some(output.as_path()) {
            // Chosen in the save panel, which asked: once is enough.
            self.run(command, Some((output, true)), window, cx);
            return;
        }
        let view = cx.entity();
        let confirmed = command.clone();
        sql_confirm::open(
            Confirmation {
                title: format!(
                    "Replace {}",
                    output
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                ),
                explanation: format!(
                    "{} already exists: the backup replaces it.",
                    output.display()
                ),
                statement: Statement::Fixed(command.display()),
                action: "Replace",
                destructive: true,
                connection: self.name.clone(),
                environment: self.environment.clone(),
                writes: false,
            },
            window,
            cx,
            move |_, window, cx| {
                let (command, output) = (confirmed.clone(), output.clone());
                view.update(cx, |this, cx| {
                    this.run(command, Some((output, true)), window, cx)
                });
            },
        );
    }

    /// Shows what the restore or the script runs and, once confirmed, runs
    /// it.
    pub fn confirm_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Ok(input)) = self.input.clone() else {
            return;
        };
        if input.refusal.is_some() && !self.allow_commands {
            return;
        }
        let Ok(command) = self.command(cx) else {
            return;
        };
        let file = input
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let view = cx.entity();
        let confirmed = command.clone();
        let script = self.mode == Mode::ExecuteScript;
        sql_confirm::open(
            Confirmation {
                title: if script {
                    format!("Run {file} in {}", self.database)
                } else {
                    format!("Restore {file} into {}", self.database)
                },
                explanation: restore_explanation(&Explained {
                    family: self.family,
                    database: &self.database,
                    connection: &self.name,
                    input: &input,
                    program: &program_name(&command),
                    allow_commands: self.allow_commands,
                    script,
                    restore: &self.restore,
                }),
                statement: Statement::Fixed(self.shown(&command)),
                action: if script { "Run" } else { "Restore" },
                destructive: true,
                connection: self.name.clone(),
                environment: self.environment.clone(),
                writes: true,
            },
            window,
            cx,
            move |_, window, cx| {
                let command = confirmed.clone();
                view.update(cx, |this, cx| this.run(command, None, window, cx));
            },
        );
    }

    /// Runs `command`, its output streamed into the log. `output`: the
    /// file or folder a backup writes, and whether it was there before.
    fn run(
        &mut self,
        command: ToolCommand,
        output: Option<(PathBuf, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shown_command = Some(self.shown(&command));
        self.output = output.as_ref().map(|(path, _)| path.clone());
        self.log.clear();
        // Said first, and kept with a task's run (K62).
        if self.mode == Mode::GlobalBackup
            && self.dumps_databases()
            && let Some(text) = left_out_closed(&self.closed, usize::MAX)
        {
            self.log.push_back(text.into());
        }
        self.roles = None;
        self.step = Step::Running;
        self.showing_clients = false;
        self.started = Some(Instant::now());
        self.elapsed = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let program = program_name(&command);
        let directory = self.mode == Mode::Backup && self.backup.format == BackupFormat::Directory;
        let total = Arc::new(AtomicU64::new(0));
        self.feed = command
            .stdin
            .is_some()
            .then(|| (command.sent.clone(), total.clone()));
        // The archive's list without the left-out extensions (K39).
        let list = self
            .use_list(cx)
            .filter(|path| {
                let arg = format!("--use-list={}", path.display());
                command.args.contains(&arg)
            })
            .zip(match &self.input {
                Some(Ok(input)) => input.list.clone(),
                _ => None,
            })
            .map(|(path, list)| (path, list, self.left_out()));
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
        let running = bridge::spawn(cx, async move {
            // The time of the file it replaces, to tell whether the tool
            // opened it; not the clock, which Linux's file times can be a
            // tick behind (K65).
            let modified = |path: &Path| {
                std::fs::metadata(path)
                    .and_then(|meta| meta.modified())
                    .ok()
            };
            let earlier = output
                .as_ref()
                .filter(|(_, existed)| *existed)
                .map(|(path, _)| modified(path));
            if let Some(input) = &command.stdin
                && let Ok(meta) = tokio::fs::metadata(input).await
            {
                total.store(meta.len(), Ordering::Relaxed);
            }
            let written = match &list {
                None => Ok(()),
                Some((path, list, left_out)) => {
                    write_new(path, &tools::list_without(list, left_out))
                }
            };
            // A directory archive goes into a folder pg_dump makes. One there
            // now (made since Start was checked) is not this run's: it is
            // never removed, and nothing runs (K48).
            let taken = output
                .as_ref()
                .filter(|_| directory)
                .map(|(path, _)| path)
                .filter(|path| std::fs::symlink_metadata(path).is_ok());
            let outcome = match written {
                Ok(()) if taken.is_some() => Err(Error::Config(format!(
                    "{} already exists: pg_dump writes a directory archive into a new folder. \
                     Nothing was run.",
                    taken
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                ))),
                Ok(()) => {
                    tools::run(
                        command,
                        move |line| {
                            let _ = sender.send(line);
                        },
                        cancel,
                    )
                    .await
                }
                Err(error) => Err(Error::Config(format!(
                    "Kerno could not write the archive's list for pg_restore: {error}. Nothing \
                     was run."
                ))),
            };
            if let Some((path, ..)) = &list {
                let _ = std::fs::remove_file(path);
            }
            let mut ended = Ended {
                outcome,
                size: None,
                removed: false,
            };
            if let Some((path, existed)) = &output {
                match &ended.outcome {
                    Ok(status) if status.success() => ended.size = backup_size(path),
                    // The tool stopped short: what it wrote is incomplete.
                    // What it never opened (it failed to connect) may be an
                    // earlier backup: kept.
                    Ok(_) | Err(Error::Cancelled) if directory => {
                        ended.removed = !existed && std::fs::remove_dir_all(path).is_ok();
                    }
                    Ok(_) | Err(Error::Cancelled) => {
                        let written = earlier
                            .as_ref()
                            .is_none_or(|earlier| modified(path) != *earlier);
                        ended.removed = written && std::fs::remove_file(path).is_ok();
                    }
                    Err(_) => {}
                }
            }
            ended
        });
        self.running = Some(cx.spawn_in(window, async move |this, cx| {
            // The channel closes once the tool ended and `run` returned.
            while let Some(line) = receiver.recv().await {
                let mut lines = vec![line];
                while let Ok(line) = receiver.try_recv() {
                    lines.push(line);
                }
                if this
                    .update(cx, |this, cx| this.push_lines(lines, cx))
                    .is_err()
                {
                    return;
                }
            }
            let ended = running.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish(program, ended, window, cx)
            });
        }));
        // The elapsed time and the progress, while it runs.
        self.ticking = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        matches!(this.step, Step::Running)
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// How much of the file a restore or script has sent to its tool:
    /// "45.2 MB of 200 MB sent (22 %)" (CLAUDE.md §4 J9).
    pub fn progress(&self) -> Option<String> {
        let (sent, total) = self.feed.as_ref()?;
        let (sent, total) = (sent.load(Ordering::Relaxed), total.load(Ordering::Relaxed));
        if total == 0 {
            return None;
        }
        Some(format!(
            "{} of {} sent ({} %)",
            byte_size(sent),
            byte_size(total),
            sent.min(total) * 100 / total
        ))
    }

    /// Runs the restore or script at once, without the confirmation Start
    /// shows (the bench cannot click it: `KERNO_BENCH=import` only, on its
    /// scratch database); what keeps it from running otherwise.
    pub fn run_unconfirmed(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if !crate::bench::active() {
            return Err("only in bench mode".into());
        }
        if matches!(self.tool_state, ToolState::Probing) || self.input.is_none() {
            return Err("probing".into());
        }
        let command = self.command(cx)?;
        self.run(command, None, window, cx);
        Ok(())
    }

    /// Whether the run succeeded, once it has ended, with what it says.
    pub fn ended(&self) -> Option<(bool, &str)> {
        match &self.step {
            Step::Finished { success, text } => Some((*success, text)),
            _ => None,
        }
    }

    /// Bytes of the file sent to the tool and the file's size; for the
    /// bench.
    pub fn fed(&self) -> Option<(u64, u64)> {
        let (sent, total) = self.feed.as_ref()?;
        Some((sent.load(Ordering::Relaxed), total.load(Ordering::Relaxed)))
    }

    fn push_lines(&mut self, lines: Vec<String>, cx: &mut Context<Self>) {
        // A tool may echo what it was given (psql's --echo-errors prints a
        // failed statement): the passwords of this run never show, nor
        // reach a saved task's log.
        let typed = self.auth_password.read(cx).value().to_string();
        let secrets: Vec<String> = self
            .params
            .as_ref()
            .and_then(|params| params.password.as_ref())
            .map(|password| password.expose().to_string())
            .into_iter()
            .chain([typed])
            .filter(|secret| !secret.is_empty())
            .collect();
        // Only the lines the log keeps: a script prints one per statement.
        let skip = lines.len().saturating_sub(LOG_LINES);
        for mut line in lines.into_iter().skip(skip) {
            for secret in &secrets {
                line = line.replace(secret.as_str(), "•••");
            }
            if self.log.len() == LOG_LINES {
                self.log.pop_front();
            }
            self.log.push_back(line.into());
        }
        self.log_scroll
            .scroll_to_item(self.log.len().saturating_sub(1), ScrollStrategy::Bottom);
        cx.notify();
    }

    fn finish(
        &mut self,
        program: String,
        ended: Ended,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.elapsed = self.started.map(|started| started.elapsed());
        self.ticking = None;
        let writes_file = !self.mode.writes();
        let folder = self.mode == Mode::Backup && self.backup.format == BackupFormat::Directory;
        let kept = match (writes_file, self.engine) {
            (true, _) if ended.removed && folder => " The incomplete folder was removed.",
            (true, _) if ended.removed => " The incomplete file was removed.",
            (true, _) => "",
            (false, Engine::Postgres)
                if self.mode == Mode::Restore && self.restore.single_transaction =>
            {
                " It ran in one transaction: nothing of it was kept."
            }
            (false, Engine::Postgres) => " What ran before it stopped is kept.",
            (false, _) => {
                " MySQL and MariaDB run the file statement by statement: what ran before it \
                 stopped is kept."
            }
        };
        let run = match &ended.outcome {
            Ok(status) if status.success() => HistoryStatus::Ok,
            Err(Error::Cancelled) => HistoryStatus::Cancelled,
            _ => HistoryStatus::Error,
        };
        let past = (self.mode == Mode::Restore
            && self.engine == Engine::Postgres
            && !self.restore.single_transaction)
            .then(|| {
                let lines: Vec<&str> = self.log.iter().map(AsRef::as_ref).collect();
                errors_ignored(&lines).map(|errors| (errors, ignored_commands(&lines)))
            })
            .flatten();
        self.step = match ended.outcome {
            Ok(status) if status.success() => {
                if self.mode == Mode::Backup
                    && let Some(folder) = self.output.as_deref().and_then(Path::parent)
                {
                    let folder = folder.to_path_buf();
                    AppState::update_settings(cx, |settings| settings.backup_folder = Some(folder));
                }
                let text = if writes_file {
                    format!(
                        "{program} finished ({}). Backed up {} to {}{}.",
                        status_text(status),
                        if self.mode == Mode::Backup {
                            self.database.clone()
                        } else {
                            format!("the server of “{}”", self.name)
                        },
                        self.output
                            .as_deref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_default(),
                        ended
                            .size
                            .map(|size| format!(" ({})", byte_size(size)))
                            .unwrap_or_default()
                    )
                } else {
                    let restored = self.on_restored.clone();
                    restored(&self.connection, &self.database, window, cx);
                    let (done, to) = if self.mode == Mode::Restore {
                        ("Restored", "into")
                    } else {
                        ("Ran", "in")
                    };
                    format!(
                        "{program} finished ({}). {done} {} {to} {}.",
                        status_text(status),
                        self.file
                            .as_deref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_default(),
                        self.database
                    )
                };
                Step::Finished {
                    success: true,
                    text,
                }
            }
            // pg_restore outside one transaction goes on past a statement
            // that fails and still exits with 1: the restore is there, less
            // those statements. Said as that, not as a failure.
            Ok(status) if past.is_some() => {
                let (errors, commands) = past.unwrap_or_default();
                let restored = self.on_restored.clone();
                restored(&self.connection, &self.database, window, cx);
                let which = match commands.as_slice() {
                    [] => String::new(),
                    [one] => format!(" The statement that failed: {one}"),
                    several => format!(
                        " The statements that failed: {}{}",
                        several
                            .iter()
                            .take(3)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" "),
                        if several.len() > 3 { " …" } else { "" }
                    ),
                };
                Step::Finished {
                    success: true,
                    text: format!(
                        "{program} went to the end ({}) past {errors} error{} it ignored: \
                         everything else was restored into {}.{which}",
                        status_text(status),
                        if errors == 1 { "" } else { "s" },
                        self.database
                    ),
                }
            }
            Ok(status) => Step::Finished {
                success: false,
                text: format!("{program} failed ({}).{kept}", status_text(status)),
            },
            Err(Error::Cancelled) => Step::Finished {
                success: false,
                text: format!("Cancelled: {program} was stopped.{kept}"),
            },
            Err(Error::Config(message)) => Step::Finished {
                success: false,
                text: message,
            },
            Err(error) => Step::Finished {
                success: false,
                text: format!("{error}. Nothing was run."),
            },
        };
        if let Step::Finished { text, .. } = &self.step {
            let text = text.clone();
            self.record_run(run, &text, cx);
        }
        if run == HistoryStatus::Error
            && self.mode == Mode::Restore
            && self.engine == Engine::Postgres
        {
            self.check_roles(cx);
        }
        if self.close_when_stopped {
            window.close_dialog(cx);
        }
        cx.notify();
    }

    /// Reads the roles and the extensions the failed restore names from its
    /// output (and, for psql, from the script's line it points at), then
    /// asks the server which of them it does not have: only those are said
    /// to be missing.
    fn check_roles(&mut self, cx: &mut Context<Self>) {
        let Some(Ok(input)) = &self.input else {
            return;
        };
        let script = input.format == BackupFormat::Sql;
        let path = input.path.clone();
        let listed = input.extensions.clone();
        let log: Vec<String> = self.log.iter().map(|line| line.to_string()).collect();
        let started = self.started;
        let user = self
            .auth_user_value(cx)
            .or_else(|| self.params.as_ref().map(|params| params.user.clone()))
            .unwrap_or_default();
        let failure = RoleFailure::new(script, &self.restore, user);
        let meta = self.connections.read(cx).meta_session(&self.connection);
        let reading = bridge::spawn_blocking(cx, move || {
            let line = |number| script.then(|| script_line(&path, number)).flatten();
            (
                tools::refused_roles(&log, line),
                tools::refused_extensions(&log, line),
            )
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let (refused, extensions) = reading.await;
            if refused.is_empty() && extensions.is_empty() {
                return;
            }
            let names: Vec<String> = refused.iter().map(|role| role.name.clone()).collect();
            let asking = cx.update(|cx| {
                bridge::spawn(cx, async move {
                    let meta = meta?;
                    let roles = absent(&meta, ROLES, &names).await;
                    // An extension is said only when the server says it does
                    // not offer it: a refusal for another reason (rights)
                    // names it too.
                    let extensions = absent(&meta, EXTENSIONS, &extensions)
                        .await
                        .unwrap_or_default();
                    Some((roles, extensions))
                })
            });
            let (missing, extensions) = asking.await.unwrap_or_default();
            if refused.is_empty() && extensions.is_empty() {
                return;
            }
            let leavable = extensions.iter().all(|name| listed.contains(name));
            let _ = this.update(cx, |this, cx| {
                // A run started since has its own output.
                if this.started == started {
                    this.roles = Some(RoleFailure {
                        refused,
                        missing,
                        extensions,
                        leavable,
                        ..failure
                    });
                    cx.notify();
                }
            });
        }));
    }

    /// Before the run (K39): reads from the archive's list the extensions it
    /// creates and the roles owning its objects, and asks the server which
    /// it lacks. Missing extensions are left out unless the user keeps
    /// them; missing owners tick "Leave out the backup's owners and
    /// privileges". Nothing is shown when the server cannot be asked.
    fn check_archive(&mut self, cx: &mut Context<Self>) {
        let Some(Ok(Input {
            list: Some(list),
            extensions,
            ..
        })) = &self.input
        else {
            return;
        };
        let (list, extensions) = (list.clone(), extensions.clone());
        let Some(meta) = self.connections.read(cx).meta_session(&self.connection) else {
            return;
        };
        let generation = self.generation;
        let asking = bridge::spawn(cx, async move {
            let lacking = absent(&meta, EXTENSIONS, &extensions).await?;
            let roles = names_of(&meta, "SELECT rolname FROM pg_catalog.pg_roles".into()).await?;
            Some(Preflight {
                extensions: lacking,
                owners: tools::missing_owners(&list, &roles),
            })
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let Some(found) = asking.await else {
                return;
            };
            if found.extensions.is_empty() && found.owners.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.leave_out = found.extensions.clone();
                if !found.owners.is_empty() && !this.without_owners() {
                    (this.restore.no_owner, this.restore.no_privileges) = (true, true);
                    this.owners_ticked = true;
                }
                this.preflight = Some(found);
                cx.notify();
            });
        }));
    }

    /// The archive's extensions this restore leaves out: those ticked that
    /// its list names.
    fn left_out(&self) -> Vec<String> {
        match &self.input {
            Some(Ok(Input {
                list: Some(_),
                extensions,
                ..
            })) if self.mode == Mode::Restore => self
                .leave_out
                .iter()
                .filter(|name| extensions.contains(name))
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Where the list without the left-out extensions is written for
    /// `--use-list` (built in the run's task, removed after it): in Kerno's
    /// own data directory, not the temporary one, which other users of the
    /// machine share on Linux and where a name they can guess could be
    /// taken first.
    fn use_list(&self, cx: &App) -> Option<PathBuf> {
        (!self.left_out().is_empty()).then(|| {
            AppState::get(cx).paths.data_dir.join(format!(
                "restore-{}-{}.list",
                std::process::id(),
                self.generation
            ))
        })
    }

    /// The command as shown to the user: with `--use-list`, what it leaves
    /// out of the archive, in words.
    fn shown(&self, command: &ToolCommand) -> String {
        let mut text = command.display();
        let left_out = self.left_out();
        if !left_out.is_empty()
            && command
                .args
                .iter()
                .any(|arg| arg.starts_with("--use-list="))
        {
            let one = left_out.len() == 1;
            let names: Vec<String> = left_out.iter().map(|name| format!("“{name}”")).collect();
            text.push_str(&format!(
                "  # leaves out of the archive {} {} (the entries that create {it} and comment \
                 on {it})",
                if one {
                    "the extension"
                } else {
                    "the extensions"
                },
                simple::listed(&names),
                it = if one { "it" } else { "them" },
            ));
        }
        text
    }

    /// The failure view's "Restore without …": the same restore without
    /// what the server lacks, the backup's owners and privileges
    /// (pg_restore's `--no-owner --no-privileges`, the wizard's two
    /// checkboxes) and the extensions it does not offer (`--use-list`);
    /// asked first like any restore.
    pub fn restore_without_owners(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(failure) = &self.roles {
            if failure.retry_owners() {
                self.restore.no_owner = true;
                self.restore.no_privileges = true;
            }
            if failure.retry_extensions() {
                for name in &failure.extensions {
                    if !self.leave_out.contains(name) {
                        self.leave_out.push(name.clone());
                    }
                }
            }
        }
        self.start(window, cx);
    }

    /// Stops the tool.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Release);
        cx.notify();
    }

    /// Whether the dialog may close now; while a tool runs, asks whether to
    /// stop it, and closes once it has stopped.
    fn may_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !matches!(self.step, Step::Running) {
            return true;
        }
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            dialog
                .title("Stop the tool?")
                .w(px(420.))
                .on_ok(|_, _, _| false)
                .content(move |content, _, _| {
                    let view = view.clone();
                    content.child(
                        v_flex()
                            .gap_3()
                            .child(
                                div().text_sm().child(
                                    "The tool is still running. Stop it and close the wizard?",
                                ),
                            )
                            .child(
                                h_flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        Button::new("backup-keep-running")
                                            .outline()
                                            .label("Keep running")
                                            .on_click(|_, window, cx| window.close_dialog(cx)),
                                    )
                                    .child(
                                        Button::new("backup-stop-and-close")
                                            .danger()
                                            .label("Stop and close")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                view.update(cx, |this, cx| {
                                                    this.close_when_stopped = true;
                                                    this.cancel(cx);
                                                });
                                            }),
                                    ),
                            ),
                    )
                })
        });
        false
    }

    /// Where the dialog is, in words; for tests.
    #[cfg(test)]
    pub fn state(&self, cx: &App) -> String {
        match &self.step {
            Step::Preparing => "preparing".into(),
            Step::Blocked(why) => format!("blocked: {why}"),
            Step::Running => "running".into(),
            Step::Finished { success, text } => {
                format!("{}: {text}", if *success { "done" } else { "failed" })
            }
            Step::Options => match (&self.tool_state, &self.input) {
                (_, Some(Err(problem))) => format!("file: {problem}"),
                _ if self.mode.writes() && self.file.is_none() => "choose a file".into(),
                (ToolState::Probing, _) => "probing".into(),
                (ToolState::Missing(text), _) => format!("no tool: {text}"),
                (ToolState::Ready { compat, .. }, _) if compat.blocks() => format!(
                    "old client: {}",
                    compat.warning(self.engine).unwrap_or_default()
                ),
                (
                    ToolState::Ready { .. },
                    Some(Ok(Input {
                        refusal: Some(found),
                        ..
                    })),
                ) if !self.allow_commands => {
                    format!("refused: line {}: {}", found.line, found.command)
                }
                (ToolState::Ready { .. }, _) => match self.command(cx) {
                    Ok(command) => format!("ready: {}", self.shown(&command)),
                    Err(problem) => format!("not ready: {problem}"),
                },
            },
        }
    }

    /// The tool's output so far; for tests.
    #[cfg(test)]
    pub fn log(&self) -> Vec<String> {
        self.log.iter().map(|line| line.to_string()).collect()
    }

    /// The archive's list as read before the run; for tests.
    #[cfg(test)]
    pub fn archive_list(&self) -> Option<String> {
        match &self.input {
            Some(Ok(input)) => input.list.as_deref().map(str::to_string),
            _ => None,
        }
    }

    /// Leaves `names` out of the restore, as the preflight's checkbox does;
    /// for tests.
    #[cfg(test)]
    pub fn set_leave_out(&mut self, names: Vec<String>) {
        self.leave_out = names;
    }

    /// The command that would run now, scan included; for tests.
    #[cfg(test)]
    pub fn pending_command(&self, cx: &App) -> Option<ToolCommand> {
        self.command(cx).ok()
    }

    /// The backup settings as the pages stand; for tests.
    #[cfg(test)]
    pub fn settings(&self, cx: &App) -> Result<BackupOptions, String> {
        self.backup_options(cx)
    }

    /// The page shown, by its label; for tests.
    #[cfg(test)]
    pub fn page(&self) -> &'static str {
        if self.showing_clients {
            return "Local client";
        }
        self.current_page().label(self.mode)
    }

    /// Whether the objects are read; for tests.
    #[cfg(test)]
    pub fn objects_loaded(&self) -> bool {
        matches!(self.objects, Loaded::Ready(_)) || matches!(self.databases, Loaded::Ready(_))
    }

    /// The client's warning, and what the file is; for tests.
    #[cfg(test)]
    pub fn notes(&self) -> (Option<String>, Option<String>) {
        (self.client_warning(), self.input_text())
    }

    /// The file a backup would write now, or why none; for tests.
    #[cfg(test)]
    pub fn file_preview(&self, cx: &App) -> Result<PathBuf, String> {
        self.output_path(cx)
    }

    /// The file or folder the last run wrote; for tests.
    #[cfg(test)]
    pub fn written(&self) -> Option<PathBuf> {
        self.output.clone()
    }

    /// Runs the backup past the check Start makes for an output already
    /// there, as when one appears between that check and the run; for
    /// tests.
    #[cfg(test)]
    pub fn run_unchecked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let command = self.command(cx).expect("a command");
        let output = self.output_path(cx).expect("an output");
        self.run(command, Some((output, false)), window, cx);
    }

    /// The settings a saved task would keep; for tests.
    #[cfg(test)]
    pub fn task_backup(&self, cx: &App) -> Option<BackupOptions> {
        match self.task_settings(cx) {
            Ok(TaskSettings::Backup(options)) => Some(options),
            _ => None,
        }
    }

    /// The schemas listed on the objects page; for tests.
    #[cfg(test)]
    pub fn schema_names(&self) -> Vec<String> {
        match &self.objects {
            Loaded::Ready(entries) => entries.iter().map(|entry| entry.name.clone()).collect(),
            _ => Vec::new(),
        }
    }

    /// The tables (and views, when shown) the right list shows; for tests.
    #[cfg(test)]
    pub fn table_names(&self) -> Vec<String> {
        self.selection
            .current
            .as_deref()
            .and_then(|schema| self.entry(schema))
            .map(|entry| self.visible(entry).into_iter().map(|t| t.name).collect())
            .unwrap_or_default()
    }

    /// The folder of the client in use; for tests.
    #[cfg(test)]
    pub fn client_in_use(&self) -> Option<PathBuf> {
        match &self.tool_state {
            ToolState::Ready { install, .. } => Some(install.dir.clone()),
            _ => None,
        }
    }
}

impl Drop for BackupDialog {
    /// Closing the dialog stops a tool that still runs.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// How a run ended: the outcome, the size of a backup written, and
/// whether an incomplete backup was removed.
struct Ended {
    outcome: kerno_core::Result<ExitStatus>,
    size: Option<u64>,
    removed: bool,
}

/// The connection's route for the tools: its secrets (in memory) and, for
/// an SSH connection, a tunnel of the dialog's own.
async fn params_for(
    config: ConnectionConfig,
    secrets: Secrets,
    // Only a key trusted already: nothing is learned here.
    host_keys: HostKeyStore,
    database: String,
    password_env: Option<kerno_core::password::PasswordEnv>,
) -> kerno_core::Result<ConnectParams> {
    // The password as a connection gets it (stored or asked, a command,
    // RDS IAM, 1Password, ~/.pgpass, ~/.my.cnf); an imported command not
    // yet confirmed is refused here too. It reaches the tool through the
    // variable it already reads (PGPASSWORD, MYSQL_PWD), never argv.
    let password = kerno_core::password::resolve(
        &config,
        secrets.password.as_ref(),
        Some(&database),
        password_env.as_ref(),
    )
    .await?
    .password;
    let tunnel = kerno_core::connector::open_tunnel(&config, &secrets, &host_keys).await?;
    Ok(ConnectParams {
        engine: config.engine,
        host: config.host,
        port: config.port,
        tunnel,
        user: config.user,
        password,
        database: Some(database),
        file: config.file,
        tls: config.tls,
        read_only: config.read_only,
        options: config.options,
    })
}

/// The schemas and their tables and views (the database alone where it
/// has no schemas: MySQL, SQLite), partitions left out as in the tree.
pub(crate) async fn fetch_objects(
    session: &dyn Session,
    database: &str,
) -> kerno_core::Result<Vec<SchemaEntry>> {
    let schemas: Vec<String> = if session.dialect().has_schemas() {
        session
            .schemas()
            .await?
            .into_iter()
            .filter(|schema| !schema.is_system)
            .map(|schema| schema.name)
            .collect()
    } else {
        vec![database.to_string()]
    };
    let mut entries = Vec::new();
    // ponytail: every schema read up front; read a schema's tables when it
    // is first shown if databases with hundreds of schemas are slow here.
    for name in schemas {
        let objects = session.objects(&name).await?;
        let relations = objects
            .relations
            .into_iter()
            .filter(|relation| !relation.is_partition)
            .map(|relation| (relation.name, relation.kind))
            .collect();
        entries.push(SchemaEntry { name, relations });
    }
    Ok(entries)
}

/// What [`examine`] works on.
struct Job {
    mode: Mode,
    engine: Engine,
    server_version: (u32, u32, u32),
    /// The tool; `None` for a restore, whose file's format says.
    tool: Option<Tool>,
    added: Vec<PathBuf>,
    default_dir: Option<PathBuf>,
    connection_dir: Option<PathBuf>,
    /// The file to restore or run.
    file: Option<PathBuf>,
    params: ConnectParams,
    database: String,
}

/// What [`examine`] found.
struct Examined {
    tool: ToolState,
    detected: Vec<ClientInstall>,
    input: Option<Result<Input, String>>,
}

/// Blocking: finds the clients and picks the connection's, reads the start
/// of the file to restore (its format says which tool restores it), and
/// checks a SQL file for client commands the tool would run on this
/// computer.
fn examine(job: Job) -> Examined {
    let detected = tools::detect_clients(&ClientRoots::system(job.added));
    // A path typed in the field may name nothing yet (K56).
    if let Some(file) = job.file.as_ref().filter(|file| !file.exists()) {
        return Examined {
            tool: ToolState::Probing,
            detected,
            input: Some(Err(format!(
                "No such file: {}. Check the path, or choose the file.",
                file.display()
            ))),
        };
    }
    let format = match &job.file {
        Some(file) if job.mode == Mode::Restore => match tools::guess_format(file, job.engine) {
            Ok(format) => Some(format),
            Err(problem) => {
                return Examined {
                    tool: ToolState::Probing,
                    detected,
                    input: Some(Err(problem)),
                };
            }
        },
        Some(_) => Some(BackupFormat::Sql),
        None => None,
    };
    let Some(tool) = job
        .tool
        .or_else(|| tools::restore_tool(job.engine, format.unwrap_or_default()))
    else {
        return Examined {
            tool: ToolState::Missing(format!("{}.", tools::NO_CLIENT_TOOLS)),
            detected,
            input: None,
        };
    };
    let picked = tools::client_for(
        &detected,
        &ClientWanted {
            engine: job.engine,
            connection_dir: job.connection_dir.as_deref(),
            default_dir: job.default_dir.as_deref(),
        },
    );
    let state = match picked {
        Err(missing) => ToolState::Missing(missing.text()),
        Ok(pick) => match pick.install.path(tool) {
            None => ToolState::Missing(format!(
                "The client in {} has no {}: choose another with “Local client…”.",
                pick.install.dir.display(),
                tool.name()
            )),
            Some(path) => ToolState::Ready {
                path: path.to_path_buf(),
                install: pick.install.clone(),
                compat: tools::compatibility(
                    pick.install.version.as_ref(),
                    job.engine,
                    job.server_version,
                ),
                ignored: pick.ignored,
            },
        },
    };
    let (Some(file), Some(format)) = (job.file, format) else {
        return Examined {
            tool: state,
            detected,
            input: None,
        };
    };
    let mut input = Input {
        path: file.clone(),
        format,
        refusal: None,
        archive_database: None,
        list: None,
        extensions: Vec::new(),
    };
    if let ToolState::Ready { path, install, .. } = &state {
        if format == BackupFormat::Sql {
            let mut options =
                RestoreOptions::new(job.engine, job.database, file.clone(), BackupFormat::Sql);
            options.tool_version = install.version.clone();
            let found = tools::restore_command(path, &job.params, &options).and_then(|command| {
                match &command.script_check {
                    Some(check) => tools::find_client_command(check, &AtomicBool::new(false)),
                    // The tool's own sandbox keeps the commands off.
                    None => Ok(None),
                }
            });
            match found {
                Ok(found) => input.refusal = found,
                Err(error) => {
                    return Examined {
                        tool: state,
                        detected,
                        input: Some(Err(error_text(&error))),
                    };
                }
            }
        } else {
            if let Some(list) = archive_list(path, &file) {
                input.archive_database = tools::list_database(&list);
                input.extensions = tools::list_extensions(&list);
                input.list = Some(list.into());
            }
        }
    }
    Examined {
        tool: state,
        detected,
        input: Some(Ok(input)),
    }
}

/// A PostgreSQL archive's list, as `pg_restore --list` prints it with the
/// archive's own pg_restore; `None` when it could not be read. Blocking.
fn archive_list(pg_restore: &Path, archive: &Path) -> Option<String> {
    let output = std::process::Command::new(pg_restore)
        .arg("--list")
        .arg(archive)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// How many errors pg_restore went past: its last line when it reached the
/// end outside one transaction ends with their number ("warning: errors
/// ignored on restore: 3"), in whatever language it speaks. `None` when it
/// stopped instead.
fn errors_ignored<S: AsRef<str>>(log: &[S]) -> Option<u64> {
    let last = log
        .iter()
        .rev()
        .map(AsRef::as_ref)
        .find(|line| !line.trim().is_empty())?;
    let (_, number) = last
        .strip_prefix("pg_restore:")?
        .trim_end()
        .rsplit_once(':')?;
    number.trim().parse().ok().filter(|errors| *errors > 0)
}

/// The statements pg_restore reports as failed: the lines it prints after
/// a translated label ("Command was: CREATE SCHEMA a;"), which are the only
/// ones that do not start with its name and hold SQL after a colon.
fn ignored_commands<S: AsRef<str>>(log: &[S]) -> Vec<String> {
    const STARTS: [&str; 12] = [
        "CREATE ",
        "ALTER ",
        "COPY ",
        "GRANT ",
        "REVOKE ",
        "COMMENT ",
        "SET ",
        "SELECT ",
        "INSERT ",
        "DROP ",
        "REFRESH ",
        "SECURITY ",
    ];
    log.iter()
        .map(AsRef::as_ref)
        .filter(|line| !line.starts_with("pg_restore:"))
        .filter_map(|line| {
            let (_, sql) = line.split_once(':')?;
            let sql = sql.trim();
            STARTS
                .iter()
                .any(|start| sql.starts_with(start))
                .then(|| sql.to_string())
        })
        .collect()
}

/// The size of a backup written: a file's, or the sum of a directory
/// archive's files (pg_dump writes them side by side, no subfolder).
/// Blocking.
fn backup_size(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_dir() {
        return meta.is_file().then_some(meta.len());
    }
    Some(
        std::fs::read_dir(path)
            .ok()?
            .filter_map(|entry| entry.ok()?.metadata().ok())
            .filter(|meta| meta.is_file())
            .map(|meta| meta.len())
            .sum(),
    )
}

/// Writes `text` to `path` as a new file of this user's only: a file left
/// there by an earlier run of Kerno is removed first, and nothing is
/// written through a link another user put there.
fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let _ = std::fs::remove_file(path);
    // It names the backup's objects: this user's to read, nobody else's.
    kerno_core::private::file_options()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(text.as_bytes())
}

/// The server's roles, by name.
const ROLES: &str = "SELECT rolname FROM pg_catalog.pg_roles WHERE rolname IN";
/// The extensions the server offers (their files are installed), by name.
const EXTENSIONS: &str = "SELECT name FROM pg_catalog.pg_available_extensions WHERE name IN";

/// The first column of what `sql` returns on `meta`; `None` when it could
/// not be asked. Runs on the Tokio side.
async fn names_of(meta: &Arc<dyn Session>, sql: String) -> Option<Vec<String>> {
    let collected = exec::collect(meta.clone(), sql).await.ok()?;
    Some(
        collected
            .results
            .first()
            .map(|result| result.rows())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| row.into_iter().next().flatten())
            .collect(),
    )
}

/// Which of `names` the catalog `select` (ending in `… IN`) does not
/// return: the roles or extensions the server lacks.
async fn absent(meta: &Arc<dyn Session>, select: &str, names: &[String]) -> Option<Vec<String>> {
    if names.is_empty() {
        return Some(Vec::new());
    }
    let quoted: Vec<String> = names
        .iter()
        .map(|name| meta.dialect().quote_string(name))
        .collect();
    let present = names_of(meta, format!("{select} ({})", quoted.join(", "))).await?;
    Some(
        names
            .iter()
            .filter(|name| !present.contains(name))
            .cloned()
            .collect(),
    )
}

/// Line `number` (from 1) of the script at `path`, as psql counted it.
/// Blocking.
fn script_line(path: &Path, number: u64) -> Option<String> {
    use std::io::BufRead as _;
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let line = file
        .split(b'\n')
        .nth(usize::try_from(number).ok()?.checked_sub(1)?)?
        .ok()?;
    Some(String::from_utf8_lossy(&line).into_owned())
}

/// What the failure view says first about refused roles (K35) and
/// extensions the server lacks (K39): what happened, in plain words, then
/// what to do; the tool's output below.
fn roles_text(failure: &RoleFailure) -> String {
    let mut parts = Vec::new();
    if !failure.refused.is_empty() {
        parts.push(owners_text(failure));
    }
    if !failure.extensions.is_empty() {
        parts.push(extensions_text(failure));
    }
    parts.join(" ")
}

/// What the failure view says about extensions the server does not offer.
fn extensions_text(failure: &RoleFailure) -> String {
    let one = failure.extensions.len() == 1;
    let names: Vec<String> = failure
        .extensions
        .iter()
        .map(|name| format!("“{name}”"))
        .collect();
    let (it, extension) = if one {
        ("it", "extension")
    } else {
        ("them", "extensions")
    };
    let mut text = format!(
        "The backup needs the {extension} {}, which this server does not have. A restore cannot \
         install an extension: its files are installed on the machine PostgreSQL runs on.",
        simple::listed(&names)
    );
    text.push_str(&if failure.script {
        format!(
            " A plain SQL script creates {it} in its own text, so psql cannot leave {it} out. \
             Install {it} on that machine, then restore again; or back up again as a compressed \
             archive, which a restore can run without {it}."
        )
    } else if failure.retry_extensions() {
        format!(
            " Restore again without {it}: what in the backup uses {it} may then fail, and is \
             said here. Or install {it} on that machine, then restore again."
        )
    } else if !failure.single_transaction {
        format!(" pg_restore went on past {it}: the rest is restored, except what uses {it}.")
    } else {
        format!(" Install {it} on that machine, then restore again.")
    });
    text
}

/// What the failure view says about refused roles (K35).
fn owners_text(failure: &RoleFailure) -> String {
    let missing: Vec<&RefusedRole> = failure
        .refused
        .iter()
        .filter(|role| {
            failure
                .missing
                .as_ref()
                .is_some_and(|missing| missing.contains(&role.name))
        })
        .collect();
    let confirmed = !missing.is_empty();
    let shown = if confirmed {
        missing
    } else {
        failure.refused.iter().collect()
    };
    let one = shown.len() == 1;
    let names: Vec<String> = shown
        .iter()
        .map(|role| format!("“{}”", role.name))
        .collect();
    let names = format!(
        "{} {}",
        if one { "the role" } else { "the roles" },
        simple::listed(&names)
    );
    let owner = shown.iter().any(|role| role.owner);
    let how = match (owner, shown.iter().any(|role| !role.owner)) {
        (true, false) => "as the owner of its objects",
        (false, _) => "in the rights it grants",
        (true, true) => "as an owner and in the rights it grants",
    };
    let (it, role) = if one {
        ("it", "role")
    } else {
        ("them", "roles")
    };
    let user = &failure.user;
    let mut text = if confirmed {
        format!("The backup names {names} {how}, and this server has no such {role}.")
    } else {
        format!(
            "The backup names {names} {how}, and the server would not let “{user}”, the user the \
             restore connects as, use {it}."
        )
    };
    let other_way = if confirmed {
        format!("create the {role} on this server")
    } else {
        format!("restore as a user allowed to use {it}")
    };
    if failure.script {
        text.push_str(&format!(
            " A plain SQL script sets its owners and rights in its own text, so psql cannot \
             leave them out. {}, then restore again; or back up again as a compressed archive, \
             which a restore can run without them.",
            capitalized(&other_way)
        ));
    } else if failure.retry {
        text.push_str(&format!(
            " Restore again without the backup's owners and privileges: what it restores then \
             belongs to “{user}”, and the backup's grants are left out. Or {other_way}, then \
             restore again."
        ));
    } else if !failure.single_transaction {
        // pg_restore went on: the objects are there, a second run would
        // find them.
        text.push_str(&if owner {
            format!(
                " pg_restore went on past {it}: the rest is restored, and what the backup gives \
                 to {it} belongs to “{user}”."
            )
        } else {
            format!(
                " pg_restore went on past {it}: the rest is restored, without the rights the \
                 backup grants {it}."
            )
        });
    } else {
        text.push_str(&format!(
            " {}, then restore again.",
            capitalized(&other_way)
        ));
    }
    text
}

/// `text` with its first letter in capitals.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// What the confirmation of a restore or a script is about.
struct Explained<'a> {
    family: ClientFamily,
    database: &'a str,
    connection: &'a str,
    input: &'a Input,
    program: &'a str,
    allow_commands: bool,
    script: bool,
    restore: &'a RestoreOptions,
}

/// What the confirmation says: the file, the database, the connection;
/// that a SQL file is a script; the database `--create` creates (and
/// `--clean` drops first); what a failure keeps.
fn restore_explanation(about: &Explained) -> String {
    let program = about.program;
    let mut text = if about.script {
        format!(
            "This runs {program} on this computer: it runs the script {} in the database “{}” \
             of “{}”.",
            about.input.path.display(),
            about.database,
            about.connection
        )
    } else {
        format!(
            "This runs {program} on this computer: it restores {} into the database “{}” of \
             “{}”, on top of what the database holds.",
            about.input.path.display(),
            about.database,
            about.connection
        )
    };
    let archive = about.input.format != BackupFormat::Sql;
    if archive && about.restore.create {
        let named = about.input.archive_database.as_deref().map_or_else(
            || "the database named in the archive".to_string(),
            |name| format!("the database “{name}” named in the archive"),
        );
        text.push_str(&format!(
            " With “create database”, pg_restore connects to “{}” only to create {named}{}, and \
             restores into that one.",
            about.database,
            if about.restore.clean {
                ", which it drops first if it exists"
            } else {
                ""
            }
        ));
    } else if archive && about.restore.clean {
        text.push_str(" With “clean”, every object of the archive is dropped first.");
    }
    if about.input.format == BackupFormat::Sql {
        text.push_str(&format!(
            " A SQL file is a script run by the client tool: {program} runs every statement it \
             holds{}.",
            if about.allow_commands {
                ", and the client commands it holds, on this computer"
            } else {
                ""
            }
        ));
    }
    text.push_str(match about.family {
        ClientFamily::Postgres if about.restore.single_transaction && !about.script => {
            " It runs in one transaction: if a statement fails, nothing is kept."
        }
        ClientFamily::Postgres => " A failure keeps what ran before it.",
        ClientFamily::MySql => {
            " MySQL and MariaDB apply it statement by statement: a failed or cancelled run \
             keeps what was already applied."
        }
    });
    text
}

/// Why a file with a client command is not run as it is.
fn refusal_text(found: &Finding, family: ClientFamily, database: &str) -> String {
    let client = match family {
        ClientFamily::Postgres => "psql",
        ClientFamily::MySql => "mysql",
    };
    let mut text = format!(
        "Line {} of this file holds {}, a {client} client command: the client tool would run \
         it on this computer, not on the server, and it can start programs, read or write \
         files, or connect elsewhere. Nothing was run.",
        found.line, found.command
    );
    if family == ClientFamily::Postgres && matches!(found.command.as_str(), "\\c" | "\\connect") {
        text.push_str(&format!(
            " pg_dump writes \\connect in a backup made with its “create database” option, to \
             switch to the database it creates. The restore goes into “{database}”, with the \
             password in the tool's environment, so a file that connects elsewhere is refused: \
             back up without that option, or allow the commands of a file you trust."
        ));
    }
    text
}

/// What a global backup says of the databases this account may not open
/// (K62): the first `max` named, then how many more; `None` for none.
fn left_out_closed(names: &[String], max: usize) -> Option<String> {
    let (first, rest) = names.split_at(names.len().min(max));
    let mut listed = first.join(", ");
    if !rest.is_empty() {
        listed.push_str(&format!(" and {} more", rest.len()));
    }
    Some(match names.len() {
        0 => return None,
        1 => format!("Left out, this account may not open it: {listed}"),
        _ => format!("Left out, this account may not open them: {listed}"),
    })
}

fn program_name(command: &ToolCommand) -> String {
    command
        .program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// An error as the dialog says it. A refused script and other settings
/// problems arrive as `Error::Config`, whose own wording starts with
/// "invalid connection settings": shown without it.
fn error_text(error: &Error) -> String {
    match error {
        Error::Config(message) => message.clone(),
        Error::Unsupported(message) => format!("Not possible: {message}."),
        // A server error with its detail and hint (C14).
        other => kerno_core::describe(other).to_string(),
    }
}

fn status_text(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit status {code}"),
        None => "stopped by a signal".into(),
    }
}

/// Seconds, as the progress page counts them.
fn seconds(elapsed: Duration) -> String {
    if elapsed < Duration::from_secs(1) {
        duration_text(elapsed)
    } else {
        format!("{} s", elapsed.as_secs())
    }
}

/// Changes the PostgreSQL options of the backup.
fn pg_set(this: &mut BackupDialog, change: impl FnOnce(&mut tools::PgBackup)) {
    if let EngineBackup::Postgres(pg) = &mut this.backup.engine {
        change(pg);
    }
}

/// Changes the MySQL options of the backup.
fn my_set(this: &mut BackupDialog, change: impl FnOnce(&mut tools::MySqlBackup)) {
    if let EngineBackup::MySql(my) = &mut this.backup.engine {
        change(my);
    }
}

// Rendering -------------------------------------------------------------------

/// The steps of a wizard, listed on the left: `current` is highlighted
/// (none while a side panel shows). Shared by the tool wizards and the copy
/// of tables.
pub(crate) fn wizard_steps(labels: &[&str], current: Option<usize>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .flex_none()
        .w(px(150.))
        .gap_1()
        .pr_3()
        .border_r_1()
        .border_color(theme.border)
        .children(labels.iter().enumerate().map(|(index, label)| {
            let here = current == Some(index);
            div()
                .id(("wizard-step", index))
                .test_support()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .text_sm()
                .when(here, |this| {
                    this.bg(theme.accent)
                        .text_color(theme.accent_foreground)
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!here, |this| this.text_color(theme.muted_foreground))
                .child(SharedString::from(format!("{}. {label}", index + 1)))
        }))
        .into_any_element()
}

/// One chip per choice, `current` selected; a click calls `set` on the view.
pub(crate) fn chips<V: 'static, T: Copy + PartialEq + 'static>(
    id: &'static str,
    choices: &[(T, &'static str)],
    current: T,
    set: fn(&mut V, T),
    cx: &Context<V>,
) -> AnyElement {
    h_flex()
        .gap_1()
        .flex_wrap()
        .children(choices.iter().enumerate().map(|(index, (choice, label))| {
            let choice = *choice;
            theme::chip((id, index), current == choice)
                .small()
                .label(*label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    set(this, choice);
                    cx.notify();
                }))
        }))
        .into_any_element()
}

impl BackupDialog {
    /// A labelled row.
    fn field(label: &'static str, field: AnyElement, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .gap_3()
            .items_start()
            .child(
                div()
                    .flex_none()
                    .w(px(110.))
                    .pt_0p5()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(field))
            .into_any_element()
    }

    fn group(title: &'static str, rows: Vec<AnyElement>) -> AnyElement {
        v_flex()
            .gap_2()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
            .children(rows)
            .into_any_element()
    }

    /// A checkbox bound to one setting of the wizard.
    fn check(
        id: &'static str,
        label: &'static str,
        checked: bool,
        set: fn(&mut Self, bool),
        cx: &Context<Self>,
    ) -> AnyElement {
        Checkbox::new(id)
            .label(label)
            .checked(checked)
            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                set(this, *checked);
                cx.notify();
            }))
            .into_any_element()
    }

    /// A checkbox that refines the one above it: indented under it, and
    /// off until that one is ticked.
    fn sub_check(
        id: &'static str,
        label: &'static str,
        checked: bool,
        enabled: bool,
        set: fn(&mut Self, bool),
        cx: &Context<Self>,
    ) -> AnyElement {
        div()
            .pl(px(24.))
            .child(
                Checkbox::new(id)
                    .label(label)
                    .checked(checked && enabled)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        set(this, *checked);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn small_input(id: &'static str, input: &Entity<InputState>, width: f32) -> AnyElement {
        div()
            .w(px(width))
            .child(TextInput::new(input).small().id(id))
            .into_any_element()
    }

    fn muted(text: impl Into<SharedString>, cx: &Context<Self>) -> AnyElement {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
            .into_any_element()
    }

    fn render_steps(&self, cx: &Context<Self>) -> AnyElement {
        let pages = self.pages();
        let labels: Vec<&str> = pages.iter().map(|page| page.label(self.mode)).collect();
        let current = pages.iter().position(|page| *page == self.current_page());
        wizard_steps(&labels, current.filter(|_| !self.showing_clients), cx)
    }

    /// Why the client in use may not suit: a folder asked for that is not
    /// used, another version than the server's.
    fn client_warning(&self) -> Option<String> {
        let ToolState::Ready {
            compat, ignored, ..
        } = &self.tool_state
        else {
            return None;
        };
        let note = ignored.as_ref().map(|dir| {
            format!(
                "{} is not a client detected or added in Settings ▸ Local clients: it is not \
                 used.",
                dir.display()
            )
        });
        match (note, compat.warning(self.engine)) {
            (Some(note), Some(more)) => Some(format!("{note} {more}")),
            (note, more) => note.or(more),
        }
    }

    /// What the file to restore or run is, once read.
    fn input_text(&self) -> Option<String> {
        let Some(Ok(input)) = &self.input else {
            return None;
        };
        let format = match (input.format, self.engine) {
            (BackupFormat::Custom, _) => "Custom archive, restored by pg_restore",
            (BackupFormat::Directory, _) => "Directory archive, restored by pg_restore",
            (BackupFormat::Tar, _) => "Tar archive, restored by pg_restore",
            (BackupFormat::Sql, Engine::Postgres) => "SQL script, run by psql",
            (BackupFormat::Sql, _) => "SQL script, run by the mysql client",
        };
        let from = input
            .archive_database
            .as_ref()
            .map(|name| format!(" (made from the database “{name}”)"))
            .unwrap_or_default();
        Some(format!("{format}{from}"))
    }

    /// The client in use, its version, and why it may not suit; `compact`
    /// (the simple view): its folder on a small line of its own.
    fn render_tool(&self, compact: bool, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mut folder = None;
        let text = match &self.tool_state {
            ToolState::Probing => "Looking for the local clients…".to_string(),
            ToolState::Missing(text) => text.clone(),
            // pg_restore or psql, mysql: the file's format says which.
            ToolState::Ready { install, .. }
                if self.mode == Mode::Restore && self.file.is_none() =>
            {
                format!(
                    "{}. Choose the file first: its format says which tool restores it.",
                    install.label()
                )
            }
            ToolState::Ready { path, install, .. } if compact => {
                folder = Some(path.display().to_string());
                install.label()
            }
            ToolState::Ready { path, install, .. } => {
                format!("{} · {}", install.label(), path.display())
            }
        };
        let warning = self.client_warning();
        let missing = matches!(self.tool_state, ToolState::Missing(_));
        let blocks = matches!(&self.tool_state, ToolState::Ready { compat, .. } if compat.blocks());
        v_flex()
            .gap_1()
            .child(
                div()
                    .id("backup-tool")
                    .test_support()
                    .text_sm()
                    .when(missing, |this| this.text_color(theme.danger))
                    .aria_label(SharedString::from(text.clone()))
                    .child(SharedString::from(text)),
            )
            .when_some(folder, |this, folder| this.child(Self::muted(folder, cx)))
            .when_some(warning, |this, warning| {
                this.child(
                    div()
                        .id("backup-client-warning")
                        .test_support()
                        .text_xs()
                        .text_color(if blocks { theme.danger } else { theme.warning })
                        .child(SharedString::from(warning.clone()))
                        .test_text(warning),
                )
            })
            .into_any_element()
    }

    /// The header of a list: its title, All, None.
    fn list_header(
        title: &'static str,
        ids: (&'static str, &'static str),
        schema: Option<String>,
        off: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let other = schema.clone();
        h_flex()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                Button::new(ids.0)
                    .ghost()
                    .small()
                    .label("All")
                    .disabled(off)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.tick_all(schema.clone(), true, cx)),
                    ),
            )
            .child(
                Button::new(ids.1)
                    .ghost()
                    .small()
                    .label("None")
                    .disabled(off)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.tick_all(other.clone(), false, cx)),
                    ),
            )
            .into_any_element()
    }

    fn render_objects(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let selection = &self.selection;
        let off = selection.complete;
        let list = |id: &'static str| {
            div()
                .id(id)
                .h(px(260.))
                .overflow_y_scroll()
                .p_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
        };
        let body = match &self.objects {
            Loaded::Loading => div()
                .id("backup-objects-loading")
                .test_support()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Reading the schemas and tables…")
                .into_any_element(),
            Loaded::Failed(why) => div()
                .id("backup-objects-error")
                .test_support()
                .text_sm()
                .text_color(theme.danger)
                .child(SharedString::from(crate::advice_after(
                    &format!("The objects could not be read: {why}"),
                    "A complete backup still works.",
                )))
                .into_any_element(),
            Loaded::Ready(entries) => {
                let schemas = list("backup-schemas").children(entries.iter().enumerate().map(
                    |(index, entry)| {
                        let (name, pick) = (entry.name.clone(), entry.name.clone());
                        let current = selection.current.as_deref() == Some(entry.name.as_str());
                        h_flex()
                            .gap_1()
                            .px_1()
                            .rounded(theme.radius)
                            .when(current, |this| this.bg(theme.accent))
                            .child(
                                Checkbox::new(("backup-schema", index))
                                    .checked(off || selection.schemas.contains(&entry.name))
                                    .disabled(off)
                                    .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                        this.toggle_schema(name.clone(), cx)
                                    })),
                            )
                            .child(
                                div()
                                    .id(("backup-schema-name", index))
                                    .test_support()
                                    .flex_1()
                                    .text_sm()
                                    .cursor_pointer()
                                    .child(SharedString::from(entry.name.clone()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.selection.current = Some(pick.clone());
                                        cx.notify();
                                    })),
                            )
                    },
                ));
                let tables: Vec<(TableRef, RelationKind)> = selection
                    .current
                    .as_deref()
                    .and_then(|schema| self.entry(schema))
                    .map(|entry| {
                        entry
                            .relations
                            .iter()
                            .filter(|(_, kind)| selection.show_views || !is_view(kind))
                            .map(|(name, kind)| {
                                (TableRef::new(entry.name.clone(), name.clone()), *kind)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let empty = tables.is_empty();
                let tables_list =
                    list("backup-tables").children(tables.into_iter().enumerate().map(
                        |(index, (table, kind))| {
                            let label = match kind {
                                RelationKind::View => format!("{} (view)", table.name),
                                RelationKind::MaterializedView => {
                                    format!("{} (materialized view)", table.name)
                                }
                                _ => table.name.clone(),
                            };
                            Checkbox::new(("backup-table", index))
                                .label(SharedString::from(label))
                                .checked(off || selection.tables.contains(&table))
                                .disabled(off)
                                .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                    this.toggle_table(table.clone(), cx)
                                }))
                        },
                    ));
                let tables_list = if empty {
                    tables_list.child(Self::muted("No table here.", cx))
                } else {
                    tables_list
                };
                let schema = selection.current.clone();
                let tables_column = v_flex()
                    .flex_1()
                    .gap_1()
                    .child(Self::list_header(
                        "Tables",
                        ("backup-tables-all", "backup-tables-none"),
                        schema,
                        off,
                        cx,
                    ))
                    .child(tables_list);
                if self.engine == Engine::Postgres {
                    h_flex()
                        .gap_3()
                        .items_start()
                        .child(
                            v_flex()
                                .flex_1()
                                .gap_1()
                                .child(Self::list_header(
                                    "Schemas",
                                    ("backup-schemas-all", "backup-schemas-none"),
                                    None,
                                    off,
                                    cx,
                                ))
                                .child(schemas),
                        )
                        .child(tables_column)
                        .into_any_element()
                } else {
                    tables_column.into_any_element()
                }
            }
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_4()
                    .child(
                        Checkbox::new("backup-complete")
                            .label("Complete backup (the whole database)")
                            .checked(selection.complete)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_complete(*checked, cx)
                            })),
                    )
                    .child(
                        Checkbox::new("backup-show-views")
                            .label("Show views")
                            .checked(selection.show_views)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_show_views(*checked, cx)
                            })),
                    ),
            )
            .child(body)
            .into_any_element()
    }

    fn render_databases(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let g = &self.global;
        let only = g.globals_only || g.roles_only || g.tablespaces_only;
        let body = match &self.databases {
            Loaded::Loading => Self::muted("Reading the databases…", cx),
            Loaded::Failed(why) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(SharedString::from(format!(
                    "The databases could not be read: {why}"
                )))
                .into_any_element(),
            Loaded::Ready(databases) => div()
                .id("global-databases")
                .h(px(260.))
                .overflow_y_scroll()
                .p_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .children(databases.iter().enumerate().map(|(index, name)| {
                    let toggled = name.clone();
                    Checkbox::new(("global-database", index))
                        .label(SharedString::from(name.clone()))
                        .checked(!only && !self.excluded.contains(name))
                        .disabled(only)
                        .on_click(cx.listener(move |this, _: &bool, _, cx| {
                            if !this.excluded.remove(&toggled) {
                                this.excluded.insert(toggled.clone());
                            }
                            cx.notify();
                        }))
                }))
                .into_any_element(),
        };
        let all: Vec<String> = match &self.databases {
            Loaded::Ready(databases) => databases.clone(),
            _ => Vec::new(),
        };
        v_flex()
            .gap_2()
            .child(Self::muted(
                "pg_dumpall writes the roles, the tablespaces and the ticked databases in one SQL \
                 script. It only reads.",
                cx,
            ))
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Databases"),
                    )
                    .child(
                        Button::new("global-databases-all")
                            .ghost()
                            .small()
                            .label("All")
                            .disabled(only)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.excluded.clear();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("global-databases-none")
                            .ghost()
                            .small()
                            .label("None")
                            .disabled(only)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.excluded = all.iter().cloned().collect();
                                cx.notify();
                            })),
                    ),
            )
            .child(body)
            .children(left_out_closed(&self.closed, 3).map(|text| {
                let all = left_out_closed(&self.closed, usize::MAX).unwrap_or_default();
                div()
                    .id("global-databases-closed")
                    .test_support()
                    .text_sm()
                    .text_color(theme.warning)
                    .child(SharedString::from(text.clone()))
                    .test_text(text)
                    .tooltip(move |window, cx| Tooltip::new(all.clone()).build(window, cx))
            }))
            .into_any_element()
    }

    /// The output folder, the file name pattern and its result.
    fn render_output(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let preview = self.output_path(cx);
        Self::group(
            "Output",
            vec![
                Self::field(
                    "Folder",
                    h_flex()
                        .gap_2()
                        .child({
                            // The field shows the end of a long path; the
                            // tooltip, all of it.
                            let folder =
                                SharedString::from(self.folder.read(cx).value().to_string());
                            div()
                                .id("backup-folder-field")
                                .flex_1()
                                .child(TextInput::new(&self.folder).small().id("backup-folder"))
                                .when(!folder.is_empty(), |this| {
                                    this.tooltip(move |window, cx| {
                                        Tooltip::new(folder.clone()).build(window, cx)
                                    })
                                })
                        })
                        .child(
                            Button::new("backup-choose-folder")
                                .small()
                                .outline()
                                .label("Choose…")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_folder(window, cx)
                                })),
                        )
                        .into_any_element(),
                    cx,
                ),
                Self::field(
                    "File name",
                    v_flex()
                        .gap_1()
                        .child(TextInput::new(&self.pattern).small().id("backup-pattern"))
                        .child(Self::muted(
                            "Variables: ${database} ${schema} ${table} ${host} ${timestamp} \
                             ${date} ${year} ${month} ${day} ${hour} ${minute} \
                             ${connection.type}. The format's extension is added.",
                            cx,
                        ))
                        .into_any_element(),
                    cx,
                ),
                Self::field(
                    "Writes",
                    div()
                        .id("backup-file-preview")
                        .test_support()
                        .text_sm()
                        .font_family(theme.mono_font_family.clone())
                        .map(|this| match preview {
                            Ok(path) => this.child(SharedString::from(path.display().to_string())),
                            Err(why) => {
                                this.text_color(theme.danger).child(SharedString::from(why))
                            }
                        })
                        .into_any_element(),
                    cx,
                ),
            ],
        )
    }

    /// Extra arguments and another user, shared by every wizard.
    fn render_advanced(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let extra_error = tools::extra_args(&self.extra_value(cx), self.family).err();
        let mut rows = vec![
            Self::field(
                "Extra arguments",
                v_flex()
                    .gap_1()
                    .child(TextInput::new(&self.extra).small().id("backup-extra-args"))
                    .map(|this| match extra_error {
                        Some(error) => this.child(
                            div()
                                .id("backup-extra-error")
                                .test_support()
                                .text_xs()
                                .text_color(theme.danger)
                                .child(SharedString::from(error_text(&error))),
                        ),
                        None => this.child(Self::muted(
                            "Passed to the tool after Kerno's own, split as a shell would. The \
                             server, the user and the password stay the connection's.",
                            cx,
                        )),
                    })
                    .into_any_element(),
                cx,
            ),
            Self::check(
                "backup-auth",
                "Run the tool as another user (the password is asked for this run, never saved)",
                self.auth,
                |this, on| this.auth = on,
                cx,
            ),
        ];
        if self.auth {
            rows.push(Self::field(
                "User",
                h_flex()
                    .gap_2()
                    .child(Self::small_input("backup-auth-user", &self.auth_user, 180.))
                    .child(Self::small_input(
                        "backup-auth-password",
                        &self.auth_password,
                        220.,
                    ))
                    .into_any_element(),
                cx,
            ));
        }
        Self::group("Advanced", rows)
    }

    fn chips<T: Copy + PartialEq + 'static>(
        id: &'static str,
        choices: &[(T, &'static str)],
        current: T,
        set: fn(&mut Self, T),
        cx: &Context<Self>,
    ) -> AnyElement {
        chips(id, choices, current, set, cx)
    }

    fn render_backup_settings(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let content = Self::field(
            "Content",
            Self::chips(
                "backup-content",
                &[
                    (BackupContent::All, "Definitions and data"),
                    (BackupContent::SchemaOnly, "Definitions only"),
                    (BackupContent::DataOnly, "Data only"),
                ],
                self.backup.content,
                |this, content| this.backup.content = content,
                cx,
            ),
            cx,
        );
        let settings = match &self.backup.engine {
            EngineBackup::Postgres(pg) => {
                let format = self.backup.format;
                let formats: Vec<(BackupFormat, &'static str)> = BackupFormat::POSTGRES
                    .iter()
                    .map(|format| (*format, format.label()))
                    .collect();
                vec![
                    Self::field(
                        "Format",
                        Self::chips(
                            "backup-format",
                            &formats,
                            format,
                            |this, format| this.backup.format = format,
                            cx,
                        ),
                        cx,
                    ),
                    Self::field(
                        "Compression",
                        h_flex()
                            .gap_3()
                            .child(Self::small_input(
                                "backup-compression",
                                &self.compression,
                                80.,
                            ))
                            .child(div().text_sm().child("Encoding"))
                            .child(Self::small_input("backup-encoding", &self.encoding, 120.))
                            .when(format == BackupFormat::Directory, |this| {
                                this.child(div().text_sm().child("Jobs"))
                                    .child(Self::small_input("backup-jobs", &self.jobs, 60.))
                            })
                            .into_any_element(),
                        cx,
                    ),
                    content,
                    Self::check(
                        "backup-inserts",
                        "Use SQL INSERT instead of COPY",
                        pg.inserts,
                        |t, on| pg_set(t, |p| p.inserts = on),
                        cx,
                    ),
                    Self::check(
                        "backup-no-privileges",
                        "Do not back up privileges (GRANT / REVOKE)",
                        pg.no_privileges,
                        |t, on| pg_set(t, |p| p.no_privileges = on),
                        cx,
                    ),
                    Self::check(
                        "backup-no-owner",
                        "Discard objects owner",
                        pg.no_owner,
                        |t, on| pg_set(t, |p| p.no_owner = on),
                        cx,
                    ),
                    Self::check(
                        "backup-clean",
                        "Add DROP statements before each CREATE (clean)",
                        pg.clean,
                        |t, on| {
                            pg_set(t, |p| {
                                p.clean = on;
                                p.if_exists &= on;
                            })
                        },
                        cx,
                    ),
                    Self::sub_check(
                        "backup-if-exists",
                        "… with IF EXISTS",
                        pg.if_exists,
                        pg.clean,
                        |t, on| pg_set(t, |p| p.if_exists = on && p.clean),
                        cx,
                    ),
                    Self::check(
                        "backup-create",
                        "Add the CREATE DATABASE statement",
                        pg.create,
                        |t, on| pg_set(t, |p| p.create = on),
                        cx,
                    ),
                ]
            }
            EngineBackup::MySql(my) => {
                let methods: Vec<(DumpMethod, &'static str)> = DumpMethod::ALL
                    .iter()
                    .map(|method| (*method, method.label()))
                    .collect();
                vec![
                    Self::field(
                        "Consistency",
                        Self::chips(
                            "backup-method",
                            &methods,
                            my.method,
                            |this, method| my_set(this, |m| m.method = method),
                            cx,
                        ),
                        cx,
                    ),
                    content,
                    Self::check(
                        "backup-routines",
                        "Procedures and functions",
                        my.routines,
                        |t, on| my_set(t, |m| m.routines = on),
                        cx,
                    ),
                    Self::check(
                        "backup-events",
                        "Events",
                        my.events,
                        |t, on| my_set(t, |m| m.events = on),
                        cx,
                    ),
                    Self::check(
                        "backup-triggers",
                        "Triggers",
                        my.triggers,
                        |t, on| my_set(t, |m| m.triggers = on),
                        cx,
                    ),
                    Self::check(
                        "backup-extended-inserts",
                        "Extended inserts (several rows per INSERT)",
                        my.extended_inserts,
                        |t, on| my_set(t, |m| m.extended_inserts = on),
                        cx,
                    ),
                    Self::check(
                        "backup-add-drop-table",
                        "Add DROP TABLE statements",
                        my.add_drop_table,
                        |t, on| my_set(t, |m| m.add_drop_table = on),
                        cx,
                    ),
                    Self::check(
                        "backup-disable-keys",
                        "Disable keys while loading",
                        my.disable_keys,
                        |t, on| my_set(t, |m| m.disable_keys = on),
                        cx,
                    ),
                    Self::check(
                        "backup-hex-blob",
                        "Binary columns in hexadecimal",
                        my.hex_blob,
                        |t, on| my_set(t, |m| m.hex_blob = on),
                        cx,
                    ),
                    Self::check(
                        "backup-comments",
                        "Comments",
                        my.comments,
                        |t, on| my_set(t, |m| m.comments = on),
                        cx,
                    ),
                ]
            }
        };
        vec![
            Self::group("Settings", settings),
            self.render_output(cx),
            self.render_advanced(cx),
        ]
    }

    fn render_global_settings(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let g = &self.global;
        let settings = vec![
            Self::field(
                "Encoding",
                Self::small_input("backup-encoding", &self.encoding, 120.),
                cx,
            ),
            Self::check(
                "global-schema-only",
                "Only the definitions (no data)",
                g.schema_only,
                |t, on| t.global.schema_only = on,
                cx,
            ),
            Self::check(
                "global-globals-only",
                "Only the globals (roles and tablespaces)",
                g.globals_only,
                |t, on| {
                    t.global.globals_only = on;
                    if on {
                        t.global.roles_only = false;
                        t.global.tablespaces_only = false;
                    }
                },
                cx,
            ),
            Self::check(
                "global-roles-only",
                "Only the roles",
                g.roles_only,
                |t, on| {
                    t.global.roles_only = on;
                    if on {
                        t.global.globals_only = false;
                        t.global.tablespaces_only = false;
                    }
                },
                cx,
            ),
            Self::check(
                "global-tablespaces-only",
                "Only the tablespaces",
                g.tablespaces_only,
                |t, on| {
                    t.global.tablespaces_only = on;
                    if on {
                        t.global.globals_only = false;
                        t.global.roles_only = false;
                    }
                },
                cx,
            ),
            Self::check(
                "global-no-privileges",
                "Do not back up privileges",
                g.no_privileges,
                |t, on| t.global.no_privileges = on,
                cx,
            ),
            Self::check(
                "global-no-owner",
                "Discard objects owner",
                g.no_owner,
                |t, on| t.global.no_owner = on,
                cx,
            ),
            Self::check(
                "global-role-passwords",
                "Include the roles' passwords",
                g.role_passwords,
                |t, on| t.global.role_passwords = on,
                cx,
            ),
        ];
        vec![
            Self::group("Settings", settings),
            self.render_output(cx),
            self.render_advanced(cx),
        ]
    }

    fn render_input_file(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let mut rows = vec![Self::field(
            "File",
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(TextInput::new(&self.file_field).small().id("restore-file")),
                )
                .child(
                    Button::new("restore-choose-file")
                        .small()
                        .outline()
                        .label("Choose…")
                        .on_click(cx.listener(|this, _, window, cx| this.choose_input(window, cx))),
                )
                .into_any_element(),
            cx,
        )];
        match &self.input {
            None => {}
            Some(Err(problem)) => rows.push(
                div()
                    .id("restore-problem")
                    .test_support()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(SharedString::from(problem.clone()))
                    .into_any_element(),
            ),
            Some(Ok(_)) => {
                rows.push(Self::field(
                    "Format",
                    div()
                        .text_sm()
                        .child(SharedString::from(self.input_text().unwrap_or_default()))
                        .into_any_element(),
                    cx,
                ));
            }
        }
        rows
    }

    /// What the archive needs that the server lacks, said before Start
    /// (K39): each missing extension, left out unless unticked; the
    /// missing owners, and that the backup's owners and privileges are
    /// left out for them.
    pub(super) fn render_preflight(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let found = self.preflight.as_ref()?;
        let theme = cx.theme();
        let quoted = |names: &[String]| {
            let names: Vec<String> = names.iter().map(|name| format!("“{name}”")).collect();
            simple::listed(&names)
        };
        let mut block = v_flex()
            .id("restore-preflight")
            .test_support()
            .gap_1()
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.warning);
        if !found.extensions.is_empty() {
            let one = found.extensions.len() == 1;
            let text = format!(
                "This backup needs the {} {}, which this server does not have.",
                if one { "extension" } else { "extensions" },
                quoted(&found.extensions)
            );
            let hint = format!(
                "A restore cannot install an extension: its files are installed on the machine \
                 PostgreSQL runs on. What in the backup uses {} may then fail.",
                if one { "it" } else { "them" }
            );
            let leaving = found
                .extensions
                .iter()
                .all(|name| self.leave_out.contains(name));
            block = block
                .child(
                    div()
                        .text_sm()
                        .child(SharedString::from(text.clone()))
                        .test_text(text),
                )
                .child(
                    Checkbox::new("restore-leave-out")
                        .label(if one {
                            "Leave it out of the restore"
                        } else {
                            "Leave them out of the restore"
                        })
                        .checked(leaving)
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.leave_out = if *checked {
                                this.preflight
                                    .as_ref()
                                    .map(|found| found.extensions.clone())
                                    .unwrap_or_default()
                            } else {
                                Vec::new()
                            };
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(hint.clone()))
                        .test_text(hint),
                );
        }
        if !found.owners.is_empty() {
            let one = found.owners.len() == 1;
            let user = self
                .auth_user_value(cx)
                .or_else(|| self.params.as_ref().map(|params| params.user.clone()))
                .unwrap_or_default();
            let text = format!(
                "This backup gives its objects to the {} {}, which this server does not have: \
                 the backup's owners and privileges are left out, and what it restores belongs \
                 to “{user}”.",
                if one { "role" } else { "roles" },
                quoted(&found.owners)
            );
            block = block.child(
                div()
                    .text_sm()
                    .child(SharedString::from(text.clone()))
                    .test_text(text),
            );
        }
        Some(block.into_any_element())
    }

    fn render_restore_settings(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let mut rows = self.render_input_file(cx);
        if self.input.is_none() {
            return rows;
        }
        rows.extend(self.render_preflight(cx));
        let r = &self.restore;
        let archive = matches!(&self.input, Some(Ok(input)) if input.format != BackupFormat::Sql);
        let options = match self.family {
            ClientFamily::Postgres if archive => vec![
                Self::check(
                    "restore-clean",
                    "Clean: drop each object before creating it",
                    r.clean,
                    |t, on| {
                        t.restore.clean = on;
                        t.restore.if_exists &= on;
                    },
                    cx,
                ),
                Self::sub_check(
                    "restore-if-exists",
                    "… with IF EXISTS",
                    r.if_exists,
                    r.clean,
                    |t, on| t.restore.if_exists = on && t.restore.clean,
                    cx,
                ),
                Self::check(
                    "restore-create",
                    "Create the database named in the archive and restore into it",
                    r.create,
                    |t, on| {
                        t.restore.create = on;
                        if on {
                            t.restore.single_transaction = false;
                        }
                    },
                    cx,
                ),
                Self::check(
                    "restore-no-owner",
                    "Discard objects owner",
                    r.no_owner,
                    |t, on| t.restore.no_owner = on,
                    cx,
                ),
                Self::check(
                    "restore-no-privileges",
                    "Do not restore privileges",
                    r.no_privileges,
                    |t, on| t.restore.no_privileges = on,
                    cx,
                ),
                Self::check(
                    "restore-single-transaction",
                    "In one transaction (all or nothing)",
                    r.single_transaction,
                    |t, on| {
                        t.restore.single_transaction = on;
                        if on {
                            t.restore.create = false;
                        }
                    },
                    cx,
                ),
                Self::field(
                    "Jobs",
                    Self::small_input("restore-jobs", &self.jobs, 60.),
                    cx,
                ),
            ],
            ClientFamily::Postgres => vec![Self::check(
                "restore-single-transaction",
                "In one transaction (all or nothing)",
                r.single_transaction,
                |t, on| t.restore.single_transaction = on,
                cx,
            )],
            ClientFamily::MySql => vec![Self::check(
                "restore-foreign-keys",
                "Turn foreign-key checks off while it runs",
                r.disable_foreign_keys,
                |t, on| t.restore.disable_foreign_keys = on,
                cx,
            )],
        };
        rows.push(Self::group("Settings", options));
        rows.push(self.render_advanced(cx));
        rows
    }

    fn render_script_settings(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let mut rows = self.render_input_file(cx);
        if self.input.is_none() {
            return rows;
        }
        if self.engine != Engine::Postgres {
            let s = &self.script;
            let levels: Vec<(ScriptLogLevel, &'static str)> = ScriptLogLevel::ALL
                .iter()
                .map(|level| (*level, level.label()))
                .collect();
            rows.push(Self::group(
                "Settings",
                vec![
                    Self::field(
                        "Log level",
                        Self::chips(
                            "script-log-level",
                            &levels,
                            s.log_level,
                            |this, level| this.script.log_level = level,
                            cx,
                        ),
                        cx,
                    ),
                    Self::check(
                        "script-no-beep",
                        "No beep on errors",
                        s.no_beep,
                        |t, on| t.script.no_beep = on,
                        cx,
                    ),
                    Self::check(
                        "script-foreign-keys",
                        "Turn foreign-key checks off while it runs",
                        s.disable_foreign_keys,
                        |t, on| t.script.disable_foreign_keys = on,
                        cx,
                    ),
                ],
            ));
        }
        rows.push(self.render_advanced(cx));
        rows
    }

    /// The client command a file holds, and the opt-in to run it anyway.
    fn render_check(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(Ok(Input {
            refusal: Some(found),
            ..
        })) = &self.input
        else {
            return div().into_any_element();
        };
        let text = refusal_text(found, self.family, &self.database);
        v_flex()
            .gap_2()
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.danger)
            .child(
                div()
                    .id("restore-refusal")
                    .test_support()
                    .text_sm()
                    .child(SharedString::from(text)),
            )
            .child(
                Checkbox::new("restore-allow-commands")
                    .label(SharedString::from(format!(
                        "This file runs commands on this computer: {} at line {}. Run it anyway",
                        found.command, found.line
                    )))
                    .checked(self.allow_commands)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.allow_commands = *checked;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// What the server is and what it asks of a client: the one line at
    /// the top of "Local client…".
    fn server_line(&self) -> String {
        let (major, minor, _) = self.server_version;
        match self.engine {
            Engine::Postgres => format!(
                "Server: PostgreSQL {major}. {} must be {major} or newer.",
                self.tool().unwrap_or(Tool::PgRestore).name()
            ),
            engine => format!(
                "Server: {} {major}.{minor}. A client of the same product and version suits it \
                 best; another one usually works.",
                engine.label()
            ),
        }
    }

    /// The clients "Local client…" lists: each with its label, a short
    /// note on its version against the server's, and whether it can be
    /// used (a PostgreSQL client older than the server never could).
    fn client_choices(
        &self,
        cx: &App,
    ) -> Vec<(&ClientInstall, String, Option<&'static str>, bool)> {
        let family = self.family;
        let in_use = match &self.tool_state {
            ToolState::Ready { install, .. } => Some(install.dir.clone()),
            _ => None,
        };
        let default = AppState::get(cx)
            .settings
            .default_clients
            .get(&family)
            .cloned();
        self.detected
            .iter()
            .filter(|install| install.family == family)
            .map(|install| {
                let compat = tools::compatibility(
                    install.version.as_ref(),
                    self.engine,
                    self.server_version,
                );
                let note = match compat {
                    Compatibility::Ok => None,
                    Compatibility::ClientOlder { .. } => Some("older than the server"),
                    Compatibility::OtherVersion { .. } => Some("another version than the server"),
                    Compatibility::Unknown => Some("its version could not be read"),
                };
                let marks: Vec<&str> = [
                    (in_use.as_ref() == Some(&install.dir), "in use"),
                    (
                        self.client_dir.as_ref() == Some(&install.dir),
                        "this connection's",
                    ),
                    (default.as_ref() == Some(&install.dir), "default"),
                ]
                .into_iter()
                .filter_map(|(on, mark)| on.then_some(mark))
                .collect();
                let label = if marks.is_empty() {
                    install.label()
                } else {
                    format!("{} · {}", install.label(), marks.join(", "))
                };
                (install, label, note, !compat.blocks())
            })
            .collect()
    }

    /// The clients of "Local client…" as listed: folder, note, whether Use
    /// is on; for tests.
    #[cfg(test)]
    pub fn client_rows(&self, cx: &App) -> Vec<(PathBuf, Option<&'static str>, bool)> {
        self.client_choices(cx)
            .into_iter()
            .map(|(install, _, note, usable)| (install.dir.clone(), note, usable))
            .collect()
    }

    /// "Local client…": what the server asks, then every client found for
    /// the engine (the one in use, the default), Browse….
    fn render_clients(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let family = self.family;
        let rows = self.client_choices(cx).into_iter().enumerate().map(
            |(index, (install, label, note, usable))| {
                let dir = install.dir.clone();
                h_flex()
                    .id(("client-install", index))
                    .test_support()
                    .flex_none()
                    .gap_2()
                    .p_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().text_sm().child(SharedString::from(label)))
                                    .children(note.map(|note| Self::muted(note, cx))),
                            )
                            .child(Self::muted(install.dir.display().to_string(), cx)),
                    )
                    .child(
                        Button::new(("client-use", index))
                            .small()
                            .outline()
                            .label("Use")
                            .disabled(!usable)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.use_client(Some(dir.clone()), cx)
                            })),
                    )
            },
        );
        let server = self.server_line();
        v_flex()
            .gap_2()
            .child(
                div()
                    .id("client-server")
                    .test_support()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .aria_label(SharedString::from(server.clone()))
                    .child(SharedString::from(server)),
            )
            .child(Self::muted(
                format!(
                    "The {} clients found on this computer and in Settings ▸ Local clients. The one \
                     chosen is remembered for “{}”; without a choice the connection uses the \
                     default, else the newest.",
                    family.label(),
                    self.name
                ),
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("client-browse")
                            .small()
                            .outline()
                            .label("Browse…")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.browse_client(cx)),
                            ),
                    )
                    .child(
                        Button::new("client-use-default")
                            .small()
                            .ghost()
                            .label("Use the default")
                            .disabled(self.client_dir.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.use_client(None, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("client-back")
                            .small()
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.showing_clients = false;
                                cx.notify();
                            })),
                    ),
            )
            // The list scrolls; the line and the buttons above stay.
            .child(
                v_flex()
                    .id("client-list")
                    .test_support()
                    .gap_2()
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .when_some(self.client_error.clone(), |this, error| {
                this.child(
                    div()
                        .id("client-error")
                        .test_support()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(SharedString::from(error)),
                )
            })
            .into_any_element()
    }

    fn render_command(&self, text: Result<String, String>, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        // A long command wraps and scrolls inside its bound, with a
        // scrollbar that says there is more.
        let scrolled = div()
            .id("backup-command-scroll")
            .p_2()
            .max_h(px(108.))
            .map(|this| match text {
                Ok(command) => this.child(
                    div()
                        .id("backup-command-text")
                        .test_support()
                        .child(SharedString::from(command.clone()))
                        .test_text(command),
                ),
                Err(reason) => this
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(reason)),
            })
            .overflow_y_scrollbar();
        div()
            .id("backup-command")
            .test_support()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .child(scrolled)
            .into_any_element()
    }

    fn render_log(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let list = uniform_list(
            "backup-log-lines",
            self.log.len(),
            cx.processor(|this, range: Range<usize>, _, _| {
                range
                    .filter_map(|index| this.log.get(index).cloned())
                    .map(|line| div().whitespace_nowrap().child(line))
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.log_scroll)
        .size_full();
        div()
            .id("backup-log")
            .test_support()
            .h(px(240.))
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .map(|this| {
                if self.log.is_empty() {
                    this.text_color(theme.muted_foreground)
                        .child("The tool's output appears here.")
                } else {
                    this.child(list)
                }
            })
            .into_any_element()
    }

    /// The page's content.
    fn render_page(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let message = |id: &'static str, text: String| {
            div()
                .id(id)
                .test_support()
                .text_sm()
                .child(SharedString::from(text))
        };
        match &self.step {
            Step::Preparing => {
                return vec![
                    message("backup-preparing", "Getting the connection ready…".into())
                        .text_color(theme.muted_foreground)
                        .into_any_element(),
                ];
            }
            Step::Blocked(why) => {
                return vec![
                    message("backup-blocked", why.clone())
                        .text_color(theme.danger)
                        .into_any_element(),
                ];
            }
            _ => {}
        }
        if self.showing_clients {
            return vec![self.render_clients(cx)];
        }
        let mut body = Vec::new();
        match self.current_page() {
            Page::Objects => body.push(match self.mode {
                Mode::GlobalBackup => self.render_databases(cx),
                _ => self.render_objects(cx),
            }),
            Page::Settings => {
                body.push(Self::field("Client", self.render_tool(false, cx), cx));
                body.extend(match self.mode {
                    Mode::Backup => self.render_backup_settings(cx),
                    Mode::GlobalBackup => self.render_global_settings(cx),
                    Mode::Restore => self.render_restore_settings(cx),
                    Mode::ExecuteScript => self.render_script_settings(cx),
                });
                if matches!(self.tool_state, ToolState::Ready { .. })
                    && (!self.mode.writes() || self.input.is_some())
                {
                    let command = self.command(cx).map(|command| self.shown(&command));
                    body.push(Self::field("Command", self.render_command(command, cx), cx));
                }
            }
            Page::Check => body.push(self.render_check(cx)),
            Page::Progress => {
                let (id, status, color) = match &self.step {
                    Step::Finished { success, text } => (
                        "backup-finished",
                        format!("{text} Took {}.", seconds(self.elapsed.unwrap_or_default())),
                        if *success {
                            theme.success
                        } else {
                            theme.danger
                        },
                    ),
                    _ => (
                        "backup-elapsed",
                        format!(
                            "Running · {}{}",
                            seconds(
                                self.started
                                    .map(|started| started.elapsed())
                                    .unwrap_or_default()
                            ),
                            self.progress()
                                .map(|text| format!(" · {text}"))
                                .unwrap_or_default()
                        ),
                        theme.muted_foreground,
                    ),
                };
                if let Some(failure) = &self.roles {
                    let text = roles_text(failure);
                    body.push(
                        div()
                            .id("restore-roles")
                            .test_support()
                            .p_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.danger)
                            .text_sm()
                            .child(SharedString::from(text.clone()))
                            .test_text(text)
                            .into_any_element(),
                    );
                }
                body.push(
                    message(id, status.clone())
                        .text_color(color)
                        .test_text(status)
                        .into_any_element(),
                );
                if let (Step::Running, Some((sent, total))) = (&self.step, self.fed())
                    && total > 0
                {
                    body.push(
                        gpui_kit::component::progress::Progress::new("backup-progress")
                            .value(sent.min(total) as f32 * 100. / total as f32)
                            .into_any_element(),
                    );
                }
                let command = self.shown_command.clone().unwrap_or_default();
                body.push(self.render_command(Ok(command), cx));
                body.push(self.render_log(cx));
            }
        }
        body
    }

    fn render_footer(&self, cx: &Context<Self>) -> AnyElement {
        let close = |label: &'static str| {
            Button::new("backup-close")
                .ghost()
                .label(label)
                .on_click(|_, window, cx| window.close_dialog(cx))
        };
        let copy = Button::new("backup-copy-log")
            .outline()
            .label("Copy output")
            .on_click(cx.listener(|this, _, _, cx| {
                let text: Vec<&str> = this.log.iter().map(|line| line.as_ref()).collect();
                cx.write_to_clipboard(ClipboardItem::new_string(text.join("\n")));
            }));
        let footer = h_flex().gap_2().pt_2();
        match &self.step {
            Step::Preparing | Step::Blocked(_) => {
                return footer
                    .justify_end()
                    .child(close("Close"))
                    .into_any_element();
            }
            Step::Running => {
                return footer
                    .justify_end()
                    .child(copy)
                    .child(
                        Button::new("backup-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .into_any_element();
            }
            Step::Finished { success, .. } => {
                let reveal = (*success && !self.mode.writes())
                    .then(|| self.output.clone())
                    .flatten();
                let retry = self.roles.as_ref().and_then(|failure| {
                    let one = failure.extensions.len() == 1;
                    match (failure.retry_owners(), failure.retry_extensions()) {
                        (false, false) => None,
                        (true, false) => Some("Restore without owners and privileges…"),
                        (false, true) if one => Some("Restore without this extension…"),
                        (false, true) => Some("Restore without these extensions…"),
                        (true, true) if one => {
                            Some("Restore without owners, privileges and this extension…")
                        }
                        (true, true) => {
                            Some("Restore without owners, privileges and these extensions…")
                        }
                    }
                });
                return footer
                    .justify_end()
                    .child(copy)
                    .when_some(reveal, |this, path| {
                        this.child(
                            Button::new("backup-reveal")
                                .outline()
                                .label(if cfg!(target_os = "macos") {
                                    "Show in Finder"
                                } else {
                                    "Show in folder"
                                })
                                .on_click(move |_, _, cx| cx.reveal_path(&path)),
                        )
                    })
                    .child(
                        Button::new("backup-back")
                            .outline()
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                    )
                    .map(|this| {
                        if let Some(label) = retry {
                            this.child(close("Close")).child(
                                Button::new("restore-without-owners")
                                    .primary()
                                    .label(label)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.restore_without_owners(window, cx)
                                    })),
                            )
                        } else {
                            this.child(close("Close").primary())
                        }
                    })
                    .into_any_element();
            }
            Step::Options => {}
        }
        let first = self.pages().first() == Some(&self.page);
        let last = self.page == self.last_settings_page();
        let (start_id, start_label) = match self.mode {
            Mode::Backup | Mode::GlobalBackup => ("backup-start", "Start"),
            Mode::Restore => ("restore-start", "Restore…"),
            Mode::ExecuteScript => ("restore-start", "Run…"),
        };
        footer
            .child(
                Button::new("backup-local-client")
                    .outline()
                    .label("Local client…")
                    .disabled(self.showing_clients)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.showing_clients = true;
                        cx.notify();
                    })),
            )
            .when(self.simple_offered(), |this| {
                this.child(
                    Button::new("wizard-simple")
                        .ghost()
                        .label("Simple view")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.back_to_simple(window, cx)),
                        ),
                )
            })
            .child(div().flex_1())
            .child(close("Cancel"))
            .child(
                Button::new("wizard-back")
                    .outline()
                    .label("Back")
                    .disabled(first)
                    .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
            )
            .map(|this| {
                if last {
                    // A new task is saved first; a run is started first.
                    let new_task = self.purpose == Purpose::NewTask;
                    let main = |button: Button, main: bool| {
                        if main {
                            button.primary()
                        } else {
                            button.outline()
                        }
                    };
                    this.when(self.task().is_some(), |this| {
                        this.child(
                            Button::new("wizard-save-task")
                                .outline()
                                .label("Save task")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.save_task(true, window, cx)
                                })),
                        )
                    })
                    .child(
                        main(Button::new("wizard-save-as-task"), new_task)
                            .label("Save as task…")
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.save_task(false, window, cx)
                                }),
                            ),
                    )
                    .child(
                        main(Button::new(start_id), !new_task)
                            .label(start_label)
                            .disabled(self.blocked(cx).is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                    )
                } else {
                    this.child(
                        Button::new("wizard-next")
                            .primary()
                            .label("Next")
                            .on_click(cx.listener(|this, _, _, cx| this.next(cx))),
                    )
                }
            })
            .into_any_element()
    }
}

/// What the dialog draws around its page (its title, the intro, the
/// footer, paddings and the kit's margins), with room to spare: a short
/// window shortens the page, which scrolls, and never cuts the buttons.
const DIALOG_CHROME: f32 = 230.;

impl Render for BackupDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page_height = (window.viewport_size().height - px(DIALOG_CHROME))
            .min(px(520.))
            .max(px(160.));
        let simple = self.view == View::Simple;
        let intro = match self.mode {
            Mode::Backup => format!(
                "Back up the database “{}” of “{}” with the engine's dump tool, run on this \
                 computer.",
                self.database, self.name
            ),
            Mode::GlobalBackup => format!(
                "Back up the whole server of “{}” (roles, tablespaces, databases) with \
                 pg_dumpall, run on this computer.",
                self.name
            ),
            Mode::Restore => format!(
                "Restore a backup into the database “{}” of “{}” with the engine's client tool, \
                 run on this computer.",
                self.database, self.name
            ),
            Mode::ExecuteScript => format!(
                "Run a SQL script in the database “{}” of “{}” with the engine's client, run on \
                 this computer.",
                self.database, self.name
            ),
        };
        v_flex()
            .id("backup-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(
                div()
                    .text_sm()
                    .child(SharedString::from(intro.clone()))
                    .test_text(intro),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .when(!simple, |this| this.child(self.render_steps(cx)))
                    .child(
                        div()
                            .id("wizard-page")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .child(
                                // Each part keeps its height and the page
                                // scrolls, with a scrollbar that says so: a
                                // bounded box (the command) is never squeezed
                                // to a sliver at the page's bottom edge.
                                v_flex()
                                    .id("wizard-page-scroll")
                                    .gap_3()
                                    .max_h(page_height)
                                    .children(
                                        if simple && self.simple_page_shown() {
                                            self.render_simple(cx)
                                        } else {
                                            self.render_page(cx)
                                        }
                                        .into_iter()
                                        .map(|part| div().flex_none().child(part)),
                                    )
                                    .overflow_y_scrollbar(),
                            ),
                    ),
            )
            .child(if simple && matches!(self.step, Step::Options) {
                self.render_simple_footer(cx)
            } else {
                self.render_footer(cx)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// K35: what the failure view says, per case; the retry offered only
    /// where a second run can work.
    #[test]
    fn a_restore_that_went_past_errors_is_told_from_one_that_stopped() {
        // pg_restore's own end of a parallel restore, English and French.
        let english = [
            "pg_restore: creating SCHEMA \"archive\"",
            "pg_restore: error: could not execute query: ERROR:  schema \"archive\" already exists",
            "Command was: CREATE SCHEMA archive;",
            "",
            "pg_restore: finished main parallel loop",
            "pg_restore: warning: errors ignored on restore: 1",
        ];
        assert_eq!(errors_ignored(&english), Some(1));
        assert_eq!(ignored_commands(&english), ["CREATE SCHEMA archive;"]);
        let french = [
            "pg_restore: erreur : could not execute query: ERROR:  schema \"archive\" already exists",
            "La commande était : CREATE SCHEMA archive;",
            "pg_restore: attention : erreurs ignorées lors de la restauration : 12",
            "",
        ];
        assert_eq!(errors_ignored(&french), Some(12));
        assert_eq!(ignored_commands(&french), ["CREATE SCHEMA archive;"]);
        // A run that stopped: its last line is the error, with no count.
        let stopped = [
            "pg_restore: error: could not execute query: ERROR:  role \"app_owner\" does not exist",
            "Command was: ALTER SCHEMA archive OWNER TO app_owner;",
        ];
        assert_eq!(errors_ignored(&stopped), None);
        let refused = [
            "pg_restore: error: connection to server at \"localhost\" (::1), port 5432 failed: FATAL:  database \"x\" does not exist",
        ];
        assert_eq!(errors_ignored(&refused), None);
        // Lines of parallel workers that ran into each other are not SQL.
        let garbled = [
            "launching item 4891 FK CONSTRAINT t fk_a",
            " creating INDEX \"public.i\"",
        ];
        assert!(ignored_commands(&garbled).is_empty());
    }

    #[test]
    fn a_refused_role_is_said_with_what_to_do() {
        let options = |single_transaction: bool, without: bool| {
            let mut options =
                RestoreOptions::new(Engine::Postgres, "kerno", "/tmp/x", BackupFormat::Custom);
            options.single_transaction = single_transaction;
            (options.no_owner, options.no_privileges) = (without, without);
            options
        };
        let failure = |script: bool, options: &RestoreOptions, roles: &[(&str, bool)]| {
            let mut failure = RoleFailure::new(script, options, "kerno".into());
            failure.refused = roles
                .iter()
                .map(|(name, owner)| RefusedRole {
                    name: name.to_string(),
                    owner: *owner,
                })
                .collect();
            failure
        };
        let archive = options(true, false);
        // Missing, as the server said.
        let mut missing = failure(false, &archive, &[("app_owner", true)]);
        missing.missing = Some(vec!["app_owner".into()]);
        assert!(missing.retry);
        assert!(roles_text(&missing).starts_with(
            "The backup names the role “app_owner” as the owner of its objects, and this server \
             has no such role. Restore again without"
        ));
        // The role exists, or the server could not be asked.
        let mut refused = failure(false, &archive, &[("app_owner", true), ("reader", false)]);
        refused.missing = Some(Vec::new());
        assert_eq!(
            roles_text(&refused),
            "The backup names the roles “app_owner” and “reader” as an owner and in the rights it \
             grants, and the server would not let “kerno”, the user the restore connects as, use \
             them. Restore again without the backup's owners and privileges: what it restores \
             then belongs to “kerno”, and the backup's grants are left out. Or restore as a user \
             allowed to use them, then restore again."
        );
        // pg_restore went on: no retry into what is now there.
        let went_on = failure(false, &options(false, false), &[("reader", false)]);
        assert!(!went_on.retry);
        assert_eq!(
            roles_text(&went_on),
            "The backup names the role “reader” in the rights it grants, and the server would not \
             let “kerno”, the user the restore connects as, use it. pg_restore went on past it: \
             the rest is restored, without the rights the backup grants it."
        );
        // Already without them; a script.
        assert!(!failure(false, &options(true, true), &[("a", true)]).retry);
        let script = failure(true, &options(true, false), &[("a", true)]);
        assert!(!script.retry && roles_text(&script).contains("psql cannot leave them out"));
    }

    /// K39: an extension the server lacks, after a failure.
    #[test]
    fn a_missing_extension_is_said_with_what_to_do() {
        let failure = |script: bool, single_transaction: bool, leavable: bool| {
            let mut options =
                RestoreOptions::new(Engine::Postgres, "kerno", "/tmp/x", BackupFormat::Custom);
            options.single_transaction = single_transaction;
            let mut failure = RoleFailure::new(script, &options, "kerno".into());
            failure.extensions = vec!["pgaudit".into()];
            failure.leavable = leavable;
            failure
        };
        let archive = failure(false, true, true);
        assert!(archive.retry_extensions() && !archive.retry_owners());
        assert_eq!(
            roles_text(&archive),
            "The backup needs the extension “pgaudit”, which this server does not have. A \
             restore cannot install an extension: its files are installed on the machine \
             PostgreSQL runs on. Restore again without it: what in the backup uses it may then \
             fail, and is said here. Or install it on that machine, then restore again."
        );
        let script = failure(true, true, false);
        assert!(!script.retry_extensions());
        assert!(roles_text(&script).ends_with(
            "A plain SQL script creates it in its own text, so psql cannot leave it out. Install \
             it on that machine, then restore again; or back up again as a compressed archive, \
             which a restore can run without it."
        ));
        let went_on = failure(false, false, true);
        assert!(!went_on.retry_extensions());
        assert!(
            roles_text(&went_on).ends_with(
                "pg_restore went on past it: the rest is restored, except what uses it."
            )
        );
        // A list that could not be read cannot leave it out.
        let unread = failure(false, true, false);
        assert!(!unread.retry_extensions());
        assert!(roles_text(&unread).ends_with(" Install it on that machine, then restore again."));
    }

    #[test]
    fn mysql_through_a_tunnel_with_verify_full_is_refused() {
        let mut config = ConnectionConfig::new(Engine::MySql);
        config.tls.mode = TlsMode::VerifyFull;
        assert!(route_problem(&config).is_none());
        config.ssh = Some(kerno_core::testing::ssh(
            kerno_core::config::SshAuth::Password,
        ));
        assert!(route_problem(&config).unwrap().contains("127.0.0.1"));
        config.tls.mode = TlsMode::VerifyCa;
        assert!(route_problem(&config).is_none());
        // libpq checks the server's name, kept through the tunnel.
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.tls.mode = TlsMode::VerifyFull;
        config.ssh = Some(kerno_core::testing::ssh(
            kerno_core::config::SshAuth::Password,
        ));
        assert!(route_problem(&config).is_none());
    }

    struct Case<'a> {
        engine: Engine,
        input: &'a Input,
        program: &'a str,
        allow: bool,
        restore: &'a RestoreOptions,
        script: bool,
    }

    fn explain(case: Case) -> String {
        restore_explanation(&Explained {
            family: ClientFamily::of(case.engine).expect("a server engine"),
            database: "shop_copy",
            connection: "Shop (prod)",
            input: case.input,
            program: case.program,
            allow_commands: case.allow,
            script: case.script,
            restore: case.restore,
        })
    }

    #[test]
    fn the_restore_confirmation_names_what_it_runs_and_what_a_failure_keeps() {
        let input = |format| Input {
            path: "/backups/shop.sql".into(),
            format,
            refusal: None,
            archive_database: Some("shop".into()),
            list: None,
            extensions: Vec::new(),
        };
        let pg = RestoreOptions::new(Engine::Postgres, "shop_copy", "/b", BackupFormat::Sql);
        let my = RestoreOptions::new(Engine::MySql, "shop_copy", "/b", BackupFormat::Sql);
        let (sql, custom) = (input(BackupFormat::Sql), input(BackupFormat::Custom));
        let case = |engine, input, program, restore| Case {
            engine,
            input,
            program,
            allow: false,
            restore,
            script: false,
        };
        let text = explain(case(Engine::Postgres, &sql, "psql", &pg));
        for part in [
            "/backups/shop.sql",
            "“shop_copy”",
            "“Shop (prod)”",
            "A SQL file is a script run by the client tool: psql runs every statement it holds.",
            "one transaction",
        ] {
            assert!(text.contains(part), "{part}: {text}");
        }
        let mysql = explain(Case {
            allow: true,
            ..case(Engine::MySql, &sql, "mysql", &my)
        });
        assert!(
            mysql.contains("and the client commands it holds, on this computer")
                && mysql.contains("a failed or cancelled run keeps what was already applied"),
            "{mysql}"
        );
        let archive = explain(case(Engine::Postgres, &custom, "pg_restore", &pg));
        assert!(
            !archive.contains("script") && archive.contains("pg_restore"),
            "{archive}"
        );
        // --create restores into the archive's database, dropped first
        // with --clean: the confirmation names it.
        let create = RestoreOptions {
            create: true,
            clean: true,
            single_transaction: false,
            ..pg.clone()
        };
        let text = explain(case(Engine::Postgres, &custom, "pg_restore", &create));
        assert!(
            text.contains(
                "only to create the database “shop” named in the archive, which it drops first"
            ),
            "{text}"
        );
        let script = explain(Case {
            script: true,
            ..case(Engine::Postgres, &sql, "psql", &pg)
        });
        assert!(
            script.contains("runs the script") && !script.contains("one transaction"),
            "{script}"
        );
    }

    #[test]
    fn a_connect_in_a_dump_says_why_it_is_refused() {
        let found = |command: &str| Finding {
            line: 12,
            command: command.into(),
        };
        let connect = refusal_text(&found("\\connect"), ClientFamily::Postgres, "shop_copy");
        assert!(connect.starts_with("Line 12 of this file holds \\connect, a psql"));
        assert!(connect.contains("“create database” option") && connect.contains("“shop_copy”"));
        assert!(
            refusal_text(&found("\\c"), ClientFamily::Postgres, "x").contains("create database")
        );
        let shell = refusal_text(&found("\\!"), ClientFamily::Postgres, "x");
        assert!(shell.contains("start programs") && !shell.contains("create database"));
        let system = refusal_text(&found("system"), ClientFamily::MySql, "x");
        assert!(system.contains("a mysql client command"));
    }

    #[test]
    fn a_refused_script_is_shown_without_the_settings_prefix() {
        let error = Error::Config("dump.sql, line 2: \\! is a psql client command".into());
        assert_eq!(
            error_text(&error),
            "dump.sql, line 2: \\! is a psql client command"
        );
        assert!(error.to_string().starts_with("invalid connection settings"));
        assert_eq!(error_text(&Error::Cancelled), "query cancelled");
    }

    /// K62: the databases a global backup leaves out, said in one line.
    #[test]
    fn the_databases_left_out_are_named() {
        let names = |list: &[&str]| list.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(left_out_closed(&[], 3), None);
        assert_eq!(
            left_out_closed(&names(&["billing"]), 3).unwrap(),
            "Left out, this account may not open it: billing"
        );
        let five = names(&["a", "b", "c", "d", "e"]);
        assert_eq!(
            left_out_closed(&five, 3).unwrap(),
            "Left out, this account may not open them: a, b, c and 2 more"
        );
        assert_eq!(
            left_out_closed(&five, usize::MAX).unwrap(),
            "Left out, this account may not open them: a, b, c, d, e"
        );
    }
}
