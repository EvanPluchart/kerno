//! Saved tasks (DBeaver's Database Tasks view, `DatabaseTasksView`): the
//! tool wizards' settings kept under a name, run again on demand. No
//! scheduler, as in DBeaver Community.
//!
//! A task holds no secret: an authentication override keeps its user, and
//! its password is asked at each run. Its connection is looked up by id at
//! each run, with the rules it has then (read-only, environment); the
//! client tool is chosen as for any run of the wizard (a folder that is
//! not a detected or added local client never runs); a restore or a script
//! shows its command and asks before each run; the log kept with the task
//! is the wizard's, its passwords masked.

use std::rc::Rc;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _,
    WeakEntity, Window, div, px, uniform_list,
};
use kerno_core::storage::{HistoryStatus, SavedTask, now_ms};
use kerno_core::tools::TaskSettings;
use kerno_core::{Engine, TxState};

use crate::backup_dialog::{BackupDialog, Mode, Purpose};
use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::library::ago;
use crate::query::tab::TabStatus;
use crate::state::AppState;
use crate::tabs::Target;

/// Opens a tool's wizard on a target, as the window does (the tree is
/// read again after a restore).
pub type OpenWizard = dyn Fn(Target, Mode, &mut Window, &mut App) -> Option<Entity<BackupDialog>>;

/// What can be done to a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskAction {
    Run,
    Edit,
    Duplicate,
    Rename,
    Delete,
    ShowLog,
}

impl TaskAction {
    pub const ALL: [TaskAction; 6] = [
        Self::Run,
        Self::Edit,
        Self::Duplicate,
        Self::Rename,
        Self::Delete,
        Self::ShowLog,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Run => "Run",
            Self::Edit => "Edit…",
            Self::Duplicate => "Duplicate",
            Self::Rename => "Rename…",
            Self::Delete => "Delete…",
            Self::ShowLog => "Show log",
        }
    }
}

/// The actions of `task` with why each cannot be used, if it cannot:
/// `connected_to` is the name of its connection, `None` once deleted.
pub fn task_menu(
    task: &SavedTask,
    connected_to: Option<&str>,
) -> Vec<(TaskAction, Option<&'static str>)> {
    TaskAction::ALL
        .into_iter()
        .map(|action| {
            let reason = match action {
                TaskAction::Run | TaskAction::Edit if connected_to.is_none() => {
                    Some("its connection was deleted")
                }
                TaskAction::ShowLog if task.last_run_at_ms.is_none() => Some("never run"),
                _ => None,
            };
            (action, reason)
        })
        .collect()
}

/// The wizard of a task's kind.
pub fn mode_of(settings: &TaskSettings) -> Mode {
    match settings {
        TaskSettings::Backup(_) => Mode::Backup,
        TaskSettings::GlobalBackup(_) => Mode::GlobalBackup,
        TaskSettings::Restore(_) => Mode::Restore,
        TaskSettings::ExecuteScript(_) => Mode::ExecuteScript,
    }
}

/// The tools a task can be made of on `engine`, with their names.
pub fn task_tools(engine: Engine) -> Vec<(Mode, &'static str)> {
    let mut tools = vec![(Mode::Backup, "Backup")];
    if engine == Engine::Postgres {
        tools.push((Mode::GlobalBackup, "Global backup"));
    }
    tools.extend([
        (Mode::Restore, "Restore"),
        (Mode::ExecuteScript, "Execute script"),
    ]);
    tools
}

/// How a task's last run ended, in words.
fn status_text(task: &SavedTask) -> &'static str {
    match task.last_status {
        None => "never run",
        Some(HistoryStatus::Ok) => "succeeded",
        Some(HistoryStatus::Error) => "failed",
        Some(HistoryStatus::Cancelled) => "cancelled",
    }
}

