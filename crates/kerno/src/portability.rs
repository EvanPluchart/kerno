//! Moving Kerno's setup to another computer (J15, DBeaver #22579): the
//! connections, the looks of their folders, the environments and the saved
//! tasks, to a JSON file and back, without any secret.
//!
//! Export: a dialog says what to include, then the file. Import: a preview
//! lists what the file adds, replaces (same id, same name for a task) or
//! leaves as it is, then a "Passwords" step for the connections written:
//! enter the password now (kept in the secret store), ask when connecting,
//! or use the secret store's entry already filed under the connection's id
//! (looked up, never shown).

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    TestSupportExt as _, Window, div, px,
};
use kerno_core::Secret;
use kerno_core::config::{
    ConnectionConfig, ConnectionsFile, ImportAction, ImportPlan, PortableConnection, PortableTask,
    export_connections_file, import_connections_file, plan_import, reaches_another_server,
};
use kerno_core::environment::{self, EnvironmentDef};
use kerno_core::secrets;
use kerno_core::storage::{SavedConnection, SavedTask};

use crate::bridge;
use crate::connections::manager::Connections;
use crate::state::AppState;
use crate::tasks_view::TasksView;
use crate::theme;

/// "3 connections", "1 task".
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "a, b and c".
fn listed(items: &[String]) -> String {
    match items {
        [] => "nothing".into(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

// Export ------------------------------------------------------------------------

/// What an export can include.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Connections,
    Folders,
    Environments,
    Tasks,
}

impl Part {
    pub const ALL: [Part; 4] = [
        Part::Connections,
        Part::Folders,
        Part::Environments,
        Part::Tasks,
    ];

    fn label(self) -> &'static str {
        match self {
            Part::Connections => "Connections",
            Part::Folders => "Folder colours and icons",
            Part::Environments => "Environments (yours, and the built-ins you edited)",
            Part::Tasks => "Saved tasks",
        }
    }
}

enum ExportState {
    Options,
    Writing,
    Done(String),
    Failed(String),
}

/// The export dialog.
pub struct ExportSetup {
    connections: Entity<Connections>,
    include: [bool; 4],
    /// The saved tasks; `None` while they are read.
    tasks: Option<Vec<SavedTask>>,
    state: ExportState,
    work: Option<Task<()>>,
}

/// Opens the export dialog.
pub fn open_export(
    connections: Entity<Connections>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ExportSetup> {
    let view = cx.new(|cx| {
        let storage = AppState::get(cx).storage.clone();
        let reading = bridge::spawn_blocking(cx, move || storage.tasks());
        let work = cx.spawn(async move |this, cx| {
            let tasks = reading.await.unwrap_or_default();
            let _ = this.update(cx, |this: &mut ExportSetup, cx| {
                this.tasks = Some(tasks);
                cx.notify();
            });
        });
        ExportSetup {
            connections,
            include: [true; 4],
            tasks: None,
            state: ExportState::Options,
            work: Some(work),
        }
    });
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title("Export connections and settings")
            .w(px(560.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    view
}

impl ExportSetup {
    pub fn set_included(&mut self, part: Part, included: bool, cx: &mut Context<Self>) {
        self.include[part as usize] = included;
        cx.notify();
    }

    fn included(&self, part: Part) -> bool {
        self.include[part as usize]
    }

    /// The saved connections, the unsaved ones (`kerno <url>`) left out.
    fn saved(&self, cx: &App) -> Vec<SavedConnection> {
        let connections = self.connections.read(cx);
        connections
            .saved()
            .iter()
            .filter(|saved| !connections.is_unsaved(&saved.id))
            .cloned()
            .collect()
    }

    /// The file to write, but its tasks, read from storage with it; and
    /// the saved connections, which give the tasks their engine.
    fn file(&self, cx: &App) -> (ConnectionsFile, Vec<SavedConnection>) {
        let settings = &AppState::get(cx).settings;
        let saved = self.saved(cx);
        let connections: Vec<PortableConnection> = match self.included(Part::Connections) {
            true => saved
                .iter()
                .map(|saved| PortableConnection {
                    id: Some(saved.id.clone()),
                    config: saved.config.clone(),
                })
                .collect(),
            false => Vec::new(),
        };
        // The environments the connections are in always go with them: a
        // production connection must not land in "local" elsewhere.
        let environments = settings
            .environments
            .iter()
            .filter(|def| {
                self.included(Part::Environments)
                    || connections.iter().any(|c| c.config.environment == def.id)
            })
            .cloned()
            .collect();
        let folders = match self.included(Part::Folders) {
            true => settings.folder_looks.clone(),
            false => BTreeMap::new(),
        };
        let file = ConnectionsFile {
            connections,
            folders,
            environments,
            ..ConnectionsFile::default()
        };
        (file, saved)
    }

    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let picking = cx.prompt_for_new_path(&home, Some("kerno-connections.json"));
        self.work = Some(cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = picking.await {
                let _ = this.update_in(cx, |this, window, cx| this.export_to(path, window, cx));
            }
        }));
    }

    /// Writes the file to `path`.
    pub fn export_to(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let (mut file, saved) = self.file(cx);
        let tasks_included = self.included(Part::Tasks);
        let storage = AppState::get(cx).storage.clone();
        self.state = ExportState::Writing;
        let writing = bridge::spawn_blocking(cx, move || {
            let mut left_out = 0;
            if tasks_included {
                for task in storage.tasks()? {
                    // A task whose connection is gone has no engine to
                    // check its arguments against: left out, and said.
                    let Some(connection) =
                        saved.iter().find(|saved| saved.id == task.connection_id)
                    else {
                        left_out += 1;
                        continue;
                    };
                    file.tasks.push(PortableTask {
                        name: task.name,
                        description: task.description,
                        connection_id: task.connection_id,
                        engine: connection.config.engine,
                        settings: task.settings,
                    });
                }
            }
            let message = export_message(&file, &path, left_out);
            let json = export_connections_file(file)?;
            std::fs::write(&path, json).map_err(|error| {
                kerno_core::Error::io(format!("writing {}", path.display()), error)
            })?;
            Ok::<_, kerno_core::Error>(message)
        });
        self.work = Some(cx.spawn_in(window, async move |this, cx| {
            let outcome = writing.await;
            let _ = this.update(cx, |this, cx| {
                this.state = match outcome {
                    Ok(message) => ExportState::Done(message),
                    Err(error) => ExportState::Failed(format!("The export failed: {error}")),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Where the export is, in words; for tests.
    #[cfg(test)]
    pub fn state(&self) -> String {
        match &self.state {
            ExportState::Options => "options".into(),
            ExportState::Writing => "writing".into(),
            ExportState::Done(message) => format!("done: {message}"),
            ExportState::Failed(error) => format!("failed: {error}"),
        }
    }
}

/// What an export wrote, in words.
fn export_message(file: &ConnectionsFile, path: &std::path::Path, left_out: usize) -> String {
    let builtins = environment::builtin_environments();
    let mut written = Vec::new();
    let mut note = |n: usize, one: &str, many: &str| {
        if n > 0 {
            written.push(count(n, one, many));
        }
    };
    note(file.connections.len(), "connection", "connections");
    note(
        file.folders
            .values()
            .filter(|look| look.color.is_some() || look.glyph.is_some())
            .count(),
        "folder look",
        "folder looks",
    );
    note(
        file.environments
            .iter()
            .filter(|def| !builtins.contains(def))
            .count(),
        "environment",
        "environments",
    );
    note(file.tasks.len(), "task", "tasks");
    let mut message = format!(
        "Exported {} to {}. No password, passphrase or key is in the file.",
        listed(&written),
        path.display()
    );
    if left_out > 0 {
        message.push_str(&format!(
            " {} left out: {} connection is gone.",
            count(left_out, "task was", "tasks were"),
            if left_out == 1 { "its" } else { "their" }
        ));
    }
    message
}

impl Render for ExportSetup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, success, danger) = (theme.muted_foreground, theme.success, theme.danger);
        let settings = &AppState::get(cx).settings;
        let builtins = environment::builtin_environments();
        let counts = [
            Some(self.saved(cx).len()),
            Some(settings.folder_looks.len()),
            Some(
                settings
                    .environments
                    .iter()
                    .filter(|def| !builtins.contains(def))
                    .count(),
            ),
            self.tasks.as_ref().map(Vec::len),
        ];
        let close = Button::new("portable-close")
            .ghost()
            .label("Close")
            .on_click(|_, window, cx| window.close_dialog(cx));
        let body: AnyElement = match &self.state {
            ExportState::Options | ExportState::Writing => v_flex()
                .gap_2()
                .children(Part::ALL.into_iter().enumerate().map(|(index, part)| {
                    let shown = match counts[index] {
                        Some(n) => format!("{} ({n})", part.label()),
                        None => format!("{} (reading…)", part.label()),
                    };
                    Checkbox::new(("portable-include", index))
                        .label(SharedString::from(shown))
                        .checked(self.include[index])
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.set_included(part, *checked, cx)
                        }))
                }))
                .child(div().text_xs().text_color(muted).child(
                    "Passwords, passphrases and keys stay in this computer's secret store. Local \
                     client folders and folder pictures stay too. The environments your \
                     connections use always go with them.",
                ))
                .child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(close.label("Cancel"))
                        .child(
                            Button::new("portable-choose-file")
                                .primary()
                                .label("Choose file…")
                                .disabled(matches!(self.state, ExportState::Writing))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.choose_file(window, cx)),
                                ),
                        ),
                )
                .into_any_element(),
            ExportState::Done(message) | ExportState::Failed(message) => {
                let failed = matches!(self.state, ExportState::Failed(_));
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .id("portable-done")
                            .test_support()
                            .text_sm()
                            .text_color(if failed { danger } else { success })
                            .child(SharedString::from(message.clone())),
                    )
                    .child(h_flex().justify_end().child(close.primary()))
                    .into_any_element()
            }
        };
        v_flex()
            .id("portable-export-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(body)
    }
}

