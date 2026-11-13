//! The main window: title bar, sidebar, tabs and status bar, and the
//! commands that act on them.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, TitleBar, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, AnyView, App, AppContext as _, BorrowAppContext as _, ClipboardItem,
    Context, Entity, EntityId, FocusHandle, Focusable, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, PathPromptOptions, Render, ScrollHandle,
    SharedString, Styled as _, Subscription, Task, TestSupportExt as _, WeakEntity, Window, div,
    px,
};
use kerno_core::config::{
    ConnectionConfig, Secret, export_connections_with_folders, import_connections_file,
};

use crate::actions::{
    self, About, AddRow, ApplyChanges, BackupDatabase, CancelQuery, CloseAllTabs, CloseOtherTabs,
    CloseTab, CloseTabsToTheRight, CollapseConnectionFolders, CollapseSchemaTree,
    CommitTransaction, CopyObjectName, CopyQualifiedName, CopyTabFilePath, DeleteRows,
    DiscardChanges, DisconnectTabConnection, DropObject, DuplicateRows, DuplicateTab,
    EditConnection, EditObject, EditTabConnection, ExplainAnalyze, ExplainStatement,
    ExportConnections, ExportData, FindInEditor, FormatSql, ImportConnections, ImportCsv,
    NewConnection, NewConnectionFolder, NewQueryOnObject, NewQueryTab, NextTab, OpenAdmin,
    OpenCommandPalette, OpenConnection, OpenSettings, OpenSqlFile, PreviousTab,
    ReconnectTabConnection, RefreshConnections, RefreshData, RefreshObject, RefreshSchemaTree,
    RenameObject, RenameTab, ReopenClosedTab, ReplaceInEditor, RestoreDatabase, RevealTabFile,
    RollbackTransaction, RunAll, RunStatement, SaveFile, SaveFileAs, SaveQuery, SetDefault,
    SetDefaultSchema, SetNull, ShowHistory, ShowSavedQueries, ToggleComment, ToggleCommitMode,
    ToggleSidebar, UseDarkTheme, UseLightTheme, UseSystemTheme,
};
use crate::actions::{
    CloseResultTab, CloseTabsToTheLeft, CountStatementRows, MaximizeResultsPanel, NextStatement,
    PreviousStatement, RunFromCursor, RunStatementInNewTab, SwitchResultsFocus, TogglePinnedResult,
    ToggleResultsPanel,
};
use crate::actions::{DisconnectAll, DisconnectOtherConnections, ToggleReadOnly};
use crate::admin::tab::AdminTab;
use crate::backup_dialog::{self, BackupDialog};
use crate::bridge;
use crate::connections::dialog::ConnectionDialog;
use crate::connections::manager::{
    Connections, ConnectionsEvent, InputAnswer, InputRequest, Status,
};
use crate::data::tab::{DataTab, DataTabEvent};
use crate::import_dialog::{self, ImportDialog};
use crate::library::{self, Library, Shelf};
use crate::navigator::actions::Action as TreeAction;
use crate::navigator::actions::FilterAction;
use crate::navigator::actions::ToolEntry;
use crate::navigator::view::{Navigator, NavigatorEvent};
use crate::query::grid::{Grid, GridAction, Panel, RowMove};
use crate::query::tab::{QueryTab, QueryTabEvent, Scope, TabStatus, duration_text};
use crate::sidebar::{Sidebar, SidebarAction, SidebarEvent, Subject};
use crate::state::{ACTIVE_TAB_KEY, AppState};
use crate::structure::editor::{TableEditor, TableEditorEvent};
use crate::structure::tab::{PROPERTIES_SECTION, StructureTab, StructureTabEvent};
use crate::tabs::{
    self, Entry, Loss, StatusFacts, StatusItem, TabAction, TabKind, TabSnapshot, Target,
};
use crate::tasks_view::TasksView;
use crate::theme::{self, Appearance};
use gpui_kit::StatefulInteractiveElement as _;
use gpui_kit::WeakFocusHandle;
use gpui_kit::component::Disableable as _;
use kerno_core::TxState;
use kerno_core::browse::Filter;
use kerno_core::schema::{RelationKind, TableRef};
use kerno_core::sql::CellValue;
use kerno_core::storage::SavedQuery;

/// How long after the last change the open tabs are written to storage.
const SAVE_TABS_DELAY: Duration = Duration::from_millis(400);

/// How many closed query tabs can be reopened.
const CLOSED_TABS: usize = 10;

pub(crate) mod toolbar;
pub(crate) mod updates;

/// Where a menu of the central area opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuOf {
    /// The tab at this index.
    Tab(usize),
    /// The empty area of the tab bar.
    Bar,
    /// An item of the status bar.
    Status(StatusItem),
}

/// A tab of the central area.
#[derive(Clone)]
enum OpenTab {
    Query(Entity<QueryTab>),
    Data(Entity<DataTab>),
    Structure(Entity<StructureTab>),
    Admin(Entity<AdminTab>),
    /// Not restored after a restart: an unsaved draft is not worth keeping.
    TableEditor(Entity<TableEditor>),
    /// The saved tasks; not restored after a restart (reopened from Tools ▸
    /// Tasks or the palette).
    Tasks(Entity<TasksView>),
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

    fn admin(&self) -> Option<&Entity<AdminTab>> {
        match self {
            OpenTab::Admin(tab) => Some(tab),
            _ => None,
        }
    }

    fn table_editor(&self) -> Option<&Entity<TableEditor>> {
        match self {
            OpenTab::TableEditor(tab) => Some(tab),
            _ => None,
        }
    }

    fn tasks(&self) -> Option<&Entity<TasksView>> {
        match self {
            OpenTab::Tasks(tab) => Some(tab),
            _ => None,
        }
    }

    /// A restored tab reads nothing, and opens no session, until it is
    /// shown.
    fn defer_until_shown(&self, cx: &mut App) {
        match self {
            OpenTab::Data(tab) => tab.update(cx, |tab, cx| tab.defer_until_shown(cx)),
            OpenTab::Structure(tab) => tab.update(cx, |tab, cx| tab.defer_until_shown(cx)),
            OpenTab::Admin(tab) => tab.update(cx, |tab, cx| tab.defer_until_shown(cx)),
            // A query tab opens its session on its first run.
            OpenTab::Query(_) | OpenTab::TableEditor(_) | OpenTab::Tasks(_) => {}
        }
    }

    /// The tab is on screen: a deferred one reads what it shows.
    fn reveal(&self, window: &mut Window, cx: &mut App) {
        match self {
            OpenTab::Data(tab) => tab.update(cx, |tab, cx| tab.reveal(window, cx)),
            OpenTab::Structure(tab) => tab.update(cx, |tab, cx| tab.reveal(window, cx)),
            OpenTab::Admin(tab) => tab.update(cx, |tab, cx| tab.reveal(window, cx)),
            // Called at every frame: the Tasks view reads its list when it
            // opens and when a task changes, never from here.
            OpenTab::Query(_) | OpenTab::TableEditor(_) | OpenTab::Tasks(_) => {}
        }
    }

    /// Another tab is on screen: auto-refresh stops.
    fn hide(&self, cx: &mut App) {
        match self {
            OpenTab::Data(tab) => tab.update(cx, |tab, cx| tab.hide(cx)),
            OpenTab::Query(tab) => tab.update(cx, |tab, cx| tab.hide(cx)),
            OpenTab::Structure(_)
            | OpenTab::Admin(_)
            | OpenTab::TableEditor(_)
            | OpenTab::Tasks(_) => {}
        }
    }

    fn entity_id(&self) -> EntityId {
        match self {
            OpenTab::Query(tab) => tab.entity_id(),
            OpenTab::Data(tab) => tab.entity_id(),
            OpenTab::Structure(tab) => tab.entity_id(),
            OpenTab::Admin(tab) => tab.entity_id(),
            OpenTab::TableEditor(tab) => tab.entity_id(),
            OpenTab::Tasks(tab) => tab.entity_id(),
        }
    }

    fn kind(&self, cx: &App) -> TabKind {
        match self {
            OpenTab::Query(tab) => TabKind::Query {
                file: tab.read(cx).file().is_some(),
            },
            _ => TabKind::Other,
        }
    }

