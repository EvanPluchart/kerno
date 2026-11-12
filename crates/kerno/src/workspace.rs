//! The main window: title bar, sidebar, tabs and status bar, and the
//! commands that act on them.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, TitleBar, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, BorrowAppContext as _, Context, Entity, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions,
    Render, SharedString, Styled as _, Subscription, Task, Window, div, px,
};
use kerno_core::config::{ConnectionConfig, Secret, export_connections, import_connections};

use crate::actions::{
    self, About, CancelQuery, CloseTab, ExplainAnalyze, ExplainStatement, ExportConnections,
    ImportConnections, NewConnection, NewQueryTab, NextTab, OpenCommandPalette, OpenSettings,
    OpenSqlFile, PreviousTab, RunAll, RunStatement, SaveFile, SaveFileAs, SaveQuery, ShowHistory,
    ShowSavedQueries, ToggleSidebar, UseDarkTheme, UseLightTheme, UseSystemTheme,
};
use crate::bridge;
use crate::connections::dialog::ConnectionDialog;
use crate::connections::manager::{
    Connections, ConnectionsEvent, InputAnswer, InputRequest, Status,
};
use crate::library::{self, Library, Shelf};
use crate::navigator::view::{Navigator, NavigatorEvent};
use crate::query::tab::{QueryTab, Scope, TabStatus, duration_text};
use crate::sidebar::Sidebar;
use crate::state::{ACTIVE_TAB_KEY, AppState};
use crate::tabs::{TabSnapshot, Target};
use crate::theme::{self, Appearance};
use kerno_core::TxState;
use kerno_core::storage::SavedQuery;

/// How long after the last change the open tabs are written to storage.
const SAVE_TABS_DELAY: Duration = Duration::from_millis(400);

/// The root view of a Kerno window.
pub struct Workspace {
    focus_handle: FocusHandle,
    connections: Entity<Connections>,
    sidebar: Entity<Sidebar>,
    tabs: Vec<Entity<QueryTab>>,
    active_tab: usize,
    /// Number given to the next "Query N" tab.
    next_query_number: usize,
    palette: Entity<CommandState>,
    pending_save: Option<Task<()>>,
    tab_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    /// Creates the workspace with the saved connections and the tabs that
    /// were open last time.
    pub fn new(
        connections: Entity<Connections>,
        tabs: Vec<TabSnapshot>,
        active_tab: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let palette = cx.new(|cx| CommandState::new(window, cx));
        let follow_system = cx.observe_window_appearance(window, |_, window, cx| {
            if AppState::get(cx).appearance() == Appearance::System {
                theme::apply(Appearance::System, Some(window), cx);
            }
        });
        // Text typed in the last moments before quitting is not lost.
        let flush_on_quit = cx.on_app_quit(|this, cx| this.save_tabs(cx));
        let sidebar = cx.new(|cx| Sidebar::new(connections.clone(), window, cx));
        let navigator = sidebar.read(cx).navigator().clone();
        let navigator_events = cx.subscribe_in(&navigator, window, Self::on_navigator_event);
        let connection_events = cx.subscribe_in(&connections, window, Self::on_connections_event);

        let mut workspace = Self {
            focus_handle: cx.focus_handle(),
            connections,
            sidebar,
            tabs: Vec::new(),
            active_tab: 0,
            next_query_number: 1,
            palette,
            pending_save: None,
            tab_subscriptions: Vec::new(),
            _subscriptions: vec![
                follow_system,
                flush_on_quit,
                connection_events,
                navigator_events,
            ],
        };
        for snapshot in tabs {
            match snapshot {
                TabSnapshot::Query {
                    title,
                    sql,
                    target,
                    file,
                    unsaved,
                } => {
                    let tab = workspace.push_query_tab(title, &sql, target, window, cx);
                    if let Some(file) = file {
                        tab.update(cx, |tab, cx| tab.set_file(file, unsaved, cx));
                    }
                }
            }
        }
        // The tab that was active is active again.
        workspace.active_tab = active_tab.min(workspace.tabs.len().saturating_sub(1));
        workspace
    }