// Import ------------------------------------------------------------------------

/// What happens to a written connection's password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The secret store's entry under the connection's id is used.
    Linked,
    /// Typed now, kept in the secret store.
    EnterNow,
    /// Asked at connect; an entry under the id is removed.
    Ask,
}

/// A connection of the file, as the Passwords step sees it.
struct Row {
    /// The secret store holds a password under its id.
    linked: bool,
    /// It replaces a connection on another server: `(before, after)`.
    moved: Option<(String, String)>,
    /// The local client folder of the connection it replaces, kept.
    client_dir: Option<PathBuf>,
    choice: Choice,
    password: Entity<InputState>,
}

/// What the import does with a written connection's password.
struct Password {
    /// Typed now; `None` for "Ask when connecting".
    secret: Option<Secret>,
    /// Written under a new id (see [`ImportSetup::import`]).
    fresh_id: bool,
    /// Every secret under the id (password, SSH password, key passphrase)
    /// is removed first: the connection is not linked to them.
    forget: bool,
}

enum Step {
    Preview,
    Passwords,
    Importing,
    Done(String),
    Failed(String),
}

/// The import dialog.
pub struct ImportSetup {
    connections: Entity<Connections>,
    tasks_views: Vec<Entity<TasksView>>,
    file: ConnectionsFile,
    plan: ImportPlan,
    rows: Vec<Row>,
    tasks: Vec<SavedTask>,
    /// Write the items that replace one with the same id.
    replace: bool,
    step: Step,
    problem: Option<String>,
    work: Option<Task<()>>,
}