pub struct TasksView {
    connections: Entity<Connections>,
    open_wizard: Rc<OpenWizard>,
    tasks: Vec<SavedTask>,
    loaded: bool,
    error: Option<String>,
    selected: Option<i64>,
    focus_handle: FocusHandle,
    /// A run or an edit waiting for its connection to open, or for its
    /// wizard to be ready to start.
    waiting: Option<Subscription>,
    /// How many times the list was read; for tests.
    reads: usize,
    /// The wizard opened last.
    opened: Option<WeakEntity<BackupDialog>>,
    loading: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl TasksView {
    pub fn new(
        connections: Entity<Connections>,
        open_wizard: Rc<OpenWizard>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![cx.observe(&connections, |_, _, cx| cx.notify())];
        let mut view = Self {
            connections,
            open_wizard,
            tasks: Vec::new(),
            loaded: false,
            error: None,
            selected: None,
            focus_handle: cx.focus_handle(),
            waiting: None,
            reads: 0,
            opened: None,
            loading: None,
            _subscriptions: subscriptions,
        };
        view.reload(cx);
        view
    }

    /// Reads the tasks again: when the view opens, when a task was saved,
    /// ran or was deleted, on Refresh. Never from a render (the window
    /// renders the active tab at every frame); it redraws only when what it
    /// read differs.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.reads += 1;
        let storage = AppState::get(cx).storage.clone();
        let reading = bridge::spawn_blocking(cx, move || storage.tasks());
        self.loading = Some(cx.spawn(async move |this, cx| {
            let read = reading.await;
            let _ = this.update(cx, |this, cx| {
                let (tasks, error) = match read {
                    Ok(tasks) => (tasks, None),
                    Err(error) => (this.tasks.clone(), Some(error.to_string())),
                };
                if this.loaded && tasks == this.tasks && error == this.error {
                    return;
                }
                this.tasks = tasks;
                this.error = error;
                this.loaded = true;
                cx.notify();
            });
        }));
    }

    pub fn title(&self) -> SharedString {
        "Tasks".into()
    }

    pub fn status(&self, _: &App) -> TabStatus {
        TabStatus {
            connection: None,
            read_only: false,
            database: None,
            transaction: TxState::Idle,
            running: false,
            rows: Some(self.tasks.len()),
            elapsed: None,
            notices: 0,
            pending: 0,
        }
    }

    fn task(&self, id: i64) -> Option<&SavedTask> {
        self.tasks.iter().find(|task| task.id == id)
    }

    /// The name of a task's connection, while it is saved.
    fn connection_name(&self, task: &SavedTask, cx: &App) -> Option<String> {
        self.connections
            .read(cx)
            .get(&task.connection_id)
            .map(|saved| saved.config.name.clone())
    }

    /// The actions of the task `id` with why each cannot be used now.
    pub fn menu_of(&self, id: i64, cx: &App) -> Vec<(TaskAction, Option<&'static str>)> {
        match self.task(id) {
            Some(task) => task_menu(task, self.connection_name(task, cx).as_deref()),
            None => Vec::new(),
        }
    }

    /// Does what an action says to the task `id`, or says why it cannot.
    pub fn perform(
        &mut self,
        action: TaskAction,
        id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.task(id).cloned() else {
            return;
        };
        self.selected = Some(id);
        if let Some((_, Some(reason))) = self
            .menu_of(id, cx)
            .into_iter()
            .find(|(listed, _)| *listed == action)
        {
            window.push_notification(
                Notification::warning(SharedString::from(format!(
                    "{} “{}”: {reason}.",
                    action.label().trim_end_matches('…'),
                    task.name
                ))),
                cx,
            );
            return;
        }
        match action {
            TaskAction::Run | TaskAction::Edit => {
                let edit = action == TaskAction::Edit;
                let connection = task.connection_id.clone();
                self.when_connected(connection, window, cx, move |this, window, cx| {
                    this.open_task(task, edit, window, cx)
                });
            }
            TaskAction::Duplicate => {
                let copy = SavedTask {
                    id: 0,
                    name: format!("{} (copy)", task.name),
                    created_at_ms: now_ms(),
                    last_run_at_ms: None,
                    last_status: None,
                    last_log: String::new(),
                    ..task
                };
                self.store(copy, cx);
            }
            TaskAction::Rename => {
                let view = cx.entity();
                ask_task_name(
                    "Rename task",
                    task.name.clone(),
                    task.description.clone(),
                    window,
                    cx,
                    move |name, description, _, cx| {
                        let renamed = SavedTask {
                            name,
                            description,
                            ..task.clone()
                        };
                        view.update(cx, |this, cx| this.store(renamed, cx));
                    },
                );
            }
            TaskAction::Delete => self.confirm_delete(task, window, cx),
            TaskAction::ShowLog => show_log(&task, window, cx),
        }
    }