    /// What closing the tab would lose.
    fn losses(&self, cx: &App) -> Vec<Loss> {
        match self {
            OpenTab::Query(tab) => {
                let tab = tab.read(cx);
                let mut losses = Vec::new();
                match tab.tx_state() {
                    TxState::Idle => {}
                    TxState::Open => losses.push(Loss::Transaction),
                    TxState::Failed => losses.push(Loss::FailedTransaction),
                }
                if tab.file().is_some() && tab.unsaved() {
                    losses.push(Loss::Unsaved);
                }
                match tab.pending_count(cx) {
                    0 => {}
                    count => losses.push(Loss::Pending(count)),
                }
                losses
            }
            OpenTab::Data(tab) => match tab.read(cx).pending_count(cx) {
                0 => Vec::new(),
                count => vec![Loss::Pending(count)],
            },
            OpenTab::Structure(_) | OpenTab::Admin(_) | OpenTab::Tasks(_) => Vec::new(),
            OpenTab::TableEditor(tab) => {
                if tab.read(cx).dirty(cx) {
                    vec![Loss::Form]
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// The saved connection the tab works on.
    fn connection(&self, cx: &App) -> Option<String> {
        match self {
            OpenTab::Query(tab) => tab.read(cx).connection_id().map(str::to_string),
            OpenTab::Data(tab) => Some(tab.read(cx).target().connection.clone()),
            OpenTab::Structure(tab) => Some(tab.read(cx).target().connection.clone()),
            OpenTab::Admin(tab) => Some(tab.read(cx).target().connection.clone()),
            OpenTab::TableEditor(tab) => Some(tab.read(cx).target().connection.clone()),
            OpenTab::Tasks(_) => None,
        }
    }

    /// Where the tab works: connection, database and schema.
    fn target(&self, cx: &App) -> Option<Target> {
        match self {
            OpenTab::Query(tab) => tab.read(cx).target().cloned(),
            OpenTab::Data(tab) => Some(tab.read(cx).target().clone()),
            OpenTab::Structure(tab) => Some(tab.read(cx).target().clone()),
            OpenTab::Admin(tab) => Some(tab.read(cx).target().clone()),
            OpenTab::TableEditor(tab) => Some(tab.read(cx).target().clone()),
            OpenTab::Tasks(_) => None,
        }
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            OpenTab::Query(tab) => tab.focus_handle(cx),
            OpenTab::Data(tab) => tab.focus_handle(cx),
            OpenTab::Structure(tab) => tab.focus_handle(cx),
            OpenTab::Admin(tab) => tab.focus_handle(cx),
            OpenTab::TableEditor(tab) => tab.focus_handle(cx),
            OpenTab::Tasks(tab) => tab.focus_handle(cx),
        }
    }

    /// What is stored to open the tab again at the next launch, if anything.
    fn snapshot(&self, cx: &App) -> Option<TabSnapshot> {
        match self {
            OpenTab::Query(tab) => Some(tab.read(cx).snapshot(cx)),
            OpenTab::Data(tab) => Some(tab.read(cx).snapshot()),
            OpenTab::Structure(tab) => Some(tab.read(cx).snapshot()),
            OpenTab::Admin(tab) => Some(tab.read(cx).snapshot()),
            OpenTab::TableEditor(_) | OpenTab::Tasks(_) => None,
        }
    }

    fn title(&self, cx: &App) -> SharedString {
        match self {
            OpenTab::Query(tab) => tab.read(cx).title(),
            OpenTab::Data(tab) => tab.read(cx).title(),
            OpenTab::Structure(tab) => tab.read(cx).title(),
            OpenTab::Admin(tab) => tab.read(cx).title(cx),
            OpenTab::TableEditor(tab) => tab.read(cx).title(),
            OpenTab::Tasks(tab) => tab.read(cx).title(),
        }
    }

    /// The tab's name, marked when it holds something not saved or not
    /// applied yet.
    fn label(&self, cx: &App) -> SharedString {
        let marked = match self {
            OpenTab::Query(tab) => {
                let tab = tab.read(cx);
                tab.file().is_some() && tab.unsaved() || tab.pending_count(cx) > 0
            }
            OpenTab::Data(tab) => tab.read(cx).pending_count(cx) > 0,
            OpenTab::Structure(_) | OpenTab::Admin(_) | OpenTab::Tasks(_) => false,
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
            OpenTab::Admin(tab) => tab.read(cx).status(cx),
            OpenTab::TableEditor(tab) => tab.read(cx).status(cx),
            OpenTab::Tasks(tab) => tab.read(cx).status(cx),
        }
    }

    fn view(&self) -> AnyView {
        match self {
            OpenTab::Query(tab) => tab.clone().into(),
            OpenTab::Data(tab) => tab.clone().into(),
            OpenTab::Structure(tab) => tab.clone().into(),
            OpenTab::Admin(tab) => tab.clone().into(),
            OpenTab::TableEditor(tab) => tab.clone().into(),
            OpenTab::Tasks(tab) => tab.clone().into(),
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
    /// The last query tabs closed, the latest last; in memory only.
    closed: Vec<TabSnapshot>,
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
    /// The active tab, written on each frame: an administration tab
    /// refreshes on its own only while it is the one shown.
    shown_tab: Rc<Cell<Option<EntityId>>>,
    /// The scroll of the tab strip.
    tab_scroll: ScrollHandle,
    /// The active tab and the tab count the strip last scrolled for.
    revealed_tab: Option<(usize, usize)>,
    /// A dialog was open at the last frame: see `keep_focus`.
    dialog_open: bool,
    /// What had the keyboard in the window, while no dialog was open.
    last_focus: Option<WeakFocusHandle>,
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
            closed: Vec::new(),
            next_query_number: 1,
            palette,
            pending_save: None,
            pending_editor: None,
            tab_target,
            shown_tab: Rc::new(Cell::new(None)),
            tab_scroll: ScrollHandle::new(),
            revealed_tab: None,
            dialog_open: false,
            last_focus: None,
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
            workspace.push_snapshot(snapshot, window, cx);
            if let Some(tab) = workspace.tabs.last() {
                tab.defer_until_shown(cx);
            }
        }
        // The tab that was active is active again.
        workspace.active_tab = active_tab.min(workspace.tabs.len().saturating_sub(1));
        workspace
    }

    /// Opens a tab again from what was stored of it.
    fn push_snapshot(
        &mut self,
        snapshot: TabSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match snapshot {
            TabSnapshot::Query {
                title,
                sql,
                target,
                file,
                unsaved,
                source,
            } => {
                let tab = self.push_query_tab(title, &sql, target, window, cx);
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
                self.push_data_tab(
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
                self.push_structure_tab(target, TableRef::new(schema, table), kind, window, cx);
            }
            TabSnapshot::Admin { target } => {
                self.push_admin_tab(target, window, cx);
            }
        }
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

    /// The tab at `index`, when it shows a table's structure; for tests.
    #[cfg(test)]
    pub fn structure_tab_at(&self, index: usize) -> Option<Entity<StructureTab>> {
        self.tabs.get(index)?.structure().cloned()
    }

    /// The tab at `index`, when it shows a table's data; for tests.
    #[cfg(test)]
    pub fn data_tab_at(&self, index: usize) -> Option<Entity<DataTab>> {
        self.tabs.get(index)?.data().cloned()
    }

    /// The tab at `index`, when it administers a server; for tests.
    #[cfg(test)]
    pub fn admin_tab_at(&self, index: usize) -> Option<Entity<AdminTab>> {
        self.tabs.get(index)?.admin().cloned()
    }

    /// Whether the active tab is wholly inside the tab strip, and whether
    /// the strip overflows; for tests.
    #[cfg(test)]
    pub fn active_tab_in_view(&self) -> (bool, bool) {
        let strip = &self.tab_scroll;
        let (viewport, offset) = (strip.bounds(), strip.offset().x);
        // Item bounds are laid out unscrolled: on screen at x + offset.
        let inside = strip.bounds_for_item(self.active_tab).is_some_and(|tab| {
            tab.left() + offset >= viewport.left() - px(1.)
                && tab.right() + offset <= viewport.right() + px(1.)
        });
        (inside, strip.max_offset().x > px(0.))
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

    /// The active tab, when it administers a server.
    #[cfg(test)]
    pub fn active_admin_tab(&self) -> Option<&Entity<AdminTab>> {
        self.tabs.get(self.active_tab)?.admin()
    }

    /// The active tab, when it is the Tasks view.
    #[cfg(test)]
    pub fn active_tasks_tab(&self) -> Option<&Entity<TasksView>> {
        self.tabs.get(self.active_tab)?.tasks()
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
                    // In the table's schema; a MySQL table's is its database.
                    DataTabEvent::OpenQuery { title, sql } => {
                        let schema = tab.read(cx).table().schema.clone();
                        let has_schemas = this
                            .connections
                            .read(cx)
                            .meta_session(&target.connection)
                            .is_some_and(|meta| meta.dialect().has_schemas());
                        let target = if has_schemas {
                            Target {
                                schema: Some(schema),
                                ..target
                            }
                        } else {
                            Target {
                                database: Some(schema),
                                schema: None,
                                ..target
                            }
                        };
                        this.open_query(title.clone(), sql, Some(target), window, cx);
                    }
                    DataTabEvent::TableGone { table } => {
                        let navigator = this.sidebar.read(cx).navigator().clone();
                        navigator.update(cx, |navigator, cx| {
                            navigator.refresh_tables(
                                &target.connection,
                                target.database.as_deref(),
                                &table.schema,
                                cx,
                            )
                        });
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

    /// Opens the administration of a server in a tab of its own.
    fn push_admin_tab(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<AdminTab> {
        let (connections, shown) = (self.connections.clone(), self.shown_tab.clone());
        let tab = cx.new(|cx| AdminTab::new(target, connections, shown, window, cx));
        // One subscription per tab: `close_tab_at` removes them by index. The
        // tab's title follows the connection's name.
        self.tab_subscriptions
            .push(cx.observe(&tab, |_, _, cx| cx.notify()));
        self.tabs.push(OpenTab::Admin(tab.clone()));
        self.active_tab = self.tabs.len() - 1;
        tab
    }

    /// Shows the administration of a connection's server: in the tab that
    /// already shows it, else in a new one.
    pub fn open_admin(
        &mut self,
        connection: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<AdminTab> {
        let open = self.tabs.iter().position(|tab| {
            tab.admin()
                .is_some_and(|tab| tab.read(cx).target().connection == connection)
        });
        if let Some(index) = open {
            self.activate_tab(index, window, cx);
            return self.tabs[index].admin().expect("an admin tab").clone();
        }
        let tab = self.push_admin_tab(connection_target(connection), window, cx);
        self.activate_tab(self.tabs.len() - 1, window, cx);
        self.schedule_save(cx);
        tab
    }

    /// Runs a Filter ▸ entry of the schema tree on its selected node.
    fn filter_in_tree(
        &mut self,
        action: FilterAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let navigator = self.sidebar.read(cx).navigator().clone();
        navigator.update(cx, |navigator, cx| {
            navigator.filter_selected(action, window, cx)
        });
    }

    /// The palette's "Server Administration": of the active tab's
    /// connection, else of the one selected in the sidebar.
    fn open_admin_command(&mut self, _: &OpenAdmin, window: &mut Window, cx: &mut Context<Self>) {
        self.open_admin_here(window, cx);
    }

    /// The administration of the active tab's connection, else of the one
    /// selected in the sidebar; says so when there is none.
    fn open_admin_here(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<AdminTab>> {
        let connection = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.connection(cx))
            .or_else(|| self.sidebar.read(cx).selected(cx));
        match connection {
            Some(connection) => Some(self.open_admin(&connection, window, cx)),
            None => {
                window.push_notification(
                    Notification::warning("Server Administration: select a connection first."),
                    cx,
                );
                None
            }
        }
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
            cx.subscribe_in(
                &tab,
                window,
                |this, tab, event: &QueryTabEvent, window, cx| match event {
                    QueryTabEvent::SwitchTarget(target) => {
                        this.switch_target(tab.clone(), target.clone(), window, cx)
                    }
                    QueryTabEvent::OpenQuery { title, sql } => {
                        let target = tab.read(cx).target().cloned();
                        this.open_query(title.clone(), sql, target, window, cx);
                    }
                    QueryTabEvent::SourceApplied => {
                        let tab = tab.read(cx);
                        if let (Some(target), Some(schema)) = (tab.target().cloned(), tab.source())
                        {
                            let schema = schema.to_string();
                            let navigator = this.sidebar.read(cx).navigator().clone();
                            navigator.update(cx, |navigator, cx| {
                                navigator.refresh_source(&target, &schema, cx)
                            });
                        }
                    }
                },
            ),
        ));
        self.tabs.push(OpenTab::Query(tab.clone()));
        self.active_tab = self.tabs.len() - 1;
        tab
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(index) {
            self.active_tab = index;
            tab.reveal(window, cx);
            tab.focus_handle(cx).focus(window, cx);
            self.schedule_save(cx);
            if AppState::get(cx).settings.link_with_editor {
                self.link_active_tab(cx);
            }
            cx.notify();
        }
    }

    /// Selects the table of the active tab (data, structure or form) in the
    /// schema tree, and its connection in the list (DBeaver's "Link with
    /// editor", `core.navigator.linkeditor`).
    fn link_active_tab(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        let linked = if let Some(tab) = tab.data() {
            let tab = tab.read(cx);
            Some((tab.target().clone(), tab.table().clone(), tab.kind()))
        } else if let Some(tab) = tab.structure() {
            let tab = tab.read(cx);
            Some((tab.target().clone(), tab.table().clone(), tab.kind()))
        } else if let Some(tab) = tab.table_editor() {
            let tab = tab.read(cx);
            tab.table()
                .map(|table| (tab.target().clone(), table.clone(), RelationKind::Table))
        } else {
            None
        };
        let Some((target, table, kind)) = linked else {
            return;
        };
        let navigator = self.sidebar.read(cx).navigator().clone();
        navigator.update(cx, |navigator, cx| {
            navigator.reveal(&target, &table, kind, cx)
        });
    }

    /// Removes the tab at `index` without asking: see `close_tabs`.
    fn close_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        // Its server session is closed now, not whenever the tab is dropped
        // (its results may be held a while longer).
        match &tab {
            OpenTab::Query(tab) => tab.update(cx, |tab, cx| tab.close_session(cx)),
            OpenTab::Data(tab) => tab.update(cx, |tab, cx| tab.close_session(cx)),
            OpenTab::Admin(tab) => tab.update(cx, |tab, cx| tab.close_session(cx)),
            OpenTab::Structure(_) | OpenTab::TableEditor(_) | OpenTab::Tasks(_) => {}
        }
        // A closed query tab can be opened again, with its text.
        if let OpenTab::Query(tab) = tab {
            self.closed.push(tab.read(cx).snapshot(cx));
            if self.closed.len() > CLOSED_TABS {
                self.closed.remove(0);
            }
        }
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

    fn position(&self, tab: &OpenTab) -> Option<usize> {
        let id = tab.entity_id();
        self.tabs.iter().position(|open| open.entity_id() == id)
    }

    /// Closes `tabs`, after one question when any of them would lose
    /// something; nothing is closed when the question is cancelled.
    fn close_tabs(&mut self, tabs: Vec<OpenTab>, window: &mut Window, cx: &mut Context<Self>) {
        let losses: Vec<(String, Loss)> = tabs
            .iter()
            .flat_map(|tab| {
                let title = tab.title(cx).to_string();
                tab.losses(cx)
                    .into_iter()
                    .map(move |loss| (title.clone(), loss))
            })
            .collect();
        let confirm = AppState::get(cx).settings.confirm_close_tab;
        let Some((title, text, ok)) = tabs::close_question(&losses, confirm) else {
            self.close_now(&tabs, window, cx);
            return;
        };
        let workspace = cx.entity().downgrade();
        let (title, text) = (SharedString::from(title), SharedString::from(text));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (workspace, tabs) = (workspace.clone(), tabs.clone());
            alert
                .title(title.clone())
                .description(text.clone())
                .confirm()
                .ok_text(ok)
                .ok_variant(ButtonVariant::Danger)
                .on_ok({
                    let workspace = workspace.clone();
                    move |_, window, cx| {
                        // Once the dialog has closed, so the focus goes to
                        // the tab that is left.
                        let (workspace, tabs) = (workspace.clone(), tabs.clone());
                        window.defer(cx, move |window, cx| {
                            let _ =
                                workspace.update(cx, |this, cx| this.close_now(&tabs, window, cx));
                        });
                        true
                    }
                })
                // Cancelled too, the keyboard goes back to the active tab.
                .on_close(move |_, window, cx| {
                    let workspace = workspace.clone();
                    window.defer(cx, move |window, cx| {
                        let _ = workspace.update(cx, |this, cx| this.focus_active(window, cx));
                    });
                })
        });
    }

    /// Gives the keyboard to the active tab.
    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        match self.tabs.get(self.active_tab) {
            Some(tab) => tab.focus_handle(cx).focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
    }

    /// Closes those of `tabs` still open; the active tab stays active when
    /// it is not one of them.
    fn close_now(&mut self, tabs: &[OpenTab], window: &mut Window, cx: &mut Context<Self>) {
        let active = self.tabs.get(self.active_tab).cloned();
        for tab in tabs {
            let Some(index) = self.position(tab) else {
                continue;
            };
            if let OpenTab::Query(query) = tab {
                // Rolled back now, not whenever its session is dropped.
                query.update(cx, |query, cx| query.end_transaction(false, window, cx));
            }
            self.close_tab_at(index, window, cx);
        }
        if let Some(index) = active.and_then(|tab| self.position(&tab)) {
            self.activate_tab(index, window, cx);
        }
    }

    /// The entries of a menu, as they stand now.
    fn menu_entries(&self, of: MenuOf, cx: &App) -> Vec<Entry> {
        match of {
            MenuOf::Tab(index) => match self.tabs.get(index) {
                Some(tab) => tabs::tab_menu(tab.kind(cx), index, self.tabs.len()),
                None => Vec::new(),
            },
            MenuOf::Bar => tabs::bar_menu(self.tabs.len(), self.closed.len()),
            MenuOf::Status(item) => {
                let text = self
                    .status_items(cx)
                    .into_iter()
                    .find(|(kind, ..)| *kind == item)
                    .map(|(_, text, _)| text)
                    .unwrap_or_default();
                tabs::status_menu(item, &text, &self.status_facts(cx))
            }
        }
    }

    /// A menu, built when it opens.
    fn menu(
        &self,
        of: MenuOf,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let workspace = cx.entity();
        move |menu, window, cx| {
            let entries = workspace.read(cx).menu_entries(of, cx);
            let workspace = workspace.clone();
            let index = match of {
                MenuOf::Tab(index) => Some(index),
                _ => None,
            };
            let menu = tabs::fill_menu(
                menu,
                entries,
                Rc::new(move |action, window, cx| {
                    let workspace = workspace.clone();
                    // Once the menu has closed. A status bar item keeps the
                    // focus it took (and may go away): the keyboard goes back
                    // to the tab before the entry runs, so a dialog it opens
                    // gives the focus back there.
                    window.defer(cx, move |window, cx| {
                        workspace.update(cx, |this, cx| {
                            if matches!(of, MenuOf::Status(_)) || window.focused(cx).is_none() {
                                this.focus_active(window, cx);
                            }
                            this.perform(index, action, window, cx);
                        })
                    });
                }),
                window,
                cx,
            );
            tabs::fit_window(menu, window)
        }
    }

    /// What the status bar menus depend on.
    fn status_facts(&self, cx: &App) -> StatusFacts {
        let tab = self.tabs.get(self.active_tab);
        let status = tab.map(|tab| tab.status(cx));
        let query = tab.and_then(OpenTab::query).map(|tab| tab.read(cx));
        let connection = tab.and_then(|tab| tab.connection(cx));
        StatusFacts {
            query: query.is_some(),
            connected: connection.as_ref().is_some_and(|id| {
                matches!(
                    self.connections.read(cx).status(id),
                    Status::Connected { .. }
                )
            }),
            databases: query
                .map(|tab| tab.databases().to_vec())
                .unwrap_or_default(),
            database: status.as_ref().and_then(|status| status.database.clone()),
            transaction: status
                .as_ref()
                .map_or(TxState::Idle, |status| status.transaction),
            manual_commit: query.is_some_and(|tab| tab.manual_commit()),
            running: status.is_some_and(|status| status.running),
            open_connections: self.connections.read(cx).connected_count(),
        }
    }

    /// Does what a menu entry (or its command) says, on the tab at `index`
    /// or the active one.
    pub fn perform(
        &mut self,
        index: Option<usize>,
        action: TabAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = index.unwrap_or(self.active_tab);
        let tab = self.tabs.get(index).cloned();
        let query = tab.as_ref().and_then(OpenTab::query).cloned();
        let file = query
            .as_ref()
            .and_then(|tab| tab.read(cx).file().map(Path::to_path_buf));
        // A command run from the palette where its menu entry would be
        // disabled says why, and does nothing.
        let offered = match (&tab, &action) {
            (
                Some(tab),
                TabAction::Rename | TabAction::Duplicate | TabAction::CopyPath | TabAction::Reveal,
            ) => Some(tabs::tab_menu(tab.kind(cx), index, self.tabs.len())),
            (_, TabAction::Disconnect | TabAction::Reconnect) => Some(tabs::status_menu(
                StatusItem::Connection,
                "",
                &self.status_facts(cx),
            )),
            _ => None,
        };
        if let Some(entries) = offered {
            let reason = entries
                .into_iter()
                .find_map(|entry| match entry {
                    Entry::Item(offered, reason) if offered == action => Some(reason),
                    _ => None,
                })
                .unwrap_or(Some("the tab has no file"));
            if let Some(reason) = reason {
                window.push_notification(
                    Notification::warning(SharedString::from(format!(
                        "{}: {reason}.",
                        action.label().trim_end_matches('…')
                    ))),
                    cx,
                );
                return;
            }
        }
        let connection = tab.as_ref().and_then(|tab| tab.connection(cx));
        match action {
            TabAction::Close => self.close_tabs(tab.into_iter().collect(), window, cx),
            TabAction::CloseOthers => {
                let others = self
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, tab)| tab.clone())
                    .collect();
                self.close_tabs(others, window, cx)
            }
            TabAction::CloseToRight => {
                let right = self.tabs.iter().skip(index + 1).cloned().collect();
                self.close_tabs(right, window, cx)
            }
            TabAction::CloseAll => self.close_tabs(self.tabs.clone(), window, cx),
            TabAction::Rename => {
                if let Some(query) = query {
                    self.rename_tab(query, window, cx)
                }
            }
            TabAction::Duplicate => {
                if let Some(query) = query {
                    let (sql, target) = {
                        let query = query.read(cx);
                        (query.sql(cx), query.target().cloned())
                    };
                    let title = format!("Query {}", self.next_query_number);
                    self.open_query(title, &sql, target, window, cx);
                }
            }
            TabAction::CopyPath => {
                if let Some(file) = file {
                    cx.write_to_clipboard(ClipboardItem::new_string(file.display().to_string()))
                }
            }
            TabAction::Reveal => {
                if let Some(file) = file {
                    cx.reveal_path(&file)
                }
            }
            TabAction::NewQuery => self.new_query_tab(&NewQueryTab, window, cx),
            TabAction::ReopenClosed => {
                if let Some(snapshot) = self.closed.pop() {
                    self.push_snapshot(snapshot, window, cx);
                    self.activate_tab(self.tabs.len() - 1, window, cx);
                }
            }
            TabAction::Disconnect => {
                if let Some(id) = connection {
                    let action = format!("Disconnect “{}”", self.connection_name(&id, cx));
                    self.disconnect(vec![id], action, window, cx);
                }
            }
            TabAction::Reconnect => {
                if let Some(id) = connection {
                    self.reconnect(id, window, cx);
                }
            }
            TabAction::Administration => {
                if let Some(id) = connection {
                    self.open_admin(&id, window, cx);
                }
            }
            TabAction::EditConnection => {
                if let Some(id) = connection {
                    Sidebar::edit(self.connections.clone(), id, window, cx)
                }
            }
            TabAction::UseDatabase(database) => {
                if let Some(query) = query {
                    let target = query.read(cx).target().cloned().map(|target| Target {
                        database: Some(database),
                        schema: None,
                        ..target
                    });
                    self.switch_target(query, target, window, cx);
                }
            }
            TabAction::Commit | TabAction::Rollback => {
                if let Some(query) = query {
                    let commit = action == TabAction::Commit;
                    query.update(cx, |query, cx| query.end_transaction(commit, window, cx));
                }
            }
            TabAction::TransactionLog => match query {
                Some(query) => query.update(cx, |query, cx| query.show_transaction_log(window, cx)),
                None => window.push_notification(
                    Notification::warning(
                        "Transaction log: open a query tab; its transaction's statements are \
                         listed.",
                    ),
                    cx,
                ),
            },
            TabAction::ManualCommit(manual) => {
                if let Some(query) = query {
                    query.update(cx, |query, cx| query.set_manual_commit(manual, cx));
                }
            }
            TabAction::Cancel => self.cancel_query(&CancelQuery, window, cx),
            TabAction::ReviewChanges => self.apply_changes(&ApplyChanges, window, cx),
            TabAction::DiscardChanges => self.discard_changes(&DiscardChanges, window, cx),
            TabAction::CopyText(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            TabAction::AboutReadOnly => window.open_alert_dialog(cx, |alert, _, _| {
                alert.title("A read-only connection").description(
                    "Kerno refuses statements that write before they are sent, and the server                      session is opened read-only. Edit the connection and clear “Read-only” to                      change data from Kerno.",
                )
            }),
            TabAction::AboutMemoryOnly => window.open_alert_dialog(cx, |alert, _, _| {
                alert.title("Passwords kept in memory only").description(
                    "No system secret store (macOS Keychain or a Secret Service on Linux) can be                      used, so Kerno writes no password to disk: each password is asked for when                      you connect and forgotten when Kerno quits. Start a Secret Service (GNOME                      Keyring, KWallet) and restart Kerno to save them.",
                )
            }),
            TabAction::ShowMessages => {
                if let Some(query) = query {
                    query.update(cx, |query, cx| query.show_messages(cx));
                }
            }
            TabAction::NewConnection => self.new_connection(&NewConnection, window, cx),
            TabAction::DisconnectAll => self.disconnect_all(&DisconnectAll, window, cx),
            TabAction::Update
            | TabAction::OpenReleasePage
            | TabAction::SkipVersion
            | TabAction::TurnOffUpdateChecks => self.update_action(action, window, cx),
            // The editor runs its entries itself.
            other => tracing::warn!(?other, "not an action of the window"),
        }
    }

    /// Asks for a new title for a query tab.
    fn rename_tab(&mut self, tab: Entity<QueryTab>, window: &mut Window, cx: &mut Context<Self>) {
        let current = tab.read(cx).title();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let workspace = cx.entity().downgrade();
        let submit = {
            let input = input.clone();
            Rc::new(move |cx: &mut App| {
                let name = input.read(cx).value().trim().to_string();
                if name.is_empty() {
                    return false;
                }
                tab.update(cx, |tab, cx| tab.set_title(name, cx));
                let _ = workspace.update(cx, |this, cx| {
                    this.schedule_save(cx);
                    cx.notify();
                });
                true
            })
        };
        let focus = input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, on_enter, on_click) = (input.clone(), submit.clone(), submit.clone());
            dialog
                .title("Rename tab")
                .w(px(380.))
                .on_ok(move |_, _, cx| on_enter(cx))
                .content(move |content, _, _| {
                    let on_click = on_click.clone();
                    content.child(
                        v_flex()
                            .gap_3()
                            .child(Input::new(&input).id("tab-name"))
                            .child(
                                h_flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        Button::new("tab-name-cancel")
                                            .ghost()
                                            .label("Cancel")
                                            .on_click(|_, window, cx| window.close_dialog(cx)),
                                    )
                                    .child(
                                        Button::new("tab-name-ok")
                                            .primary()
                                            .label("Rename")
                                            .on_click(move |_, window, cx| {
                                                if on_click(cx) {
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
        self.perform(None, TabAction::Close, window, cx);
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

    pub(crate) fn set_appearance(appearance: Appearance, window: &mut Window, cx: &mut App) {
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
        crate::settings_view::SettingsView::open(self.connections.clone(), window, cx);
    }

    fn about(&mut self, _: &About, window: &mut Window, cx: &mut Context<Self>) {
        window.open_alert_dialog(cx, |alert, _, cx| {
            alert.width(px(440.)).child(crate::brand::about(cx))
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
        // child of: on confirm the action is dispatched from what had the
        // keyboard before (the editor, the tree), which keeps it.
        let target = self
            .last_focus()
            .filter(|last| self.focus_handle.contains(last, window))
            .unwrap_or_else(|| self.focus_handle.clone());
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
                    // Ranked for what is typed: the list follows the query.
                    let query = palette.read(cx).query(cx);
                    let confirmed = palette.clone();
                    let mut command = Command::new(&palette)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder("Type a command")
                        .max_h(px(420.))
                        .on_confirm(move |index, window, cx| {
                            window.close_dialog(cx);
                            let query = confirmed.read(cx).query(cx);
                            if let Some(command) = palette_command(&query, index.section, index.row)
                            {
                                let action = (command.action)();
                                let target = target.clone();
                                window.defer(cx, move |window, cx| {
                                    target.focus(window, cx);
                                    target.dispatch_action(action.as_ref(), window, cx);
                                });
                            }
                        });
                    for (group, commands) in palette_groups(&query) {
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
            NavigatorEvent::RunQuery { title, sql, target } => {
                let tab = self.open_query(title.clone(), sql, target.clone(), window, cx);
                tab.update(cx, |tab, cx| tab.run(Scope::All, window, cx));
            }
            NavigatorEvent::EditTable {
                target,
                table,
                column,
            } => {
                let editor = self.open_table_editor(
                    target.clone(),
                    table.schema.clone(),
                    Some(table.clone()),
                    window,
                    cx,
                );
                editor.update(cx, |editor, cx| {
                    editor.focus_column(column.clone(), window, cx)
                });
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
            NavigatorEvent::ShowProperties {
                target,
                table,
                kind,
            } => {
                let tab = self.open_structure(target.clone(), table.clone(), *kind, window, cx);
                tab.update(cx, |tab, cx| tab.select(PROPERTIES_SECTION, cx));
            }
            NavigatorEvent::Warn(text) => window
                .push_notification(Notification::warning(SharedString::from(text.clone())), cx),
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
            NavigatorEvent::Tool {
                target,
                entry,
                tables,
            } => self.run_tool(target.clone(), entry.clone(), tables.clone(), window, cx),
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
    /// selected row: on a folder or a connection, the sidebar's entry that
    /// does the same (F2 renames, Delete deletes, F5 refreshes…).
    fn on_tree(&mut self, action: TreeAction, window: &mut Window, cx: &mut Context<Self>) {
        let navigator = self.sidebar.read(cx).navigator().clone();
        if let Some(subject) = navigator.read(cx).selected_subject(cx) {
            let mapped = match (&subject, action) {
                (_, TreeAction::Rename) => Some(SidebarAction::Rename),
                (_, TreeAction::Drop) => Some(SidebarAction::Delete),
                (_, TreeAction::Refresh) => Some(SidebarAction::Refresh),
                (Subject::Connection(_), TreeAction::CopyName) => Some(SidebarAction::CopyName),
                (Subject::Connection(_), TreeAction::NewQueryHere) => {
                    Some(SidebarAction::NewSqlEditor)
                }
                _ => None,
            };
            if let Some(mapped) = mapped {
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar.perform(&subject, mapped, window, cx)
                });
            }
            return;
        }
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
    pub fn open_query(
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
                    AppState::update_settings(cx, |settings| {
                        settings.remember_script(path.clone())
                    });
                    tab.update(cx, |tab, cx| tab.set_file(path, false, cx));
                    this.schedule_save(cx);
                }
                Err(error) => {
                    // A recent script that went away leaves the list.
                    AppState::update_settings(cx, |settings| {
                        settings.recent_scripts.retain(|recent| *recent != path)
                    });
                    window.push_notification(
                        Notification::error(SharedString::from(format!(
                            "{} could not be opened: {error}",
                            path.display()
                        ))),
                        cx,
                    )
                }
            });
        })
        .detach();
    }

    fn save_file(&mut self, _: &SaveFile, window: &mut Window, cx: &mut Context<Self>) {
        // A table's data saves its pending changes, as DBeaver's Ctrl+S
        // does, through the review: nothing is written unseen.
        if let Some(tab) = self.active_data_tab().cloned() {
            tab.update(cx, |tab, cx| tab.review(window, cx));
            return;
        }
        // The same for the changes of a result, while the results have the
        // keyboard; the editor's text is saved otherwise.
        if let Some(tab) = self.active_query_tab().cloned()
            && tab.read(cx).results_focused(window, cx)
            && tab.read(cx).pending_count(cx) > 0
        {
            tab.update(cx, |tab, cx| tab.review(window, cx));
            return;
        }
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
                    AppState::update_settings(cx, |settings| {
                        settings.remember_script(path.clone())
                    });
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

    /// Runs a command on the active query tab, when there is one.
    fn on_query(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        command: impl FnOnce(&mut QueryTab, &mut Window, &mut Context<QueryTab>),
    ) {
        if let Some(tab) = self.active_query_tab() {
            tab.update(cx, |tab, cx| command(tab, window, cx));
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

    // Backup and restore ---------------------------------------------------

    /// The palette's "Back Up Database…" / "Restore Database…": the database
    /// of the active tab, else the default one of the connection the tree
    /// shows.
    fn backup_restore_command(
        &mut self,
        restore: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.target(cx))
            .or_else(|| self.default_target(cx));
        match target {
            Some(target) => {
                self.backup_restore(target, restore, window, cx);
            }
            None => window.push_notification(
                Notification::warning(
                    "Connect first: right-click a database in the tree and choose “Back up…” \
                     or “Restore…”.",
                ),
                cx,
            ),
        }
    }

    /// A Tools ▸ entry from the palette or the menu bar: on the database of
    /// the active tab, else on the default one of the connection the tree
    /// shows.
    fn tool_command(&mut self, entry: ToolEntry, window: &mut Window, cx: &mut Context<Self>) {
        let target = self
            .tabs
            .get(self.active_tab)
            .and_then(|tab| tab.target(cx))
            .or_else(|| self.default_target(cx));
        match target {
            Some(target) => self.run_tool(target, entry, Vec::new(), window, cx),
            None => window.push_notification(
                Notification::warning(SharedString::from(format!(
                    "{}: connect first, or right-click a database in the tree ▸ Tools.",
                    entry.label().trim_end_matches('…')
                ))),
                cx,
            ),
        }
    }

    /// Opens the backup of the database of `target`, or the restore of a
    /// file into it. A restore refreshes that database in the tree.
    pub fn backup_restore(
        &mut self,
        target: Target,
        restore: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<BackupDialog>> {
        let mode = if restore {
            backup_dialog::Mode::Restore
        } else {
            backup_dialog::Mode::Backup
        };
        let dialog = self.open_wizard(target, mode, window, cx)?;
        let workspace = cx.entity().downgrade();
        dialog.update(cx, |dialog, cx| {
            dialog.set_purpose(
                backup_dialog::Purpose::Run,
                move |cx| reload_tasks(&workspace, cx),
                cx,
            )
        });
        Some(dialog)
    }

    /// Opens the wizard of `mode` on the database of `target`. A restore
    /// or a script refreshes that database in the tree.
    fn open_wizard(
        &mut self,
        target: Target,
        mode: backup_dialog::Mode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<BackupDialog>> {
        let navigator = self.sidebar.read(cx).navigator().clone();
        open_wizard_with(
            self.connections.clone(),
            navigator,
            target,
            mode,
            window,
            cx,
        )
    }

    /// Does what a Tools ▸ entry says on `target` (maintenance tools open in
    /// the tree): a wizard, with only `tables` or the schema of `target`
    /// ticked for a backup; the administration on its sub-tab; the foreign
    /// data wrappers; a new task; the Tasks view.
    pub fn run_tool(
        &mut self,
        target: Target,
        entry: ToolEntry,
        tables: Vec<TableRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use backup_dialog::Mode;
        let mode = match &entry {
            ToolEntry::Backup => Some(Mode::Backup),
            ToolEntry::GlobalBackup => Some(Mode::GlobalBackup),
            ToolEntry::Restore => Some(Mode::Restore),
            ToolEntry::ExecuteScript => Some(Mode::ExecuteScript),
            _ => None,
        };
        if let Some(mode) = mode {
            let schema = target.schema.clone();
            let workspace = cx.entity().downgrade();
            if let Some(dialog) = self.open_wizard(target, mode, window, cx) {
                dialog.update(cx, |dialog, cx| {
                    dialog.set_purpose(
                        backup_dialog::Purpose::Run,
                        move |cx| reload_tasks(&workspace, cx),
                        cx,
                    );
                    if mode == Mode::Backup {
                        if !tables.is_empty() {
                            dialog.only_tables(tables, cx);
                        } else if let Some(schema) = schema {
                            dialog.only_schemas(vec![schema], cx);
                        }
                    }
                });
            }
            return;
        }
        match entry {
            ToolEntry::Dashboard => {
                self.open_admin(&target.connection, window, cx);
            }
            ToolEntry::SessionManager | ToolEntry::LockManager => {
                let section = if entry == ToolEntry::LockManager {
                    crate::admin::tab::LOCKS
                } else {
                    crate::admin::tab::SESSIONS
                };
                let tab = self.open_admin(&target.connection, window, cx);
                tab.update(cx, |tab, cx| tab.select(section, window, cx));
            }
            ToolEntry::ForeignDataWrappers => {
                crate::fdw_dialog::open(self.connections.clone(), target, window, cx);
            }
            ToolEntry::NewTask => self.new_task(target, tables, window, cx),
            ToolEntry::Tasks => {
                self.open_tasks(window, cx);
            }
            ToolEntry::Backup
            | ToolEntry::GlobalBackup
            | ToolEntry::Restore
            | ToolEntry::ExecuteScript
            | ToolEntry::Maintain(_) => {}
        }
    }

    /// Tools ▸ Create new task…: the tool to make a task of, then its
    /// wizard, whose main button saves the task.
    fn new_task(
        &mut self,
        target: Target,
        tables: Vec<TableRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self
            .connections
            .read(cx)
            .get(&target.connection)
            .map(|saved| saved.config.engine)
        else {
            return;
        };
        let workspace = cx.entity().downgrade();
        crate::tasks_view::pick_tool(engine, window, cx, move |mode, window, cx| {
            let (target, tables, workspace) = (target.clone(), tables.clone(), workspace.clone());
            let _ = workspace.clone().update(cx, |this, cx| {
                let schema = target.schema.clone();
                if let Some(dialog) = this.open_wizard(target, mode, window, cx) {
                    dialog.update(cx, |dialog, cx| {
                        dialog.set_purpose(
                            backup_dialog::Purpose::NewTask,
                            move |cx| reload_tasks(&workspace, cx),
                            cx,
                        );
                        if mode == backup_dialog::Mode::Backup {
                            if !tables.is_empty() {
                                dialog.only_tables(tables, cx);
                            } else if let Some(schema) = schema {
                                dialog.only_schemas(vec![schema], cx);
                            }
                        }
                    });
                }
            });
        });
    }

    /// Shows the Tasks view: the tab that shows it, else a new one.
    pub fn open_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<TasksView> {
        if let Some(index) = self.tabs.iter().position(|tab| tab.tasks().is_some()) {
            self.activate_tab(index, window, cx);
            let tab = self.tabs[index].tasks().expect("the tasks tab").clone();
            tab.update(cx, |tab, cx| tab.reload(cx));
            return tab;
        }
        let (connections, navigator) = (
            self.connections.clone(),
            self.sidebar.read(cx).navigator().clone(),
        );
        let open: Rc<crate::tasks_view::OpenWizard> = Rc::new(move |target, mode, window, cx| {
            open_wizard_with(
                connections.clone(),
                navigator.clone(),
                target,
                mode,
                window,
                cx,
            )
        });
        let connections = self.connections.clone();
        let tab = cx.new(|cx| TasksView::new(connections, open, cx));
        self.tab_subscriptions
            .push(cx.observe(&tab, |_, _, cx| cx.notify()));
        self.tabs.push(OpenTab::Tasks(tab.clone()));
        self.activate_tab(self.tabs.len() - 1, window, cx);
        tab
    }

    /// Reads the tasks again in the Tasks view, if it is open.
    fn reload_tasks(&mut self, cx: &mut Context<Self>) {
        for tab in self.tabs.iter().filter_map(OpenTab::tasks) {
            tab.update(cx, |tab, cx| tab.reload(cx));
        }
    }

    fn refresh_data(&mut self, _: &RefreshData, window: &mut Window, cx: &mut Context<Self>) {
        self.on_data_tab(window, cx, |tab, window, cx| tab.refresh(window, cx));
    }

    /// Runs a command on the rows on screen: a table's data, or the result
    /// of the active query tab (which says why when its rows take no
    /// change).
    fn on_rows(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        data: impl FnOnce(&mut DataTab, &mut Window, &mut Context<DataTab>),
        query: impl FnOnce(&mut QueryTab, &mut Window, &mut Context<QueryTab>),
    ) {
        if let Some(tab) = self.active_data_tab().cloned() {
            tab.update(cx, |tab, cx| data(tab, window, cx));
        } else if let Some(tab) = self.active_query_tab().cloned() {
            tab.update(cx, |tab, cx| query(tab, window, cx));
        }
    }

    fn add_row(&mut self, _: &AddRow, window: &mut Window, cx: &mut Context<Self>) {
        self.on_rows(
            window,
            cx,
            |tab, window, cx| tab.add_row(window, cx),
            |tab, window, cx| tab.add_row(window, cx),
        );
    }

    fn duplicate_rows(&mut self, _: &DuplicateRows, window: &mut Window, cx: &mut Context<Self>) {
        self.on_rows(
            window,
            cx,
            |tab, window, cx| tab.duplicate_rows(None, window, cx),
            |tab, window, cx| tab.duplicate_rows(None, window, cx),
        );
    }

    fn delete_rows(&mut self, _: &DeleteRows, window: &mut Window, cx: &mut Context<Self>) {
        self.on_rows(
            window,
            cx,
            |tab, window, cx| tab.delete_rows(None, window, cx),
            |tab, window, cx| tab.delete_rows(None, window, cx),
        );
    }

    fn set_null(&mut self, _: &SetNull, window: &mut Window, cx: &mut Context<Self>) {
        self.on_rows(
            window,
            cx,
            |tab, window, cx| tab.set_selected(CellValue::Null, window, cx),
            |tab, window, cx| tab.set_selected(CellValue::Null, window, cx),
        );
    }

    fn set_default(&mut self, _: &SetDefault, window: &mut Window, cx: &mut Context<Self>) {
        self.on_rows(
            window,
            cx,
            |tab, window, cx| tab.set_selected(CellValue::Default, window, cx),
            |tab, window, cx| tab.set_selected(CellValue::Default, window, cx),
        );
    }

    fn apply_changes(&mut self, _: &ApplyChanges, window: &mut Window, cx: &mut Context<Self>) {
        self.on_rows(
            window,
            cx,
            |tab, window, cx| tab.review(window, cx),
            |tab, window, cx| tab.review(window, cx),
        );
    }

    fn discard_changes(&mut self, _: &DiscardChanges, window: &mut Window, cx: &mut Context<Self>) {
        self.on_rows(
            window,
            cx,
            |tab, _, cx| tab.discard(cx),
            |tab, _, cx| tab.discard(cx),
        );
    }

    fn copy_from_row_above(
        &mut self,
        _: &actions::CopyFromRowAbove,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_rows(
            window,
            cx,
            |tab, window, cx| tab.copy_from_row(true, window, cx),
            |tab, window, cx| tab.copy_from_row(true, window, cx),
        );
    }

    fn copy_from_row_below(
        &mut self,
        _: &actions::CopyFromRowBelow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_rows(
            window,
            cx,
            |tab, window, cx| tab.copy_from_row(false, window, cx),
            |tab, window, cx| tab.copy_from_row(false, window, cx),
        );
    }

    /// Esc: with nothing to revert, the key goes on (the grid then clears
    /// its selection).
    fn revert_cells(&mut self, _: &actions::RevertCells, _: &mut Window, cx: &mut Context<Self>) {
        let reverted = if let Some(tab) = self.active_data_tab().cloned() {
            tab.update(cx, |tab, cx| tab.revert_cells(cx))
        } else {
            self.active_query_tab()
                .cloned()
                .is_some_and(|tab| tab.update(cx, |tab, cx| tab.revert_cells(cx)))
        };
        if !reverted {
            cx.propagate();
        }
    }

    fn undo_change(
        &mut self,
        _: &actions::UndoChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_rows(
            window,
            cx,
            |tab, _, cx| {
                tab.undo(cx);
            },
            |tab, _, cx| {
                tab.undo(cx);
            },
        );
    }

    /// Cmd+Shift+C, in a table's data and in a query result.
    fn copy_with_column_names(
        &mut self,
        _: &actions::CopyWithColumnNames,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let format = crate::query::grid::CopyFormat::TsvWithHeader;
        if let Some(tab) = self.active_data_tab().cloned() {
            tab.update(cx, |tab, cx| tab.copy_selection(format, cx));
        } else if let Some(tab) = self.active_query_tab().cloned() {
            tab.update(cx, |tab, cx| tab.copy_selection(Some(format), cx));
        }
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
        navigator.update(cx, |navigator, cx| {
            if let Some(connection) = navigator.connection().map(str::to_string) {
                navigator.reload(&connection, cx)
            }
        });
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
            SidebarEvent::Tool(id, entry) => {
                self.run_tool(connection_target(id), entry.clone(), Vec::new(), window, cx)
            }
            SidebarEvent::Disconnect { ids, action } => {
                self.disconnect(ids.clone(), action.clone(), window, cx)
            }
            SidebarEvent::Reconnect(id) => self.reconnect(id.clone(), window, cx),
            SidebarEvent::SetReadOnly(id, read_only) => {
                self.set_read_only(id.clone(), *read_only, window, cx)
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
                    // In plain words first, the raw message below.
                    let explained = kerno_core::error::explain(&message, &saved.config);
                    let title = format!("{}: {}", saved.config.name, explained.headline);
                    let raw = explained.raw.clone();
                    window.push_notification(
                        Notification::error(SharedString::from(explained.next_step))
                            .title(SharedString::from(title))
                            .content(move |_, _, cx| {
                                crate::connections::dialog::raw_message(
                                    "connection-failed-raw",
                                    &raw,
                                    cx,
                                )
                                .into_any_element()
                            }),
                        cx,
                    );
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
            ConnectionsEvent::DeleteRequested { id } => {
                self.delete_connection(id.clone(), window, cx)
            }
            ConnectionsEvent::ReconnectRequested { id } => self.reconnect(id.clone(), window, cx),
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
        // Folders travel with their colour and glyph, never their picture.
        let folders = AppState::get(cx).settings.folder_looks.clone();
        let target = path.clone();
        let writing = bridge::spawn_blocking(cx, move || {
            let json = export_connections_with_folders(&configs, &folders)?;
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
            import_connections_file(&json)
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = reading.await;
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok(file) => {
                    let count = file.connections.len();
                    this.connections.update(cx, |connections, cx| {
                        connections.import(file.connections, cx);
                        connections.import_folder_looks(file.folders, cx);
                    });
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
            // The tab bar's menu, so a closed tab can come back.
            return v_flex()
                .id("tabs-empty")
                .test_support()
                .context_menu(self.menu(MenuOf::Bar, cx))
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                // The mark, with its clear space (drawn about 52 px tall).
                .child(crate::brand::mark(px(80.), cx))
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

        // The active tab's accent marker under its title: olive on light,
        // Lime on dark (the theme's primary).
        let (active, accent, none) = (self.active_tab, theme.primary, theme.transparent);
        let marker = move |index| if index == active { accent } else { none };
        let marked = |title: gpui_kit::Stateful<gpui_kit::Div>, index| {
            title
                .h_full()
                .flex()
                .items_center()
                .border_b_2()
                .border_color(marker(index))
        };
        let tab_bar = TabBar::new("tabs")
            .track_scroll(&self.tab_scroll)
            .selected_index(self.active_tab)
            .on_click(
                cx.listener(|this, index: &usize, window, cx| {
                    this.activate_tab(*index, window, cx)
                }),
            )
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                // The connection's tag as a dot, then its engine's logo; the
                // title takes the colour of its environment, when it has one.
                let config = self.connection_config(tab.connection(cx).as_deref(), cx);
                let dot = config
                    .as_ref()
                    .and_then(|config| theme::connection_color(config, cx));
                let chrome = config
                    .as_ref()
                    .and_then(|config| theme::environment_chrome(config, cx));
                Tab::new()
                    .when_some(config.as_ref(), |tab, config| {
                        tab.prefix(
                            h_flex()
                                .flex_none()
                                .ml_1()
                                .gap_1()
                                .children(dot.map(|color| {
                                    div()
                                        .id(("tab-dot", index))
                                        .test_support()
                                        .flex_none()
                                        .size(px(6.))
                                        .rounded_full()
                                        .bg(color)
                                }))
                                .child(
                                    div()
                                        .id(("tab-logo", index))
                                        .test_support()
                                        .flex_none()
                                        .child(crate::engine_logo::engine_logo(
                                            config.engine,
                                            px(12.),
                                            cx,
                                        )),
                                ),
                        )
                    })
                    // A middle click closes the tab, as in a browser.
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.perform(Some(index), TabAction::Close, window, cx)
                        }),
                    )
                    .child(
                        div()
                            .id(("tab-title", index))
                            .map(|title| marked(title, index))
                            .test_support()
                            .context_menu(self.menu(MenuOf::Tab(index), cx))
                            .map(|title| match chrome {
                                // The kit's tab paints its own background, so
                                // the tint goes on the title.
                                Some(chrome) => title.child(
                                    div()
                                        .id(("tab-env", index))
                                        .test_support()
                                        .px_1()
                                        .rounded_sm()
                                        .bg(chrome.bar)
                                        .text_color(chrome.bar_text)
                                        .child(tab.label(cx)),
                                ),
                                None => title.child(tab.label(cx)),
                            }),
                    )
                    .suffix(
                        Button::new(("close-tab", index))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .tooltip("Close tab")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                // The click must not also reach the tab, which
                                // would activate the tab being closed.
                                cx.stop_propagation();
                                this.perform(Some(index), TabAction::Close, window, cx)
                            })),
                    )
            }))
            .last_empty_space(
                div()
                    .id("tab-bar-empty")
                    .test_support()
                    .flex_1()
                    .min_w(px(24.))
                    .h_full()
                    .context_menu(self.menu(MenuOf::Bar, cx)),
            )
            .suffix(
                Button::new("new-tab")
                    .ghost()
                    .xsmall()
                    .flex_none()
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
    fn status_items(&self, cx: &App) -> Vec<(StatusItem, String, Option<Hsla>)> {
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
                StatusItem::Summary,
                match self.connections.read(cx).connected_count() {
                    0 => "Not connected".to_string(),
                    1 => "1 connection open".to_string(),
                    count => format!("{count} connections open"),
                },
                None,
            )];
        };
        let mut items = vec![(StatusItem::Connection, connection, None)];
        items.extend(database.map(|database| (StatusItem::Database, database, None)));
        if read_only {
            items.push((
                StatusItem::ReadOnly,
                "read-only".into(),
                Some(theme.warning),
            ));
        }
        match transaction {
            TxState::Idle => {}
            TxState::Open => items.push((
                StatusItem::Transaction,
                "transaction open".into(),
                Some(theme.warning),
            )),
            TxState::Failed => items.push((
                StatusItem::Transaction,
                "transaction failed: roll back".into(),
                Some(theme.danger),
            )),
        }
        if running {
            items.push((StatusItem::Running, "running…".into(), None));
        }
        if pending > 0 {
            items.push((
                StatusItem::Pending,
                format!(
                    "{pending} pending change{}",
                    if pending == 1 { "" } else { "s" }
                ),
                Some(theme.warning),
            ));
        }
        items.extend(rows.map(|rows| {
            (
                StatusItem::Rows,
                format!("{rows} row{}", if rows == 1 { "" } else { "s" }),
                None,
            )
        }));
        items.extend(elapsed.map(|elapsed| (StatusItem::Timing, duration_text(elapsed), None)));
        if notices > 0 {
            items.push((
                StatusItem::Messages,
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
            .map(|(_, text, _)| text)
            .collect()
    }

    /// An item of the status bar: a click or a right-click opens its
    /// actions, when it has any.
    fn render_status_item(
        &self,
        item: StatusItem,
        text: impl Into<SharedString>,
        color: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        // The connection carries its tag colour, as its tabs do, and its
        // engine's logo, as the tab toolbar's connection selector does.
        let saved = (item == StatusItem::Connection)
            .then(|| {
                let tab = self.tabs.get(self.active_tab)?;
                self.connections
                    .read(cx)
                    .get(&tab.connection(cx)?)
                    .map(|saved| saved.config.clone())
            })
            .flatten();
        let dot = saved
            .as_ref()
            .and_then(|config| theme::connection_color(config, cx));
        let logo = saved.as_ref().map(|config| {
            div()
                .id("status-connection-logo")
                .test_support()
                .flex_none()
                .child(crate::engine_logo::engine_logo(config.engine, px(12.), cx))
        });
        let text = h_flex()
            .gap_1p5()
            .children(dot.map(|dot| {
                div()
                    .id("status-connection-dot")
                    .test_support()
                    .flex_none()
                    .size(px(6.))
                    .rounded_full()
                    .bg(dot)
            }))
            .children(logo)
            .child(div().text_color(color).child(text.into()));
        if self.menu_entries(MenuOf::Status(item), cx).is_empty() {
            return text.into_any_element();
        }
        div()
            .id(item.id())
            .test_support()
            .context_menu(self.menu(MenuOf::Status(item), cx))
            .child(
                Button::new(SharedString::from(format!("{}-button", item.id())))
                    .ghost()
                    .xsmall()
                    .child(text)
                    // The bar is at the bottom: the menu opens above it.
                    .dropdown_menu_with_anchor(
                        Anchor::BottomLeft,
                        self.menu(MenuOf::Status(item), cx),
                    ),
            )
            .into_any_element()
    }

    /// What the environment of the active tab's connection paints on the
    /// chrome; `None` when it tints nothing.
    pub fn active_chrome(&self, cx: &App) -> Option<theme::EnvironmentChrome> {
        let connection = self.tabs.get(self.active_tab)?.connection(cx)?;
        theme::environment_chrome(&self.connection_config(Some(connection.as_str()), cx)?, cx)
    }

    fn render_status_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, warning) = (theme.muted_foreground, theme.warning);
        let memory_only = !AppState::get(cx).secrets.is_persistent();
        let chrome = self.active_chrome(cx);
        h_flex()
            .id("status-bar")
            .test_support()
            .h(px(24.))
            .px_3()
            .gap_3()
            .justify_between()
            .border_t_1()
            .border_color(theme.status_bar_border)
            // The environment of the active tab's connection tints the bar;
            // its text then takes the one colour that reads on it (each item
            // says what it is in words).
            .bg(chrome.map_or(theme.status_bar, |chrome| chrome.bar))
            .text_xs()
            .text_color(chrome.map_or(theme.muted_foreground, |chrome| chrome.bar_text))
            .child(
                h_flex()
                    .gap_3()
                    .children(
                        self.status_items(cx)
                            .into_iter()
                            .map(|(item, text, color)| {
                                let color = match chrome {
                                    Some(chrome) => chrome.bar_text,
                                    None => color.unwrap_or(muted),
                                };
                                self.render_status_item(item, text, color, cx)
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .children(self.update_item(cx))
                    .when(memory_only, |this| {
                        this.child(self.render_status_item(
                            StatusItem::MemoryOnly,
                            "Passwords are kept in memory only (no system secret store in use)",
                            warning,
                            cx,
                        ))
                    }),
            )
    }
}

// Ending what sessions hold before they close; the keyboard after a dialog --

/// What runs once the sessions' transactions are ended.
type Then = Box<dyn FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>)>;

impl Workspace {
    /// The settings of a saved connection, when there is one.
    fn connection_config(&self, id: Option<&str>, cx: &App) -> Option<ConnectionConfig> {
        Some(self.connections.read(cx).get(id?)?.config.clone())
    }

    fn connection_name(&self, id: &str, cx: &App) -> String {
        self.connections
            .read(cx)
            .get(id)
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default()
    }

    /// The connection a connection command acts on: the active tab's, else
    /// the one selected in the sidebar.
    fn command_connection(&self, cx: &App) -> Option<String> {
        self.tabs
            .get(self.active_tab)
            .and_then(|tab| tab.connection(cx))
            .or_else(|| self.sidebar.read(cx).selected(cx))
    }

    /// Closes the connections `ids` once what their tabs hold is ended.
    fn disconnect(
        &mut self,
        ids: Vec<String>,
        action: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ids.is_empty() {
            window.push_notification(
                Notification::warning(SharedString::from(format!("{action}: none is open."))),
                cx,
            );
            return;
        }
        let tabs = self.tabs_on(&ids, cx);
        let then: Then = Box::new(move |this, _, cx| {
            this.connections.update(cx, |connections, cx| {
                for id in &ids {
                    connections.disconnect(id, cx);
                }
            })
        });
        self.end_then(action, tabs, then, window, cx);
    }

    /// Closes the connection `id` and opens it again, the same way.
    fn reconnect(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let action = format!("Reconnect “{}”", self.connection_name(&id, cx));
        let tabs = self.tabs_on(std::slice::from_ref(&id), cx);
        let then: Then = Box::new(move |this, _, cx| {
            this.connections.update(cx, |connections, cx| {
                connections.disconnect(&id, cx);
                connections.connect(&id, cx);
            })
        });
        self.end_then(action, tabs, then, window, cx);
    }

    /// Deletes the connection `id` once what its tabs hold is ended, after
    /// the alert that confirmed the deletion has closed.
    fn delete_connection(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let action = format!("Delete “{}”", self.connection_name(&id, cx));
        let tabs = self.tabs_on(std::slice::from_ref(&id), cx);
        let then: Then = Box::new(move |this, _, cx| {
            this.connections
                .update(cx, |connections, cx| connections.delete(&id, cx))
        });
        let workspace = cx.entity().downgrade();
        window.defer(cx, move |window, cx| {
            let _ = workspace.update(cx, |this, cx| this.end_then(action, tabs, then, window, cx));
        });
    }

    /// Saves the read-only flag of a connection; an open one reconnects, so
    /// what its tabs hold is ended first, the same way (CON-12).
    fn set_read_only(
        &mut self,
        id: String,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self.connection_name(&id, cx);
        let action = if read_only {
            format!("Reconnect “{name}” read-only")
        } else {
            format!("Reconnect “{name}” with changes allowed")
        };
        let open = !matches!(
            self.connections.read(cx).status(&id),
            Status::Disconnected | Status::Failed { .. }
        );
        let tabs = if open {
            self.tabs_on(std::slice::from_ref(&id), cx)
        } else {
            Vec::new()
        };
        let then: Then = Box::new(move |this, _, cx| {
            this.connections.update(cx, |connections, cx| {
                connections.set_read_only(&id, read_only, cx)
            })
        });
        self.end_then(action, tabs, then, window, cx);
    }

    /// Moves a query tab to `target`, once its transaction is ended.
    fn switch_target(
        &mut self,
        tab: Entity<QueryTab>,
        target: Option<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (title, from) = {
            let tab = tab.read(cx);
            (tab.title(), tab.target().cloned())
        };
        let to = match (&from, &target) {
            (Some(from), Some(to)) if from.connection == to.connection => {
                match (&to.database, &to.schema) {
                    (_, Some(schema)) if from.database == to.database => {
                        format!("the schema “{schema}”")
                    }
                    (Some(database), _) => format!("the database “{database}”"),
                    _ => "its connection's own database".to_string(),
                }
            }
            (_, Some(to)) => format!("“{}”", self.connection_name(&to.connection, cx)),
            (_, None) => "no connection".to_string(),
        };
        let action = format!("Switch “{title}” to {to}");
        let switched = tab.clone();
        let then: Then = Box::new(move |this, _, cx| {
            switched.update(cx, |tab, cx| tab.set_target(target, cx));
            this.schedule_save(cx);
            cx.notify();
        });
        self.end_then(action, vec![OpenTab::Query(tab)], then, window, cx);
    }

    /// The tabs working on the connections `ids`.
    fn tabs_on(&self, ids: &[String], cx: &App) -> Vec<OpenTab> {
        self.tabs
            .iter()
            .filter(|tab| tab.connection(cx).is_some_and(|id| ids.contains(&id)))
            .cloned()
            .collect()
    }

    /// The one check before sessions close (disconnect, reconnect, read-only,
    /// a tab moved): when `tabs` hold an open transaction or pending changes,
    /// one dialog lists them and asks how to end them; Cancel changes
    /// nothing. Without any, `then` runs now.
    fn end_then(
        &mut self,
        action: String,
        tabs: Vec<OpenTab>,
        then: Then,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let losses: Vec<(String, Loss)> = tabs
            .iter()
            .flat_map(|tab| {
                let title = tab.title(cx).to_string();
                tab.losses(cx)
                    .into_iter()
                    .map(move |loss| (title.clone(), loss))
            })
            .collect();
        let Some((title, text, ending)) = tabs::end_question(&action, &losses) else {
            then(self, window, cx);
            return;
        };
        let workspace = cx.entity().downgrade();
        let then = Rc::new(RefCell::new(Some(then)));
        // `Some(commit)` ends the transactions, `None` only discards.
        let choose = Rc::new(
            move |end: Option<bool>, window: &mut Window, cx: &mut App| {
                window.close_dialog(cx);
                if let Some(then) = then.borrow_mut().take() {
                    let tabs = tabs.clone();
                    let _ = workspace
                        .update(cx, |this, cx| this.finish_then(end, tabs, then, window, cx));
                }
            },
        );
        let (title, text) = (SharedString::from(title), SharedString::from(text));
        window.open_dialog(cx, move |dialog, _, _| {
            let (choose, text, ending) = (choose.clone(), text.clone(), ending.clone());
            let label = title.clone();
            dialog
                .title(title.clone())
                .w(px(500.))
                // Enter never ends a transaction.
                .on_ok(|_, _, _| false)
                .content(move |content, _, _| {
                    let button = |id: &'static str, label: String, end: Option<bool>| {
                        let choose = choose.clone();
                        Button::new(id)
                            .label(SharedString::from(label))
                            .on_click(move |_, window, cx| choose(end, window, cx))
                    };
                    let choices: Vec<Button> = match &ending {
                        tabs::Ending::Transactions { commit } => vec![
                            button("end-rollback", "Roll back and continue".into(), Some(false))
                                .outline(),
                            button(
                                "end-commit",
                                match commit {
                                    Some(why) => format!("Commit and continue ({why})"),
                                    None => "Commit and continue".into(),
                                },
                                Some(true),
                            )
                            // Neither answer is the safe one: no primary.
                            .outline()
                            .disabled(commit.is_some()),
                        ],
                        tabs::Ending::Discard => vec![
                            button("end-discard", "Discard and continue".into(), None).danger(),
                        ],
                    };
                    content.child(
                        v_flex()
                            .gap_3()
                            .child(
                                v_flex()
                                    .id("end-question")
                                    .test_support()
                                    .aria_label(label.clone())
                                    .gap_1()
                                    .text_sm()
                                    .children(text.split('\n').enumerate().map(|(index, line)| {
                                        let line = SharedString::from(line.to_string());
                                        div()
                                            .id(("end-line", index))
                                            .test_support()
                                            .aria_label(line.clone())
                                            .child(line)
                                    })),
                            )
                            .child(
                                h_flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        Button::new("end-cancel")
                                            .ghost()
                                            .label("Cancel")
                                            .on_click(|_, window, cx| window.close_dialog(cx)),
                                    )
                                    .children(choices),
                            ),
                    )
                })
        });
    }

    /// Ends the transactions of `tabs` (commits them with `Some(true)`),
    /// waits for the server, discards the pending changes, then runs
    /// `then`. When a COMMIT or ROLLBACK fails, nothing else is done.
    fn finish_then(
        &mut self,
        end: Option<bool>,
        tabs: Vec<OpenTab>,
        then: Then,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let endings: Vec<Task<Result<(), String>>> = match end {
            Some(commit) => tabs
                .iter()
                .filter_map(OpenTab::query)
                .map(|tab| tab.update(cx, |tab, cx| tab.finish_transaction(commit, cx)))
                .collect(),
            None => Vec::new(),
        };
        let discarded: Vec<Entity<DataTab>> =
            tabs.iter().filter_map(OpenTab::data).cloned().collect();
        let queries: Vec<Entity<QueryTab>> =
            tabs.iter().filter_map(OpenTab::query).cloned().collect();
        cx.spawn_in(window, async move |this, cx| {
            let mut failures = Vec::new();
            for ending in endings {
                if let Err(failure) = ending.await {
                    failures.push(failure);
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if !failures.is_empty() {
                    let text = format!("{}. Nothing else was done.", failures.join("; "));
                    window.push_notification(Notification::error(SharedString::from(text)), cx);
                    return;
                }
                for tab in &discarded {
                    tab.update(cx, |tab, cx| tab.discard(cx));
                }
                for tab in &queries {
                    tab.update(cx, |tab, cx| tab.discard_results(cx));
                }
                then(this, window, cx);
            });
        })
        .detach();
    }

    /// Gives the keyboard back once a dialog, an alert or the palette has
    /// closed. The kit gives it back to what had it when the dialog opened,
    /// when that still exists; a menu entry, a status bar button or the
    /// palette do not, and the window then had no focus (no global key
    /// worked). Called on each frame: remembers what had the keyboard in
    /// the window while no dialog was open, and gives it back to that (or
    /// to the active tab) when a dialog closed and the focus is nowhere in
    /// the window, or whenever nothing at all has the focus.
    fn keep_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was_open = std::mem::replace(&mut self.dialog_open, window.has_active_dialog(cx));
        if self.dialog_open {
            return;
        }
        if self.focus_handle.contains_focused(window, cx) {
            self.last_focus = window.focused(cx).map(|focus| focus.downgrade());
        } else if was_open || window.focused(cx).is_none() {
            // The focus nowhere at all (nothing focused yet, or what had it
            // was dropped) also leaves every key and menu-bar command
            // without a path to the workspace: Cmd+, did nothing.
            let workspace = cx.entity().downgrade();
            // Once what closing the dialog ran is done (it may have closed
            // the tab that had the keyboard, or given it elsewhere).
            window.defer(cx, move |window, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    if this.focus_handle.contains_focused(window, cx) {
                        return;
                    }
                    match this
                        .last_focus()
                        .filter(|last| this.focus_handle.contains(last, window))
                    {
                        Some(last) => last.focus(window, cx),
                        None => this.focus_active(window, cx),
                    }
                });
            });
        }
    }

    /// What had the keyboard in the window before the palette opened.
    fn last_focus(&self) -> Option<FocusHandle> {
        self.last_focus.as_ref().and_then(WeakFocusHandle::upgrade)
    }

    fn disconnect_all(&mut self, _: &DisconnectAll, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.sidebar.read(cx).closing(&Subject::List, cx);
        self.disconnect(ids, "Disconnect all connections".into(), window, cx);
    }

    fn disconnect_others(
        &mut self,
        _: &DisconnectOtherConnections,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.command_connection(cx) else {
            window.push_notification(
                Notification::warning("Disconnect other connections: open a tab on a connection, or select one in the sidebar."),
                cx,
            );
            return;
        };
        let ids = self
            .sidebar
            .read(cx)
            .closing(&Subject::Connection(id.clone()), cx);
        let action = format!(
            "Disconnect the connections other than “{}”",
            self.connection_name(&id, cx)
        );
        self.disconnect(ids, action, window, cx);
    }

    fn toggle_read_only(
        &mut self,
        _: &ToggleReadOnly,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.command_connection(cx) else {
            window.push_notification(
                Notification::warning(
                    "Read-only: open a tab on a connection, or select one in the sidebar.",
                ),
                cx,
            );
            return;
        };
        let read_only = self
            .connections
            .read(cx)
            .get(&id)
            .is_some_and(|saved| saved.config.read_only);
        self.set_read_only(id, !read_only, window, cx);
    }
}

// Keyboard coverage: commands that reach the tree, the sidebar, the grids
// and the editor from the palette, the menu bar and their keys -------------

impl Workspace {
    /// Registers the handlers of these commands on the window's root.
    fn more_actions(
        &self,
        root: gpui_kit::Stateful<gpui_kit::Div>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        use crate::query::grid::CopyFormat;
        root.on_action(cx.listener(|_, command: &actions::RunCommand, window, cx| {
            if let Some(found) = actions::COMMANDS
                .iter()
                .find(|found| found.id == command.id)
            {
                window.dispatch_action((found.action)(), cx);
            }
        }))
        .on_action(cx.listener(Self::quick_open))
        .on_action(
            cx.listener(|this, _: &actions::FocusNavigatorFilter, window, cx| {
                if !AppState::get(cx).settings.sidebar_visible {
                    AppState::update_settings(cx, |settings| settings.sidebar_visible = true);
                }
                let navigator = this.sidebar.read(cx).navigator().clone();
                navigator.update(cx, |navigator, cx| navigator.focus_filter(window, cx));
                cx.notify();
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::DuplicateConnection, window, cx| {
                this.on_selected_connection(SidebarAction::Duplicate, window, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::DeleteConnection, window, cx| {
                this.on_selected_connection(SidebarAction::Delete, window, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &actions::NextResultTab, window, cx| {
            this.on_query(window, cx, |tab, _, cx| tab.cycle_result(true, cx))
        }))
        .on_action(
            cx.listener(|this, _: &actions::PreviousResultTab, window, cx| {
                this.on_query(window, cx, |tab, _, cx| tab.cycle_result(false, cx))
            }),
        )
        .on_action(cx.listener(|this, _: &actions::FetchMoreRows, window, cx| {
            this.on_query(window, cx, |tab, _, cx| tab.fetch(false, cx))
        }))
        .on_action(cx.listener(|this, _: &actions::FetchAllRows, window, cx| {
            this.on_query(window, cx, |tab, _, cx| tab.fetch(true, cx))
        }))
        .on_action(
            cx.listener(|this, _: &actions::CopyAsCsv, _, cx| this.copy_as(CopyFormat::Csv, cx)),
        )
        .on_action(
            cx.listener(|this, _: &actions::CopyAsJson, _, cx| this.copy_as(CopyFormat::Json, cx)),
        )
        .on_action(cx.listener(|this, _: &actions::CopyAsMarkdown, _, cx| {
            this.copy_as(CopyFormat::Markdown, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::CopyAsSqlInsert, _, cx| {
            this.copy_as(CopyFormat::SqlInsert, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::ViewCellValue, window, cx| {
            if let Some(grid) = this.active_grid(cx) {
                crate::query::grid::Grid::view_selected_value(&grid, window, cx);
            }
        }))
        .on_action(cx.listener(|_, _: &actions::ToggleSoftWrap, _, cx| {
            AppState::update_settings(cx, |settings| settings.soft_wrap = !settings.soft_wrap);
        }))
        .on_action(cx.listener(|_, _: &actions::GridZoomIn, window, cx| {
            crate::settings_view::zoom_grid(Some(1.0), window, cx)
        }))
        .on_action(cx.listener(|_, _: &actions::GridZoomOut, window, cx| {
            crate::settings_view::zoom_grid(Some(-1.0), window, cx)
        }))
        .on_action(cx.listener(|_, _: &actions::GridZoomReset, window, cx| {
            crate::settings_view::zoom_grid(None, window, cx)
        }))
        .on_action(
            cx.listener(|this, _: &actions::ToggleRecordView, window, cx| {
                if let Some(grid) = this.active_grid(cx) {
                    Grid::toggle_record(&grid, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::FirstRow, _, cx| this.move_row(RowMove::First, cx)),
        )
        .on_action(
            cx.listener(|this, _: &actions::PreviousRow, _, cx| {
                this.move_row(RowMove::Previous, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::NextRow, _, cx| this.move_row(RowMove::Next, cx)),
        )
        .on_action(
            cx.listener(|this, _: &actions::LastRow, _, cx| this.move_row(RowMove::Last, cx)),
        )
        .on_action(cx.listener(|this, _: &actions::GoToRow, window, cx| {
            if let Some(grid) = this.active_grid(cx) {
                crate::query::grid_view::go_to_row(&grid, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &actions::GoToColumn, window, cx| {
            if let Some(grid) = this.active_grid(cx) {
                crate::query::grid_view::go_to_column(&grid, window, cx);
            }
        }))
        .on_action(
            cx.listener(|this, _: &actions::CopyRowNumbers, window, cx| {
                this.on_grid(GridAction::CopyRowNumbers, window, cx)
            }),
        )
        .on_action(cx.listener(|_, _: &actions::ToggleSelectionStats, _, cx| {
            AppState::update_settings(cx, |settings| {
                settings.selection_stats = !settings.selection_stats
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ShowAggregatePanel, window, cx| {
                this.on_grid(GridAction::Panel(Panel::Aggregate), window, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::ShowColumnsPanel, window, cx| {
                this.on_grid(GridAction::Panel(Panel::Columns), window, cx)
            }),
        )
        // Filters, counts and auto-refresh of a table's rows or a result.
        .on_action(
            cx.listener(|this, _: &actions::FilterDistinctValues, window, cx| {
                this.on_grid(GridAction::DistinctValues, window, cx)
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ClearFilters, window, cx| {
            this.on_grid(GridAction::ClearFilters, window, cx)
        }))
        .on_action(
            cx.listener(|this, _: &actions::CalculateRowCount, window, cx| {
                if let Some(tab) = this.active_data_tab().cloned() {
                    tab.update(cx, |tab, cx| tab.count_exactly(window, cx));
                } else if let Some(tab) = this.active_query_tab().cloned() {
                    tab.update(cx, |tab, cx| tab.count_result(window, cx));
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::ToggleAutoRefresh, window, cx| {
                let every = |on: Option<std::time::Duration>| {
                    on.is_none()
                        .then_some(crate::data::tab::AUTO_REFRESH_DEFAULT)
                };
                if let Some(tab) = this.active_data_tab().cloned() {
                    tab.update(cx, |tab, cx| {
                        tab.set_auto_refresh(every(tab.auto_refresh()), window, cx)
                    });
                } else if let Some(tab) = this.active_query_tab().cloned() {
                    tab.update(cx, |tab, cx| {
                        tab.set_auto_refresh(every(tab.auto_refresh()), window, cx)
                    });
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::OpenTableAtCursor, window, cx| {
                this.open_at_cursor(false, window, cx)
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::OpenStructureAtCursor, window, cx| {
                this.open_at_cursor(true, window, cx)
            }),
        )
        .on_action(cx.listener(|_, _: &actions::OpenKeymapFile, window, cx| {
            crate::settings_view::open_keymap_file(window, cx)
        }))
        .on_action(
            cx.listener(|this, _: &actions::ToggleBlockComment, window, cx| {
                this.on_query(window, cx, QueryTab::toggle_block_comment)
            }),
        )
        .on_action(cx.listener(|this, _: &actions::UpperCase, window, cx| {
            this.on_query(window, cx, |tab, window, cx| {
                tab.change_case(true, window, cx)
            })
        }))
        .on_action(cx.listener(|this, _: &actions::LowerCase, window, cx| {
            this.on_query(window, cx, |tab, window, cx| {
                tab.change_case(false, window, cx)
            })
        }))
        .on_action(
            cx.listener(|this, _: &actions::CopySelectedQuery, window, cx| {
                this.on_query(window, cx, |tab, _, cx| {
                    tab.copy_statement(cx);
                })
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::RefreshCompletionMetadata, window, cx| {
                this.on_query(window, cx, |tab, _, cx| tab.refresh_completion(cx))
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::ExportFromQuery, window, cx| {
                this.on_query(window, cx, QueryTab::export_from_query)
            }),
        )
        .on_action(cx.listener(|this, _: &actions::RevertFile, window, cx| {
            this.on_query(window, cx, QueryTab::revert_file)
        }))
        .on_action(
            cx.listener(|this, _: &actions::LinkWithEditor, window, cx| {
                let on = !AppState::get(cx).settings.link_with_editor;
                AppState::update_settings(cx, |settings| settings.link_with_editor = on);
                if on {
                    this.link_active_tab(cx);
                }
                let text = if on {
                    "Link with editor: on. The schema tree follows the active tab."
                } else {
                    "Link with editor: off."
                };
                window.push_notification(text, cx);
            }),
        )
    }

    /// Open Table (Cmd+P) on the active tab's connection, or the one
    /// selected in the sidebar.
    fn quick_open(&mut self, _: &actions::QuickOpen, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.command_connection(cx) else {
            window.push_notification(
                Notification::warning(
                    "Open table: open a tab on a connection, or select one in the sidebar.",
                ),
                cx,
            );
            return;
        };
        let Some(session) = self.connections.read(cx).meta_session(&id) else {
            let text = format!(
                "Open table: connect “{}” first.",
                self.connection_name(&id, cx)
            );
            window.push_notification(Notification::warning(SharedString::from(text)), cx);
            return;
        };
        crate::quick_open::open(cx.entity(), id, session, window, cx);
    }

    /// Runs a sidebar action on the connection selected in the sidebar.
    fn on_selected_connection(
        &mut self,
        action: SidebarAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.sidebar.read(cx).selected(cx) else {
            window.push_notification(
                Notification::warning("Select a connection in the sidebar first."),
                cx,
            );
            return;
        };
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.perform(&Subject::Connection(id), action, window, cx)
        });
    }

    /// Copies the selection of the grid on screen in `format`.
    fn copy_as(&mut self, format: crate::query::grid::CopyFormat, cx: &mut Context<Self>) {
        if let Some(tab) = self.active_data_tab().cloned() {
            tab.update(cx, |tab, cx| tab.copy_selection(format, cx));
        } else if let Some(tab) = self.active_query_tab().cloned() {
            tab.update(cx, |tab, cx| tab.copy_selection(Some(format), cx));
        }
    }

    /// The grid on screen: the active table's rows, or the active result.
    fn active_grid(
        &self,
        cx: &App,
    ) -> Option<Entity<gpui_kit::component::table::TableState<crate::query::grid::Grid>>> {
        self.active_data_tab()
            .and_then(|tab| tab.read(cx).grid())
            .or_else(|| {
                self.active_query_tab()
                    .and_then(|tab| tab.read(cx).active_grid())
            })
    }

    /// Moves the selection of the grid on screen to another row.
    fn move_row(&mut self, to: RowMove, cx: &mut Context<Self>) {
        if let Some(grid) = self.active_grid(cx) {
            Grid::move_row(&grid, to, cx);
        }
    }

    /// Runs an entry of the grid menus on the selection of the grid on
    /// screen.
    fn on_grid(&mut self, action: GridAction, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(grid) = self.active_grid(cx) {
            Grid::perform(&grid, action, window, cx);
        }
    }

    /// Opens the rows (or the structure) of the table named at the active
    /// editor's cursor.
    fn open_at_cursor(&mut self, structure: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_query_tab().cloned() else {
            return;
        };
        let (found, target) = {
            let tab = tab.read(cx);
            (tab.relation_at_cursor(cx), tab.target().cloned())
        };
        let (Some((table, kind)), Some(target)) = (found, target) else {
            window.push_notification(
                Notification::warning(
                    "No table or view of this tab's connection is named at the cursor.",
                ),
                cx,
            );
            return;
        };
        let target = Target {
            schema: None,
            ..target
        };
        if structure {
            self.open_structure(target, table, kind, window, cx);
        } else {
            self.open_data(target, table, kind, Filter::default(), window, cx);
        }
    }
}

/// Where a tab runs on a connection: its own database and schema.
/// Opens the wizard of `mode` on `target`; a restore or a script that
/// succeeded refreshes its database in `navigator`.
fn open_wizard_with(
    connections: Entity<Connections>,
    navigator: Entity<Navigator>,
    target: Target,
    mode: backup_dialog::Mode,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<BackupDialog>> {
    backup_dialog::open(
        connections,
        target,
        mode,
        window,
        cx,
        move |connection, database, _, cx| {
            navigator.update(cx, |navigator, cx| {
                navigator.refresh_database(connection, database, cx)
            })
        },
    )
}

/// Reads the tasks again in the Tasks view of `workspace`, if it is open.
fn reload_tasks(workspace: &WeakEntity<Workspace>, cx: &mut App) {
    let _ = workspace.update(cx, |workspace, cx| workspace.reload_tasks(cx));
}

fn connection_target(id: &str) -> Target {
    Target {
        connection: id.to_string(),
        database: None,
        schema: None,
    }
}

/// The commands shown in the palette for `query`: grouped, in display
/// order, when nothing is typed; otherwise one list of the matches, the
/// title itself first, then titles that start with it, then the others
/// (other words of a title, keywords) in display order.
fn palette_groups(query: &str) -> Vec<(&'static str, Vec<&'static actions::Command>)> {
    let mut groups: Vec<(&'static str, Vec<&'static actions::Command>)> = Vec::new();
    for command in actions::COMMANDS {
        match groups.iter_mut().find(|(group, _)| *group == command.group) {
            Some((_, commands)) => commands.push(command),
            None => groups.push((command.group, vec![command])),
        }
    }
    // As the kit filters: case aside, the query as typed.
    let query = query.to_lowercase();
    if query.is_empty() {
        return groups;
    }
    let rank = |command: &actions::Command| {
        let title = command.label.to_lowercase();
        let title = title.trim_end_matches('…');
        if title == query {
            Some(0)
        } else if title.starts_with(&query) {
            Some(1)
        } else if title.contains(&query)
            || (command.keywords.iter()).any(|word| word.to_lowercase().contains(&query))
        {
            Some(2)
        } else {
            None
        }
    };
    let mut ranked: Vec<(u8, &'static actions::Command)> = groups
        .into_iter()
        .flat_map(|(_, commands)| commands)
        .filter_map(|command| Some((rank(command)?, command)))
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    let matches = ranked.into_iter().map(|(_, command)| command).collect();
    vec![("Matches", matches)]
}

/// The command at a palette position, for `query`.
fn palette_command(query: &str, section: usize, row: usize) -> Option<&'static actions::Command> {
    palette_groups(query).get(section)?.1.get(row).copied()
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(tab) = self.tabs.get(self.active_tab) {
            tab.reveal(window, cx);
        }
        self.keep_focus(window, cx);
        // The active tab is scrolled into view whenever it or the tabs
        // change (a tab opened, restored, closed or activated by key); a
        // scroll of the strip by hand is left alone until then.
        let shown = (self.active_tab, self.tabs.len());
        if self.revealed_tab != Some(shown) {
            self.tab_scroll.scroll_to_item(self.active_tab);
            // Before the strip's first layout there is no width to scroll
            // in: again on the next frame.
            if self.tab_scroll.bounds().size.width > px(0.) {
                self.revealed_tab = Some(shown);
            } else {
                window.request_animation_frame();
            }
        }
        *self.tab_target.borrow_mut() = self
            .active_query_tab()
            .and_then(|tab| tab.read(cx).target().cloned());
        let shown = self.tabs.get(self.active_tab).map(OpenTab::entity_id);
        // The tabs no longer shown stop refreshing on their own.
        if self.shown_tab.get() != shown {
            for tab in &self.tabs {
                if Some(tab.entity_id()) != shown {
                    tab.hide(cx);
                }
            }
        }
        self.shown_tab.set(shown);
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
            .on_action(cx.listener(|this, _: &CloseOtherTabs, window, cx| {
                this.perform(None, TabAction::CloseOthers, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseTabsToTheRight, window, cx| {
                this.perform(None, TabAction::CloseToRight, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseAllTabs, window, cx| {
                this.perform(None, TabAction::CloseAll, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RenameTab, window, cx| {
                this.perform(None, TabAction::Rename, window, cx)
            }))
            .on_action(cx.listener(|this, _: &DuplicateTab, window, cx| {
                this.perform(None, TabAction::Duplicate, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| {
                this.perform(None, TabAction::ReopenClosed, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CopyTabFilePath, window, cx| {
                this.perform(None, TabAction::CopyPath, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RevealTabFile, window, cx| {
                this.perform(None, TabAction::Reveal, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CommitTransaction, window, cx| {
                this.perform(None, TabAction::Commit, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RollbackTransaction, window, cx| {
                this.perform(None, TabAction::Rollback, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleCommitMode, window, cx| {
                let manual = this
                    .active_query_tab()
                    .is_some_and(|tab| tab.read(cx).manual_commit());
                this.perform(None, TabAction::ManualCommit(!manual), window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &DisconnectTabConnection, window, cx| {
                    this.perform(None, TabAction::Disconnect, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ReconnectTabConnection, window, cx| {
                this.perform(None, TabAction::Reconnect, window, cx)
            }))
            .on_action(cx.listener(Self::disconnect_all))
            .on_action(cx.listener(Self::disconnect_others))
            .on_action(cx.listener(Self::toggle_read_only))
            .on_action(cx.listener(|this, _: &EditTabConnection, window, cx| {
                this.perform(None, TabAction::EditConnection, window, cx)
            }))
            .on_action(cx.listener(Self::open_admin_command))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::use_system_theme))
            .on_action(cx.listener(Self::use_light_theme))
            .on_action(cx.listener(Self::use_dark_theme))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::about))
            .on_action(cx.listener(Self::check_for_updates))
            .on_action(cx.listener(Self::open_command_palette))
            .on_action(cx.listener(Self::new_connection))
            .on_action(cx.listener(Self::open_sql_file))
            .on_action(cx.listener(Self::save_file))
            .on_action(cx.listener(Self::save_file_as))
            .on_action(cx.listener(Self::show_history))
            .on_action(cx.listener(Self::show_saved_queries))
            .on_action(cx.listener(Self::save_query))
            .on_action(cx.listener(Self::run_statement))
            .on_action(cx.listener(|this, _: &RunStatementInNewTab, window, cx| {
                this.on_query(window, cx, QueryTab::run_in_new_tab)
            }))
            .on_action(cx.listener(|this, _: &RunFromCursor, window, cx| {
                this.on_query(window, cx, |tab, window, cx| {
                    tab.run(Scope::FromCursor, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &NextStatement, window, cx| {
                this.on_query(window, cx, |tab, window, cx| {
                    tab.go_to_statement(true, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &PreviousStatement, window, cx| {
                this.on_query(window, cx, |tab, window, cx| {
                    tab.go_to_statement(false, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &CountStatementRows, window, cx| {
                this.on_query(window, cx, QueryTab::count_rows)
            }))
            .on_action(cx.listener(|this, _: &TogglePinnedResult, window, cx| {
                this.on_query(window, cx, |tab, window, cx| {
                    tab.perform_on_active_result(TabAction::Pin, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &CloseResultTab, window, cx| {
                this.on_query(window, cx, |tab, window, cx| {
                    tab.perform_on_active_result(TabAction::Close, window, cx)
                })
            }))
            .on_action(cx.listener(|this, _: &ToggleResultsPanel, window, cx| {
                this.on_query(window, cx, QueryTab::toggle_results)
            }))
            .on_action(cx.listener(|this, _: &MaximizeResultsPanel, window, cx| {
                this.on_query(window, cx, QueryTab::maximize_results)
            }))
            .on_action(cx.listener(|this, _: &SwitchResultsFocus, window, cx| {
                this.on_query(window, cx, QueryTab::switch_focus)
            }))
            .on_action(cx.listener(|this, _: &CloseTabsToTheLeft, window, cx| {
                let left = this.tabs[..this.active_tab.min(this.tabs.len())].to_vec();
                this.close_tabs(left, window, cx)
            }))
            .on_action(cx.listener(Self::run_all))
            .on_action(cx.listener(Self::cancel_query))
            .on_action(cx.listener(Self::toggle_comment))
            .on_action(cx.listener(Self::format_sql))
            .on_action(cx.listener(Self::find_in_editor))
            .on_action(cx.listener(Self::refresh_data))
            .on_action(cx.listener(Self::export_data))
            .on_action(cx.listener(Self::import_csv))
            .on_action(cx.listener(|this, _: &BackupDatabase, window, cx| {
                this.backup_restore_command(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RestoreDatabase, window, cx| {
                this.backup_restore_command(true, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &crate::actions::GlobalBackup, window, cx| {
                    this.tool_command(ToolEntry::GlobalBackup, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::ExecuteScript, window, cx| {
                    this.tool_command(ToolEntry::ExecuteScript, window, cx)
                }),
            )
            .on_action(cx.listener(
                |this, _: &crate::actions::ForeignDataWrappers, window, cx| {
                    this.tool_command(ToolEntry::ForeignDataWrappers, window, cx)
                },
            ))
            .on_action(
                cx.listener(|this, _: &crate::actions::CreateTask, window, cx| {
                    this.tool_command(ToolEntry::NewTask, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::ShowTasks, window, cx| {
                    this.open_tasks(window, cx);
                }),
            )
            .on_action(cx.listener(Self::add_row))
            .on_action(cx.listener(Self::duplicate_rows))
            .on_action(cx.listener(Self::delete_rows))
            .on_action(cx.listener(Self::set_null))
            .on_action(cx.listener(Self::set_default))
            .on_action(cx.listener(Self::apply_changes))
            .on_action(cx.listener(Self::discard_changes))
            .on_action(cx.listener(Self::copy_from_row_above))
            .on_action(cx.listener(Self::copy_from_row_below))
            .on_action(cx.listener(Self::revert_cells))
            .on_action(cx.listener(Self::undo_change))
            .on_action(cx.listener(Self::copy_with_column_names))
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
                // F4 on a connection edits it; on a folder, opens Edit folder….
                let edit = match navigator.read(cx).selected_subject(cx) {
                    Some(subject @ Subject::Connection(_)) => Some((subject, SidebarAction::Edit)),
                    Some(subject) => Some((subject, SidebarAction::EditFolder)),
                    None => None,
                };
                match edit {
                    Some((subject, action)) => this.sidebar.update(cx, |sidebar, cx| {
                        sidebar.perform(&subject, action, window, cx)
                    }),
                    None => {
                        navigator.update(cx, |navigator, cx| navigator.edit_selected(window, cx))
                    }
                }
            }))
            .on_action(
                cx.listener(|this, _: &crate::actions::MaintenanceTools, window, cx| {
                    let navigator = this.sidebar.read(cx).navigator().clone();
                    navigator.update(cx, |navigator, cx| navigator.tools_on_selected(window, cx));
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::ShowTransactionLog, window, cx| {
                    this.perform(None, TabAction::TransactionLog, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::HideObjectInTree, window, cx| {
                    this.filter_in_tree(FilterAction::Hide, window, cx)
                }),
            )
            .on_action(cx.listener(
                |this, _: &crate::actions::ShowOnlyObjectInTree, window, cx| {
                    this.filter_in_tree(FilterAction::ShowOnly, window, cx)
                },
            ))
            .on_action(cx.listener(
                |this, _: &crate::actions::ConfigureObjectFilter, window, cx| {
                    this.filter_in_tree(FilterAction::Configure, window, cx)
                },
            ))
            .on_action(
                cx.listener(|this, _: &crate::actions::ToggleObjectFilter, window, cx| {
                    this.filter_in_tree(FilterAction::Toggle, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::ClearObjectFilter, window, cx| {
                    this.filter_in_tree(FilterAction::Clear, window, cx)
                }),
            )
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
            .on_action(cx.listener(
                |this, _: &crate::actions::ReadObjectInConsole, window, cx| {
                    this.on_tree(TreeAction::ReadInConsole, window, cx)
                },
            ))
            .on_action(cx.listener(
                |this, _: &crate::actions::GenerateRoutineCall, window, cx| {
                    this.on_tree(TreeAction::GenerateCall, window, cx)
                },
            ))
            .on_action(
                cx.listener(|this, _: &crate::actions::ExecuteRoutine, window, cx| {
                    this.on_tree(TreeAction::ExecuteRoutine, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::AddColumnToObject, window, cx| {
                    this.on_tree(TreeAction::AddColumn, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::BackupObject, window, cx| {
                    this.on_tree(TreeAction::Backup, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::OpenLockManager, window, cx| {
                    if let Some(tab) = this.open_admin_here(window, cx) {
                        tab.update(cx, |tab, cx| {
                            tab.select(crate::admin::tab::LOCKS, window, cx)
                        });
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &EditConnection, window, cx| {
                this.sidebar
                    .update(cx, |sidebar, cx| sidebar.edit_selected(window, cx))
            }))
            .on_action(cx.listener(|this, _: &OpenConnection, _, cx| {
                this.sidebar
                    .update(cx, |sidebar, cx| sidebar.open_selected(cx))
            }))
            .map(|root| self.more_actions(root, cx))
            .map(|root| self.toolbar_actions(root, cx))
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
            .when(AppState::get(cx).settings.toolbar_visible, |this| {
                this.child(self.render_toolbar(window, cx))
            })
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_status_bar(cx))
    }
}