/// What the file and this computer hold, read off the UI thread.
struct Loaded {
    file: ConnectionsFile,
    plan: ImportPlan,
    tasks: Vec<SavedTask>,
    linked: Vec<bool>,
}

/// Reads `path`, compares it with this computer and opens the import
/// preview, which the task returns; a file that cannot be read or is not
/// an export says why.
pub fn open_import(
    path: PathBuf,
    connections: Entity<Connections>,
    tasks_views: Vec<Entity<TasksView>>,
    window: &mut Window,
    cx: &mut App,
) -> Task<Option<Entity<ImportSetup>>> {
    let state = AppState::get(cx);
    let (storage, store) = (state.storage.clone(), state.secrets.clone());
    let environments = state.settings.environments.clone();
    let saved = connections.read(cx).saved().to_vec();
    let source = path.clone();
    let reading = bridge::spawn_blocking(cx, move || {
        let json = std::fs::read_to_string(&source).map_err(|error| {
            kerno_core::Error::io(format!("reading {}", source.display()), error)
        })?;
        let file = import_connections_file(&json)?;
        let tasks = storage.tasks()?;
        let plan = plan_import(&file, &saved, &environments, &tasks);
        // Whether any secret is filed under the id; only the answer is kept.
        let linked = file
            .connections
            .iter()
            .map(|connection| match &connection.id {
                Some(id) => secrets::has_secrets(store.as_ref(), id),
                None => Ok(false),
            })
            .collect::<kerno_core::Result<Vec<bool>>>()?;
        Ok::<_, kerno_core::Error>(Loaded {
            file,
            plan,
            tasks,
            linked,
        })
    });
    window.spawn(cx, async move |cx| {
        let outcome = reading.await;
        cx.update(|window, cx| match outcome {
            Ok(loaded) => {
                let view =
                    cx.new(|cx| ImportSetup::new(connections, tasks_views, loaded, window, cx));
                let shown = view.clone();
                window.open_dialog(cx, move |dialog, _, _| {
                    let view = shown.clone();
                    dialog
                        .title("Import connections and settings")
                        .w(px(640.))
                        .on_ok(|_, _, _| false)
                        .content(move |content, _, _| content.child(view.clone()))
                });
                Some(view)
            }
            Err(error) => {
                window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "{} was not imported: {error}",
                        path.display()
                    ))),
                    cx,
                );
                None
            }
        })
        .ok()
        .flatten()
    })
}