    /// Runs `then` once the connection `id` is open, opening it first when
    /// it is closed (DBeaver connects before running a task).
    fn when_connected(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if matches!(
            self.connections.read(cx).status(&id),
            Status::Connected { .. }
        ) {
            then(self, window, cx);
            return;
        }
        self.connections
            .update(cx, |connections, cx| connections.connect(&id, cx));
        let mut then = Some(then);
        self.waiting = Some(cx.observe_in(
            &self.connections.clone(),
            window,
            move |this, connections, window, cx| match connections.read(cx).status(&id) {
                Status::Connected { .. } => {
                    this.waiting = None;
                    if let Some(then) = then.take() {
                        then(this, window, cx);
                    }
                }
                Status::Failed { message } => {
                    this.waiting = None;
                    window.push_notification(
                        Notification::error(SharedString::from(format!(
                            "The task's connection did not open: {message}"
                        ))),
                        cx,
                    );
                }
                _ => {}
            },
        ));
    }

    /// Opens the task's wizard on its settings: to edit them, or to run it
    /// as soon as the wizard is ready. A task with an authentication
    /// override waits for its password, typed for this run.
    fn open_task(
        &mut self,
        task: SavedTask,
        edit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = Target {
            connection: task.connection_id.clone(),
            database: task.settings.database().map(str::to_string),
            schema: None,
        };
        let Some(dialog) = (self.open_wizard)(target, mode_of(&task.settings), window, cx) else {
            return;
        };
        self.opened = Some(dialog.downgrade());
        let view = cx.entity().downgrade();
        let asks_password = task.settings.auth_user().map(str::to_string);
        dialog.update(cx, |dialog, cx| {
            dialog.apply(task.settings.clone(), window, cx);
            if !edit {
                dialog.show_last_page(cx);
            }
            dialog.set_purpose(
                Purpose::Task(Box::new(task)),
                move |cx| {
                    let _ = view.update(cx, |this, cx| this.reload(cx));
                },
                cx,
            );
        });
        if edit {
            return;
        }
        if let Some(user) = asks_password {
            window.push_notification(
                Notification::info(SharedString::from(format!(
                    "Type the password of “{user}” for this run, then start it."
                ))),
                cx,
            );
            return;
        }
        let mut started = false;
        self.waiting = Some(
            cx.observe_in(&dialog, window, move |this, dialog, window, cx| {
                if !started && dialog.read(cx).ready_to_start(cx) {
                    started = true;
                    this.waiting = None;
                    dialog.update(cx, |dialog, cx| dialog.start(window, cx));
                }
            }),
        );
    }

