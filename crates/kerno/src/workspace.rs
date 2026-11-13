//! The main window: title bar, sidebar, tabs and status bar, and the
//! commands that act on them.

use std::cell::{Cell, RefCell};
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
    AnyView, App, AppContext as _, BorrowAppContext as _, Context, Entity, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions,
    Render, SharedString, Styled as _, Subscription, Task, Window, div, px,
};
use kerno_core::config::{ConnectionConfig, Secret, export_connections, import_connections};

use crate::actions::{
    self, About, AddRow, ApplyChanges, CancelQuery, CloseTab, CollapseConnectionFolders,
    CollapseSchemaTree, CopyObjectName, CopyQualifiedName, DeleteRows, DiscardChanges, DropObject,
    DuplicateRows, EditConnection, EditObject, ExplainAnalyze, ExplainStatement, ExportConnections,
    ExportData, FindInEditor, FormatSql, ImportConnections, ImportCsv, NewConnection,
    NewConnectionFolder, NewQueryOnObject, NewQueryTab, NextTab, OpenCommandPalette,
    OpenConnection, OpenSettings, OpenSqlFile, PreviousTab, RefreshConnections, RefreshData,
    RefreshObject, RefreshSchemaTree, RenameObject, ReplaceInEditor, RunAll, RunStatement,
    SaveFile, SaveFileAs, SaveQuery, SetDefault, SetDefaultSchema, SetNull, ShowHistory,
    ShowSavedQueries, ToggleComment, ToggleSidebar, UseDarkTheme, UseLightTheme, UseSystemTheme,
};
use crate::bridge;
use crate::connections::dialog::ConnectionDialog;
use crate::connections::manager::{
    Connections, ConnectionsEvent, InputAnswer, InputRequest, Status,
};
use crate::data::tab::{DataTab, DataTabEvent};
use crate::import_dialog::{self, ImportDialog};
use crate::library::{self, Library, Shelf};
use crate::navigator::actions::Action as TreeAction;
use crate::navigator::view::{Navigator, NavigatorEvent};
use crate::query::tab::{QueryTab, QueryTabEvent, Scope, TabStatus, duration_text};
use crate::settings::TableDoubleClick;
use crate::sidebar::{Sidebar, SidebarAction, SidebarEvent, Subject};
use crate::state::{ACTIVE_TAB_KEY, AppState};
use crate::structure::editor::{TableEditor, TableEditorEvent};
use crate::structure::tab::{StructureTab, StructureTabEvent};
use crate::tabs::{TabSnapshot, Target};
use crate::theme::{self, Appearance};
use kerno_core::TxState;
use kerno_core::browse::Filter;
use kerno_core::schema::{RelationKind, TableRef};
use kerno_core::sql::CellValue;
use kerno_core::storage::SavedQuery;

/// How long after the last change the open tabs are written to storage.
const SAVE_TABS_DELAY: Duration = Duration::from_millis(400);

/// A tab of the central area.
#[derive(Clone)]
enum OpenTab {
    Query(Entity<QueryTab>),
    Data(Entity<DataTab>),
    Structure(Entity<StructureTab>),
    /// Not restored after a restart: an unsaved draft is not worth keeping.
    TableEditor(Entity<TableEditor>),
}

impl OpenTab {
    fn query(&self) -> Option<&Entity<QueryTab>> {
        match self {
            OpenTab::Query(tab) => Some(tab),
            _ => None,
        }
    }

    fn data(&self) -> Option<&Entity<DataTab>> {
        match self {
            OpenTab::Data(tab) => Some(tab),
            _ => None,
        }
    }

    fn structure(&self) -> Option<&Entity<StructureTab>> {
        match self {
            OpenTab::Structure(tab) => Some(tab),
            _ => None,
        }
    }

    fn table_editor(&self) -> Option<&Entity<TableEditor>> {
        match self {
            OpenTab::TableEditor(tab) => Some(tab),
            _ => None,
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            OpenTab::Query(tab) => tab.focus_handle(cx),
            OpenTab::Data(tab) => tab.focus_handle(cx),
            OpenTab::Structure(tab) => tab.focus_handle(cx),
            OpenTab::TableEditor(tab) => tab.focus_handle(cx),
        }
    }

    /// What is stored to open the tab again at the next launch, if anything.
    fn snapshot(&self, cx: &App) -> Option<TabSnapshot> {
        match self {
            OpenTab::Query(tab) => Some(tab.read(cx).snapshot(cx)),
            OpenTab::Data(tab) => Some(tab.read(cx).snapshot()),
            OpenTab::Structure(tab) => Some(tab.read(cx).snapshot()),
            OpenTab::TableEditor(_) => None,
        }
    }