impl ImportSetup {
    fn new(
        connections: Entity<Connections>,
        tasks_views: Vec<Entity<TasksView>>,
        loaded: Loaded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let Loaded {
            file,
            plan,
            tasks,
            linked,
        } = loaded;
        let saved = connections.read(cx).saved().to_vec();
        let rows = file
            .connections
            .iter()
            .zip(&plan.connections)
            .zip(linked)
            .map(|((imported, action), linked)| {
                let existing = imported
                    .id
                    .as_deref()
                    .and_then(|id| saved.iter().find(|saved| saved.id == id));
                let moved = existing
                    .filter(|existing| {
                        *action == ImportAction::Replace
                            && reaches_another_server(&existing.config, &imported.config)
                    })
                    .map(|existing| (server(&existing.config), server(&imported.config)));
                let choice = default_choice(linked, existing.map(|e| &e.config), &imported.config);
                Row {
                    linked,
                    moved,
                    client_dir: existing.and_then(|existing| existing.config.client_dir.clone()),
                    choice,
                    password: cx.new(|cx| {
                        InputState::new(window, cx)
                            .masked(true)
                            .placeholder("Password")
                    }),
                }
            })
            .collect();
        Self {
            connections,
            tasks_views,
            file,
            plan,
            rows,
            tasks,
            replace: true,
            step: Step::Preview,
            problem: None,
            work: None,
        }
    }

    /// Whether the item with `action` is written.
    fn writes(&self, action: ImportAction) -> bool {
        match action {
            ImportAction::Add => true,
            ImportAction::Replace => self.replace,
            ImportAction::Unchanged => false,
        }
    }

