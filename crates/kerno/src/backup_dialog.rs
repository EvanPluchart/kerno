//! Backup and restore of a database through the engine's own client tools
//! (`kerno_core::tools`): the tool found or chosen, with its version; the
//! options; the exact command that will run, without its secret; then the
//! tool's output as it comes, a cancel, and how it ended. A restore is
//! confirmed first, twice on production, and refused on a read-only
//! connection.

use std::collections::{BTreeMap, VecDeque};
use std::io::Read as _;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input as TextInput, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _,
    UniformListScrollHandle, Window, div, px, uniform_list,
};
use kerno_core::Error;
use kerno_core::config::{ConnectParams, ConnectionConfig, Engine, Environment, Secrets, TlsMode};
use kerno_core::format::byte_size;
use kerno_core::schema::TableRef;
use kerno_core::ssh::{Tunnel, TunnelRequest};
use kerno_core::tools::{
    self, BackupContent, BackupFormat, BackupOptions, Finding, RestoreOptions, Tool, ToolCommand,
    ToolVersion,
};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::state::AppState;
use crate::tabs::Target;

/// Backing a database up, or restoring a file into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Backup,
    Restore,
}

/// Called with the connection and the database once a restore succeeded.
type Restored = dyn Fn(&str, &str, &mut Window, &mut App);

/// Lines of the tool's output kept on screen: the last ones.
const LOG_LINES: usize = 5_000;

/// What was found for the tool.
#[derive(Debug, Clone)]
enum ToolState {
    /// Looking for it and asking its version.
    Probing,
    Ready {
        tool: Tool,
        path: PathBuf,
        version: ToolVersion,
    },
    /// None of the binaries found can be used: each with its version
    /// (`None`: it does not run). `chosen` when the user picked the one path.
    Unusable {
        tool: Tool,
        found: Vec<(PathBuf, Option<ToolVersion>)>,
        chosen: bool,
    },
}

/// The file to restore, once read.
#[derive(Debug, Clone)]
struct Input {
    path: PathBuf,
    format: BackupFormat,
    /// The first client command the file holds, which the tool would run
    /// on this computer: the restore needs the explicit opt-in.
    refusal: Option<Finding>,
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

pub struct BackupDialog {
    mode: Mode,
    connection: String,
    /// The connection's name, as the confirmation shows it.
    name: String,
    environment: Environment,
    engine: Engine,
    database: String,
    /// PostgreSQL's major version: its tools must be at least as recent.
    server_major: Option<u32>,
    /// How the tool reaches the server, secrets in memory only.
    params: Option<ConnectParams>,
    tool_state: ToolState,
    format: BackupFormat,
    content: BackupContent,
    /// Backup: only these tables, comma-separated; every table when empty.
    tables: Entity<InputState>,
    /// Backup: the file chosen; a name is proposed until then.
    output: Option<PathBuf>,
    /// Restore: the file chosen, and what reading it found.
    file: Option<PathBuf>,
    input: Option<Result<Input, String>>,
    /// The explicit opt-in to a file's client commands; reset per file.
    allow_commands: bool,
    step: Step,
    /// The command that runs or ran, as shown.
    shown_command: Option<String>,
    log: VecDeque<SharedString>,
    log_scroll: UniformListScrollHandle,
    cancel: Arc<AtomicBool>,
    /// Bumped at each probe, so a late answer is dropped.
    generation: u64,
    on_restored: Rc<Restored>,
    probing: Option<Task<()>>,
    running: Option<Task<()>>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Opens the backup of the database of `target` (its connection's default
/// database when it names none), or the restore of a file into it; or says
/// why it cannot.
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
                if mode == Mode::Restore && config.read_only {
                    Err(format!(
                        "“{}” is a read-only connection: a restore writes to the database.",
                        config.name
                    ))
                } else if let Some(problem) = route_problem(config) {
                    Err(problem.to_string())
                } else if let Some(database) = database {
                    Ok((config.clone(), database, meta.server().version_number.0))
                } else {
                    Err(
                        "Choose a database first: right-click it in the tree and choose \
                         “Back up…” or “Restore…”."
                            .into(),
                    )
                }
            }
            _ => Err(
                "Connect first: the client tool reaches the server the way the \
                      connection does."
                    .into(),
            ),
        }
    };
    let (config, database, server_major) = match checked {
        Ok(checked) => checked,
        Err(refusal) => {
            window.push_notification(Notification::warning(SharedString::from(refusal)), cx);
            return None;
        }
    };
    let title = match mode {
        Mode::Backup => format!("Back up {database}"),
        Mode::Restore => format!("Restore into {database}"),
    };
    let opening = Opening {
        connection: target.connection,
        config,
        database,
        server_major,
        mode,
    };
    let on_restored: Rc<Restored> = Rc::new(on_restored);
    let view = cx.new(|cx| BackupDialog::new(connections, opening, on_restored, window, cx));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title(SharedString::from(title.clone()))
            .w(px(720.))
            .on_ok(|_, _, _| false)
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
    server_major: u32,
    mode: Mode,
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
            server_major,
            mode,
        } = opening;
        let tables = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Every table, or some: schema.table, schema.other")
        });
        let subscriptions = vec![cx.subscribe(&tables, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })];
        let mut dialog = Self {
            mode,
            connection,
            name: config.name.clone(),
            environment: config.environment,
            engine: config.engine,
            database,
            server_major: (config.engine == Engine::Postgres).then_some(server_major),
            params: None,
            tool_state: ToolState::Probing,
            format: BackupFormat::Sql,
            content: BackupContent::All,
            tables,
            output: None,
            file: None,
            input: None,
            allow_commands: false,
            step: Step::Preparing,
            shown_command: None,
            log: VecDeque::new(),
            log_scroll: UniformListScrollHandle::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            generation: 0,
            on_restored,
            probing: None,
            running: None,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        dialog.prepare(&connections, config, cx);
        dialog
    }
}