    fn title(&self, cx: &App) -> SharedString {
        match self {
            OpenTab::Query(tab) => tab.read(cx).title(),
            OpenTab::Data(tab) => tab.read(cx).title(),
            OpenTab::Structure(tab) => tab.read(cx).title(),
            OpenTab::TableEditor(tab) => tab.read(cx).title(),
        }
    }

    /// The tab's name, marked when it holds something not saved or not
    /// applied yet.
    fn label(&self, cx: &App) -> SharedString {
        let marked = match self {
            OpenTab::Query(tab) => {
                let tab = tab.read(cx);
                tab.file().is_some() && tab.unsaved()
            }
            OpenTab::Data(tab) => tab.read(cx).pending_count(cx) > 0,
            OpenTab::Structure(_) => false,
            OpenTab::TableEditor(tab) => tab.read(cx).dirty(cx),
        };
        let title = self.title(cx);
        if marked {
            SharedString::from(format!("{title} •"))
        } else {
            title
        }
    }

    fn status(&self, cx: &App) -> TabStatus {
        match self {
            OpenTab::Query(tab) => tab.read(cx).status(cx),
            OpenTab::Data(tab) => tab.read(cx).status(cx),
            OpenTab::Structure(tab) => tab.read(cx).status(cx),
            OpenTab::TableEditor(tab) => tab.read(cx).status(cx),
        }
    }

    fn view(&self) -> AnyView {
        match self {
            OpenTab::Query(tab) => tab.clone().into(),
            OpenTab::Data(tab) => tab.clone().into(),
            OpenTab::Structure(tab) => tab.clone().into(),
            OpenTab::TableEditor(tab) => tab.clone().into(),
        }
    }
}

/// The root view of a Kerno window.
pub struct Workspace {
    focus_handle: FocusHandle,
    connections: Entity<Connections>,
    sidebar: Entity<Sidebar>,
    tabs: Vec<OpenTab>,
    active_tab: usize,
    /// Number given to the next "Query N" tab.
    next_query_number: usize,
    palette: Entity<CommandState>,
    pending_save: Option<Task<()>>,
    /// A connection being opened for "New SQL editor": its tab opens once
    /// it is connected.
    pending_editor: Option<String>,
    /// Where the active query tab runs, written on each frame for the
    /// schema tree's "Set as default for this tab".
    tab_target: Rc<RefCell<Option<Target>>>,
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
        let flush_on_quit = cx.on_app_quit(|this, cx| {
            let (tabs, settings) = (this.save_tabs(cx), AppState::flush_settings(cx));
            async move {
                tabs.await;
                settings.await;
            }
        });
        let sidebar = cx.new(|cx| Sidebar::new(connections.clone(), window, cx));
        let navigator = sidebar.read(cx).navigator().clone();
        let tab_target = navigator.read(cx).tab_target();
        let navigator_events = cx.subscribe_in(&navigator, window, Self::on_navigator_event);
        let connection_events = cx.subscribe_in(&connections, window, Self::on_connections_event);
        let sidebar_events = cx.subscribe_in(&sidebar, window, Self::on_sidebar_event);