    /// The connections the Passwords step asks about: written, with a
    /// password kept in the secret store (not a command, not "ask each
    /// time").
    fn password_rows(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&index| {
                let config = &self.file.connections[index].config;
                self.writes(self.plan.connections[index])
                    && config.save_password
                    && config.password_source.is_stored()
            })
            .collect()
    }

    pub fn set_replace(&mut self, replace: bool, cx: &mut Context<Self>) {
        self.replace = replace;
        cx.notify();
    }

    pub fn choose(&mut self, index: usize, choice: Choice, cx: &mut Context<Self>) {
        if choice == Choice::Linked && !self.rows[index].linked {
            return;
        }
        self.rows[index].choice = choice;
        self.problem = None;
        cx.notify();
    }

    /// Next from the preview: the Passwords step, or the import when no
    /// connection needs one.
    pub fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.password_rows().is_empty() {
            self.import(window, cx);
        } else {
            self.step = Step::Passwords;
            cx.notify();
        }
    }

    /// Writes what the preview listed.
    pub fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.password_rows();
        let mut passwords: BTreeMap<usize, Password> = BTreeMap::new();
        for (index, row) in self.rows.iter().enumerate() {
            // Not linked, the store's secrets under the id must not be
            // used: a new connection gets an id of its own (the entry stays
            // with whoever has it), a replaced one has them removed. This
            // holds for the connections the step does not show too, whose
            // SSH secrets would otherwise follow.
            let unlinked = row.linked && row.choice != Choice::Linked;
            let secret = match row.choice {
                Choice::EnterNow if shown.contains(&index) => {
                    let typed = row.password.read(cx).value().to_string();
                    if typed.is_empty() {
                        self.problem = Some(format!(
                            "Enter the password of “{}”, or choose “Ask when connecting”.",
                            self.file.connections[index].config.name
                        ));
                        cx.notify();
                        return;
                    }
                    Some(Secret::new(typed))
                }
                _ if unlinked => None,
                _ => continue,
            };
            let fresh_id = unlinked && self.plan.connections[index] == ImportAction::Add;
            passwords.insert(
                index,
                Password {
                    secret,
                    fresh_id,
                    forget: unlinked && !fresh_id,
                },
            );
        }
        // The connections replaced, and those of them whose secrets change:
        // an open one reconnects with its new settings (J15).
        let replaced: Vec<String> = (self.file.connections.iter())
            .zip(&self.plan.connections)
            .filter(|(_, action)| self.replace && **action == ImportAction::Replace)
            .filter_map(|(connection, _)| connection.id.clone())
            .collect();
        let secrets_changed: Vec<String> = (passwords.keys())
            .filter_map(|index| self.file.connections[*index].id.clone())
            .filter(|id| replaced.contains(id))
            .collect();
        let reopened = (replaced, secrets_changed);
        let state = AppState::get(cx);
        let (storage, store) = (state.storage.clone(), state.secrets.clone());
        let replace = self.replace;
        let connections: Vec<(PortableConnection, ImportAction, Option<PathBuf>)> = self
            .file
            .connections
            .iter()
            .cloned()
            .zip(&self.plan.connections)
            .zip(&self.rows)
            .map(|((connection, action), row)| (connection, *action, row.client_dir.clone()))
            .collect();
        let mut tasks: Vec<SavedTask> = self
            .file
            .tasks
            .iter()
            .zip(&self.plan.tasks)
            .filter(|(_, action)| self.writes(**action))
            .map(|(task, _)| {
                let existing = self.tasks.iter().find(|saved| saved.name == task.name);
                let mut saved = existing.cloned().unwrap_or_else(|| {
                    SavedTask::new(&task.name, &task.connection_id, task.settings.clone())
                });
                saved.description = task.description.clone();
                saved.connection_id = task.connection_id.clone();
                saved.settings = task.settings.clone();
                saved
            })
            .collect();
        let writing = bridge::spawn_blocking(cx, move || {
            let mut written = 0;
            // Ids given anew, for the tasks that name the old ones.
            let mut renamed = std::collections::HashMap::new();
            for (index, (connection, action, client_dir)) in connections.into_iter().enumerate() {
                let mut config = connection.config;
                let password = passwords.get(&index);
                let fresh_id = password.is_some_and(|password| password.fresh_id);
                let id = match (action, connection.id) {
                    (ImportAction::Add, Some(id)) if !fresh_id => {
                        storage.add_connection(&id, &config).map(|()| id)?
                    }
                    (ImportAction::Add, old) => {
                        let id = storage.save_connection(None, &config)?;
                        if let Some(old) = old {
                            renamed.insert(old, id.clone());
                        }
                        id
                    }
                    (ImportAction::Replace, Some(id)) if replace => {
                        // The local client stays this computer's choice.
                        config.client_dir = client_dir;
                        storage.save_connection(Some(&id), &config)?
                    }
                    _ => continue,
                };
                written += 1;
                if let Some(password) = password {
                    // The secrets of the connection it replaces, all of
                    // them, would otherwise go to its new server. A fresh
                    // id has none.
                    if password.forget {
                        secrets::delete_secrets(store.as_ref(), &id)?;
                    }
                    if let Some(secret) = &password.secret {
                        secrets::save_password(store.as_ref(), &id, secret)?;
                    }
                }
            }
            let tasks_written = tasks.len();
            for task in &mut tasks {
                if let Some(id) = renamed.get(&task.connection_id) {
                    task.connection_id = id.clone();
                }
                storage.save_task(task)?;
            }
            Ok::<_, kerno_core::Error>((written, tasks_written))
        });
        self.step = Step::Importing;
        self.work = Some(cx.spawn_in(window, async move |this, cx| {
            let outcome = writing.await;
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok((connections, tasks)) => this.finish(connections, tasks, reopened, window, cx),
                Err(error) => {
                    // What was written before the failure is shown, and
                    // goes into the open connections.
                    let (ids, secrets) = reopened;
                    this.connections
                        .update(cx, |list, cx| list.replaced(ids, &secrets, cx));
                    this.step = Step::Failed(format!("The import stopped: {error}"));
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// The settings part of the import, then the summary.
    fn finish(
        &mut self,
        connections: usize,
        tasks: usize,
        (replaced, secrets): (Vec<String>, Vec<String>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let environments: Vec<(EnvironmentDef, ImportAction)> = self
            .file
            .environments
            .iter()
            .cloned()
            .zip(self.plan.environments.iter().copied())
            .filter(|(_, action)| self.writes(*action))
            .collect();
        let environment_count = environments.len();
        if !environments.is_empty() {
            AppState::update_settings(cx, move |settings| {
                for (def, _) in environments {
                    match settings
                        .environments
                        .iter_mut()
                        .find(|kept| kept.id == def.id)
                    {
                        Some(kept) => *kept = def,
                        None => settings.environments.push(def),
                    }
                }
                settings.environments =
                    environment::with_builtins(std::mem::take(&mut settings.environments));
            });
        }
        let looks = std::mem::take(&mut self.file.folders);
        self.connections.update(cx, |list, cx| {
            list.import_folder_looks(looks, cx);
            list.replaced(replaced, &secrets, cx);
        });
        for view in &self.tasks_views {
            view.update(cx, |view, cx| view.reload(cx));
        }
        let mut written = Vec::new();
        for (n, one, many) in [
            (connections, "connection", "connections"),
            (environment_count, "environment", "environments"),
            (tasks, "task", "tasks"),
        ] {
            if n > 0 {
                written.push(count(n, one, many));
            }
        }
        let message = format!("Imported {}.", listed(&written));
        window.push_notification(
            Notification::success(SharedString::from(message.clone())),
            cx,
        );
        self.step = Step::Done(message);
        cx.notify();
    }

    /// The preview and the choices, in words; for tests.
    #[cfg(test)]
    pub fn summary(&self) -> String {
        let step = match &self.step {
            Step::Preview => "preview".to_string(),
            Step::Passwords => "passwords".into(),
            Step::Importing => "importing".into(),
            Step::Done(message) => format!("done: {message}"),
            Step::Failed(error) => format!("failed: {error}"),
        };
        let connections: Vec<String> = self
            .file
            .connections
            .iter()
            .zip(&self.plan.connections)
            .zip(&self.rows)
            .map(|((connection, action), row)| {
                format!(
                    "{} {action:?} {:?}{}",
                    connection.config.name,
                    row.choice,
                    if row.moved.is_some() { " moved" } else { "" }
                )
            })
            .collect();
        format!(
            "{step} | {} | environments {:?} | tasks {:?}",
            connections.join(", "),
            self.plan.environments,
            self.plan.tasks
        )
    }

    fn render_preview(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, warning) = (theme.muted_foreground, theme.warning);
        let builtins = environment::builtin_environments();
        let what = |action: ImportAction| match action {
            ImportAction::Add => "New",
            ImportAction::Replace if self.replace => "Replaces the one with the same id",
            ImportAction::Replace => "Same id here: skipped",
            ImportAction::Unchanged => "Already here: skipped",
        };
        let line = |id: (&'static str, usize),
                    name: String,
                    action: ImportAction,
                    note: Option<String>| {
            v_flex()
                .id(id)
                .test_support()
                .child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(div().flex_1().truncate().child(SharedString::from(name)))
                        .child(div().text_color(muted).child(what(action))),
                )
                .when_some(note, |this, note| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(warning)
                            .child(SharedString::from(note)),
                    )
                })
                .into_any_element()
        };
        let heading = |text: String| {
            div()
                .pt_1()
                .text_xs()
                .font_semibold()
                .text_color(muted)
                .child(SharedString::from(text))
                .into_any_element()
        };
        let mut items = Vec::new();
        if !self.file.connections.is_empty() {
            items.push(heading(count(
                self.file.connections.len(),
                "Connection",
                "Connections",
            )));
            for (index, (connection, action)) in self
                .file
                .connections
                .iter()
                .zip(&self.plan.connections)
                .enumerate()
            {
                let note = self.rows[index]
                    .moved
                    .as_ref()
                    .map(|(before, after)| format!("Its server changes: {before} → {after}."));
                items.push(line(
                    ("import-connection", index),
                    connection.config.name.clone(),
                    *action,
                    note,
                ));
            }
        }
        if !self.file.environments.is_empty() {
            items.push(heading(count(
                self.file.environments.len(),
                "Environment",
                "Environments",
            )));
            for (index, (def, action)) in self
                .file
                .environments
                .iter()
                .zip(&self.plan.environments)
                .enumerate()
            {
                // Rules weaker than Kerno ships are worth a look.
                let note = builtins
                    .iter()
                    .find(|builtin| builtin.id == def.id && builtin.rules != def.rules)
                    .filter(|_| *action == ImportAction::Replace)
                    .map(|builtin| {
                        format!("Changes the rules of the built-in “{}”.", builtin.name)
                    });
                items.push(line(
                    ("import-environment", index),
                    def.name.clone(),
                    *action,
                    note,
                ));
            }
        }
        if !self.file.tasks.is_empty() {
            items.push(heading(count(self.file.tasks.len(), "Task", "Tasks")));
            for (index, (task, action)) in self.file.tasks.iter().zip(&self.plan.tasks).enumerate()
            {
                items.push(line(
                    ("import-task", index),
                    task.name.clone(),
                    *action,
                    None,
                ));
            }
        }
        if !self.file.folders.is_empty() {
            items.push(heading(format!(
                "{}: added where this computer has none",
                count(self.file.folders.len(), "Folder look", "Folder looks")
            )));
        }
        let replaced = self
            .plan
            .connections
            .iter()
            .chain(&self.plan.environments)
            .chain(&self.plan.tasks)
            .filter(|action| **action == ImportAction::Replace)
            .count();
        let mut body = vec![
            div()
                .id("import-items")
                .max_h(px(320.))
                .overflow_y_scroll()
                .child(v_flex().gap_1().children(items))
                .into_any_element(),
        ];
        if replaced > 0 {
            body.push(
                Checkbox::new("import-replace")
                    .label(SharedString::from(format!(
                        "Replace the {} with the same id",
                        count(replaced, "item", "items")
                    )))
                    .checked(self.replace)
                    .on_click(
                        cx.listener(|this, checked: &bool, _, cx| this.set_replace(*checked, cx)),
                    )
                    .into_any_element(),
            );
        }
        let has_passwords = !self.password_rows().is_empty();
        body.push(
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("import-cancel")
                        .ghost()
                        .label("Cancel")
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
                .child(
                    Button::new("import-next")
                        .primary()
                        .label(if has_passwords {
                            "Next: Passwords"
                        } else {
                            "Import"
                        })
                        .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
                )
                .into_any_element(),
        );
        body
    }

    fn render_passwords(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (muted, warning, danger) = (theme.muted_foreground, theme.warning, theme.danger);
        let mut body = vec![
            div()
                .text_sm()
                .text_color(muted)
                .child(
                    "The file holds no password. Choose, for each connection, where its password \
                     comes from.",
                )
                .into_any_element(),
        ];
        for index in self.password_rows() {
            let row = &self.rows[index];
            let mut choices = vec![
                (Choice::EnterNow, "Enter now".to_string()),
                (Choice::Ask, "Ask when connecting".to_string()),
            ];
            if row.linked {
                choices.insert(
                    0,
                    (
                        Choice::Linked,
                        link_label(&self.file.connections[index].config),
                    ),
                );
            }
            body.push(
                v_flex()
                    .id(("import-password-row", index))
                    .test_support()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .text_sm()
                                    .child(SharedString::from(
                                        self.file.connections[index].config.name.clone(),
                                    )),
                            )
                            .children(choices.into_iter().map(|(choice, label)| {
                                let id = match choice {
                                    Choice::Linked => "import-linked",
                                    Choice::EnterNow => "import-enter",
                                    Choice::Ask => "import-ask",
                                };
                                theme::chip((id, index), row.choice == choice)
                                    .xsmall()
                                    .label(label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.choose(index, choice, cx)
                                    }))
                            })),
                    )
                    .when(row.choice == Choice::EnterNow, |this| {
                        this.child(
                            Input::new(&row.password)
                                .small()
                                .id(("import-password", index)),
                        )
                    })
                    .when(
                        row.linked
                            && row.choice != Choice::Linked
                            && self.plan.connections[index] == ImportAction::Add,
                        |this| {
                            this.child(div().text_xs().text_color(muted).child(
                                "Saved under a new id: the keychain entry stays with the \
                                 connection that has it.",
                            ))
                        },
                    )
                    .when_some(row.moved.as_ref(), |this, (before, after)| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(warning)
                                .child(SharedString::from(format!(
                                    "Its server changes ({before} → {after}): the password kept \
                                 for {before} is not used unless you link it."
                                ))),
                        )
                    })
                    .into_any_element(),
            );
        }
        if let Some(problem) = &self.problem {
            body.push(
                div()
                    .id("import-problem")
                    .test_support()
                    .text_sm()
                    .text_color(danger)
                    .child(SharedString::from(problem.clone()))
                    .into_any_element(),
            );
        }
        body.push(
            h_flex()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("import-back")
                        .ghost()
                        .label("Back")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.step = Step::Preview;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("import-confirm")
                        .primary()
                        .label("Import")
                        .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
                )
                .into_any_element(),
        );
        body
    }
}