    /// Writes `task` (a new one when its id is 0) and reads the list again.
    fn store(&mut self, task: SavedTask, cx: &mut Context<Self>) {
        let storage = AppState::get(cx).storage.clone();
        let saving = bridge::spawn_blocking(cx, move || storage.save_task(&task));
        cx.spawn(async move |this, cx| {
            let saved = saving.await;
            let _ = this.update(cx, |this, cx| match saved {
                Ok(id) => {
                    this.selected = Some(id);
                    this.reload(cx);
                }
                Err(error) => {
                    this.error = Some(error.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn confirm_delete(&mut self, task: SavedTask, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let text = format!(
            "Delete the task “{}”? Its settings and its last run are removed; the files it \
             wrote stay.",
            task.name
        );
        window.open_dialog(cx, move |dialog, _, _| {
            let (view, text) = (view.clone(), text.clone());
            let id = task.id;
            dialog
                .title("Delete task")
                .w(px(440.))
                .on_ok(|_, _, _| false)
                .content(move |content, _, _| {
                    let view = view.clone();
                    content.child(
                        v_flex()
                            .gap_3()
                            .child(div().text_sm().child(SharedString::from(text.clone())))
                            .child(
                                h_flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        Button::new("task-delete-cancel")
                                            .ghost()
                                            .label("Cancel")
                                            .on_click(|_, window, cx| window.close_dialog(cx)),
                                    )
                                    .child(
                                        Button::new("task-delete-confirm")
                                            .danger()
                                            .label("Delete")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                view.update(cx, |this, cx| this.delete(id, cx));
                                            }),
                                    ),
                            ),
                    )
                })
        });
    }

    fn delete(&mut self, id: i64, cx: &mut Context<Self>) {
        let storage = AppState::get(cx).storage.clone();
        let deleting = bridge::spawn_blocking(cx, move || storage.delete_task(id));
        cx.spawn(async move |this, cx| {
            let deleted = deleting.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = deleted {
                    this.error = Some(error.to_string());
                }
                if this.selected == Some(id) {
                    this.selected = None;
                }
                this.reload(cx);
            });
        })
        .detach();
    }

    /// The "New task" menu: each saved connection and the tools a task
    /// can be made of on its engine.
    fn new_task_menu(
        view: Entity<Self>,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut App,
    ) -> PopupMenu {
        let saved: Vec<(String, String, Engine)> = view
            .read(cx)
            .connections
            .read(cx)
            .saved()
            .iter()
            .map(|saved| {
                (
                    saved.id.clone(),
                    saved.config.name.clone(),
                    saved.config.engine,
                )
            })
            .collect();
        if saved.is_empty() {
            return menu.item(PopupMenuItem::new("No connection saved").disabled(true));
        }
        let view = view.downgrade();
        for (id, name, engine) in saved {
            let view = view.clone();
            let submenu = PopupMenu::build(window, cx, move |mut submenu, _, _| {
                for (mode, label) in task_tools(engine) {
                    let (view, id) = (view.clone(), id.clone());
                    submenu = submenu.item(PopupMenuItem::new(format!("{label}…")).on_click(
                        move |_, window, cx| {
                            let id = id.clone();
                            let _ = view.update(cx, |this, cx| this.new_task(id, mode, window, cx));
                        },
                    ));
                }
                submenu
            });
            menu = menu.item(PopupMenuItem::submenu(SharedString::from(name), submenu));
        }
        menu
    }

    /// Opens the wizard of `mode` on the connection `id` (its default
    /// database) for a new task.
    pub fn new_task(
        &mut self,
        id: String,
        mode: Mode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        self.when_connected(id.clone(), window, cx, move |this, window, cx| {
            let target = Target {
                connection: id,
                database: None,
                schema: None,
            };
            if let Some(dialog) = (this.open_wizard)(target, mode, window, cx) {
                dialog.update(cx, |dialog, cx| {
                    dialog.set_purpose(
                        Purpose::NewTask,
                        move |cx| {
                            let _ = view.update(cx, |this, cx| this.reload(cx));
                        },
                        cx,
                    )
                });
            }
        });
    }

    /// How many times the list was read; for tests.
    #[cfg(test)]
    pub fn reads(&self) -> usize {
        self.reads
    }

    /// The wizard opened last, while it is open; for tests.
    #[cfg(test)]
    pub fn opened(&self) -> Option<Entity<BackupDialog>> {
        self.opened.as_ref()?.upgrade()
    }

    /// The tasks listed, by name; for tests.
    #[cfg(test)]
    pub fn listed(&self) -> Vec<SavedTask> {
        self.tasks.clone()
    }

    fn render_row(&self, index: usize, task: &SavedTask, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let connection = self.connection_name(task, cx);
        let selected = self.selected == Some(task.id);
        let id = task.id;
        let view = cx.entity();
        let last_run = task
            .last_run_at_ms
            .map(|at| ago(now_ms(), at))
            .unwrap_or_else(|| "—".into());
        let status = status_text(task);
        let status_color = match task.last_status {
            Some(HistoryStatus::Ok) => theme.success,
            Some(HistoryStatus::Error) => theme.danger,
            _ => theme.muted_foreground,
        };
        let cell = |width: f32, text: String| {
            div().w(px(width)).flex_none().min_w_0().child(
                div()
                    .truncate()
                    .child(SharedString::from(text.clone()))
                    .test_text(text),
            )
        };
        h_flex()
            .id(("task-row", index))
            .test_support()
            .w_full()
            .gap_3()
            .px_3()
            .py_1p5()
            .text_sm()
            .border_b_1()
            .border_color(theme.border)
            .when(selected, |this| this.bg(theme.list_active))
            .hover(|this| this.bg(theme.list_hover))
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    this.selected = Some(id);
                    if event.click_count() == 2 {
                        this.perform(TaskAction::Run, id, window, cx);
                    }
                    cx.notify();
                }),
            )
            .context_menu(move |mut menu, _, cx| {
                for (action, reason) in view.read(cx).menu_of(id, cx) {
                    let view = view.clone();
                    let label = match reason {
                        Some(reason) => format!("{} ({reason})", action.label()),
                        None => action.label().to_string(),
                    };
                    menu = menu.item(
                        PopupMenuItem::new(SharedString::from(label))
                            .disabled(reason.is_some())
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| this.perform(action, id, window, cx))
                            }),
                    );
                }
                menu
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .child(SharedString::from(task.name.clone()))
                            .test_text(task.name.clone()),
                    )
                    .when(!task.description.is_empty(), |this| {
                        this.child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(task.description.clone())),
                        )
                    }),
            )
            .child(cell(110., task.settings.label().to_string()))
            .child(
                cell(
                    160.,
                    connection
                        .clone()
                        .unwrap_or_else(|| "deleted connection".into()),
                )
                .when(connection.is_none(), |this| this.text_color(theme.danger)),
            )
            .child(cell(100., last_run))
            .child(cell(90., status.to_string()).text_color(status_color))
            .into_any_element()
    }

    fn render_toolbar(&self, cx: &Context<Self>) -> AnyElement {
        let view = cx.entity();
        let selected = self.selected.filter(|id| self.task(*id).is_some());
        let menu = selected.map(|id| self.menu_of(id, cx)).unwrap_or_default();
        let mut bar = h_flex().gap_2().px_3().py_2().child(
            Button::new("tasks-new")
                .small()
                .primary()
                .label("New task")
                .dropdown_caret(true)
                .dropdown_menu({
                    let view = view.clone();
                    move |menu, window, cx| Self::new_task_menu(view.clone(), menu, window, cx)
                }),
        );
        for (index, (action, reason)) in menu.into_iter().enumerate() {
            let id = selected.unwrap_or_default();
            bar =
                bar.child(
                    Button::new(("tasks-action", index))
                        .small()
                        .outline()
                        .label(action.label())
                        .disabled(reason.is_some())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.perform(action, id, window, cx)
                        })),
                );
        }
        bar.child(div().flex_1())
            .child(
                Button::new("tasks-refresh")
                    .small()
                    .ghost()
                    .label("Refresh")
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
            .into_any_element()
    }
}