        let mut workspace = Self {
            focus_handle: cx.focus_handle(),
            connections,
            sidebar,
            tabs: Vec::new(),
            active_tab: 0,
            next_query_number: 1,
            palette,
            pending_save: None,
            pending_editor: None,
            tab_target,
            tab_subscriptions: Vec::new(),
            _subscriptions: vec![
                follow_system,
                flush_on_quit,
                connection_events,
                navigator_events,
                sidebar_events,
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
                    source,
                } => {
                    let tab = workspace.push_query_tab(title, &sql, target, window, cx);
                    tab.update(cx, |tab, cx| {
                        if let Some(file) = file {
                            tab.set_file(file, unsaved, cx);
                        }
                        tab.set_source(source, cx);
                    });
                }
                TabSnapshot::Data {
                    target,
                    schema,
                    table,
                    view,
                } => {
                    let kind = if view {
                        RelationKind::View
                    } else {
                        RelationKind::Table
                    };
                    workspace.push_data_tab(
                        target,
                        TableRef::new(schema, table),
                        kind,
                        Filter::default(),
                        window,
                        cx,
                    );
                }
                TabSnapshot::Structure {
                    target,
                    schema,
                    table,
                    view,
                    materialized,
                } => {
                    let kind = match (view, materialized) {
                        (_, true) => RelationKind::MaterializedView,
                        (true, false) => RelationKind::View,
                        (false, false) => RelationKind::Table,
                    };
                    workspace.push_structure_tab(
                        target,
                        TableRef::new(schema, table),
                        kind,
                        window,
                        cx,
                    );
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
            .map(|tab| tab.title(cx).to_string())
            .collect()
    }

    /// What the tab bar writes on each tab; for tests.
    #[cfg(test)]
    pub fn tab_labels(&self, cx: &App) -> Vec<String> {
        self.tabs
            .iter()
            .map(|tab| tab.label(cx).to_string())
            .collect()
    }

    /// Index of the active tab, when there is one.
    #[cfg(test)]
    pub fn active_tab_index(&self) -> Option<usize> {
        (!self.tabs.is_empty()).then_some(self.active_tab)
    }

    /// The active tab, when it is a query tab.
    pub fn active_query_tab(&self) -> Option<&Entity<QueryTab>> {
        self.tabs.get(self.active_tab)?.query()
    }

    /// The active tab, when it shows a table's data.
    pub fn active_data_tab(&self) -> Option<&Entity<DataTab>> {
        self.tabs.get(self.active_tab)?.data()
    }

    /// The active tab, when it shows a table's structure.
    #[cfg(test)]
    pub fn active_structure_tab(&self) -> Option<&Entity<StructureTab>> {
        self.tabs.get(self.active_tab)?.structure()
    }

    /// The active tab, when it is a table form.
    #[cfg(test)]
    pub fn active_table_editor(&self) -> Option<&Entity<TableEditor>> {
        self.tabs.get(self.active_tab)?.table_editor()
    }

    /// Opens the data of a table in a tab of its own.
    fn push_data_tab(
        &mut self,
        target: Target,
        table: TableRef,
        kind: RelationKind,
        filter: Filter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<DataTab> {
        let connections = self.connections.clone();
        let tab = cx.new(|cx| DataTab::new(target, table, kind, filter, connections, window, cx));
        // The other end of a foreign key opens beside it.
        self.tab_subscriptions.push(cx.subscribe_in(
            &tab,
            window,
            |this, tab, event: &DataTabEvent, window, cx| {
                let target = tab.read(cx).target().clone();
                match event {
                    DataTabEvent::Open {
                        table,
                        kind,
                        filter,
                    } => {
                        this.open_data(target, table.clone(), *kind, filter.clone(), window, cx);
                    }
                    DataTabEvent::OpenStructure { table, kind } => {
                        this.open_structure(target, table.clone(), *kind, window, cx);
                    }
                }
            },
        ));
        self.tabs.push(OpenTab::Data(tab.clone()));
        self.active_tab = self.tabs.len() - 1;
        tab
    }

    /// Shows the data of a table: in the tab that already shows it, when
    /// nothing asks for other rows, else in a new one.
    pub fn open_data(
        &mut self,
        target: Target,
        table: TableRef,
        kind: RelationKind,
        filter: Filter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<DataTab> {
        let open = self.tabs.iter().position(|tab| {
            tab.data().is_some_and(|tab| {
                let tab = tab.read(cx);
                filter.is_empty() && tab.target() == &target && tab.table() == &table
            })
        });
        if let Some(index) = open {
            self.activate_tab(index, window, cx);
            return self.tabs[index].data().expect("a data tab").clone();
        }
        let tab = self.push_data_tab(target, table, kind, filter, window, cx);
        self.activate_tab(self.tabs.len() - 1, window, cx);
        self.schedule_save(cx);
        tab
    }

    /// Opens the structure of a table in a tab of its own.
    fn push_structure_tab(
        &mut self,
        target: Target,
        table: TableRef,
        kind: RelationKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<StructureTab> {
        let connections = self.connections.clone();
        let tab = cx.new(|cx| StructureTab::new(target, table, kind, connections, window, cx));
        // One subscription per tab: `close_tab_at` removes them by index.
        self.tab_subscriptions.push(cx.subscribe_in(
            &tab,
            window,
            |this, tab, event: &StructureTabEvent, window, cx| {
                let target = tab.read(cx).target().clone();
                match event {
                    StructureTabEvent::OpenStructure { table, kind } => {
                        this.open_structure(target, table.clone(), *kind, window, cx);
                    }
                    StructureTabEvent::OpenData { table, kind } => {
                        this.open_data(target, table.clone(), *kind, Filter::default(), window, cx);
                    }
                    StructureTabEvent::EditTable { table } => {
                        let schema = table.schema.clone();
                        this.open_table_editor(target, schema, Some(table.clone()), window, cx);
                    }
                }
            },
        ));
        self.tabs.push(OpenTab::Structure(tab.clone()));
        self.active_tab = self.tabs.len() - 1;
        tab
    }

    /// Shows the structure of a table: in the tab that already shows it,
    /// else in a new one.
    pub fn open_structure(
        &mut self,
        target: Target,
        table: TableRef,
        kind: RelationKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<StructureTab> {
        let open = self.tabs.iter().position(|tab| {
            tab.structure().is_some_and(|tab| {
                let tab = tab.read(cx);
                tab.target() == &target && tab.table() == &table
            })
        });
        if let Some(index) = open {
            self.activate_tab(index, window, cx);
            return self.tabs[index]
                .structure()
                .expect("a structure tab")
                .clone();
        }
        let tab = self.push_structure_tab(target, table, kind, window, cx);
        self.activate_tab(self.tabs.len() - 1, window, cx);
        self.schedule_save(cx);
        tab
    }

    /// Opens the form of `table`, or of a new table in `schema` when `None`:
    /// in the tab that already shows it, else in a new one.
    pub fn open_table_editor(
        &mut self,
        target: Target,
        schema: String,
        table: Option<TableRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TableEditor> {
        let open = table.as_ref().and_then(|table| {
            self.tabs.iter().position(|tab| {
                tab.table_editor().is_some_and(|tab| {
                    let tab = tab.read(cx);
                    tab.target() == &target && tab.table() == Some(table)
                })
            })
        });
        if let Some(index) = open {
            self.activate_tab(index, window, cx);
            return self.tabs[index]
                .table_editor()
                .expect("a table editor")
                .clone();
        }
        let connections = self.connections.clone();
        let tab = cx.new(|cx| TableEditor::new(target, schema, table, connections, window, cx));
        // One subscription per tab: `close_tab_at` removes them by index.
        self.tab_subscriptions
            .push(cx.subscribe_in(&tab, window, Self::on_table_editor_event));
        self.tabs.push(OpenTab::TableEditor(tab.clone()));
        self.activate_tab(self.tabs.len() - 1, window, cx);
        tab
    }

    /// Shows what a table form changed: the tree, the structure tab of the
    /// table, and the new table's structure in place of its form.
    fn on_table_editor_event(
        &mut self,
        editor: &Entity<TableEditor>,
        event: &TableEditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let TableEditorEvent::Applied { from, table } = event;
        let target = editor.read(cx).target().clone();
        let navigator = self.sidebar.read(cx).navigator().clone();
        navigator.update(cx, |navigator, cx| {
            navigator.refresh_tables(
                &target.connection,
                target.database.as_deref(),
                &table.schema,
                cx,
            )
        });
        let shown = from.as_ref().unwrap_or(table);
        for tab in self.tabs.iter().filter_map(OpenTab::structure) {
            let same = {
                let tab = tab.read(cx);
                tab.target() == &target && tab.table() == shown
            };
            if same {
                let table = table.clone();
                tab.update(cx, |tab, cx| tab.set_table(table, window, cx));
            }
        }
        if from.is_none() {
            // A new table: its structure takes the place of its form.
            let (editor, table) = (editor.clone(), table.clone());
            cx.defer_in(window, move |this, window, cx| {
                if let Some(index) = this
                    .tabs
                    .iter()
                    .position(|tab| tab.table_editor() == Some(&editor))
                {
                    this.close_tab_at(index, window, cx);
                }
                this.open_structure(target, table, RelationKind::Table, window, cx);
            });
        }
        cx.notify();
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
        // One subscription per tab: they are dropped with it, by index.
        self.tab_subscriptions.push(Subscription::join(
            cx.observe(&editor, |this, _, cx| this.schedule_save(cx)),
            cx.subscribe(&tab, |this, tab, event: &QueryTabEvent, cx| match event {
                QueryTabEvent::SourceApplied => {
                    let tab = tab.read(cx);
                    if let (Some(target), Some(schema)) = (tab.target().cloned(), tab.source()) {
                        let schema = schema.to_string();
                        let navigator = this.sidebar.read(cx).navigator().clone();
                        navigator.update(cx, |navigator, cx| {
                            navigator.refresh_source(&target, &schema, cx)
                        });
                    }
                }
            }),
        ));
        self.tabs.push(OpenTab::Query(tab.clone()));
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
            .filter_map(|tab| tab.snapshot(cx))
            .collect();
        // Counted among the tabs that are stored.
        let active_tab = self.tabs[..self.active_tab.min(self.tabs.len())]
            .iter()
            .filter(|tab| tab.snapshot(cx).is_some())
            .count()
            .min(snapshots.len().saturating_sub(1));
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
        let target = self.default_target(cx);
        self.new_query_on(target, window, cx);
    }

    /// Opens an empty "Query N" tab running on `target`.
    fn new_query_on(
        &mut self,
        target: Option<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = format!("Query {}", self.next_query_number);
        self.open_query(title, "", target, window, cx);
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
                    let double_click = AppState::get(cx).settings.table_double_click;
                    let opens = TableDoubleClick::ALL
                        .iter()
                        .position(|choice| *choice == double_click);
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
                            )
                            .child(
                                div()
                                    .pt_2()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("Double click on a table or view in the tree"),
                            )
                            .child(
                                RadioGroup::new("table-double-click")
                                    .children(TableDoubleClick::ALL.map(TableDoubleClick::label))
                                    .selected_index(opens)
                                    .on_change(|index, _, cx| {
                                        if let Some(choice) = TableDoubleClick::ALL.get(*index) {
                                            AppState::update_settings(cx, |settings| {
                                                settings.table_double_click = *choice
                                            });
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
            NavigatorEvent::NewQuery { target } => {
                self.new_query_on(Some(target.clone()), window, cx);
            }
            NavigatorEvent::OpenQuery { title, sql, target } => {
                self.push_query_tab(title.clone(), sql, target.clone(), window, cx);
                self.activate_tab(self.tabs.len() - 1, window, cx);
                self.schedule_save(cx);
            }
            NavigatorEvent::OpenData {
                target,
                table,
                kind,
            } => {
                self.open_data(
                    target.clone(),
                    table.clone(),
                    *kind,
                    Filter::default(),
                    window,
                    cx,
                );
            }
            NavigatorEvent::ShowStructure {
                target,
                table,
                kind,
            } => {
                self.open_structure(target.clone(), table.clone(), *kind, window, cx);
            }
            NavigatorEvent::CreateTable { target, schema } => {
                self.open_table_editor(target.clone(), schema.clone(), None, window, cx);
            }
            NavigatorEvent::OpenSource {
                title,
                sql,
                target,
                schema,
            } => {
                let tab = self.open_query(title.clone(), sql, target.clone(), window, cx);
                tab.update(cx, |tab, cx| tab.set_source(Some(schema.clone()), cx));
            }
            NavigatorEvent::ImportCsv {
                target,
                schema,
                table,
            } => {
                self.import_csv_into(target.clone(), schema.clone(), table.clone(), window, cx);
            }
            NavigatorEvent::SetDefault { target } => {
                let tab = self.active_query_tab().filter(|tab| {
                    tab.read(cx)
                        .target()
                        .is_some_and(|tab| tab.connection == target.connection)
                });
                match tab.cloned() {
                    Some(tab) => {
                        tab.update(cx, |tab, cx| tab.set_target(Some(target.clone()), cx));
                        self.schedule_save(cx);
                    }
                    None => window.push_notification(
                        Notification::warning("Open a query tab on this connection first."),
                        cx,
                    ),
                }
            }
        }
    }

    /// Runs a key of the schema tree (or its palette command) on the
    /// selected node.
    fn on_tree(&mut self, action: TreeAction, window: &mut Window, cx: &mut Context<Self>) {
        let navigator = self.sidebar.read(cx).navigator().clone();
        navigator.update(cx, |navigator, cx| {
            navigator.perform_on_selected(action, window, cx)
        });
    }

    /// The sidebar; for tests.
    #[cfg(test)]
    pub fn sidebar(&self) -> Entity<Sidebar> {
        self.sidebar.clone()
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
            .map(connection_target)
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
        if let Some(index) = self.tabs.iter().position(|tab| {
            tab.query()
                .is_some_and(|tab| tab.read(cx).file() == Some(path.as_path()))
        }) {
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
            .active_query_tab()
            .and_then(|tab| tab.read(cx).file().map(Path::to_path_buf));
        match file {
            Some(path) => self.save_active_tab_to(path, window, cx),
            None => self.save_file_as(&SaveFileAs, window, cx),
        }
    }

    fn save_file_as(&mut self, _: &SaveFileAs, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_query_tab() else {
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
        let Some(tab) = self.active_query_tab().cloned() else {
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
            .active_query_tab()
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
        let Some(tab) = self.active_query_tab() else {
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
        if let Some(tab) = self.active_query_tab() {
            tab.update(cx, |tab, cx| tab.run(Scope::AtCursor, window, cx));
        }
    }

    fn run_all(&mut self, _: &RunAll, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_query_tab() {
            tab.update(cx, |tab, cx| tab.run(Scope::All, window, cx));
        }
    }

    fn explain_statement(
        &mut self,
        _: &ExplainStatement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.active_query_tab() {
            tab.update(cx, |tab, cx| tab.explain(false, window, cx));
        }
    }

    fn explain_analyze(&mut self, _: &ExplainAnalyze, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_query_tab() {
            tab.update(cx, |tab, cx| tab.explain(true, window, cx));
        }
    }

    fn toggle_comment(&mut self, _: &ToggleComment, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_query_tab() {
            tab.update(cx, |tab, cx| tab.toggle_comment(window, cx));
        }
    }

    fn format_sql(&mut self, _: &FormatSql, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_query_tab() {
            tab.update(cx, |tab, cx| tab.format(window, cx));
        }
    }

    /// Runs `act` on the active tab when it shows a table's data.
    fn on_data_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        act: impl FnOnce(&mut DataTab, &mut Window, &mut Context<DataTab>),
    ) {
        if let Some(tab) = self.active_data_tab().cloned() {
            tab.update(cx, |tab, cx| act(tab, window, cx));
        }
    }

    // Export and import ------------------------------------------------------

    /// Exports what is on screen: the rows of a table, or a query result.
    fn export_data(&mut self, _: &ExportData, window: &mut Window, cx: &mut Context<Self>) {
        // The tabs say themselves why they cannot export (not connected).
        let opened = if let Some(tab) = self.active_data_tab().cloned() {
            tab.update(cx, |tab, cx| tab.export(window, cx));
            true
        } else if let Some(tab) = self.active_query_tab().cloned() {
            tab.update(cx, |tab, cx| tab.export_result(window, cx))
                .is_some()
        } else {
            false
        };
        if !opened {
            window.push_notification(
                Notification::warning(
                    "Open the rows of a table, or run a statement that returns rows: \
                     that is what is exported.",
                ),
                cx,
            );
        }
    }

    /// Imports a CSV file into the table on screen, or into a new table of
    /// the connection being browsed.
    fn import_csv(&mut self, _: &ImportCsv, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_data_tab()
            && tab.read(cx).kind().is_table()
        {
            let tab = tab.read(cx);
            let (target, table) = (tab.target().clone(), tab.table().clone());
            self.import_csv_into(target, table.schema.clone(), Some(table), window, cx);
            return;
        }
        let meta = self.default_target(cx).and_then(|target| {
            let meta = self.connections.read(cx).meta_session(&target.connection)?;
            Some((target, meta))
        });
        let Some((target, meta)) = meta else {
            window.push_notification(
                Notification::warning(
                    "Connect first: the file is imported into a new table of the connection \
                     shown in the sidebar.",
                ),
                cx,
            );
            return;
        };
        let schema = if meta.dialect().has_schemas() {
            Some("public".to_string())
        } else {
            meta.database()
        };
        let Some(schema) = schema else {
            window.push_notification(
                Notification::warning(
                    "Choose a database first: right-click it in the tree and choose \
                     “Import CSV…”.",
                ),
                cx,
            );
            return;
        };
        self.import_csv_into(target, schema, None, window, cx);
    }

    /// Opens the import of a CSV file into `table`, or into a new table of
    /// `schema`.
    pub fn import_csv_into(
        &mut self,
        target: Target,
        schema: String,
        table: Option<TableRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ImportDialog>> {
        let workspace = cx.entity().downgrade();
        let imported = target.clone();
        import_dialog::open(
            self.connections.clone(),
            target,
            schema,
            table,
            window,
            cx,
            move |table, window, cx| {
                let _ =
                    workspace.update(cx, |this, cx| this.imported(&imported, table, window, cx));
            },
        )
    }

    /// Shows what an import added: the tables of its schema in the tree,
    /// and the rows of an open data tab of the table.
    fn imported(
        &mut self,
        target: &Target,
        table: &TableRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let navigator = self.sidebar.read(cx).navigator().clone();
        navigator.update(cx, |navigator, cx| {
            navigator.refresh_tables(
                &target.connection,
                target.database.as_deref(),
                &table.schema,
                cx,
            )
        });
        for tab in self.tabs.iter().filter_map(OpenTab::data) {
            let same = {
                let tab = tab.read(cx);
                let shown = tab.target();
                shown.connection == target.connection
                    && tab.table() == table
                    && (shown.database.is_none()
                        || target.database.is_none()
                        || shown.database == target.database)
            };
            if same {
                tab.update(cx, |tab, cx| tab.refresh(window, cx));
            }
        }
    }

    fn refresh_data(&mut self, _: &RefreshData, window: &mut Window, cx: &mut Context<Self>) {
        self.on_data_tab(window, cx, |tab, window, cx| tab.refresh(window, cx));
    }

    fn add_row(&mut self, _: &AddRow, window: &mut Window, cx: &mut Context<Self>) {
        self.on_data_tab(window, cx, |tab, window, cx| tab.add_row(window, cx));
    }

    fn duplicate_rows(&mut self, _: &DuplicateRows, window: &mut Window, cx: &mut Context<Self>) {
        self.on_data_tab(window, cx, |tab, window, cx| {
            tab.duplicate_rows(None, window, cx)
        });
    }

    fn delete_rows(&mut self, _: &DeleteRows, window: &mut Window, cx: &mut Context<Self>) {
        self.on_data_tab(window, cx, |tab, window, cx| {
            tab.delete_rows(None, window, cx)
        });
    }

    fn set_null(&mut self, _: &SetNull, window: &mut Window, cx: &mut Context<Self>) {
        self.on_data_tab(window, cx, |tab, window, cx| {
            tab.set_selected(CellValue::Null, window, cx)
        });
    }

    fn set_default(&mut self, _: &SetDefault, window: &mut Window, cx: &mut Context<Self>) {
        self.on_data_tab(window, cx, |tab, window, cx| {
            tab.set_selected(CellValue::Default, window, cx)
        });
    }

    fn apply_changes(&mut self, _: &ApplyChanges, window: &mut Window, cx: &mut Context<Self>) {
        self.on_data_tab(window, cx, |tab, window, cx| tab.review(window, cx));
    }

    fn discard_changes(&mut self, _: &DiscardChanges, window: &mut Window, cx: &mut Context<Self>) {
        self.on_data_tab(window, cx, |tab, _, cx| tab.discard(cx));
    }

    fn find_in_editor(&mut self, _: &FindInEditor, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_query_tab() {
            tab.update(cx, |tab, cx| tab.find(false, window, cx));
        }
    }

    fn replace_in_editor(
        &mut self,
        _: &ReplaceInEditor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.active_query_tab() {
            tab.update(cx, |tab, cx| tab.find(true, window, cx));
        }
    }

    fn cancel_query(&mut self, _: &CancelQuery, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_query_tab() {
            tab.update(cx, |tab, cx| tab.cancel(cx));
        }
        if let Some(tab) = self.active_data_tab() {
            tab.update(cx, |tab, cx| tab.cancel(cx));
        }
    }

    // Connections --------------------------------------------------------------

    fn new_connection(&mut self, _: &NewConnection, window: &mut Window, cx: &mut Context<Self>) {
        ConnectionDialog::open(self.connections.clone(), None, window, cx);
    }

    fn on_list(&mut self, action: SidebarAction, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.perform(&Subject::List, action, window, cx)
        });
    }

    fn new_connection_folder(
        &mut self,
        _: &NewConnectionFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_list(SidebarAction::NewFolder, window, cx);
    }

    fn refresh_connections(
        &mut self,
        _: &RefreshConnections,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_list(SidebarAction::Refresh, window, cx);
    }

    fn collapse_connection_folders(
        &mut self,
        _: &CollapseConnectionFolders,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_list(SidebarAction::CollapseAll, window, cx);
    }

    fn refresh_schema_tree(
        &mut self,
        _: &RefreshSchemaTree,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let navigator = self.sidebar.read(cx).navigator().clone();
        navigator.update(cx, |navigator, cx| navigator.reload(cx));
    }

    fn collapse_schema_tree(
        &mut self,
        _: &CollapseSchemaTree,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let navigator = self.sidebar.read(cx).navigator().clone();
        navigator.update(cx, |navigator, cx| navigator.collapse_all(cx));
    }

    fn on_sidebar_event(
        &mut self,
        _: &Entity<Sidebar>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SidebarEvent::ImportConnections => {
                self.import_connections(&ImportConnections, window, cx)
            }
            SidebarEvent::NewEditor(id) => {
                if matches!(
                    self.connections.read(cx).status(id),
                    Status::Connected { .. }
                ) {
                    self.new_query_on(Some(connection_target(id)), window, cx);
                } else {
                    // Connect first (with its prompts); the tab follows.
                    self.pending_editor = Some(id.clone());
                    self.connections
                        .update(cx, |connections, cx| connections.connect(id, cx));
                }
            }
        }
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
                if self.pending_editor.as_ref() == Some(id) {
                    match self.connections.read(cx).status(id) {
                        Status::Connected { .. } => {
                            self.pending_editor = None;
                            self.new_query_on(Some(connection_target(id)), window, cx);
                        }
                        Status::Disconnected | Status::Failed { .. } => self.pending_editor = None,
                        Status::Connecting | Status::WaitingForInput => {}
                    }
                }
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
                Tab::new().label(tab.label(cx)).suffix(
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
                    .map(|tab| div().flex_1().min_h_0().child(tab.view())),
            )
            .into_any_element()
    }

    /// What the status bar says, left to right: the active tab's
    /// connection, database, transaction, rows and timing; without a tab,
    /// how many connections are open.
    fn status_items(&self, cx: &App) -> Vec<(String, Option<gpui_kit::Hsla>)> {
        let theme = cx.theme();
        let status = self.tabs.get(self.active_tab).map(|tab| tab.status(cx));
        let Some(TabStatus {
            connection: Some(connection),
            read_only,
            database,
            transaction,
            running,
            rows,
            elapsed,
            notices,
            pending,
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
        if pending > 0 {
            items.push((
                format!(
                    "{pending} pending change{}",
                    if pending == 1 { "" } else { "s" }
                ),
                Some(theme.warning),
            ));
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

/// Where a tab runs on a connection: its own database and schema.
fn connection_target(id: &str) -> Target {
    Target {
        connection: id.to_string(),
        database: None,
        schema: None,
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
        *self.tab_target.borrow_mut() = self
            .active_query_tab()
            .and_then(|tab| tab.read(cx).target().cloned());
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
            .on_action(cx.listener(Self::toggle_comment))
            .on_action(cx.listener(Self::format_sql))
            .on_action(cx.listener(Self::find_in_editor))
            .on_action(cx.listener(Self::refresh_data))
            .on_action(cx.listener(Self::export_data))
            .on_action(cx.listener(Self::import_csv))
            .on_action(cx.listener(Self::add_row))
            .on_action(cx.listener(Self::duplicate_rows))
            .on_action(cx.listener(Self::delete_rows))
            .on_action(cx.listener(Self::set_null))
            .on_action(cx.listener(Self::set_default))
            .on_action(cx.listener(Self::apply_changes))
            .on_action(cx.listener(Self::discard_changes))
            .on_action(cx.listener(Self::replace_in_editor))
            .on_action(cx.listener(Self::explain_statement))
            .on_action(cx.listener(Self::explain_analyze))
            .on_action(cx.listener(Self::import_connections))
            .on_action(cx.listener(Self::export_connections))
            .on_action(cx.listener(Self::new_connection_folder))
            .on_action(cx.listener(Self::refresh_connections))
            .on_action(cx.listener(Self::collapse_connection_folders))
            .on_action(cx.listener(Self::refresh_schema_tree))
            .on_action(cx.listener(Self::collapse_schema_tree))
            .on_action(cx.listener(|this, _: &EditObject, window, cx| {
                let navigator = this.sidebar.read(cx).navigator().clone();
                navigator.update(cx, |navigator, cx| navigator.edit_selected(window, cx));
            }))
            .on_action(cx.listener(|this, _: &RenameObject, window, cx| {
                this.on_tree(TreeAction::Rename, window, cx)
            }))
            .on_action(cx.listener(|this, _: &DropObject, window, cx| {
                this.on_tree(TreeAction::Drop, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RefreshObject, window, cx| {
                this.on_tree(TreeAction::Refresh, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyObjectName, window, cx| {
                this.on_tree(TreeAction::CopyName, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyQualifiedName, window, cx| {
                this.on_tree(TreeAction::CopyQualifiedName, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewQueryOnObject, window, cx| {
                this.on_tree(TreeAction::NewQueryHere, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SetDefaultSchema, window, cx| {
                this.on_tree(TreeAction::SetDefault, window, cx)
            }))
            .on_action(cx.listener(|this, _: &EditConnection, window, cx| {
                this.sidebar
                    .update(cx, |sidebar, cx| sidebar.edit_selected(window, cx))
            }))
            .on_action(cx.listener(|this, _: &OpenConnection, _, cx| {
                this.sidebar
                    .update(cx, |sidebar, cx| sidebar.open_selected(cx))
            }))
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