/// `user@host:port` of a connection, and its tunnel's host and jumps.
fn server(config: &ConnectionConfig) -> String {
    // A database file (SQLite): its path.
    if config.engine.is_file() {
        return kerno_core::config::connection_url(config);
    }
    let mut text = format!("{}@{}:{}", config.user, config.host, config.port);
    if let Some(ssh) = &config.ssh {
        text.push_str(&format!(" through {}", ssh.host));
        if let Some(jump) = ssh.jump.as_deref().filter(|jump| !jump.is_empty()) {
            text.push_str(&format!(" (jump {jump})"));
        }
    }
    text
}

/// The link choice, naming where the saved secrets will go: all of them or
/// none.
fn link_label(config: &ConnectionConfig) -> String {
    let database = format!("{}@{}:{}", config.user, config.host, config.port);
    match &config.ssh {
        None => format!("Use the keychain's saved secrets (database password → {database})"),
        Some(ssh) => {
            let mut tunnel = format!("{}@{}:{}", ssh.user, ssh.host, ssh.port);
            if let Some(jump) = ssh.jump.as_deref().filter(|jump| !jump.is_empty()) {
                tunnel.push_str(&format!(" via {jump}"));
            }
            format!(
                "Use the keychain's saved secrets (database password → {database}; SSH password \
                 and key passphrase → {tunnel})"
            )
        }
    }
}