impl Focusable for TasksView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TasksView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let body: AnyElement = if let Some(error) = &self.error {
            div()
                .id("tasks-error")
                .test_support()
                .p_4()
                .text_sm()
                .text_color(theme.danger)
                .child(SharedString::from(error.clone()))
                .into_any_element()
        } else if self.loaded && self.tasks.is_empty() {
            v_flex()
                .id("tasks-empty")
                .test_support()
                .p_6()
                .gap_2()
                .text_sm()
                .child("No saved task yet.")
                .child(div().text_color(theme.muted_foreground).child(
                    "A task keeps a tool's settings (backup, restore, script) to run them \
                             again: use “Save as task…” on a wizard's last step, Tools ▸ Create \
                             new task… in the tree, or New task above. Tasks run on demand: \
                             there is no scheduler.",
                ))
                .into_any_element()
        } else {
            let header = h_flex()
                .gap_3()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .border_b_1()
                .border_color(theme.border)
                .child(div().flex_1().child("Task"))
                .child(div().w(px(110.)).flex_none().child("Tool"))
                .child(div().w(px(160.)).flex_none().child("Connection"))
                .child(div().w(px(100.)).flex_none().child("Last run"))
                .child(div().w(px(90.)).flex_none().child("Status"));
            let rows: Vec<AnyElement> = self
                .tasks
                .iter()
                .enumerate()
                .map(|(index, task)| self.render_row(index, task, cx))
                .collect();
            v_flex()
                .id("tasks-list")
                .test_support()
                .flex_1()
                .overflow_y_scroll()
                .child(header)
                .children(rows)
                .into_any_element()
        };
        v_flex()
            .id("tasks-view")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(self.render_toolbar(cx))
            .child(body)
    }
}

/// Asks which tool a new task is made of, then calls `pick` with it
/// (DBeaver's "Create new task": the task type first, then its wizard).
pub fn pick_tool(
    engine: Engine,
    window: &mut Window,
    cx: &mut App,
    pick: impl Fn(Mode, &mut Window, &mut App) + 'static,
) {
    let pick = Rc::new(pick);
    window.open_dialog(cx, move |dialog, _, _| {
        let pick = pick.clone();
        dialog
            .title("Create new task")
            .w(px(420.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, cx| {
                let pick = pick.clone();
                content.child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "Choose the tool: its wizard opens, and “Save as task…” on \
                                     its last step keeps the settings under a name.",
                                ),
                        )
                        .children(task_tools(engine).into_iter().enumerate().map(
                            move |(index, (mode, label))| {
                                let pick = pick.clone();
                                Button::new(("task-tool", index))
                                    .outline()
                                    .label(format!("{label}…"))
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        pick(mode, window, cx);
                                    })
                            },
                        )),
                )
            })
    });
}

