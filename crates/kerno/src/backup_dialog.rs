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

use std::collections::{BTreeSet, VecDeque};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input as TextInput, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
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
use kerno_core::format::byte_size;
use kerno_core::schema::{RelationKind, TableRef};
use kerno_core::ssh::{Tunnel, TunnelRequest};
use kerno_core::tools::{
    self, BackupContent, BackupFormat, BackupOptions, ClientFamily, ClientInstall, ClientRoots,
    ClientWanted, Compatibility, DumpMethod, EngineBackup, Finding, GlobalBackupOptions, NameVars,
    RestoreOptions, ScriptLogLevel, ScriptOptions, Stamp, Tool, ToolCommand,
};
use kerno_core::{Error, Session};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::query::tab::duration_text;
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::state::AppState;
use crate::tabs::Target;
use crate::theme;

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

/// Called with the connection and the database once a restore or a
/// script succeeded.
type Restored = dyn Fn(&str, &str, &mut Window, &mut App);

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

/// The tables and views of one schema (the database on MySQL).
#[derive(Debug, Clone)]
struct SchemaEntry {
    name: String,
    relations: Vec<(String, RelationKind)>,
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
    connections: Entity<Connections>,
    connection: String,
    /// The connection's name, as the confirmation shows it.
    name: String,
    environment: String,
    host: String,
    engine: Engine,
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
    /// Global backup: the databases left out.
    excluded: BTreeSet<String>,
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
    /// Restore / script: the file chosen, and what reading it found.
    file: Option<PathBuf>,
    input: Option<Result<Input, String>>,
    /// The explicit opt-in to a file's client commands; reset per file.
    allow_commands: bool,
    step: Step,
    /// The command that runs or ran, as shown.
    shown_command: Option<String>,
    /// The file or folder the backup writes.
    output: Option<PathBuf>,
    started: Option<Instant>,
    elapsed: Option<Duration>,
    log: VecDeque<SharedString>,
    log_scroll: UniformListScrollHandle,
    cancel: Arc<AtomicBool>,
    /// Close the dialog once the stopped tool has ended.
    close_when_stopped: bool,
    /// Bumped at each probe, so a late answer is dropped.
    generation: u64,
    on_restored: Rc<Restored>,
    probing: Option<Task<()>>,
    running: Option<Task<()>>,
    ticking: Option<Task<()>>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

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
                } else if mode == Mode::GlobalBackup && config.engine != Engine::Postgres {
                    Err("The global backup is PostgreSQL's pg_dumpall.".into())
                } else if let Some(problem) = route_problem(config) {
                    Err(problem.to_string())
                } else if let Some(database) = database {
                    Ok((config.clone(), database, meta.server().version_number, meta))
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
    let (config, database, server_version, meta) = match checked {
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
    };
    let on_restored: Rc<Restored> = Rc::new(on_restored);
    let view = cx.new(|cx| BackupDialog::new(connections, opening, on_restored, window, cx));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        let asked = shown.clone();
        dialog
            .title(SharedString::from(title.clone()))
            .w(px(860.))
            .on_ok(|_, _, _| false)
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
        } = opening;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let global = GlobalBackupOptions::default();
        let pattern_default = match mode {
            Mode::GlobalBackup => global.file_pattern.clone(),
            _ => tools::DEFAULT_FILE_PATTERN.to_string(),
        };
        let mut input = |value: String, placeholder: &'static str| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let folder = input(home.display().to_string(), "Folder");
        let pattern = input(pattern_default, "${database}-${timestamp}");
        let extra = input(String::new(), "--name=value …");
        let compression = input(String::new(), "default");
        let encoding = input(String::new(), "the server's");
        let jobs = input(String::new(), "1");
        let auth_user = input(String::new(), "user");
        let auth_password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("password, for this run only")
        });
        let subscriptions = [
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
        })
        .collect();
        let mut dialog = Self {
            mode,
            connections: connections.clone(),
            connection,
            name: config.name.clone(),
            environment: config.environment.clone(),
            host: config.host.clone(),
            engine: config.engine,
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
            file: None,
            input: None,
            allow_commands: false,
            step: Step::Preparing,
            shown_command: None,
            output: None,
            started: None,
            elapsed: None,
            log: VecDeque::new(),
            log_scroll: UniformListScrollHandle::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            close_when_stopped: false,
            generation: 0,
            on_restored,
            probing: None,
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
        // user's own known_hosts (read, never written) or in Kerno's.
        let known_hosts: Vec<PathBuf> = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".ssh/known_hosts"))
            .into_iter()
            .chain([AppState::get(cx).paths.known_hosts()])
            .collect();
        let database = self.database.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let prepared = match secrets.await {
                Err(error) => Err(error),
                Ok(secrets) => {
                    let opening = cx.update(|cx| {
                        bridge::spawn(cx, params_for(config, secrets, known_hosts, database))
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
        let (engine, database) = (self.engine, self.database.clone());
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
                            let objects = fetch_objects(&*session, engine, &database).await;
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
                cx.notify();
            });
        }));
    }

    /// Lists the server's databases, for the global backup.
    fn load_databases(&mut self, meta: Arc<dyn Session>, cx: &mut Context<Self>) {
        let reading = bridge::spawn(cx, async move { meta.databases().await });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let loaded = match reading.await {
                Ok(databases) => Loaded::Ready(
                    databases
                        .into_iter()
                        .filter(|database| !database.name.starts_with("template"))
                        .map(|database| database.name)
                        .collect(),
                ),
                Err(error) => Loaded::Failed(error_text(&error)),
            };
            let _ = this.update(cx, |this, cx| {
                this.databases = loaded;
                cx.notify();
            });
        }));
    }

    /// The tool the wizard runs; a restore's follows its file's format.
    fn tool(&self) -> Option<Tool> {
        match self.mode {
            Mode::Backup => Some(tools::backup_tool(self.engine)),
            Mode::GlobalBackup => Some(Tool::PgDumpall),
            Mode::ExecuteScript => Some(tools::script_tool(self.engine)),
            Mode::Restore => None,
        }
    }

    /// Finds the clients, picks the connection's, and reads the file to
    /// restore or run (its format says which tool), off the UI thread.
    fn probe(&mut self, cx: &mut Context<Self>) {
        let Some(params) = self.params.clone() else {
            return;
        };
        if self.mode.writes() && self.file.is_none() {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.tool_state = ToolState::Probing;
        self.input = None;
        self.allow_commands = false;
        let settings = &AppState::get(cx).settings;
        let family = ClientFamily::of(self.engine);
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
        let family = ClientFamily::of(self.engine);
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
    fn choose_input(&mut self, cx: &mut Context<Self>) {
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
        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picking.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| this.load_input(path, cx));
            }
        }));
    }

    /// Takes `path` as the file to restore or run. The opt-in to its client
    /// commands starts unchecked.
    pub fn load_input(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
        self.selection.complete = tables.is_empty();
        self.selection.schemas = tables.iter().map(|table| table.schema.clone()).collect();
        if let Some(first) = tables.first() {
            self.selection.current = Some(first.schema.clone());
            self.page = Page::Settings;
        }
        self.selection.tables = tables.into_iter().collect();
        cx.notify();
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
            || selection.schemas.iter().any(|schema| {
                self.entry(schema).is_some_and(|entry| {
                    self.visible(entry)
                        .iter()
                        .any(|table| !selection.tables.contains(table))
                })
            });
        let tables = || selection.tables.iter().cloned().collect();
        Ok(match (self.engine, partial) {
            (_, true) => (Vec::new(), tables()),
            (Engine::Postgres, false) => (selection.schemas.iter().cloned().collect(), Vec::new()),
            (Engine::MySql | Engine::MariaDb, false) => (Vec::new(), Vec::new()),
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
                let options = self.global_options(cx)?;
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
                    engine: self.engine,
                    database: &self.database,
                    connection: &self.name,
                    input: &input,
                    program: &program_name(&command),
                    allow_commands: self.allow_commands,
                    script,
                    restore: &self.restore,
                }),
                statement: Statement::Fixed(command.display()),
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
        self.shown_command = Some(command.display());
        self.output = output.as_ref().map(|(path, _)| path.clone());
        self.log.clear();
        self.step = Step::Running;
        self.showing_clients = false;
        self.started = Some(Instant::now());
        self.elapsed = None;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let program = program_name(&command);
        let directory = self.mode == Mode::Backup && self.backup.format == BackupFormat::Directory;
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
        let running = bridge::spawn(cx, async move {
            let started = std::time::SystemTime::now();
            let outcome = tools::run(
                command,
                move |line| {
                    let _ = sender.send(line);
                },
                cancel,
            )
            .await;
            let mut ended = Ended {
                outcome,
                size: None,
                removed: false,
            };
            if let Some((path, existed)) = &output {
                match &ended.outcome {
                    Ok(status) if status.success() => {
                        ended.size = std::fs::metadata(path)
                            .ok()
                            .filter(|meta| meta.is_file())
                            .map(|meta| meta.len());
                    }
                    // The tool stopped short: what it wrote is incomplete.
                    // What it never opened (it failed to connect) may be an
                    // earlier backup: kept.
                    Ok(_) | Err(Error::Cancelled) if directory => {
                        ended.removed = !existed && std::fs::remove_dir_all(path).is_ok();
                    }
                    Ok(_) | Err(Error::Cancelled) => {
                        let written = std::fs::metadata(path)
                            .and_then(|meta| meta.modified())
                            .is_ok_and(|modified| modified >= started);
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
        // The elapsed time, every second while it runs.
        self.ticking = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
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

    fn push_lines(&mut self, lines: Vec<String>, cx: &mut Context<Self>) {
        for line in lines {
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
        let kept = match (writes_file, self.engine) {
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
        self.step = match ended.outcome {
            Ok(status) if status.success() => {
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
        if self.close_when_stopped {
            window.close_dialog(cx);
        }
        cx.notify();
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
                    Ok(command) => format!("ready: {}", command.display()),
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
    known_hosts: Vec<PathBuf>,
    database: String,
) -> kerno_core::Result<ConnectParams> {
    let tunnel = match &config.ssh {
        None => None,
        Some(ssh) => Some(Arc::new(
            Tunnel::open(TunnelRequest {
                ssh,
                password: secrets.ssh_password.as_ref(),
                key_passphrase: secrets.ssh_key_passphrase.as_ref(),
                known_hosts: &known_hosts,
                // Only a key trusted already: nothing is learned here.
                learn_into: None,
                trusted_fingerprint: None,
            })
            .await?,
        )),
    };
    Ok(ConnectParams {
        engine: config.engine,
        host: config.host,
        port: config.port,
        tunnel,
        user: config.user,
        password: secrets.password,
        database: Some(database),
        tls: config.tls,
        read_only: config.read_only,
        options: config.options,
    })
}

/// The schemas and their tables and views (the database alone on MySQL),
/// partitions left out as in the tree.
async fn fetch_objects(
    session: &dyn Session,
    engine: Engine,
    database: &str,
) -> kerno_core::Result<Vec<SchemaEntry>> {
    let schemas: Vec<String> = match engine {
        Engine::Postgres => session
            .schemas()
            .await?
            .into_iter()
            .filter(|schema| !schema.is_system)
            .map(|schema| schema.name)
            .collect(),
        Engine::MySql | Engine::MariaDb => vec![database.to_string()],
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
    let tool = job
        .tool
        .unwrap_or_else(|| tools::restore_tool(job.engine, format.unwrap_or_default()));
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
            input.archive_database = archive_database(path, &file);
        }
    }
    Examined {
        tool: state,
        detected,
        input: Some(Ok(input)),
    }
}

/// The database a PostgreSQL archive was dumped from, as `pg_restore
/// --list` prints it (`;     dbname: shop`), read with the archive's own
/// pg_restore. Blocking.
fn archive_database(pg_restore: &Path, archive: &Path) -> Option<String> {
    let output = std::process::Command::new(pg_restore)
        .arg("--list")
        .arg(archive)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .take(40)
        .find_map(|line| {
            line.trim_start_matches(';')
                .trim()
                .strip_prefix("dbname:")
                .map(|name| name.trim().to_string())
        })
}

/// What the confirmation of a restore or a script is about.
struct Explained<'a> {
    engine: Engine,
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
    text.push_str(match about.engine {
        Engine::Postgres if about.restore.single_transaction && !about.script => {
            " It runs in one transaction: if a statement fails, nothing is kept."
        }
        Engine::Postgres => " A failure keeps what ran before it.",
        Engine::MySql | Engine::MariaDb => {
            " MySQL and MariaDB apply it statement by statement: a failed or cancelled run \
             keeps what was already applied."
        }
    });
    text
}

/// Why a file with a client command is not run as it is.
fn refusal_text(found: &Finding, engine: Engine, database: &str) -> String {
    let client = match engine {
        Engine::Postgres => "psql",
        Engine::MySql | Engine::MariaDb => "mysql",
    };
    let mut text = format!(
        "Line {} of this file holds {}, a {client} client command: the client tool would run \
         it on this computer, not on the server, and it can start programs, read or write \
         files, or connect elsewhere. Nothing was run.",
        found.line, found.command
    );
    if engine == Engine::Postgres && matches!(found.command.as_str(), "\\c" | "\\connect") {
        text.push_str(&format!(
            " pg_dump writes \\connect in a backup made with its “create database” option, to \
             switch to the database it creates. The restore goes into “{database}”, with the \
             password in the tool's environment, so a file that connects elsewhere is refused: \
             back up without that option, or allow the commands of a file you trust."
        ));
    }
    text
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
        other => other.to_string(),
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
        let theme = cx.theme();
        let current = self.current_page();
        v_flex()
            .flex_none()
            .w(px(150.))
            .gap_1()
            .pr_3()
            .border_r_1()
            .border_color(theme.border)
            .children(self.pages().into_iter().enumerate().map(|(index, page)| {
                let here = page == current && !self.showing_clients;
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
                    .child(SharedString::from(format!(
                        "{}. {}",
                        index + 1,
                        page.label(self.mode)
                    )))
            }))
            .into_any_element()
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

    /// The client in use, its version, and why it may not suit.
    fn render_tool(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let text = match &self.tool_state {
            ToolState::Probing => "Looking for the local clients…".to_string(),
            ToolState::Missing(text) => text.clone(),
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
                    .child(SharedString::from(text)),
            )
            .when_some(warning, |this, warning| {
                this.child(
                    div()
                        .id("backup-client-warning")
                        .test_support()
                        .text_xs()
                        .text_color(if blocks { theme.danger } else { theme.warning })
                        .child(SharedString::from(warning)),
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
                .child(SharedString::from(format!(
                    "The objects could not be read: {why}. A complete backup still works."
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
                    "The databases could not be read: {why}."
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
                        .child(
                            div()
                                .flex_1()
                                .child(TextInput::new(&self.folder).small().id("backup-folder")),
                        )
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
        let extra_error =
            tools::extra_args(&self.extra_value(cx), ClientFamily::of(self.engine)).err();
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
                    Self::check(
                        "backup-if-exists",
                        "… with IF EXISTS",
                        pg.if_exists,
                        |t, on| {
                            pg_set(t, |p| {
                                p.if_exists = on;
                                p.clean |= on;
                            })
                        },
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
        let file = match &self.file {
            Some(path) => path.display().to_string(),
            None => "No file chosen".into(),
        };
        let mut rows = vec![Self::field(
            "File",
            h_flex()
                .gap_2()
                .child(
                    div()
                        .id("restore-file")
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(SharedString::from(file)),
                )
                .child(
                    Button::new("restore-choose-file")
                        .small()
                        .outline()
                        .label("Choose…")
                        .on_click(cx.listener(|this, _, _, cx| this.choose_input(cx))),
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

    fn render_restore_settings(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let mut rows = self.render_input_file(cx);
        if self.input.is_none() {
            return rows;
        }
        let r = &self.restore;
        let archive = matches!(&self.input, Some(Ok(input)) if input.format != BackupFormat::Sql);
        let options = match self.engine {
            Engine::Postgres if archive => vec![
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
                Self::check(
                    "restore-if-exists",
                    "… with IF EXISTS",
                    r.if_exists,
                    |t, on| {
                        t.restore.if_exists = on;
                        t.restore.clean |= on;
                    },
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
            Engine::Postgres => vec![Self::check(
                "restore-single-transaction",
                "In one transaction (all or nothing)",
                r.single_transaction,
                |t, on| t.restore.single_transaction = on,
                cx,
            )],
            Engine::MySql | Engine::MariaDb => vec![Self::check(
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
        let text = refusal_text(found, self.engine, &self.database);
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

    /// "Local client…": every client found for the engine, the one in use,
    /// the default, Browse….
    fn render_clients(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let family = ClientFamily::of(self.engine);
        let in_use = match &self.tool_state {
            ToolState::Ready { install, .. } => Some(install.dir.clone()),
            _ => None,
        };
        let default = AppState::get(cx)
            .settings
            .default_clients
            .get(&family)
            .cloned();
        let rows = self
            .detected
            .iter()
            .filter(|install| install.family == family)
            .enumerate()
            .map(|(index, install)| {
                let dir = install.dir.clone();
                let compat = tools::compatibility(
                    install.version.as_ref(),
                    self.engine,
                    self.server_version,
                );
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
                h_flex()
                    .id(("client-install", index))
                    .test_support()
                    .gap_2()
                    .p_1()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().child(SharedString::from(label)))
                            .child(Self::muted(install.dir.display().to_string(), cx))
                            .when(compat.blocks(), |this| {
                                this.child(div().text_xs().text_color(theme.danger).child(
                                    SharedString::from(
                                        compat.warning(self.engine).unwrap_or_default(),
                                    ),
                                ))
                            }),
                    )
                    .child(
                        Button::new(("client-use", index))
                            .small()
                            .outline()
                            .label("Use")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.use_client(Some(dir.clone()), cx)
                            })),
                    )
            });
        v_flex()
            .gap_2()
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
            .children(rows)
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
        div()
            .id("backup-command")
            .test_support()
            .p_2()
            .max_h(px(110.))
            .overflow_y_scroll()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .map(|this| match text {
                Ok(command) => this.child(SharedString::from(command)),
                Err(reason) => this
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(reason)),
            })
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
                body.push(Self::field("Client", self.render_tool(cx), cx));
                body.extend(match self.mode {
                    Mode::Backup => self.render_backup_settings(cx),
                    Mode::GlobalBackup => self.render_global_settings(cx),
                    Mode::Restore => self.render_restore_settings(cx),
                    Mode::ExecuteScript => self.render_script_settings(cx),
                });
                if matches!(self.tool_state, ToolState::Ready { .. }) {
                    let command = self.command(cx).map(|command| command.display());
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
                            "Running · {}",
                            seconds(
                                self.started
                                    .map(|started| started.elapsed())
                                    .unwrap_or_default()
                            )
                        ),
                        theme.muted_foreground,
                    ),
                };
                body.push(
                    message(id, status.clone())
                        .text_color(color)
                        .test_text(status)
                        .into_any_element(),
                );
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
                    .child(close("Close").primary())
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
                    this.child(
                        Button::new(start_id)
                            .primary()
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

impl Render for BackupDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(div().text_sm().child(SharedString::from(intro)))
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(self.render_steps(cx))
                    .child(
                        v_flex()
                            .id("wizard-page")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .gap_3()
                            .max_h(px(520.))
                            .overflow_y_scroll()
                            .children(self.render_page(cx)),
                    ),
            )
            .child(self.render_footer(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            engine: case.engine,
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
        let connect = refusal_text(&found("\\connect"), Engine::Postgres, "shop_copy");
        assert!(connect.starts_with("Line 12 of this file holds \\connect, a psql"));
        assert!(connect.contains("“create database” option") && connect.contains("“shop_copy”"));
        assert!(refusal_text(&found("\\c"), Engine::Postgres, "x").contains("create database"));
        let shell = refusal_text(&found("\\!"), Engine::Postgres, "x");
        assert!(shell.contains("start programs") && !shell.contains("create database"));
        let system = refusal_text(&found("system"), Engine::MySql, "x");
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
}