/// The first choice of the Passwords step for a connection of the file.
/// The secret store's entry under its id is linked by default only when
/// this data directory has that connection and the file keeps it on the
/// same server: ids travel in every export, so a file could otherwise
/// attach a saved password to a server it chose (a new connection has
/// nothing to compare with). Anything else starts on "Ask when
/// connecting"; linking stays an explicit choice that names the server.
fn default_choice(
    linked: bool,
    existing: Option<&ConnectionConfig>,
    imported: &ConnectionConfig,
) -> Choice {
    match existing {
        Some(existing) if linked && !reaches_another_server(existing, imported) => Choice::Linked,
        _ => Choice::Ask,
    }
}

impl Render for ImportSetup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (success, danger) = (theme.success, theme.danger);
        let body = match &self.step {
            Step::Preview => self.render_preview(cx),
            Step::Passwords => self.render_passwords(cx),
            Step::Importing => vec![div().text_sm().child("Importing…").into_any_element()],
            Step::Done(message) | Step::Failed(message) => {
                let failed = matches!(self.step, Step::Failed(_));
                vec![
                    div()
                        .id("import-done")
                        .test_support()
                        .text_sm()
                        .text_color(if failed { danger } else { success })
                        .child(SharedString::from(message.clone()))
                        .into_any_element(),
                    h_flex()
                        .justify_end()
                        .child(
                            Button::new("import-close")
                                .primary()
                                .label("Close")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .into_any_element(),
                ]
            }
        };
        v_flex()
            .id("import-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .children(body)
    }
}