/// Asks a task's name and description, then calls `save`.
pub fn ask_task_name(
    title: &'static str,
    name: String,
    description: String,
    window: &mut Window,
    cx: &mut App,
    save: impl Fn(String, String, &mut Window, &mut App) + 'static,
) {
    let name = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Nightly backup")
            .default_value(name)
    });
    let description = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Description (optional)")
            .default_value(description)
    });
    let save = Rc::new(save);
    let focus = name.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let submit = {
            let (name, description, save) = (name.clone(), description.clone(), save.clone());
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let typed = name.read(cx).value().trim().to_string();
                if typed.is_empty() {
                    return false;
                }
                let description = description.read(cx).value().trim().to_string();
                save(typed, description, window, cx);
                true
            })
        };
        let on_enter = submit.clone();
        let (name, description) = (name.clone(), description.clone());
        dialog
            .title(title)
            .w(px(460.))
            .on_ok(move |_, window, cx| on_enter(window, cx))
            .content(move |content, _, _| {
                let submit = submit.clone();
                content.child(
                    v_flex()
                        .gap_3()
                        .pt_1()
                        .child(Input::new(&name).id("task-name"))
                        .child(Input::new(&description).id("task-description"))
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("task-name-cancel")
                                        .ghost()
                                        .label("Cancel")
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("task-name-ok")
                                        .primary()
                                        .label("Save")
                                        .on_click(move |_, window, cx| {
                                            if submit(window, cx) {
                                                window.close_dialog(cx);
                                            }
                                        }),
                                ),
                        ),
                )
            })
    });
    window.defer(cx, move |window, cx| {
        focus.read(cx).focus_handle(cx).focus(window, cx)
    });
}

/// Shows the log of a task's last run.
fn show_log(task: &SavedTask, window: &mut Window, cx: &mut App) {
    let lines: Rc<Vec<SharedString>> = Rc::new(
        task.last_log
            .lines()
            .map(|line| SharedString::from(line.to_string()))
            .collect(),
    );
    let title = SharedString::from(format!(
        "Last run of “{}” · {}",
        task.name,
        status_text(task)
    ));
    window.open_dialog(cx, move |dialog, _, _| {
        let lines = lines.clone();
        dialog
            .title(title.clone())
            .w(px(760.))
            .content(move |content, _, cx| {
                let lines = lines.clone();
                let count = lines.len();
                let mono = cx.theme().mono_font_family.clone();
                content.child(
                    div()
                        .id("task-log")
                        .test_support()
                        .h(px(360.))
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted)
                        .p_2()
                        .child(
                            uniform_list("task-log-lines", count, move |range, _, _| {
                                range
                                    .map(|index| {
                                        div()
                                            .font_family(mono.clone())
                                            .text_xs()
                                            .whitespace_nowrap()
                                            .child(lines[index].clone())
                                    })
                                    .collect()
                            })
                            .size_full(),
                        ),
                )
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::tools::{BackupOptions, ScriptOptions};

    #[test]
    fn a_task_says_what_it_cannot_do() {
        let backup = TaskSettings::Backup(BackupOptions::new(Engine::Postgres, "shop"));
        let mut task = SavedTask::new("nightly", "c1", backup);
        let labels = |menu: Vec<(TaskAction, Option<&'static str>)>| {
            menu.into_iter()
                .map(|(action, reason)| match reason {
                    Some(reason) => format!("{} ({reason})", action.label()),
                    None => action.label().to_string(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(task_menu(&task, Some("Shop"))),
            [
                "Run",
                "Edit…",
                "Duplicate",
                "Rename…",
                "Delete…",
                "Show log (never run)"
            ]
        );
        task.last_run_at_ms = Some(1);
        assert_eq!(
            labels(task_menu(&task, None)),
            [
                "Run (its connection was deleted)",
                "Edit… (its connection was deleted)",
                "Duplicate",
                "Rename…",
                "Delete…",
                "Show log"
            ]
        );
        let script = TaskSettings::ExecuteScript(ScriptOptions::new(Engine::MySql, "shop", "/s"));
        assert_eq!(mode_of(&script), Mode::ExecuteScript);
        let names = |engine| {
            task_tools(engine)
                .into_iter()
                .map(|(_, name)| name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(Engine::Postgres),
            ["Backup", "Global backup", "Restore", "Execute script"]
        );
        assert_eq!(
            names(Engine::MariaDb),
            ["Backup", "Restore", "Execute script"]
        );
    }
}