impl BackupDialog {
    /// Gets the secrets the connection uses and opens its SSH tunnel, then
    /// looks for the backup tool.
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

    /// Finds the tool and its version, and for a restore reads the file
    /// first (its format says which tool), off the UI thread.
    fn probe(&mut self, cx: &mut Context<Self>) {
        let Some(params) = self.params.clone() else {
            return;
        };
        if self.mode == Mode::Restore && self.file.is_none() {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.tool_state = ToolState::Probing;
        self.input = None;
        self.allow_commands = false;
        let job = Job {
            engine: self.engine,
            server_major: self.server_major,
            paths: AppState::get(cx).settings.tool_paths.clone(),
            file: self.file.clone(),
            params,
            database: self.database.clone(),
        };
        let examining = bridge::spawn_blocking(cx, move || examine(job));
        self.probing = Some(cx.spawn(async move |this, cx| {
            let (tool, input) = examining.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.tool_state = tool;
                    this.input = input;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// Asks for the binary of the tool, remembers it, and looks again.
    fn choose_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tool = match &self.tool_state {
            ToolState::Ready { tool, .. } | ToolState::Unusable { tool, .. } => *tool,
            ToolState::Probing => return,
        };
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(format!("Use this {}", tool_name(tool)).into()),
        });
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picking.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| {
                    AppState::update_settings(cx, |settings| {
                        settings
                            .tool_paths
                            .insert(tool_name(tool).to_string(), path);
                    });
                    this.probe(cx);
                });
            }
        }));
    }

    /// Asks for the file to restore.
    fn choose_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Restore".into()),
        });
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picking.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| this.load_input(path, cx));
            }
        }));
    }

    /// Takes `path` as the file to restore. The opt-in to its client
    /// commands starts unchecked.
    pub fn load_input(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.file = Some(path);
        self.probe(cx);
    }

    /// Asks where to write the backup; with `then_run`, backs up to it.
    fn choose_output(&mut self, then_run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let proposed = self.output_path();
        let directory = proposed.parent().map(Path::to_path_buf).unwrap_or_default();
        let name = proposed
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let picking = cx.prompt_for_new_path(&directory, name.as_deref());
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = picking.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.output = Some(path);
                    if then_run {
                        this.back_up(window, cx);
                    }
                    cx.notify();
                });
            }
        }));
    }

    /// The backup file: the one chosen, or the name proposed.
    fn output_path(&self) -> PathBuf {
        self.output.clone().unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from);
            home.unwrap_or_default()
                .join(default_name(&self.database, self.format, today()))
        })
    }

    /// The tables typed, qualified: `schema.table`, or a bare name in
    /// `public` (PostgreSQL) or in the database (MySQL, MariaDB).
    fn tables(&self, cx: &App) -> Vec<TableRef> {
        let default_schema = match self.engine {
            Engine::Postgres => "public",
            Engine::MySql | Engine::MariaDb => self.database.as_str(),
        };
        self.tables
            .read(cx)
            .value()
            .split([',', ' ', '\n'])
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| match name.split_once('.') {
                Some((schema, table)) => TableRef::new(schema, table),
                None => TableRef::new(default_schema, name),
            })
            .collect()
    }

    /// The command as the options stand, or why there is none.
    fn command(&self, cx: &App) -> Result<ToolCommand, String> {
        let (ToolState::Ready { path, version, .. }, Some(params)) =
            (&self.tool_state, &self.params)
        else {
            return Err("The command appears once the tool is found.".into());
        };
        let built = match self.mode {
            Mode::Backup => tools::backup_command(
                path,
                params,
                &BackupOptions {
                    database: self.database.clone(),
                    output: self.output_path(),
                    format: self.format,
                    content: self.content,
                    tables: self.tables(cx),
                },
            ),
            Mode::Restore => {
                let Some(Ok(input)) = &self.input else {
                    return Err("Choose the file to restore.".into());
                };
                tools::restore_command(
                    path,
                    params,
                    &RestoreOptions {
                        database: self.database.clone(),
                        input: input.path.clone(),
                        format: input.format,
                        allow_client_commands: self.allow_commands,
                        tool_version: Some(version.clone()),
                    },
                )
            }
        };
        built.map_err(|error| error_text(&error))
    }

    /// Starts the backup, asking for the file first if none was chosen.
    pub fn back_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.output.is_none() {
            self.choose_output(true, window, cx);
            return;
        }
        if let Ok(command) = self.command(cx) {
            self.run(command, window, cx);
        }
    }

    /// Shows what the restore runs and, once confirmed, runs it.
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
        sql_confirm::open(
            Confirmation {
                title: format!("Restore {file} into {}", self.database),
                explanation: restore_explanation(&Explained {
                    engine: self.engine,
                    database: &self.database,
                    connection: &self.name,
                    input: &input,
                    program: &program_name(&command),
                    allow_commands: self.allow_commands,
                }),
                statement: Statement::Fixed(command.display()),
                action: "Restore",
                destructive: true,
                connection: self.name.clone(),
                environment: self.environment,
            },
            window,
            cx,
            move |_, window, cx| {
                let command = confirmed.clone();
                view.update(cx, |this, cx| this.run(command, window, cx));
            },
        );
    }

    /// Runs `command`, its output streamed into the log.
    fn run(&mut self, command: ToolCommand, window: &mut Window, cx: &mut Context<Self>) {
        self.shown_command = Some(command.display());
        self.log.clear();
        self.step = Step::Running;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let output = (self.mode == Mode::Backup).then(|| self.output_path());
        let program = program_name(&command);
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
            if let Some(path) = &output {
                match &ended.outcome {
                    Ok(status) if status.success() => {
                        ended.size = std::fs::metadata(path).ok().map(|meta| meta.len());
                    }
                    // The tool stopped short: a file it wrote is incomplete.
                    // One it never opened (it failed to connect) may be an
                    // earlier backup the save prompt offered to replace: kept.
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
        let backup = self.mode == Mode::Backup;
        let kept = match (backup, self.engine) {
            (true, _) if ended.removed => " The incomplete file was removed.",
            (true, _) => "",
            (false, Engine::Postgres) => " It ran in one transaction: nothing of it was kept.",
            (false, _) => {
                " MySQL and MariaDB restore statement by statement: what ran before it stopped \
                 is kept."
            }
        };
        self.step = match ended.outcome {
            Ok(status) if status.success() => {
                let text = if backup {
                    let path = self.output_path();
                    format!(
                        "{program} finished ({}). Backed up {} to {}{}.",
                        status_text(status),
                        self.database,
                        path.display(),
                        ended
                            .size
                            .map(|size| format!(" ({})", byte_size(size)))
                            .unwrap_or_default()
                    )
                } else {
                    let restored = self.on_restored.clone();
                    restored(&self.connection, &self.database, window, cx);
                    format!(
                        "{program} finished ({}). Restored {} into {}.",
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
        cx.notify();
    }

    /// Stops the tool.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Release);
        cx.notify();
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
                (ToolState::Probing, _) if self.mode == Mode::Restore && self.file.is_none() => {
                    "choose a file".into()
                }
                (ToolState::Probing, _) => "probing".into(),
                (ToolState::Unusable { .. }, _) => format!("no tool: {}", self.unusable_text()),
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

    fn unusable_text(&self) -> String {
        match &self.tool_state {
            ToolState::Unusable {
                tool,
                found,
                chosen,
            } => unusable_text(*tool, found, *chosen, self.server_major),
            _ => String::new(),
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
/// whether an incomplete backup file was removed.
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

/// What [`examine`] works on.
struct Job {
    engine: Engine,
    server_major: Option<u32>,
    paths: BTreeMap<String, PathBuf>,
    /// The file to restore; `None` for a backup.
    file: Option<PathBuf>,
    params: ConnectParams,
    database: String,
}

/// Blocking: reads the start of the file to restore (its format says
/// which tool restores it), finds the tool and its version, and checks a
/// SQL file for client commands the tool would run on this computer.
fn examine(job: Job) -> (ToolState, Option<Result<Input, String>>) {
    let chosen = |tool: Tool| job.paths.get(tool_name(tool)).cloned();
    let Some(file) = job.file else {
        let tool = tools::backup_tool(job.engine);
        return (resolve(tool, chosen(tool), job.server_major), None);
    };
    let format = match guess_format(&file, job.engine) {
        Ok(format) => format,
        Err(problem) => return (ToolState::Probing, Some(Err(problem))),
    };
    let tool = tools::restore_tool(job.engine, format);
    let state = resolve(tool, chosen(tool), job.server_major);
    let mut refusal = None;
    if let ToolState::Ready { path, version, .. } = &state
        && format == BackupFormat::Sql
    {
        let options = RestoreOptions {
            database: job.database,
            input: file.clone(),
            format,
            allow_client_commands: false,
            tool_version: Some(version.clone()),
        };
        let found = tools::restore_command(path, &job.params, &options).and_then(|command| {
            match &command.script_check {
                Some(check) => tools::find_client_command(check, &AtomicBool::new(false)),
                // The tool's own sandbox keeps the commands off.
                None => Ok(None),
            }
        });
        match found {
            Ok(found) => refusal = found,
            Err(error) => return (state, Some(Err(error_text(&error)))),
        }
    }
    (
        state,
        Some(Ok(Input {
            path: file,
            format,
            refusal,
        })),
    )
}

/// The tool to use: the binary the user chose, or the first one installed
/// that runs and, for PostgreSQL, is at least as recent as the server.
fn resolve(tool: Tool, chosen: Option<PathBuf>, server_major: Option<u32>) -> ToolState {
    let candidates = match &chosen {
        Some(path) => vec![path.clone()],
        None => tools::find_all(tool),
    };
    let mut found = Vec::new();
    for path in candidates {
        let version = tools::version(&path);
        if let Some(version) = version
            .clone()
            .filter(|version| recent_enough(version, server_major))
        {
            return ToolState::Ready {
                tool,
                path,
                version,
            };
        }
        found.push((path, version));
    }
    ToolState::Unusable {
        tool,
        found,
        chosen: chosen.is_some(),
    }
}

/// PostgreSQL's tools refuse a server of a newer major version.
fn recent_enough(version: &ToolVersion, server_major: Option<u32>) -> bool {
    server_major.is_none_or(|major| version.number.0 >= major)
}

/// A custom-format archive starts with `PGDMP`; anything else is a script.
fn guess_format(path: &Path, engine: Engine) -> Result<BackupFormat, String> {
    let mut start = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(5).read_to_end(&mut start))
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    match (start == b"PGDMP", engine) {
        (false, _) => Ok(BackupFormat::Sql),
        (true, Engine::Postgres) => Ok(BackupFormat::Custom),
        (true, Engine::MySql | Engine::MariaDb) => Err(format!(
            "{} is a PostgreSQL custom-format archive: MySQL and MariaDB restore SQL files.",
            path.display()
        )),
    }
}

/// Why no tool can be used, how it is usually installed, and what to do.
fn unusable_text(
    tool: Tool,
    found: &[(PathBuf, Option<ToolVersion>)],
    chosen: bool,
    server_major: Option<u32>,
) -> String {
    let name = tool_name(tool);
    let install = match tool {
        Tool::PgDump | Tool::PgRestore | Tool::Psql => {
            "with “brew install libpq” on macOS, or the postgresql-client package of the Linux \
             distribution"
        }
        Tool::MysqlDump | Tool::Mysql => {
            "with “brew install mysql-client” (or “mariadb”) on macOS, or the mysql-client or \
             mariadb-client package of the Linux distribution"
        }
    };
    if found.is_empty() {
        return format!(
            "{name} was not found: Kerno looked in $PATH and in {}. It is usually installed \
             {install}. Choose where it is.",
            tools::FALLBACK_DIRS.join(", ")
        );
    }
    let each: Vec<String> = found
        .iter()
        .map(|(path, version)| match (version, server_major) {
            (None, _) => format!(
                "{} does not run (it is missing or not a program)",
                path.display()
            ),
            (Some(version), Some(major)) => format!(
                "{} is {}, older than this PostgreSQL {major} server",
                path.display(),
                version.text
            ),
            (Some(version), None) => format!("{} is {}", path.display(), version.text),
        })
        .collect();
    let refuses = if server_major.is_some() {
        format!(" {name} refuses a server newer than itself.")
    } else {
        String::new()
    };
    format!(
        "No {name} that Kerno {} can be used: {}.{refuses} A recent one is usually installed \
         {install}. Choose where it is.",
        if chosen { "was given" } else { "found" },
        each.join("; ")
    )
}

/// What the confirmation of a restore is about.
struct Explained<'a> {
    engine: Engine,
    database: &'a str,
    connection: &'a str,
    input: &'a Input,
    program: &'a str,
    allow_commands: bool,
}

/// What the confirmation of a restore says: the file, the database, the
/// connection; that a SQL file is a script; what a failure keeps.
fn restore_explanation(about: &Explained) -> String {
    let program = about.program;
    let mut text = format!(
        "This runs {program} on this computer: it restores {} into the database “{}” of “{}”, \
         on top of what the database holds.",
        about.input.path.display(),
        about.database,
        about.connection
    );
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
        Engine::Postgres => " It runs in one transaction: if a statement fails, nothing is kept.",
        Engine::MySql | Engine::MariaDb => {
            " MySQL and MariaDB apply it statement by statement: a failed or cancelled restore \
             keeps what was already applied."
        }
    });
    text
}

/// Why a file with a client command is not restored as it is.
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

/// The name settings and messages use for a tool: its first binary name.
fn tool_name(tool: Tool) -> &'static str {
    tool.names()[0]
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
        other => other.to_string(),
    }
}

fn status_text(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit status {code}"),
        None => "stopped by a signal".into(),
    }
}

/// `<database>-<yyyy-mm-dd>.sql`, or `.dump` for a custom archive.
fn default_name(
    database: &str,
    format: BackupFormat,
    (year, month, day): (i64, u32, u32),
) -> String {
    let extension = match format {
        BackupFormat::Sql => "sql",
        BackupFormat::Custom => "dump",
    };
    format!("{database}-{year:04}-{month:02}-{day:02}.{extension}")
}

// ponytail: the UTC date; the local one needs the time zone database.
fn today() -> (i64, u32, u32) {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    civil_from_days(i64::try_from(seconds / 86_400).unwrap_or_default())
}

/// The date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

impl BackupDialog {
    /// A labelled row of the options.
    fn field(label: &'static str, field: AnyElement, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .gap_3()
            .items_start()
            .child(
                div()
                    .flex_none()
                    .w(px(96.))
                    .pt_0p5()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().child(field))
            .into_any_element()
    }

    fn render_tool(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let choose = Button::new("backup-choose-tool")
            .small()
            .outline()
            .label("Choose…")
            .on_click(cx.listener(|this, _, window, cx| this.choose_tool(window, cx)));
        let text = match &self.tool_state {
            ToolState::Probing => {
                return div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Looking for the tool…")
                    .into_any_element();
            }
            ToolState::Ready { path, version, .. } => {
                format!("{} · {}", path.display(), version.text)
            }
            ToolState::Unusable { .. } => self.unusable_text(),
        };
        let unusable = matches!(self.tool_state, ToolState::Unusable { .. });
        h_flex()
            .gap_2()
            .items_start()
            .child(
                div()
                    .id("backup-tool")
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .when(unusable, |this| this.text_color(theme.danger))
                    .child(SharedString::from(text)),
            )
            .child(choose)
            .into_any_element()
    }

    fn render_backup_options(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let formats: &[(BackupFormat, &str)] = match self.engine {
            Engine::Postgres => &[
                (BackupFormat::Sql, "Plain SQL"),
                (BackupFormat::Custom, "Custom archive (pg_restore)"),
            ],
            Engine::MySql | Engine::MariaDb => &[(BackupFormat::Sql, "SQL")],
        };
        let contents = [
            (BackupContent::All, "Definitions and data"),
            (BackupContent::SchemaOnly, "Definitions only"),
            (BackupContent::DataOnly, "Data only"),
        ];
        let output = match &self.output {
            Some(path) => path.display().to_string(),
            None => format!(
                "{} (asked when the backup starts)",
                self.output_path().display()
            ),
        };
        vec![
            Self::field(
                "Format",
                h_flex()
                    .gap_1()
                    .children(formats.iter().enumerate().map(|(index, (format, label))| {
                        let format = *format;
                        Button::new(("backup-format", index))
                            .small()
                            .outline()
                            .label(*label)
                            .selected(self.format == format)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.format = format;
                                cx.notify();
                            }))
                    }))
                    .into_any_element(),
                cx,
            ),
            Self::field(
                "Content",
                h_flex()
                    .gap_1()
                    .children(
                        contents
                            .into_iter()
                            .enumerate()
                            .map(|(index, (content, label))| {
                                Button::new(("backup-content", index))
                                    .small()
                                    .outline()
                                    .label(label)
                                    .selected(self.content == content)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.content = content;
                                        cx.notify();
                                    }))
                            }),
                    )
                    .into_any_element(),
                cx,
            ),
            Self::field(
                "Tables",
                TextInput::new(&self.tables)
                    .small()
                    .id("backup-tables")
                    .into_any_element(),
                cx,
            ),
            Self::field(
                "File",
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("backup-output")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(SharedString::from(output)),
                    )
                    .child(
                        Button::new("backup-choose-file")
                            .small()
                            .outline()
                            .label("Choose…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_output(false, window, cx)
                            })),
                    )
                    .into_any_element(),
                cx,
            ),
        ]
    }

    fn render_restore_file(&self, cx: &Context<Self>) -> Vec<AnyElement> {
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
            Some(Ok(input)) => {
                let format = match (input.format, self.engine) {
                    (BackupFormat::Custom, _) => "Custom-format archive, restored by pg_restore",
                    (BackupFormat::Sql, Engine::Postgres) => "SQL script, run by psql",
                    (BackupFormat::Sql, _) => "SQL script, run by the mysql client",
                };
                rows.push(Self::field(
                    "Format",
                    div().text_sm().child(format).into_any_element(),
                    cx,
                ));
                if let Some(found) = &input.refusal {
                    rows.push(self.render_refusal(found, cx));
                }
            }
        }
        rows
    }

    /// The client command a file holds, and the opt-in to run it anyway.
    fn render_refusal(&self, found: &Finding, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
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

    fn render_command(&self, text: Result<String, String>, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        div()
            .id("backup-command")
            .test_support()
            .p_2()
            .max_h(px(120.))
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
            .h(px(220.))
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
}

impl Render for BackupDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let close = |label: &'static str| {
            Button::new("backup-close")
                .ghost()
                .label(label)
                .on_click(|_, window, cx| window.close_dialog(cx))
        };
        let message = |id: &'static str, text: String| {
            div()
                .id(id)
                .test_support()
                .text_sm()
                .child(SharedString::from(text))
        };
        let footer = h_flex().justify_end().gap_2();
        let copy = || {
            Button::new("backup-copy-log")
                .outline()
                .label("Copy output")
                .on_click(cx.listener(|this, _, _, cx| {
                    let text: Vec<&str> = this.log.iter().map(|line| line.as_ref()).collect();
                    cx.write_to_clipboard(ClipboardItem::new_string(text.join("\n")));
                }))
        };
        let intro = match self.mode {
            Mode::Backup => format!(
                "Back up the database “{}” of “{}” with the engine's dump tool, run on this \
                 computer.",
                self.database, self.name
            ),
            Mode::Restore => format!(
                "Restore a backup file into the database “{}” of “{}” with the engine's client \
                 tool, run on this computer.",
                self.database, self.name
            ),
        };
        let mut body: Vec<AnyElement> = Vec::new();
        match &self.step {
            Step::Preparing => {
                body.push(
                    message("backup-preparing", "Getting the connection ready…".into())
                        .text_color(theme.muted_foreground)
                        .into_any_element(),
                );
                body.push(footer.child(close("Close")).into_any_element());
            }
            Step::Blocked(why) => {
                body.push(
                    message("backup-blocked", why.clone())
                        .text_color(theme.danger)
                        .into_any_element(),
                );
                body.push(footer.child(close("Close")).into_any_element());
            }
            Step::Options => {
                if self.mode == Mode::Restore {
                    body.extend(self.render_restore_file(cx));
                }
                let tool_known = self.mode == Mode::Backup || matches!(self.input, Some(Ok(_)));
                if tool_known {
                    body.push(Self::field("Tool", self.render_tool(cx), cx));
                }
                // Without a tool, choosing one is all there is to do.
                let ready = matches!(self.tool_state, ToolState::Ready { .. });
                if ready {
                    if self.mode == Mode::Backup {
                        body.extend(self.render_backup_options(cx));
                    }
                    let command = self.command(cx).map(|command| command.display());
                    let blocked = command.is_err()
                        || matches!(
                            &self.input,
                            Some(Ok(Input { refusal: Some(_), .. })) if !self.allow_commands
                        );
                    body.push(Self::field("Command", self.render_command(command, cx), cx));
                    let start = match self.mode {
                        Mode::Backup => Button::new("backup-start")
                            .label("Back up…")
                            .on_click(cx.listener(|this, _, window, cx| this.back_up(window, cx))),
                        Mode::Restore => Button::new("restore-start").label("Restore…").on_click(
                            cx.listener(|this, _, window, cx| this.confirm_restore(window, cx)),
                        ),
                    };
                    body.push(
                        footer
                            .child(close("Cancel"))
                            .child(start.primary().disabled(blocked))
                            .into_any_element(),
                    );
                } else {
                    body.push(footer.child(close("Close")).into_any_element());
                }
            }
            Step::Running => {
                let command = self.shown_command.clone().unwrap_or_default();
                body.push(self.render_command(Ok(command), cx));
                body.push(self.render_log(cx));
                body.push(
                    footer
                        .child(copy())
                        .child(
                            Button::new("backup-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        )
                        .into_any_element(),
                );
            }
            Step::Finished { success, text } => {
                let command = self.shown_command.clone().unwrap_or_default();
                body.push(
                    message("backup-finished", text.clone())
                        .text_color(if *success {
                            theme.success
                        } else {
                            theme.danger
                        })
                        .into_any_element(),
                );
                body.push(self.render_command(Ok(command), cx));
                body.push(self.render_log(cx));
                let reveal = (*success && self.mode == Mode::Backup).then(|| self.output_path());
                body.push(
                    footer
                        .child(copy())
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
                        .child(Button::new("backup-back").outline().label("Back").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.step = Step::Options;
                                cx.notify();
                            }),
                        ))
                        .child(close("Close").primary())
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .id("backup-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(div().text_sm().child(SharedString::from(intro)))
            .children(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str, major: u32) -> Option<ToolVersion> {
        Some(ToolVersion {
            text: text.into(),
            number: (major, 0, 0),
            mariadb: false,
        })
    }

    #[test]
    fn a_postgresql_tool_must_be_as_recent_as_the_server() {
        let old = version("pg_dump (PostgreSQL) 14.24", 14).unwrap();
        let new = version("pg_dump (PostgreSQL) 18.6", 18).unwrap();
        assert!(!recent_enough(&old, Some(17)));
        assert!(recent_enough(&new, Some(17)));
        assert!(recent_enough(&old, Some(14)));
        // MySQL and MariaDB clients read any server they connect to.
        assert!(recent_enough(&old, None));
    }

    #[test]
    fn the_first_recent_enough_tool_is_chosen_and_the_others_are_named() {
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            use std::os::unix::fs::PermissionsExt;
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let old = script("pg_dump", "echo 'pg_dump (PostgreSQL) 9.6.24'");
        match resolve(Tool::PgDump, Some(old.clone()), Some(17)) {
            ToolState::Unusable {
                found,
                chosen: true,
                ..
            } => {
                assert_eq!(found.len(), 1);
                assert_eq!(found[0].0, old);
                assert_eq!(found[0].1.as_ref().map(|v| v.number.0), Some(9));
            }
            other => panic!("{other:?}"),
        }
        let new = script("pg_dump-new", "echo 'pg_dump (PostgreSQL) 18.1'");
        assert!(matches!(
            resolve(Tool::PgDump, Some(new.clone()), Some(17)),
            ToolState::Ready { path, .. } if path == new
        ));
        // A path that does not run is unusable, whatever the server.
        assert!(matches!(
            resolve(Tool::Mysql, Some(dir.path().join("absent")), None),
            ToolState::Unusable { found, .. } if found[0].1.is_none()
        ));
    }

    #[test]
    fn a_custom_archive_is_told_from_a_script() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join("a.dump");
        std::fs::write(&custom, b"PGDMP\x01\x0e\x00").unwrap();
        let script = dir.path().join("a.sql");
        std::fs::write(&script, b"SELECT 1;\n").unwrap();
        let tiny = dir.path().join("tiny.sql");
        std::fs::write(&tiny, b"PG").unwrap();
        assert_eq!(
            guess_format(&custom, Engine::Postgres),
            Ok(BackupFormat::Custom)
        );
        assert_eq!(
            guess_format(&script, Engine::Postgres),
            Ok(BackupFormat::Sql)
        );
        assert_eq!(guess_format(&tiny, Engine::Postgres), Ok(BackupFormat::Sql));
        assert!(
            guess_format(&custom, Engine::MySql)
                .unwrap_err()
                .contains("PostgreSQL custom-format archive")
        );
        assert!(
            guess_format(&dir.path().join("absent"), Engine::Postgres)
                .unwrap_err()
                .contains("could not read")
        );
    }

    #[test]
    fn backups_are_named_after_the_database_and_the_day() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_770), (2026, 11, 13));
        assert_eq!(
            default_name("kerno_other", BackupFormat::Sql, (2026, 11, 3)),
            "kerno_other-2026-11-03.sql"
        );
        assert_eq!(
            default_name("shop", BackupFormat::Custom, (2026, 1, 9)),
            "shop-2026-01-09.dump"
        );
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

    #[test]
    fn a_missing_or_old_tool_says_why_and_how_it_is_installed() {
        let missing = unusable_text(Tool::Mysql, &[], false, None);
        assert!(
            missing
                .starts_with("mysql was not found: Kerno looked in $PATH and in /opt/homebrew/bin")
                && missing.contains("brew install mysql-client"),
            "{missing}"
        );
        let found = [
            (
                "/opt/homebrew/bin/pg_dump".into(),
                version("pg_dump (PostgreSQL) 14.24", 14),
            ),
            ("/x/pg_dump".into(), None),
        ];
        let old = unusable_text(Tool::PgDump, &found, false, Some(17));
        assert_eq!(
            old,
            "No pg_dump that Kerno found can be used: /opt/homebrew/bin/pg_dump is pg_dump \
             (PostgreSQL) 14.24, older than this PostgreSQL 17 server; /x/pg_dump does not run \
             (it is missing or not a program). pg_dump refuses a server newer than itself. A \
             recent one is usually installed with “brew install libpq” on macOS, or the \
             postgresql-client package of the Linux distribution. Choose where it is."
        );
    }

    #[test]
    fn the_restore_confirmation_names_what_it_runs_and_what_a_failure_keeps() {
        let input = |format| Input {
            path: "/backups/shop.sql".into(),
            format,
            refusal: None,
        };
        let explain = |engine, input: &Input, program, allow_commands| {
            restore_explanation(&Explained {
                engine,
                database: "shop_copy",
                connection: "Shop (prod)",
                input,
                program,
                allow_commands,
            })
        };
        let sql = input(BackupFormat::Sql);
        let pg = explain(Engine::Postgres, &sql, "psql", false);
        for part in [
            "/backups/shop.sql",
            "“shop_copy”",
            "“Shop (prod)”",
            "A SQL file is a script run by the client tool: psql runs every statement it holds.",
            "one transaction",
        ] {
            assert!(pg.contains(part), "{part}: {pg}");
        }
        let mysql = explain(Engine::MySql, &sql, "mysql", true);
        assert!(
            mysql.contains("and the client commands it holds, on this computer")
                && mysql.contains("a failed or cancelled restore keeps what was already applied"),
            "{mysql}"
        );
        let archive = explain(
            Engine::Postgres,
            &input(BackupFormat::Custom),
            "pg_restore",
            false,
        );
        assert!(
            !archive.contains("script") && archive.contains("pg_restore"),
            "{archive}"
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