#[cfg(test)]
mod tests {
    use kerno_core::Engine;

    use super::*;

    /// The four cases of the security review of J15: a saved password goes
    /// by default only to a connection this data directory has, on the
    /// same server.
    #[test]
    fn a_saved_password_is_linked_by_default_only_to_the_same_known_server() {
        let mut known = ConnectionConfig::new(Engine::Postgres);
        known.host = "db.example.com".into();
        let elsewhere = ConnectionConfig {
            host: "attacker.example".into(),
            ..known.clone()
        };
        let renamed = ConnectionConfig {
            name: "renamed".into(),
            ..known.clone()
        };
        // New here, with an entry under its id, then without one.
        assert_eq!(default_choice(true, None, &known), Choice::Ask);
        assert_eq!(default_choice(false, None, &known), Choice::Ask);
        // Replacing one on the same server, then on another.
        assert_eq!(default_choice(true, Some(&known), &renamed), Choice::Linked);
        assert_eq!(default_choice(true, Some(&known), &elsewhere), Choice::Ask);
        assert_eq!(default_choice(false, Some(&known), &renamed), Choice::Ask);
        // Through a tunnel: any change of the SSH side is another server,
        // the key file included (its passphrase would follow).
        let tunnel = kerno_core::SshConfig {
            host: "bastion".into(),
            port: 22,
            user: "ops".into(),
            auth: kerno_core::SshAuth::KeyFile {
                path: "/keys/id_ed25519".into(),
            },
            keepalive_secs: 30,
            client: kerno_core::SshClient::Builtin,
            jump: None,
        };
        let tunnelled = ConnectionConfig {
            ssh: Some(tunnel.clone()),
            ..known.clone()
        };
        assert_eq!(
            default_choice(true, Some(&tunnelled), &tunnelled),
            Choice::Linked
        );
        for changed in [
            kerno_core::SshConfig {
                host: "attacker.example".into(),
                ..tunnel.clone()
            },
            kerno_core::SshConfig {
                user: "root".into(),
                ..tunnel.clone()
            },
            kerno_core::SshConfig {
                port: 2222,
                ..tunnel.clone()
            },
            kerno_core::SshConfig {
                jump: Some("evil@jump".into()),
                ..tunnel.clone()
            },
            kerno_core::SshConfig {
                client: kerno_core::SshClient::OpenSsh,
                ..tunnel.clone()
            },
            kerno_core::SshConfig {
                auth: kerno_core::SshAuth::KeyFile {
                    path: "/tmp/other".into(),
                },
                ..tunnel.clone()
            },
        ] {
            let moved = ConnectionConfig {
                ssh: Some(changed.clone()),
                ..known.clone()
            };
            assert_eq!(
                default_choice(true, Some(&tunnelled), &moved),
                Choice::Ask,
                "{changed:?}"
            );
        }
        // The link names every destination.
        let label = link_label(&ConnectionConfig {
            ssh: Some(kerno_core::SshConfig {
                jump: Some("j@jump".into()),
                ..tunnel
            }),
            ..known.clone()
        });
        assert!(
            label.contains("database password → postgres@db.example.com:5432")
                && label.contains("key passphrase → ops@bastion:22 via j@jump"),
            "{label}"
        );
    }
}