    /// Titles of the open tabs, in order.
    #[cfg(test)]
    pub fn tab_titles(&self, cx: &App) -> Vec<String> {
        self.tabs
            .iter()
            .map(|tab| tab.read(cx).title().to_string())
            .collect()
    }

    /// Index of the active tab, when there is one.
    #[cfg(test)]
    pub fn active_tab_index(&self) -> Option<usize> {
        (!self.tabs.is_empty()).then_some(self.active_tab)
    }

    /// The active query tab.
    #[cfg(test)]
    pub fn active_query_tab(&self) -> Option<&Entity<QueryTab>> {
        self.tabs.get(self.active_tab)
    }

    fn push_query_tab(
        &mut self,
        title: String,
        sql: &str,
        target: Option<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<QueryTab> {
        // Keep "Query N" numbers unique across restored and new tabs.
        if let Some(number) = title
            .strip_prefix("Query ")
            .and_then(|n| n.parse::<usize>().ok())
        {
            self.next_query_number = self.next_query_number.max(number + 1);
        }
        let connections = self.connections.clone();
        let tab = cx.new(|cx| QueryTab::new(title, sql, target, connections, window, cx));
        let editor = tab.read(cx).editor().clone();
        self.tab_subscriptions
            .push(cx.observe(&editor, |this, _, cx| this.schedule_save(cx)));
        self.tabs.push(tab.clone());
        self.active_tab = self.tabs.len() - 1;
        tab
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(index) {
            self.active_tab = index;
            tab.focus_handle(cx).focus(window, cx);
            self.schedule_save(cx);
            cx.notify();
        }
    }

    fn close_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        drop(self.tab_subscriptions.remove(index));
        if self.active_tab > index || self.active_tab >= self.tabs.len() {
            self.active_tab = self.active_tab.saturating_sub(1);
        }
        match self.tabs.get(self.active_tab) {
            Some(tab) => tab.focus_handle(cx).focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Writes the open tabs to storage shortly after the last change.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.pending_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_TABS_DELAY).await;
            if let Ok(save) = this.update(cx, |this, cx| this.save_tabs(cx)) {
                save.await;
            }
        }));
    }

    /// Writes the open tabs to storage now.
    fn save_tabs(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let snapshots: Vec<TabSnapshot> = self
            .tabs
            .iter()
            .map(|tab| tab.read(cx).snapshot(cx))
            .collect();
        let active_tab = self.active_tab;
        let storage = AppState::get(cx).storage.clone();
        bridge::spawn_blocking(cx, move || {
            let saved = storage
                .save_tabs(&snapshots)
                .and_then(|()| storage.set_setting(ACTIVE_TAB_KEY, &active_tab));
            if let Err(error) = saved {
                tracing::error!(%error, "could not store the open tabs");
            }
        })
    }

    // Commands ----------------------------------------------------------------

    fn new_query_tab(&mut self, _: &NewQueryTab, window: &mut Window, cx: &mut Context<Self>) {
        let title = format!("Query {}", self.next_query_number);
        let target = self.default_target(cx);
        self.push_query_tab(title, "", target, window, cx);
        self.activate_tab(self.tabs.len() - 1, window, cx);
        self.schedule_save(cx);
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(self.active_tab, window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.activate_tab((self.active_tab + 1) % self.tabs.len(), window, cx);
        }
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            let count = self.tabs.len();
            self.activate_tab((self.active_tab + count - 1) % count, window, cx);
        }
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        AppState::update_settings(cx, |settings| {
            settings.sidebar_visible = !settings.sidebar_visible
        });
        cx.notify();
    }

    fn set_appearance(appearance: Appearance, window: &mut Window, cx: &mut App) {
        // An explicit choice ends a theme pinned for this run.
        cx.update_global::<AppState, _>(|state, _| state.appearance_override = None);
        AppState::update_settings(cx, |settings| settings.appearance = appearance);
        theme::apply(appearance, Some(window), cx);
    }

    fn use_system_theme(
        &mut self,
        _: &UseSystemTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Self::set_appearance(Appearance::System, window, cx);
    }

    fn use_light_theme(&mut self, _: &UseLightTheme, window: &mut Window, cx: &mut Context<Self>) {
        Self::set_appearance(Appearance::Light, window, cx);
    }

    fn use_dark_theme(&mut self, _: &UseDarkTheme, window: &mut Window, cx: &mut Context<Self>) {
        Self::set_appearance(Appearance::Dark, window, cx);
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, |dialog, _, _| {
            dialog
                .title("Settings")
                .w(px(440.))
                .content(|content, _, cx| {
                    let current = AppState::get(cx).appearance();
                    let selected = Appearance::ALL.iter().position(|a| *a == current);
                    content.child(
                        v_flex()
                            .gap_2()
                            .child(div().font_weight(FontWeight::MEDIUM).child("Appearance"))
                            .child(
                                RadioGroup::new("appearance")
                                    .children(Appearance::ALL.map(Appearance::label))
                                    .selected_index(selected)
                                    .on_change(|index, window, cx| {
                                        if let Some(appearance) = Appearance::ALL.get(*index) {
                                            Workspace::set_appearance(*appearance, window, cx);
                                        }
                                    }),
                            ),
                    )
                })
        });
    }

    fn about(&mut self, _: &About, window: &mut Window, cx: &mut Context<Self>) {
        window.open_alert_dialog(cx, |alert, _, _| {
            alert.title("Kerno").description(format!(
                "Version {}\n\nA fast, native database client.\nLicensed under MIT or Apache-2.0, at your option.",
                env!("CARGO_PKG_VERSION")
            ))
        });
    }

    fn open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette
            .update(cx, |palette, cx| palette.set_query("", window, cx));
        let palette = self.palette.clone();
        // Commands are handled by the workspace, which the dialog is not a
        // child of: on confirm the action is dispatched from here.
        let target = self.focus_handle.clone();
        let focused = std::rc::Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, _, _| {
            let palette = palette.clone();
            let target = target.clone();
            let focused = focused.clone();
            dialog
                .close_button(false)
                .p_0()
                .content(move |content, window, cx| {
                    if !focused.replace(true) {
                        let palette = palette.clone();
                        window.defer(cx, move |window, cx| {
                            palette.read(cx).focus_handle(cx).focus(window, cx)
                        });
                    }
                    let target = target.clone();
                    let mut command = Command::new(&palette)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder("Type a command")
                        .max_h(px(420.))
                        .on_confirm(move |index, window, cx| {
                            window.close_dialog(cx);
                            if let Some(command) = palette_command(index.section, index.row) {
                                let action = (command.action)();
                                let target = target.clone();
                                window.defer(cx, move |window, cx| {
                                    target.focus(window, cx);
                                    target.dispatch_action(action.as_ref(), window, cx);
                                });
                            }
                        });
                    for (group, commands) in palette_groups() {
                        command = command.group(CommandGroup::new().label(group).items(
                            commands.into_iter().map(|command| {
                                CommandItem::new()
                                    .label(command.label)
                                    .keywords(command.keywords.iter().copied())
                            }),
                        ));
                    }
                    content.child(command)
                })
        });
    }

    fn on_navigator_event(
        &mut self,
        _: &Entity<Navigator>,
        event: &NavigatorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            NavigatorEvent::OpenQuery { title, sql, target } => {
                self.push_query_tab(title.clone(), sql, target.clone(), window, cx);
                self.activate_tab(self.tabs.len() - 1, window, cx);
                self.schedule_save(cx);
            }
        }
    }

    /// The schema tree of the sidebar.
    #[cfg(test)]
    pub fn navigator(&self, cx: &App) -> Entity<Navigator> {
        self.sidebar.read(cx).navigator().clone()
    }

    // Files and the library -----------------------------------------------------

    /// The connection new tabs run on: the one being browsed, when open.
    fn default_target(&self, cx: &App) -> Option<Target> {
        self.sidebar
            .read(cx)
            .navigator()
            .read(cx)
            .connection()
            .filter(|id| {
                matches!(
                    self.connections.read(cx).status(id),
                    Status::Connected { .. }
                )
            })
            .map(|id| Target {
                connection: id.to_string(),
                database: None,
                schema: None,
            })
    }

    /// Opens a tab holding `sql` and gives it the keyboard.
    fn open_query(
        &mut self,
        title: String,
        sql: &str,
        target: Option<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<QueryTab> {
        let tab = self.push_query_tab(title, sql, target, window, cx);
        self.activate_tab(self.tabs.len() - 1, window, cx);
        self.schedule_save(cx);
        tab
    }

    fn open_sql_file(&mut self, _: &OpenSqlFile, window: &mut Window, cx: &mut Context<Self>) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picking.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.open_sql_file_from(path, window, cx)
                });
            }
        })
        .detach();
    }

    /// Opens the file at `path` in a tab of its own.
    pub fn open_sql_file_from(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A file that is already open is shown, not opened twice.
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.read(cx).file() == Some(path.as_path()))
        {
            self.activate_tab(index, window, cx);
            return;
        }
        let source = path.clone();
        let reading = bridge::spawn_blocking(cx, move || std::fs::read_to_string(&source));
        cx.spawn_in(window, async move |this, cx| {
            let outcome = reading.await;
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok(sql) => {
                    let target = this.default_target(cx);
                    let tab = this.open_query(String::new(), &sql, target, window, cx);
                    tab.update(cx, |tab, cx| tab.set_file(path, false, cx));
                    this.schedule_save(cx);
                }
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "{} could not be opened: {error}",
                        path.display()
                    ))),
                    cx,
                ),
            });
        })
        .detach();
    }

    fn save_file(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        let file = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.read(cx).file().map(Path::to_path_buf));
        match file {
            Some(path) => self.save_active_tab_to(path, window, cx),
            None => self.save_file_as(&SaveFileAs, window, cx),
        }
    }

    fn save_file_as(&mut self, _: &SaveFileAs, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let tab = tab.read(cx);
        let (directory, name) = match tab.file() {
            Some(file) => (
                file.parent().map(Path::to_path_buf).unwrap_or_default(),
                file.file_name()
                    .map(|name| name.to_string_lossy().into_owned()),
            ),
            None => (
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default(),
                Some(format!(
                    "{}.sql",
                    tab.title().to_lowercase().replace(' ', "-")
                )),
            ),
        };
        let picking = cx.prompt_for_new_path(&directory, name.as_deref());
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = picking.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.save_active_tab_to(path, window, cx)
                });
            }
        })
        .detach();
    }

    /// Writes the text of the active tab to `path`, which becomes its file.
    pub fn save_active_tab_to(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(self.active_tab).cloned() else {
            return;
        };
        let sql = tab.read(cx).sql(cx);
        let target = path.clone();
        let writing = bridge::spawn_blocking(cx, move || std::fs::write(&target, sql));
        cx.spawn_in(window, async move |this, cx| {
            let outcome = writing.await;
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok(()) => {
                    tab.update(cx, |tab, cx| tab.set_file(path, false, cx));
                    this.schedule_save(cx);
                    cx.notify();
                }
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "{} could not be written: {error}",
                        path.display()
                    ))),
                    cx,
                ),
            });
        })
        .detach();
    }

    fn show_library(&mut self, shelf: Shelf, window: &mut Window, cx: &mut Context<Self>) {
        let connection = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.read(cx).connection_id().map(str::to_string));
        let workspace = cx.entity();
        Library::open(
            shelf,
            connection,
            self.connections.clone(),
            window,
            cx,
            move |title, sql, target, window, cx| {
                workspace.update(cx, |this, cx| {
                    this.open_query(title, &sql, target, window, cx);
                });
            },
        );
    }

    fn show_history(&mut self, _: &ShowHistory, window: &mut Window, cx: &mut Context<Self>) {
        self.show_library(Shelf::History, window, cx);
    }

    fn show_saved_queries(
        &mut self,
        _: &ShowSavedQueries,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_library(Shelf::Saved, window, cx);
    }

    fn save_query(&mut self, _: &SaveQuery, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let tab = tab.read(cx);
        let (sql, connection_id) = (tab.sql(cx), tab.connection_id().map(str::to_string));
        if sql.trim().is_empty() {
            return;
        }
        let workspace = cx.entity();
        library::ask_name(
            tab.title().to_string(),
            window,
            cx,
            move |name, folder, window, cx| {
                let query = SavedQuery {
                    id: 0,
                    connection_id: connection_id.clone(),
                    folder,
                    name: name.clone(),
                    sql: sql.clone(),
                };
                workspace.update(cx, |this, cx| this.store_query(query, window, cx));
            },
        );
    }

    /// Writes a saved query to the store and says so.
    pub fn store_query(&mut self, query: SavedQuery, window: &mut Window, cx: &mut Context<Self>) {
        let storage = AppState::get(cx).storage.clone();
        let name = query.name.clone();
        let saving = bridge::spawn_blocking(cx, move || storage.save_query(&query));
        cx.spawn_in(window, async move |_, cx| {
            let outcome = saving.await;
            let _ = cx.update(|window, cx| match outcome {
                Ok(_) => window.push_notification(
                    Notification::success(SharedString::from(format!(
                        "Saved “{name}”. Find it under Saved Queries."
                    ))),
                    cx,
                ),
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "“{name}” was not saved: {error}"
                    ))),
                    cx,
                ),
            });
        })
        .detach();
    }

    // Running ------------------------------------------------------------------

    fn run_statement(&mut self, _: &RunStatement, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            tab.update(cx, |tab, cx| tab.run(Scope::AtCursor, window, cx));
        }
    }

    fn run_all(&mut self, _: &RunAll, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            tab.update(cx, |tab, cx| tab.run(Scope::All, window, cx));
        }
    }

    fn explain_statement(
        &mut self,
        _: &ExplainStatement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            tab.update(cx, |tab, cx| tab.explain(false, window, cx));
        }
    }

    fn explain_analyze(&mut self, _: &ExplainAnalyze, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            tab.update(cx, |tab, cx| tab.explain(true, window, cx));
        }
    }

    fn cancel_query(&mut self, _: &CancelQuery, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            tab.update(cx, |tab, cx| tab.cancel(cx));
        }
    }

    // Connections --------------------------------------------------------------

    fn new_connection(&mut self, _: &NewConnection, window: &mut Window, cx: &mut Context<Self>) {
        ConnectionDialog::open(self.connections.clone(), None, window, cx);
    }

    fn on_connections_event(
        &mut self,
        _: &Entity<Connections>,
        event: &ConnectionsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ConnectionsEvent::ListChanged => {}
            ConnectionsEvent::StatusChanged { id } => {
                let connections = self.connections.read(cx);
                if let (Status::Failed { message }, Some(saved)) =
                    (connections.status(id), connections.get(id))
                {
                    let text = format!("{}: {message}", saved.config.name);
                    window.push_notification(Notification::error(SharedString::from(text)), cx);
                }
            }
            ConnectionsEvent::Failed { message } => {
                window.push_notification(
                    Notification::error(SharedString::from(message.clone())),
                    cx,
                );
            }
            ConnectionsEvent::NeedsInput { id, request } => {
                self.ask(id.clone(), request.clone(), window, cx)
            }
        }
        cx.notify();
    }

    /// Asks the user for what a connection attempt needs.
    fn ask(
        &mut self,
        id: String,
        request: InputRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self
            .connections
            .read(cx)
            .get(&id)
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default();
        match request {
            InputRequest::DatabasePassword => self.ask_secret(
                id,
                format!("Password for “{name}”"),
                "The database password is not saved for this connection.".into(),
                InputAnswer::DatabasePassword,
                window,
                cx,
            ),
            InputRequest::SshPassword => self.ask_secret(
                id,
                format!("SSH password for “{name}”"),
                "The password of the SSH user on the tunnel server.".into(),
                InputAnswer::SshPassword,
                window,
                cx,
            ),
            InputRequest::SshKeyPassphrase { path, retry } => self.ask_secret(
                id,
                format!("Passphrase for “{name}”"),
                if retry {
                    format!("That passphrase did not open {path}. Try again.")
                } else {
                    format!("The private key {path} is encrypted.")
                },
                InputAnswer::SshKeyPassphrase,
                window,
                cx,
            ),
            InputRequest::TrustHostKey {
                host,
                port,
                algorithm,
                fingerprint,
            } => {
                let shown = fingerprint.clone();
                let connections = self.connections.clone();
                // Answers once: trust, or give the attempt up.
                let answered = Rc::new(Cell::new(false));
                let finish = Rc::new(move |trust: bool, cx: &mut App| {
                    if answered.replace(true) {
                        return;
                    }
                    connections.update(cx, |connections, cx| match trust {
                        true => connections.answer(
                            &id,
                            InputAnswer::TrustHostKey(fingerprint.clone()),
                            cx,
                        ),
                        false => connections.cancel_input(&id, cx),
                    });
                });
                let text = SharedString::from(format!(
                    "“{name}” goes through an SSH server Kerno has not seen before.\n\n\
                     {algorithm} key fingerprint:\n{shown}\n\n\
                     Compare it with the fingerprint your server's administrator gives you. \
                     If you trust it, the key is remembered and checked on every connection."
                ));
                let title = SharedString::from(format!("Trust the SSH server {host}:{port}?"));
                window.open_alert_dialog(cx, move |alert, _, _| {
                    let (on_ok, on_close) = (finish.clone(), finish.clone());
                    alert
                        .title(title.clone())
                        .description(text.clone())
                        .confirm()
                        .ok_text("Trust and connect")
                        .on_ok(move |_, _, cx| {
                            on_ok(true, cx);
                            true
                        })
                        .on_close(move |_, _, cx| on_close(false, cx))
                });
            }
        }
    }

    fn ask_secret(
        &mut self,
        id: String,
        title: String,
        explanation: String,
        answer: fn(Secret) -> InputAnswer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).masked(true));
        let connections = self.connections.clone();
        // Answers once: with what was typed, or by giving the attempt up.
        let answered = Rc::new(Cell::new(false));
        let finish = {
            let input = input.clone();
            Rc::new(move |submit: bool, cx: &mut App| {
                if answered.replace(true) {
                    return;
                }
                let secret = Secret::new(input.read(cx).value().to_string());
                connections.update(cx, |connections, cx| match submit {
                    true => connections.answer(&id, answer(secret), cx),
                    false => connections.cancel_input(&id, cx),
                });
            })
        };
        let focused = Rc::new(Cell::new(false));
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, focused) = (input.clone(), focused.clone());
            let (on_enter, on_close, on_click) = (finish.clone(), finish.clone(), finish.clone());
            let explanation = SharedString::from(explanation.clone());
            dialog
                .title(SharedString::from(title.clone()))
                .w(px(420.))
                // Enter in the field confirms the dialog.
                .on_ok(move |_, _, cx| {
                    on_enter(true, cx);
                    true
                })
                // Escape, the close button and a click outside give up.
                .on_close(move |_, _, cx| on_close(false, cx))
                .content(move |content, window, cx| {
                    if !focused.replace(true) {
                        let input = input.clone();
                        window.defer(cx, move |window, cx| {
                            input.read(cx).focus_handle(cx).focus(window, cx)
                        });
                    }
                    let (submit, cancel) = (on_click.clone(), on_click.clone());
                    content.child(
                        v_flex()
                            .gap_3()
                            .child(div().text_sm().child(explanation.clone()))
                            .child(Input::new(&input).id("secret-input").mask_toggle())
                            .child(
                                h_flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        Button::new("secret-cancel")
                                            .ghost()
                                            .label("Cancel")
                                            .on_click(move |_, window, cx| {
                                                cancel(false, cx);
                                                window.close_dialog(cx);
                                            }),
                                    )
                                    .child(
                                        Button::new("secret-ok")
                                            .primary()
                                            .label("Connect")
                                            .on_click(move |_, window, cx| {
                                                submit(true, cx);
                                                window.close_dialog(cx);
                                            }),
                                    ),
                            ),
                    )
                })
        });
    }

    fn export_connections(
        &mut self,
        _: &ExportConnections,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let picking = cx.prompt_for_new_path(&home, Some("kerno-connections.json"));
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(path))) = picking.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.export_connections_to(path, window, cx)
                });
            }
        })
        .detach();
    }

    /// Writes the saved connections, without their secrets, to `path`.
    pub fn export_connections_to(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let configs: Vec<ConnectionConfig> = self
            .connections
            .read(cx)
            .saved()
            .iter()
            .map(|saved| saved.config.clone())
            .collect();
        let count = configs.len();
        let target = path.clone();
        let writing = bridge::spawn_blocking(cx, move || {
            let json = export_connections(&configs)?;
            std::fs::write(&target, json).map_err(|error| {
                kerno_core::Error::io(format!("writing {}", target.display()), error)
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = writing.await;
            let _ = this.update_in(cx, |_, window, cx| match outcome {
                Ok(()) => window.push_notification(
                    Notification::success(SharedString::from(format!(
                        "Exported {count} connection{} to {}. Passwords are not included.",
                        if count == 1 { "" } else { "s" },
                        path.display()
                    ))),
                    cx,
                ),
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(format!("The export failed: {error}"))),
                    cx,
                ),
            });
        })
        .detach();
    }

    fn import_connections(
        &mut self,
        _: &ImportConnections,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picking = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = picking.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.import_connections_from(path, window, cx)
                });
            }
        })
        .detach();
    }

    /// Adds the connections of an exported file.
    pub fn import_connections_from(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = path.clone();
        let reading = bridge::spawn_blocking(cx, move || {
            let json = std::fs::read_to_string(&source).map_err(|error| {
                kerno_core::Error::io(format!("reading {}", source.display()), error)
            })?;
            import_connections(&json)
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = reading.await;
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok(configs) => {
                    let count = configs.len();
                    this.connections.update(cx, |connections, cx| connections.import(configs, cx));
                    window.push_notification(
                        Notification::success(SharedString::from(format!(
                            "Imported {count} connection{}. Enter their passwords when you connect.",
                            if count == 1 { "" } else { "s" }
                        ))),
                        cx,
                    );
                }
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "{} was not imported: {error}",
                        path.display()
                    ))),
                    cx,
                ),
            });
        })
        .detach();
    }

    // Rendering ---------------------------------------------------------------

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        if self.tabs.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    v_flex()
                        .items_center()
                        .gap_1()
                        .child(div().text_lg().font_semibold().child("No tab open"))
                        .child(div().text_color(theme.muted_foreground).child(
                            "Open a query tab to write SQL. Its text is kept between runs.",
                        )),
                )
                .child(
                    Button::new("empty-new-query")
                        .primary()
                        .label("New Query Tab")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(NewQueryTab), cx)
                        }),
                )
                .into_any_element();
        }

        let tab_bar = TabBar::new("tabs")
            .selected_index(self.active_tab)
            .on_click(
                cx.listener(|this, index: &usize, window, cx| {
                    this.activate_tab(*index, window, cx)
                }),
            )
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                Tab::new().label(tab_label(tab.read(cx))).suffix(
                    Button::new(("close-tab", index))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Close tab")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            // The click must not also reach the tab, which
                            // would activate the tab being closed.
                            cx.stop_propagation();
                            this.close_tab_at(index, window, cx)
                        })),
                )
            }))
            .suffix(
                Button::new("new-tab")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("New query tab")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewQueryTab), cx)),
            );
        v_flex()
            .size_full()
            .child(tab_bar)
            .children(
                self.tabs
                    .get(self.active_tab)
                    .map(|tab| div().flex_1().min_h_0().child(tab.clone())),
            )
            .into_any_element()
    }

    /// What the status bar says, left to right: the active tab's
    /// connection, database, transaction, rows and timing; without a tab,
    /// how many connections are open.
    fn status_items(&self, cx: &App) -> Vec<(String, Option<gpui_kit::Hsla>)> {
        let theme = cx.theme();
        let status = self
            .tabs
            .get(self.active_tab)
            .map(|tab| tab.read(cx).status(cx));
        let Some(TabStatus {
            connection: Some(connection),
            read_only,
            database,
            transaction,
            running,
            rows,
            elapsed,
            notices,
        }) = status
        else {
            return vec![(
                match self.connections.read(cx).connected_count() {
                    0 => "Not connected".to_string(),
                    1 => "1 connection open".to_string(),
                    count => format!("{count} connections open"),
                },
                None,
            )];
        };
        let mut items = vec![(connection, None)];
        items.extend(database.map(|database| (database, None)));
        if read_only {
            items.push(("read-only".into(), Some(theme.warning)));
        }
        match transaction {
            TxState::Idle => {}
            TxState::Open => items.push(("transaction open".into(), Some(theme.warning))),
            TxState::Failed => {
                items.push(("transaction failed: roll back".into(), Some(theme.danger)))
            }
        }
        if running {
            items.push(("running…".into(), None));
        }
        items.extend(rows.map(|rows| {
            (
                format!("{rows} row{}", if rows == 1 { "" } else { "s" }),
                None,
            )
        }));
        items.extend(elapsed.map(|elapsed| (duration_text(elapsed), None)));
        if notices > 0 {
            items.push((
                format!("{notices} message{}", if notices == 1 { "" } else { "s" }),
                None,
            ));
        }
        items
    }

    /// The text of the status bar; for tests.
    #[cfg(test)]
    pub fn status_text(&self, cx: &App) -> Vec<String> {
        self.status_items(cx)
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    fn render_status_bar(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let memory_only = !AppState::get(cx).secrets.is_persistent();
        h_flex()
            .h(px(24.))
            .px_3()
            .gap_3()
            .justify_between()
            .border_t_1()
            .border_color(theme.status_bar_border)
            .bg(theme.status_bar)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                h_flex()
                    .gap_3()
                    .children(self.status_items(cx).into_iter().map(|(text, color)| {
                        div()
                            .when_some(color, |this, color| this.text_color(color))
                            .child(SharedString::from(text))
                    })),
            )
            .when(memory_only, |this| {
                this.child(
                    div()
                        .text_color(theme.warning)
                        .child("Passwords are kept in memory only (no system secret store in use)"),
                )
            })
    }
}

/// The name on a tab: its title, marked when its file has unsaved changes.
fn tab_label(tab: &QueryTab) -> SharedString {
    if tab.file().is_some() && tab.unsaved() {
        SharedString::from(format!("{} •", tab.title()))
    } else {
        tab.title()
    }
}

/// The commands shown in the palette, grouped, in display order.
fn palette_groups() -> Vec<(&'static str, Vec<&'static actions::Command>)> {
    let mut groups: Vec<(&'static str, Vec<&'static actions::Command>)> = Vec::new();
    for command in actions::COMMANDS {
        match groups.iter_mut().find(|(group, _)| *group == command.group) {
            Some((_, commands)) => commands.push(command),
            None => groups.push((command.group, vec![command])),
        }
    }
    groups
}

/// The command at a palette position.
fn palette_command(section: usize, row: usize) -> Option<&'static actions::Command> {
    palette_groups().get(section)?.1.get(row).copied()
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar_visible = AppState::get(cx).settings.sidebar_visible;
        let background = cx.theme().background;
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;

        let main = resizable_panel().child(self.render_tabs(cx));
        let body = if sidebar_visible {
            h_resizable("workspace")
                .child(
                    resizable_panel()
                        .size(px(260.))
                        .size_range(px(200.)..px(480.))
                        .child(self.sidebar.clone()),
                )
                .child(main)
        } else {
            h_resizable("workspace").child(main)
        };

        v_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::new_query_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::use_system_theme))
            .on_action(cx.listener(Self::use_light_theme))
            .on_action(cx.listener(Self::use_dark_theme))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::about))
            .on_action(cx.listener(Self::open_command_palette))
            .on_action(cx.listener(Self::new_connection))
            .on_action(cx.listener(Self::open_sql_file))
            .on_action(cx.listener(Self::save_file))
            .on_action(cx.listener(Self::save_file_as))
            .on_action(cx.listener(Self::show_history))
            .on_action(cx.listener(Self::show_saved_queries))
            .on_action(cx.listener(Self::save_query))
            .on_action(cx.listener(Self::run_statement))
            .on_action(cx.listener(Self::run_all))
            .on_action(cx.listener(Self::cancel_query))
            .on_action(cx.listener(Self::explain_statement))
            .on_action(cx.listener(Self::explain_analyze))
            .on_action(cx.listener(Self::import_connections))
            .on_action(cx.listener(Self::export_connections))
            .size_full()
            .bg(background)
            .text_color(foreground)
            .child(
                TitleBar::new().child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(muted)
                        .child("Kerno"),
                ),
            )
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_status_bar(cx))
    }
}
