//! The sidebar's one tree: folders and connections, and under each open
//! connection its databases and objects. Loads what is opened, filters,
//! refreshes, and offers each row's actions.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName as Glyph;
use gpui_kit::base::TestTextExt as _;
use gpui_kit::base::actions::{SelectLeft, SelectRight};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::TextView;
use gpui_kit::component::tree::{TreeEntry, TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, ElementId, Entity, EventEmitter,
    FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, WeakEntity, Window, div, img, px,
};
use kerno_core::browse::Filter;
use kerno_core::config::{DatabasesShown, FolderLook, ObjectFilter, exact_mask, parent_folder};
use kerno_core::maintenance::Maintenance;
use kerno_core::schema::{Change, Dependent, ObjectRef, RelationKind, RoutineKind, TableRef};
use kerno_core::sql::{
    call_routine, delete_template, insert_template, select_template, update_template,
};
use kerno_core::table_draft::TableChange;
use kerno_core::{Engine, Error, Session, describe, exec};

use super::actions::{
    Action, FilterAction, Template, ToolEntry, ToolGroups, actions, create_template,
    default_action, edit_action, filter_actions, qualified_name, same_tool, tool_entries, tools,
    tools_now,
};
use super::filter_dialog;
use super::model::{
    self, Children, Group, Item, LINE_DETAIL, LINE_ERROR, LINE_NEXT, LINE_RETRY, LINE_STATUS,
    Listed, Load, LoadKind, Loaded, Node, NodeKind, Reveal, RowId, Tree,
};
use crate::ai_view::ask::{self as ai_ask, Entry as AiEntry};
use crate::bridge;
use crate::connections::manager::{Connections, ConnectionsEvent, Status, folder_of};
use crate::database_dialog;
use crate::engine_logo::{engine_logo, folder_glyph};
use crate::export_dialog::{self, Source};
use crate::folder_dialog::is_icon_name;
use crate::maintenance_dialog::{self, MaintenanceDialog};
use crate::sidebar::{DraggedConnection, Sidebar, Subject, set_collapsed};
use crate::sql_confirm::{self, Confirmation, Reach, Statement};
use crate::state::AppState;
use crate::tabs::Target;
use crate::theme;
use crate::tip::tip;

/// What the rest of the window does for the navigator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigatorEvent {
    /// Open a query tab holding `sql`, running on `target`.
    OpenQuery {
        title: String,
        sql: String,
        target: Option<Target>,
    },
    /// Open a query tab holding `sql` on `target`, and run all of it.
    RunQuery {
        title: String,
        sql: String,
        target: Option<Target>,
    },
    /// Open the form of `table` with the cursor on its `column`, or on a
    /// new column when `None`.
    EditTable {
        target: Target,
        table: TableRef,
        column: Option<String>,
    },
    /// Show the rows of a table or view.
    OpenData {
        target: Target,
        table: TableRef,
        kind: RelationKind,
    },
    /// Show the structure of a table or view.
    ShowStructure {
        target: Target,
        table: TableRef,
        kind: RelationKind,
    },
    /// Show the structure of a table or view on its Properties sub-tab.
    ShowProperties {
        target: Target,
        table: TableRef,
        kind: RelationKind,
    },
    /// Open the form of a new table in `schema`.
    CreateTable { target: Target, schema: String },
    /// Open a query tab in source mode holding `sql`, the script that
    /// replaces an object of `schema`.
    OpenSource {
        title: String,
        sql: String,
        target: Option<Target>,
        schema: String,
    },
    /// Import a CSV file into `table`, or into a new table of `schema`.
    ImportCsv {
        target: Target,
        schema: String,
        table: Option<TableRef>,
    },
    /// Make `target` (a database and schema) the active query tab's.
    SetDefault { target: Target },
    /// Show the ER diagram of a schema, or of a table and its neighbours.
    ViewDiagram {
        target: Target,
        scope: kerno_core::erd::Scope,
    },
    /// Export the schema of `target` as a document, `tables` ticked alone
    /// when given (K31).
    ExportSchema { target: Target, tables: Vec<String> },
    /// Compare the rows of `table` of `target` with another table's (K60).
    CompareData { target: Target, table: String },
    /// Make test data for `table` of `target` (K72).
    GenerateTestData { target: Target, table: String },
    /// Tell the user something went otherwise than asked.
    Warn(String),
    /// Ask the AI assistant this, in a query tab on `target` (K53).
    AskAi {
        target: Target,
        ask: kerno_core::ai::ask::Ask,
    },
    /// Open the extensions manager of a database, on `extension` (K27).
    ManageExtensions {
        target: Target,
        extension: Option<String>,
    },
    /// Open a Tools ▸ entry (not a maintenance tool: the navigator opens
    /// those) on `target`: a wizard on the database, with only `tables`
    /// (a table's backup) or the schema of `target` ticked, the
    /// administration, the foreign data wrappers, the tasks.
    Tool {
        target: Target,
        entry: ToolEntry,
        tables: Vec<TableRef>,
    },
}

/// The databases of a connection being browsed: the root of its tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Root {
    Loading,
    Loaded,
    Failed(String),
}

/// How long a read of the tree may take before it is stopped (C3, D-087).
const LOAD_LIMIT: Duration = Duration::from_secs(30);

/// The id [`Browse::loading`] gives the list of databases.
const ROOT_LOAD: &str = "";

/// What the tree holds of one connection while it is open.
struct Browse {
    root: Root,
    tree: Tree,
    /// The connection's own session when the browse started: another one
    /// means the connection was opened again, which closed every session
    /// below (K75).
    meta: Arc<dyn Session>,
    /// The tree's own sessions, opened as databases are browsed: one per
    /// database on PostgreSQL, one for the connection on MySQL (key `""`),
    /// never the connection's meta session, whose keep-alive a read waiting
    /// for a lock would hold, and which a stop could not close (C3, D-087).
    sessions: HashMap<String, Arc<dyn Session>>,
    /// What stops the reads of each of those sessions, by the same key.
    stops: HashMap<String, bridge::Stop>,
    /// The reads in flight, by the node they fill ([`ROOT_LOAD`]: the
    /// databases), each until it ends or its session is stopped.
    loading: HashMap<String, Loading>,
    /// Given when the browse starts, so late answers to an older one are
    /// dropped.
    generation: u64,
    /// The table to select once the way to it is loaded (see [`Navigator::reveal`]).
    revealing: Option<Revealing>,
    /// Where the user was before the connection was opened again, put back
    /// as the reads come in (K77).
    restoring: Option<Restore>,
}

/// Where the user was in the tree of a connection opened again (Reconnect,
/// the Read-only switch, its settings saved, a lost connection back): the
/// nodes open again, each read on the new sessions as an expand reads it,
/// the row selected again, the scroll put back (K77).
#[derive(Debug, Clone, Default)]
struct Restore {
    /// The nodes that were open and are not yet, each after the one above it.
    open: Vec<String>,
    /// The row that was selected (a row id of the sidebar), until it is again.
    selected: Option<String>,
    /// The tree's scroll offset, put back while the tree grows back.
    scroll: Option<gpui_kit::Point<gpui_kit::Pixels>>,
    /// Every node is open again or gone: put back once more, then dropped.
    done: bool,
}

/// The sidebar's one tree (DBeaver's Database Navigator): folders and
/// saved connections are its roots, and each open connection holds its
/// databases, schemas and objects. Several connections are browsed at
/// once; expanding a connection connects it, collapsing it never
/// disconnects it.
pub struct Navigator {
    connections: Entity<Connections>,
    /// Whose menus the folder and connection rows show.
    sidebar: WeakEntity<Sidebar>,
    /// The connections being browsed, by id.
    browse: HashMap<String, Browse>,
    /// Where the user was in the tree of a connection being opened again,
    /// until its new tree starts (K77).
    restore: HashMap<String, Restore>,
    /// The row selected at the last [`Self::sync`]: a row that went with its
    /// connection's tree is still known (K77).
    last_selected: Option<String>,
    /// The connection rows that are open (not kept between runs).
    expanded: HashSet<String>,
    /// The connection of the selected row, otherwise the one opened last.
    current: Option<String>,
    /// The last generation given to a [`Browse`]; only ever grows.
    generation: u64,
    tree_state: Entity<TreeState>,
    filter: Entity<InputState>,
    /// The filter's text, lowercase.
    filter_text: String,
    /// The folders made, closed and their looks (settings) the tree was
    /// last built with.
    folders_seen: FolderSettings,
    /// Where the active query tab runs, kept up to date by the window: what
    /// "Set as default for this tab" changes.
    tab_target: Rc<RefCell<Option<Target>>>,
    /// The confirmed statements running, each until it ends (C3).
    writes: Vec<Write>,
    /// The last key given to a [`Write`].
    last_write: u64,
    /// What has the keyboard when the tree has it (the kit keeps its
    /// handle to itself): taken when a click gives it the focus (K56).
    tree_focus: Option<gpui_kit::WeakFocusHandle>,
    _subscriptions: Vec<Subscription>,
}

/// A confirmed statement the navigator is running: its row says so and
/// offers Cancel, and so does a notification that stays until it ends (C3).
struct Write {
    key: u64,
    connection: String,
    /// The node it acts on (its id local to the connection).
    node: String,
    /// What the row and the notification say ("Dropping table t…").
    doing: String,
    /// Its session once open: what Cancel stops.
    session: Option<Arc<dyn Session>>,
    cancelled: bool,
}

/// Names the notifications of [`Write`]s.
struct WriteNote;

/// A read of the tree in flight (C3).
struct Loading {
    /// The key of its session in [`Browse::sessions`].
    session: String,
    /// What it reads, for its row ("the objects of shop").
    reading: String,
}

/// Why the reads of a session of the tree were stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Halt {
    Cancelled,
    TimedOut,
}

/// What a node's row says once its read was cancelled: not a failure, so short and muted
/// (it fits the sidebar's default width).
pub const CANCELLED_LOAD: &str = "Cancelled. Expand again to load.";

impl Halt {
    /// What the row of a read stopped this way says, plain words first; the databases of a
    /// connection (`root`) have a Try again of their own under it.
    fn message(self, reading: &str, root: bool) -> String {
        match self {
            Self::Cancelled if !root => CANCELLED_LOAD.to_string(),
            Self::Cancelled => {
                format!("Could not read {reading}: cancelled. Refresh to try again.")
            }
            Self::TimedOut => format!(
                "Could not read {reading}: the server took longer than {} s. Refresh to try again.",
                LOAD_LIMIT.as_secs()
            ),
        }
    }
}

/// A table or view [`Navigator::reveal`] is opening the way to.
#[derive(Debug, Clone)]
struct Revealing {
    /// Its database; the connection's own when `None`.
    database: Option<String>,
    table: TableRef,
    kind: RelationKind,
}

impl EventEmitter<NavigatorEvent> for Navigator {}

impl Navigator {
    pub fn new(
        connections: Entity<Connections>,
        sidebar: WeakEntity<Sidebar>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter connections and objects"));
        let subscriptions = vec![
            cx.subscribe(&tree_state, |this, _, event: &TreeEvent, cx| match event {
                TreeEvent::Expanded(id) => this.set_open(id, true, cx),
                TreeEvent::Collapsed(id) => this.set_open(id, false, cx),
            }),
            // The kit's own Up and Down move the selection: follow it.
            cx.observe(&tree_state, |this, state, cx| {
                let selected = state.read(cx).selected_item().map(|item| item.id.clone());
                if let Some(connection) = selected.as_deref().and_then(connection_of) {
                    this.current = Some(connection.to_string());
                }
                cx.notify();
            }),
            cx.subscribe(&filter, |this, filter, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter_text = filter.read(cx).value().trim().to_lowercase();
                    this.sync(cx);
                }
            }),
            cx.subscribe(&connections, |this, connections, event, cx| match event {
                ConnectionsEvent::StatusChanged { id } => this.follow_status(id, cx),
                ConnectionsEvent::ListChanged => {
                    // What was deleted goes; names, folders and the object
                    // filters kept with the connections are shown again.
                    let saved = connections.read(cx);
                    this.browse.retain(|id, _| saved.get(id).is_some());
                    this.expanded.retain(|id| saved.get(id).is_some());
                    if this
                        .current
                        .as_deref()
                        .is_some_and(|id| saved.get(id).is_none())
                    {
                        this.current = None;
                    }
                    this.sync(cx);
                }
                _ => {}
            }),
            // Folders made, and folders open and closed, are settings; the
            // others (some change at each keystroke) leave the tree alone.
            cx.observe_global::<AppState>(|this, cx| {
                let seen = folder_settings(cx);
                if seen != this.folders_seen {
                    this.folders_seen = seen;
                    this.sync(cx);
                }
            }),
        ];
        let mut navigator = Self {
            connections,
            sidebar,
            browse: HashMap::new(),
            restore: HashMap::new(),
            last_selected: None,
            expanded: HashSet::new(),
            current: None,
            generation: 0,
            tree_state,
            filter,
            filter_text: String::new(),
            folders_seen: folder_settings(cx),
            tab_target: Rc::default(),
            writes: Vec::new(),
            last_write: 0,
            tree_focus: None,
            _subscriptions: subscriptions,
        };
        navigator.sync(cx);
        navigator
    }

    /// The connection of the selected row, otherwise the one opened last.
    pub fn connection(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Selects the row of a connection and makes it the current one.
    pub fn select_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        self.current = Some(id.to_string());
        self.select_id(&model::connection_row(id), cx);
        cx.notify();
    }

    /// What expanding a connection, a double click and Enter on it do:
    /// opens its row and connects it when it is closed (DBeaver's
    /// default), then browses it. Disconnecting is the menu's.
    pub fn open_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        self.expanded.insert(id.to_string());
        self.current = Some(id.to_string());
        match self.connections.read(cx).status(id) {
            Status::Disconnected | Status::Failed { .. } => self
                .connections
                .update(cx, |connections, cx| connections.connect(id, cx)),
            Status::Connected { .. } if !self.browse.contains_key(id) => {
                self.load_databases(id, cx)
            }
            _ => {}
        }
        self.sync(cx);
    }

    /// Closes every open connection row (what the empty area's "Collapse
    /// all" does with the folders); nothing is disconnected.
    pub fn collapse_connections(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        self.sync(cx);
    }

    /// Puts the keyboard in the filter field, its text selected.
    pub fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |filter, cx| {
            filter.focus(window, cx);
            filter.select_all(window, cx);
        });
    }

    /// Whether the filter field has the keyboard; for tests.
    #[cfg(test)]
    pub fn filter_focused(&self, window: &Window, cx: &App) -> bool {
        gpui_kit::Focusable::focus_handle(self.filter.read(cx), cx).is_focused(window)
    }

    /// The text of the filter field; for tests.
    #[cfg(test)]
    pub fn filter_text(&self, cx: &App) -> String {
        self.filter.read(cx).value().to_string()
    }

    /// Where the window writes the active query tab's target.
    pub fn tab_target(&self) -> Rc<RefCell<Option<Target>>> {
        self.tab_target.clone()
    }

    /// The session that lists the databases of `connection`, when it is open.
    fn meta(&self, connection: &str, cx: &App) -> Option<Arc<dyn Session>> {
        self.connections.read(cx).meta_session(connection)
    }

    /// Whether databases hold schemas (PostgreSQL) or tables (MySQL).
    fn has_schemas(&self, connection: &str, cx: &App) -> bool {
        self.meta(connection, cx)
            .is_some_and(|meta| meta.dialect().has_schemas())
    }

    /// The tree of `connection`, when it is browsed.
    fn tree(&self, connection: &str) -> Option<&Tree> {
        self.browse.get(connection).map(|browse| &browse.tree)
    }

    /// The browse of `connection` the answer to a load started at
    /// `generation` belongs to, when it is still the one shown.
    fn live(&mut self, connection: &str, generation: u64) -> Option<&mut Browse> {
        self.browse
            .get_mut(connection)
            .filter(|browse| browse.generation == generation)
    }

    /// The connection and node of an object's row.
    fn object(&self, id: &str) -> Option<(String, Node)> {
        let RowId::Object { connection, local } = model::parse_row(id)? else {
            return None;
        };
        let node = self.tree(connection)?.node(local)?.clone();
        Some((connection.to_string(), node))
    }

    /// Where a tab opened from `node` runs: its database, and its schema
    /// when `in_schema`.
    fn target_of(connection: &str, node: &Node, in_schema: bool) -> Target {
        Target {
            connection: connection.to_string(),
            database: Some(node.database.clone()),
            // Without schemas the database is the schema.
            schema: node
                .schema
                .clone()
                .filter(|schema| in_schema && *schema != node.database),
        }
    }

    /// Browses an open connection once it is connected, opens it, and
    /// forgets its tree when it closes.
    fn follow_status(&mut self, id: &str, cx: &mut Context<Self>) {
        let status = self.connections.read(cx).status(id);
        // Opened again or lost: where the user was is kept until the tree is
        // read again (K77). Closed by the user, the tree starts afresh.
        let reopened = match &status {
            Status::Connected { .. } => !self.browsing_meta(id, cx),
            Status::Disconnected => false,
            _ => true,
        };
        if reopened
            && !self.restore.contains_key(id)
            && let Some(place) = self.place_of(id, cx)
        {
            self.restore.insert(id.to_string(), place);
        }
        match status {
            Status::Connected { .. } if !self.browse.contains_key(id) => {
                let restoring = self.restore.contains_key(id);
                self.expanded.insert(id.to_string());
                self.current = Some(id.to_string());
                self.load_databases(id, cx);
                self.sync(cx);
                if !restoring {
                    self.select_id(&model::connection_row(id), cx);
                }
                return;
            }
            // Opened again (Reconnect, the read-only toggle, a lost
            // connection): its statuses come merged, so the tree may never
            // have seen it closed, and its sessions were closed with the
            // old one. Read again from the databases, on new sessions (K75).
            Status::Connected { .. } if !self.browsing_meta(id, cx) => self.load_databases(id, cx),
            // Closed by the user: the row closes too.
            Status::Disconnected => {
                self.forget(id, cx);
                self.restore.remove(id);
                self.expanded.remove(id);
            }
            // The row stays open, so what went wrong shows under it.
            Status::Failed { .. } => {
                self.forget(id, cx);
            }
            _ => {}
        }
        self.sync(cx);
    }

    /// Where the user is in the tree of `connection`, when it is browsed:
    /// its open nodes, its selected row (or the one selected last, when the
    /// tree has already gone from the sidebar), the scroll (K77).
    fn place_of(&self, connection: &str, cx: &App) -> Option<Restore> {
        let browse = self.browse.get(connection)?;
        let state = self.tree_state.read(cx);
        // Opened again while its tree was still growing back: what was left
        // to put back is kept too.
        let pending = browse.restoring.clone().unwrap_or_default();
        let selected = (state.selected_item().map(|item| item.id.to_string()))
            .or(pending.selected)
            .or_else(|| self.last_selected.clone())
            .filter(|row| {
                matches!(model::parse_row(row), Some(RowId::Object { connection: of, .. })
                    if of == connection)
            });
        let mut open = browse.tree.open_ids();
        open.extend(pending.open);
        Some(Restore {
            open,
            selected,
            scroll: (pending.scroll)
                .or_else(|| Some(state.scroll_handle().0.borrow().base_handle.offset())),
            done: false,
        })
    }

    /// Whether the tree of `connection` was read through its session now.
    fn browsing_meta(&self, connection: &str, cx: &App) -> bool {
        let meta = self.meta(connection, cx);
        self.browse
            .get(connection)
            .is_some_and(|browse| meta.is_some_and(|meta| Arc::ptr_eq(&meta, &browse.meta)))
    }

    /// Fetches the list of databases of `connection` again, from scratch.
    pub fn reload(&mut self, connection: &str, cx: &mut Context<Self>) {
        if self.browse.contains_key(connection) {
            self.load_databases(connection, cx);
            self.sync(cx);
        }
    }

    /// Fetches the databases of every connection being browsed again.
    pub fn reload_all(&mut self, cx: &mut Context<Self>) {
        let browsed: Vec<String> = self.browse.keys().cloned().collect();
        for connection in browsed {
            self.reload(&connection, cx);
        }
    }

    /// Starts browsing `connection` anew: lists its databases and opens
    /// its default one.
    fn load_databases(&mut self, connection: &str, cx: &mut Context<Self>) {
        let Some(meta) = self.meta(connection, cx) else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        let current = meta.database();
        let revealing = self
            .forget(connection, cx)
            .and_then(|browse| browse.revealing);
        self.browse.insert(
            connection.to_string(),
            Browse {
                root: Root::Loading,
                tree: Tree::new(meta.dialect().has_schemas())
                    .with_routines(meta.dialect().has_routines()),
                meta: meta.clone(),
                sessions: HashMap::new(),
                stops: HashMap::new(),
                loading: HashMap::new(),
                generation,
                revealing,
                restoring: self.restore.remove(connection),
            },
        );
        // On the tree's session of the connection's own database.
        let database = current.clone().unwrap_or_default();
        self.track(connection, ROOT_LOAD, &database, "the databases".into(), cx);
        let listing = self.read_on(connection, &database, cx, |session| async move {
            session.databases().await
        });
        let connection = connection.to_string();
        cx.spawn(async move |this, cx| {
            let outcome = listing.await;
            let _ = this.update(cx, |this, cx| {
                let Some(browse) = this.live(&connection, generation) else {
                    return;
                };
                // Stopped: `stop_session` said so on the row.
                if browse.loading.remove(ROOT_LOAD).is_none() {
                    return;
                }
                match outcome {
                    Ok(databases) => {
                        browse.root = Root::Loaded;
                        let load = browse.tree.set_databases(databases, current.as_deref());
                        // The connection's database opens by itself: closed
                        // again when the user had closed it (K77).
                        if let (Some(restore), Some(load)) = (&browse.restoring, &load)
                            && !restore.open.contains(&load.id)
                        {
                            browse.tree.collapse(&load.id);
                        }
                        if let Some(load) = load {
                            this.run(&connection, load, cx);
                        }
                        this.advance_reveal(&connection, cx);
                        this.advance_restore(&connection, cx);
                    }
                    Err(error) => {
                        browse.root = Root::Failed(describe(&error).to_string());
                        browse.restoring = None;
                    }
                }
                this.sync(cx);
            });
        })
        .detach();
    }

    fn expand(&mut self, connection: &str, id: &str, cx: &mut Context<Self>) {
        if let Some(load) = self
            .browse
            .get_mut(connection)
            .and_then(|browse| browse.tree.expand(id))
        {
            self.run(connection, load, cx);
        }
    }

    /// Opens or closes the row `id`, whatever it is: a folder (remembered),
    /// a connection (opening it connects it), or an object.
    pub fn set_open(&mut self, id: &str, open: bool, cx: &mut Context<Self>) {
        match model::parse_row(id) {
            Some(RowId::Folder(path)) => set_collapsed(path, !open, cx),
            Some(RowId::Connection(connection)) if open => self.open_connection(connection, cx),
            Some(RowId::Connection(connection)) => {
                self.expanded.remove(connection);
            }
            Some(RowId::Object { connection, local }) if open => self.expand(connection, local, cx),
            Some(RowId::Object { connection, local }) => {
                if let Some(browse) = self.browse.get_mut(connection) {
                    browse.tree.collapse(local);
                }
            }
            Some(RowId::FolderNote(_)) | None => {}
        }
        self.sync(cx);
    }

    /// Closes every open object; folders and connections stay as they are.
    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        fn open(items: &[Item], ids: &mut Vec<String>) {
            for item in items {
                if item.expanded {
                    ids.push(item.id.clone());
                }
                open(&item.children, ids);
            }
        }
        for browse in self.browse.values_mut() {
            let mut ids = Vec::new();
            open(&browse.tree.items(), &mut ids);
            for id in ids {
                browse.tree.collapse(&id);
            }
        }
        self.sync(cx);
    }

    /// Reloads the node `id` of `connection` (or the nearest one above it
    /// that the server answers for).
    fn refresh(&mut self, connection: &str, id: &str, cx: &mut Context<Self>) {
        let Some(tree) = self
            .browse
            .get_mut(connection)
            .map(|browse| &mut browse.tree)
        else {
            return;
        };
        // A database this account may not open is never read: its refresh
        // lists the databases again, which shows a privilege granted since.
        if tree.node(id).is_some_and(Node::is_closed) {
            self.reload(connection, cx);
            return;
        }
        if let Some(target) = tree.refresh_target(id)
            && let Some(load) = tree.refresh(&target)
        {
            self.run(connection, load, cx);
            self.sync(cx);
        }
    }

    /// Reads again the tables of `schema` (in `database`, or in any database
    /// when `None`) when the tree of `connection` shows them.
    pub fn refresh_tables(
        &mut self,
        connection: &str,
        database: Option<&str>,
        schema: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.tree(connection) else {
            return;
        };
        fn ids(items: &[model::Item], out: &mut Vec<String>) {
            for item in items {
                out.push(item.id.clone());
                ids(&item.children, out);
            }
        }
        // ponytail: walks the rows shown, so a folder hidden by the filter
        // is not refreshed; look nodes up in the model if that matters.
        let mut all = Vec::new();
        ids(&tree.items(), &mut all);
        let folders: Vec<String> = all
            .into_iter()
            .filter(|id| {
                tree.node(id).is_some_and(|node| {
                    node.kind == NodeKind::Group(Group::Tables)
                        && node.schema.as_deref() == Some(schema)
                        && database.is_none_or(|database| node.database == database)
                })
            })
            .collect();
        for folder in folders {
            self.refresh(connection, &folder, cx);
        }
    }

    /// Reads `database` of `connection` again, when the tree shows it: what
    /// a restore changed.
    pub fn refresh_database(&mut self, connection: &str, database: &str, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(connection) else {
            return;
        };
        let id = tree.items().into_iter().find_map(|item| {
            let node = tree.node(&item.id)?;
            (node.kind == NodeKind::Object(ObjectRef::Database(database.to_string())))
                .then(|| item.id.clone())
        });
        if let Some(id) = id {
            self.refresh(connection, &id, cx);
        }
    }

    /// Reads the "Extensions" folder of `database` again, when the tree
    /// shows it loaded: what the extensions manager changed (K27).
    pub fn refresh_extensions(&mut self, connection: &str, database: &str, cx: &mut Context<Self>) {
        let Some(tree) = self.tree(connection) else {
            return;
        };
        fn find(tree: &model::Tree, items: &[model::Item], database: &str) -> Option<String> {
            items.iter().find_map(|item| {
                let node = tree.node(&item.id)?;
                if node.kind == NodeKind::Group(Group::Extensions) && node.database == database {
                    matches!(node.children, model::Children::Loaded(_)).then(|| item.id.clone())
                } else {
                    find(tree, &item.children, database)
                }
            })
        }
        if let Some(id) = find(tree, &tree.items(), database) {
            self.refresh(connection, &id, cx);
        }
    }

    /// Whether a node of a connection's tree is selected (not a folder or a
    /// connection row).
    pub fn has_selected_object(&self, cx: &App) -> bool {
        self.tree_state
            .read(cx)
            .selected_item()
            .is_some_and(|item| self.object(&item.id).is_some())
    }

    /// Opens the extensions manager of the database `node` is in, on the
    /// extension `node` is, if it is one (K27); MySQL has none.
    fn manage_extensions(&mut self, connection: &str, node: &Node, cx: &mut Context<Self>) {
        if !self.has_schemas(connection, cx) {
            cx.emit(NavigatorEvent::Warn(
                "Extensions are PostgreSQL's: MySQL and MariaDB have none.".into(),
            ));
            return;
        }
        let extension = match &node.kind {
            NodeKind::Extension { name } => Some(name.clone()),
            _ => None,
        };
        cx.emit(NavigatorEvent::ManageExtensions {
            target: Self::target_of(connection, node, false),
            extension,
        });
    }

    /// Stops the reads of the tree of `connection` and closes its sessions;
    /// gives what it held.
    fn forget(&mut self, connection: &str, cx: &App) -> Option<Browse> {
        let browse = self.browse.remove(connection)?;
        for stop in browse.stops.values() {
            stop.stop(cx);
        }
        Some(browse)
    }

    /// The key of the tree's session that answers for `database`: the
    /// database on PostgreSQL; on MySQL one session sees every database.
    fn session_key(&self, connection: &str, database: &str, cx: &App) -> String {
        if self.has_schemas(connection, cx) {
            database.to_string()
        } else {
            String::new()
        }
    }

    /// Runs `work` on the tree's session of `connection` that answers for
    /// `database`, opened if it is not yet: [`Self::stop_session`] stops it
    /// there (`Error::Cancelled`), with every read queued behind it, and so
    /// does [`LOAD_LIMIT`] running out (C3, D-087).
    fn read_on<
        T: Send + 'static,
        F: std::future::Future<Output = kerno_core::Result<T>> + Send + 'static,
    >(
        &mut self,
        connection: &str,
        database: &str,
        cx: &mut Context<Self>,
        work: impl FnOnce(Arc<dyn Session>) -> F + 'static,
    ) -> Task<kerno_core::Result<T>> {
        let key = self.session_key(connection, database, cx);
        let Some(browse) = self.browse.get_mut(connection) else {
            return Task::ready(Err(Error::Closed("the connection is not open".into())));
        };
        let generation = browse.generation;
        let stop = browse.stops.entry(key.clone()).or_default().clone();
        let opening = match browse.sessions.get(&key) {
            Some(session) => Task::ready(Ok(session.clone())),
            None => {
                let database = (!key.is_empty()).then(|| database.to_string());
                self.connections
                    .read(cx)
                    .open_session(connection, database, cx)
            }
        };
        let connection = connection.to_string();
        // The clock: a read with no answer in time stops its session, with
        // what is queued on it; the rows they were filling say so.
        let ended = Rc::new(std::cell::Cell::new(false));
        let late = Rc::new(std::cell::Cell::new(false));
        let timer = cx.background_executor().timer(LOAD_LIMIT);
        cx.spawn({
            let (ended, late, stop) = (ended.clone(), late.clone(), stop.clone());
            let (connection, key) = (connection.clone(), key.clone());
            async move |this, cx| {
                timer.await;
                // A stopped `Stop` is no longer the session's: nothing to do.
                if ended.get() || stop.is_stopped() {
                    return;
                }
                late.set(true);
                let _ = this.update(cx, |this, cx| {
                    this.stop_session(&connection, &key, Halt::TimedOut, cx)
                });
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let outcome = async {
                let session = opening.await?;
                // Kept for the next reads, unless stopped meanwhile or another
                // opened at the same time is kept already.
                let keep = this
                    .update(cx, |this, _| {
                        let Some(browse) = this.live(&connection, generation) else {
                            return false;
                        };
                        if stop.is_stopped() {
                            return false;
                        }
                        let kept = browse
                            .sessions
                            .entry(key.clone())
                            .or_insert_with(|| session.clone());
                        Arc::ptr_eq(kept, &session)
                    })
                    .unwrap_or(false);
                let used = session.clone();
                let outcome = cx
                    .update(|cx| {
                        stop.run(cx, session, move |session| {
                            let reading = work(session.clone());
                            async move {
                                let outcome = reading.await;
                                // A session not kept holds no connection once read.
                                if !keep {
                                    session.close().await;
                                }
                                outcome
                            }
                        })
                    })
                    .await
                    .unwrap_or(Err(Error::Cancelled));
                // A session found closed or lost is let go: the next read
                // opens a new one (K75).
                if matches!(&outcome, Err(error) if error.is_disconnect()) {
                    let _ = this.update(cx, |this, _| {
                        if let Some(browse) = this.live(&connection, generation)
                            && (browse.sessions.get(&key)).is_some_and(|s| Arc::ptr_eq(s, &used))
                        {
                            browse.sessions.remove(&key);
                        }
                    });
                }
                outcome
            }
            .await;
            ended.set(true);
            match outcome {
                Err(_) if late.get() => {
                    Err(Error::Db(Box::new(kerno_core::DbError::message(format!(
                        "the server took longer than {} s. Try again.",
                        LOAD_LIMIT.as_secs()
                    )))))
                }
                outcome => outcome,
            }
        })
    }

    /// Notes the read of node `id` (on the session of `database`): its row
    /// says why when its session is stopped.
    fn track(
        &mut self,
        connection: &str,
        id: &str,
        database: &str,
        reading: String,
        cx: &mut Context<Self>,
    ) {
        let session = self.session_key(connection, database, cx);
        if let Some(browse) = self.browse.get_mut(connection) {
            browse
                .loading
                .insert(id.to_string(), Loading { session, reading });
        }
    }

    /// Stops every read of the tree's session `key` of `connection` and
    /// closes it, which stops its statement on the server (C3); the rows
    /// they were filling say why. The next read opens a new session.
    fn stop_session(&mut self, connection: &str, key: &str, why: Halt, cx: &mut Context<Self>) {
        let Some(browse) = self.browse.get_mut(connection) else {
            return;
        };
        browse.sessions.remove(key);
        if let Some(stop) = browse.stops.remove(key) {
            stop.stop(cx);
        }
        let stopped: Vec<String> = browse
            .loading
            .iter()
            .filter(|(_, loading)| loading.session == key)
            .map(|(id, _)| id.clone())
            .collect();
        for id in stopped {
            let Some(loading) = browse.loading.remove(&id) else {
                continue;
            };
            let message = why.message(&loading.reading, id == ROOT_LOAD);
            if id == ROOT_LOAD {
                browse.root = Root::Failed(message);
                browse.restoring = None;
            } else {
                browse.tree.finish(&id, Err(message));
            }
        }
        self.advance_restore(connection, cx);
        self.sync(cx);
    }

    /// The read filling the row `id` (an object row, its "Loading…" line,
    /// a connection row or its "Loading databases…" line), when one runs:
    /// its connection and the node it fills.
    fn load_of_row(&self, id: &str) -> Option<(String, String)> {
        let (connection, node) = match model::parse_row(id)? {
            RowId::Connection(connection) => (connection, ROOT_LOAD),
            RowId::Object { connection, local } if local == LINE_STATUS => (connection, ROOT_LOAD),
            RowId::Object { connection, local } if model::is_note(local) => {
                (connection, model::parent_of(local)?)
            }
            RowId::Object { connection, local } => (connection, local),
            _ => return None,
        };
        self.browse
            .get(connection)?
            .loading
            .contains_key(node)
            .then(|| (connection.to_string(), node.to_string()))
    }

    /// Cancel on a loading row (its menu, its button, Cmd+.): stops the
    /// read on the server, with those queued behind it on its session
    /// (C3). Gives whether one ran.
    pub fn cancel_load(&mut self, row: &str, cx: &mut Context<Self>) -> bool {
        let Some((connection, node)) = self.load_of_row(row) else {
            return false;
        };
        let Some(session) = self
            .browse
            .get(&connection)
            .and_then(|browse| browse.loading.get(&node))
            .map(|loading| loading.session.clone())
        else {
            return false;
        };
        self.stop_session(&connection, &session, Halt::Cancelled, cx);
        true
    }

    /// Cmd+. in the tree: cancels the read of the selected row.
    fn cancel_selected_load(&mut self, cx: &mut Context<Self>) -> bool {
        let selected = self
            .tree_state
            .read(cx)
            .selected_item()
            .map(|item| item.id.to_string());
        selected.is_some_and(|row| self.cancel_load(&row, cx))
    }

    /// Runs a load of `connection` on the session of its database.
    fn run(&mut self, connection: &str, load: Load, cx: &mut Context<Self>) {
        self.read_load(connection, load, true, cx);
    }

    /// [`Self::run`]; `again`: a session found closed or lost is replaced
    /// and the load read once more, without asking (CLAUDE.md §4 A5, K75).
    /// Loads only read: a statement that writes is never run again.
    fn read_load(&mut self, connection: &str, load: Load, again: bool, cx: &mut Context<Self>) {
        let Some(generation) = self.browse.get(connection).map(|browse| browse.generation) else {
            return;
        };
        // What a failed load says on its row, then the server's words (C14).
        let reading = match &load.what {
            LoadKind::Schemas => "the schemas".to_string(),
            LoadKind::Objects { schema } => format!("the objects of {schema}"),
            LoadKind::Structure { table } => format!("the columns of {}", table.name),
            LoadKind::Extensions => "the extensions".to_string(),
        };
        self.track(connection, &load.id, &load.database, reading.clone(), cx);
        let what = load.what.clone();
        let fetching = self.read_on(connection, &load.database, cx, move |session| async move {
            Ok(match what {
                LoadKind::Schemas => Loaded::Schemas(session.schemas().await?),
                LoadKind::Objects { schema } => {
                    Loaded::Objects(Box::new(session.objects(&schema).await?))
                }
                LoadKind::Structure { table } => {
                    Loaded::Structure(Box::new(session.table_structure(&table).await?))
                }
                LoadKind::Extensions => Loaded::Extensions(session.extensions().await?),
            })
        });
        let connection = connection.to_string();
        cx.spawn(async move |this, cx| {
            let outcome = fetching.await;
            let _ = this.update(cx, |this, cx| {
                if again
                    && matches!(&outcome, Err(error) if error.is_disconnect())
                    && (this.live(&connection, generation))
                        .is_some_and(|browse| browse.loading.contains_key(&load.id))
                {
                    // The connection too may be gone: checked, and opened
                    // again if it is (the tree is then read again).
                    this.connections
                        .update(cx, |connections, cx| connections.verify(&connection, cx));
                    this.read_load(&connection, load, false, cx);
                    return;
                }
                let config =
                    (this.connections.read(cx).get(&connection)).map(|saved| saved.config.clone());
                let outcome = outcome.map_err(|error: kerno_core::Error| {
                    read_failure(&error, &reading, &load.database, config.as_ref())
                });
                let Some(browse) = this.live(&connection, generation) else {
                    return;
                };
                // Stopped: `stop_session` said so on the row.
                if browse.loading.remove(&load.id).is_none() {
                    return;
                }
                browse.tree.finish(&load.id, outcome);
                this.advance_reveal(&connection, cx);
                this.advance_restore(&connection, cx);
                this.sync(cx);
            });
        })
        .detach();
    }

    /// Selects a table or view in the tree, opening and loading the way to
    /// it (DBeaver's "Link with editor", `core.navigator.linkeditor`): the
    /// folders and the row of `target`'s connection open first. A
    /// connection that is not open is only selected, never connected;
    /// nothing is selected when the table is not there.
    pub fn reveal(
        &mut self,
        target: &Target,
        table: &TableRef,
        kind: RelationKind,
        cx: &mut Context<Self>,
    ) {
        let connection = target.connection.clone();
        let folder = self
            .connections
            .read(cx)
            .get(&connection)
            .and_then(|saved| folder_of(&saved.config));
        let mut above = folder.as_deref();
        while let Some(folder) = above {
            if AppState::get(cx)
                .settings
                .collapsed_folders
                .contains(folder)
            {
                set_collapsed(folder, false, cx);
            }
            above = parent_folder(folder);
        }
        self.current = Some(connection.clone());
        let Some(browse) = self.browse.get_mut(&connection) else {
            self.sync(cx);
            self.select_id(&model::connection_row(&connection), cx);
            return;
        };
        browse.revealing = Some(Revealing {
            database: target.database.clone(),
            table: table.clone(),
            kind,
        });
        self.expanded.insert(connection.clone());
        self.advance_reveal(&connection, cx);
    }

    /// Takes the reveal one load further; called again as loads finish.
    fn advance_reveal(&mut self, connection: &str, cx: &mut Context<Self>) {
        let default = self.meta(connection, cx).and_then(|meta| meta.database());
        let Some(browse) = self.browse.get_mut(connection) else {
            return;
        };
        // The databases come first.
        if browse.root != Root::Loaded {
            return;
        }
        let Some(revealing) = browse.revealing.clone() else {
            return;
        };
        let database = revealing.database.or(default).unwrap_or_default();
        let path = browse
            .tree
            .relation_path(&database, &revealing.table, revealing.kind);
        match browse.tree.reveal(&path) {
            Reveal::Waiting(Some(load)) => self.run(connection, load, cx),
            Reveal::Waiting(None) => {}
            Reveal::Found => {
                browse.revealing = None;
                self.sync(cx);
                let local = path.last().cloned().unwrap_or_default();
                let shown = self.select_id(&model::object_row(connection, &local), cx);
                // Loaded but hidden by an object filter: say so, and leave
                // the filter as the user set it.
                if !shown
                    && self
                        .tree(connection)
                        .is_some_and(|tree| tree.node(&local).is_some())
                {
                    cx.emit(NavigatorEvent::Warn(format!(
                        "{} is hidden by an object filter of the schema tree: turn the \
                         filter off (Filter ▸ Turn on or off) to see it there.",
                        revealing.table.name
                    )));
                }
                return;
            }
            Reveal::Missing => browse.revealing = None,
        }
        self.sync(cx);
    }

    /// Opens again the nodes of a [`Restore`] that are there, reading those
    /// whose children are not known yet; one that is not there once the node
    /// above it is read is gone, and forgotten. Called again as loads finish.
    fn advance_restore(&mut self, connection: &str, cx: &mut Context<Self>) {
        let Some(browse) = self.browse.get_mut(connection) else {
            return;
        };
        // The databases come first.
        if browse.root != Root::Loaded {
            return;
        }
        let Some(mut restore) = browse.restoring.take() else {
            return;
        };
        let tree = &mut browse.tree;
        let mut loads = Vec::new();
        restore.open.retain(|id| {
            if tree.node(id).is_some() {
                loads.extend(tree.expand(id));
                return false;
            }
            // Not there: waits while the way to it is being read.
            let mut above = model::parent_of(id);
            while let Some(parent) = above {
                if let Some(node) = tree.node(parent) {
                    return matches!(node.children, Children::Loading);
                }
                above = model::parent_of(parent);
            }
            false
        });
        restore.done = restore.open.is_empty() && loads.is_empty() && browse.loading.is_empty();
        browse.restoring = Some(restore);
        for load in loads {
            self.run(connection, load, cx);
        }
    }

    /// Selects the row `id` and scrolls to it; whether it is shown.
    fn select_id(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let id = SharedString::from(id.to_string());
        self.tree_state
            .update(cx, |state, cx| {
                let index = state.index_of(&id)?;
                state.set_selected_index(Some(index), cx);
                state.scroll_to_item(index, gpui_kit::ScrollStrategy::Center);
                Some(())
            })
            .is_some()
    }

    /// The table or view a node of `connection` is in or under (itself for
    /// a table), with its node id.
    fn relation_above(
        &self,
        connection: &str,
        id: &str,
    ) -> Option<(TableRef, RelationKind, String)> {
        let tree = self.tree(connection)?;
        let mut id = id;
        loop {
            if let NodeKind::Object(ObjectRef::Relation(table, kind)) = &tree.node(id)?.kind {
                return Some((table.clone(), *kind, id.to_string()));
            }
            id = model::parent_of(id)?;
        }
    }

    // Actions -----------------------------------------------------------------

    /// Does what a menu entry says for the node `id`.
    pub fn perform(
        &mut self,
        action: Action,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((connection, node)) = self.object(id) else {
            return;
        };
        let connection = connection.as_str();
        match action {
            Action::Refresh => self.refresh(connection, &node.id, cx),
            Action::Properties
                if matches!(
                    node.kind,
                    NodeKind::Object(ObjectRef::Database(_) | ObjectRef::Schema(_))
                ) =>
            {
                self.show_properties(connection, &node, window, cx)
            }
            Action::OpenData | Action::OpenStructure | Action::Properties => {
                // A column opens its table; a foreign key the table at its
                // other end.
                let opened = match &node.kind {
                    NodeKind::Object(ObjectRef::Relation(table, kind)) => {
                        Some((table.clone(), *kind))
                    }
                    NodeKind::ForeignKey { other, .. } => {
                        Some((other.clone(), RelationKind::Table))
                    }
                    NodeKind::Column { .. } => self
                        .relation_above(connection, &node.id)
                        .map(|(table, kind, _)| (table, kind)),
                    _ => None,
                };
                if let Some((table, kind)) = opened {
                    let target = Self::target_of(connection, &node, false);
                    cx.emit(match action {
                        Action::OpenData => NavigatorEvent::OpenData {
                            target,
                            table,
                            kind,
                        },
                        Action::Properties => NavigatorEvent::ShowProperties {
                            target,
                            table,
                            kind,
                        },
                        _ => NavigatorEvent::ShowStructure {
                            target,
                            table,
                            kind,
                        },
                    });
                }
            }
            Action::CopyName => cx.write_to_clipboard(ClipboardItem::new_string(name_of(&node))),
            Action::CopyQualifiedName => {
                if let (NodeKind::Object(object), Some(meta)) =
                    (&node.kind, self.meta(connection, cx))
                {
                    let name = qualified_name(object, meta.dialect());
                    cx.write_to_clipboard(ClipboardItem::new_string(name));
                }
            }
            Action::SetDefault => {
                if self.unavailable(connection, action, &node, cx).is_none() {
                    let target = Self::target_of(connection, &node, true);
                    cx.emit(NavigatorEvent::SetDefault { target });
                }
            }
            Action::Export => {
                if let NodeKind::Object(ObjectRef::Relation(table, _)) = &node.kind {
                    let target = Self::target_of(connection, &node, false);
                    let source = Source::Table {
                        table: table.clone(),
                        filter: Filter::default(),
                        sort: None,
                    };
                    export_dialog::open(self.connections.clone(), target, source, window, cx);
                }
            }
            Action::ImportCsv => {
                if self.unavailable(connection, action, &node, cx).is_none()
                    && let Some(schema) = node.schema.clone()
                {
                    let target = Self::target_of(connection, &node, false);
                    let table = match &node.kind {
                        NodeKind::Object(ObjectRef::Relation(table, _)) => Some(table.clone()),
                        _ => None,
                    };
                    cx.emit(NavigatorEvent::ImportCsv {
                        target,
                        schema,
                        table,
                    });
                }
            }
            Action::ViewDiagram => {
                let scope = match &node.kind {
                    NodeKind::Object(ObjectRef::Schema(schema) | ObjectRef::Database(schema)) => {
                        Some(kerno_core::erd::Scope::Schema(schema.clone()))
                    }
                    NodeKind::Object(ObjectRef::Relation(table, _)) => {
                        Some(kerno_core::erd::Scope::Around(table.clone()))
                    }
                    _ => None,
                };
                if let Some(scope) = scope {
                    let target = Self::target_of(connection, &node, false);
                    cx.emit(NavigatorEvent::ViewDiagram { target, scope });
                }
            }
            Action::CompareWith => self.perform_tool(ToolEntry::CompareSchemas, id, window, cx),
            Action::CompareData => {
                if let NodeKind::Object(ObjectRef::Relation(table, _)) = &node.kind {
                    let target = Self::target_of(connection, &node, true);
                    let table = table.name.clone();
                    cx.emit(NavigatorEvent::CompareData { target, table });
                }
            }
            Action::GenerateTestData => {
                if let NodeKind::Object(ObjectRef::Relation(table, _)) = &node.kind {
                    let target = Self::target_of(connection, &node, true);
                    let table = table.name.clone();
                    cx.emit(NavigatorEvent::GenerateTestData { target, table });
                }
            }
            Action::ExportSchema => {
                let tables = match &node.kind {
                    NodeKind::Object(ObjectRef::Relation(table, _)) => vec![table.name.clone()],
                    _ => Vec::new(),
                };
                let target = Self::target_of(connection, &node, true);
                cx.emit(NavigatorEvent::ExportSchema { target, tables });
            }
            Action::Backup | Action::Restore => {
                let entry = if action == Action::Backup {
                    ToolEntry::Backup
                } else {
                    ToolEntry::Restore
                };
                self.perform_tool(entry, id, window, cx);
            }
            Action::CreateTable => {
                if self.unavailable(connection, action, &node, cx).is_none()
                    && let Some(schema) = node.schema.clone()
                {
                    let target = Self::target_of(connection, &node, false);
                    cx.emit(NavigatorEvent::CreateTable { target, schema });
                }
            }
            Action::RevealTable => {
                if let NodeKind::ForeignKey { other, .. } = &node.kind {
                    let target = Self::target_of(connection, &node, false);
                    self.reveal(&target, other, RelationKind::Table, cx);
                }
            }
            Action::AddColumn | Action::EditColumn => {
                let column = match (&node.kind, action) {
                    (NodeKind::Column { name, .. }, Action::EditColumn) => Some(name.clone()),
                    _ => None,
                };
                if self.unavailable(connection, action, &node, cx).is_none()
                    && let Some((table, _, _)) = self.relation_above(connection, &node.id)
                {
                    let target = Self::target_of(connection, &node, false);
                    cx.emit(NavigatorEvent::EditTable {
                        target,
                        table,
                        column,
                    });
                }
            }
            Action::NewQueryHere
            | Action::CreateView
            | Action::CreateFunction
            | Action::CreateProcedure
            | Action::CreateSequence
            | Action::CreateTrigger
            | Action::CreateEvent => {
                if self.unavailable(connection, action, &node, cx).is_none() {
                    self.open_template(connection, action, &node, window, cx)
                }
            }
            Action::Truncate => {
                self.open_tools(
                    id,
                    Some(&Maintenance::Truncate(Default::default())),
                    window,
                    cx,
                );
            }
            Action::Tools => {
                self.open_tools(id, None, window, cx);
            }
            Action::Generate(template) => {
                self.generate(connection, template, &node, false, window, cx)
            }
            Action::ReadInConsole => {
                self.generate(connection, Template::Select, &node, true, window, cx)
            }
            Action::GenerateCall | Action::ExecuteRoutine => self.call(
                connection,
                &node,
                action == Action::ExecuteRoutine,
                window,
                cx,
            ),
            Action::ShowDdl => self.show_ddl(connection, &node, window, cx),
            Action::EditSource => {
                if self.unavailable(connection, action, &node, cx).is_none() {
                    self.edit_source(connection, &node, window, cx)
                }
            }
            Action::CreateDatabase if self.offers_database_options(connection, cx) => {
                self.create_database_form(id, window, cx);
            }
            Action::ManageExtensions | Action::ShowInExtensions => {
                self.manage_extensions(connection, &node, cx)
            }
            Action::Rename
            | Action::Drop
            | Action::CreateSchema
            | Action::CreateDatabase
            | Action::CreateEnum
            | Action::AddEnumValue
            | Action::RenameEnumValue
            | Action::DropCascade
            | Action::RenameColumn
            | Action::DropColumn => self.confirm(connection, action, &node, window, cx),
        }
    }

    /// The kind of object the Filter ▸ entries of `node` act on: a
    /// folder's objects, or the node's own kind.
    fn filter_kind(node: &Node) -> String {
        match node.kind {
            NodeKind::Group(_) => model::children_key(&node.id),
            _ => model::filter_key(&node.id),
        }
    }

    /// The filter of `connection` on the objects of kind `key`.
    fn object_filter(&self, connection: &str, key: &str, cx: &App) -> Option<ObjectFilter> {
        let saved = self.connections.read(cx).get(connection)?;
        saved.config.navigator_filters.get(key).cloned()
    }

    /// Why a Filter ▸ entry cannot be used on `node`, when it cannot.
    pub fn filter_unavailable(
        &self,
        connection: &str,
        action: FilterAction,
        node: &Node,
        cx: &App,
    ) -> Option<&'static str> {
        let missing = self
            .object_filter(connection, &Self::filter_kind(node), cx)
            .is_none();
        (matches!(action, FilterAction::Toggle | FilterAction::Clear) && missing)
            .then_some("no filter")
    }

    /// Does what a Filter ▸ entry says for the node `id`.
    pub fn perform_filter(
        &mut self,
        action: FilterAction,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((connection, node)) = self.object(id) else {
            return;
        };
        if !filter_actions(&node).contains(&action)
            || self
                .filter_unavailable(&connection, action, &node, cx)
                .is_some()
        {
            return;
        }
        let key = Self::filter_kind(&node);
        let current = self.object_filter(&connection, &key, cx);
        let filter = match action {
            FilterAction::Hide | FilterAction::ShowOnly => {
                let Some(name) = model::filter_name(&node) else {
                    return;
                };
                let mut filter = current.unwrap_or_default();
                filter.enabled = true;
                let mask = exact_mask(name);
                let masks = if action == FilterAction::Hide {
                    &mut filter.exclude
                } else {
                    &mut filter.include
                };
                if !masks.contains(&mask) {
                    masks.push(mask);
                }
                Some(filter)
            }
            FilterAction::Toggle => current.map(|filter| ObjectFilter {
                enabled: !filter.enabled,
                ..filter
            }),
            FilterAction::Clear => None,
            FilterAction::Configure => {
                let view = cx.entity();
                filter_dialog::open(
                    &model::filter_kind_label(&key),
                    current.unwrap_or_default(),
                    window,
                    cx,
                    move |filter, _, cx| {
                        view.update(cx, |this, cx| {
                            this.set_object_filter(&connection, &key, filter, cx)
                        })
                    },
                );
                return;
            }
        };
        self.set_object_filter(&connection, &key, filter, cx);
    }

    /// Saves the filter of a kind of object with the connection, and shows
    /// the tree through it.
    fn set_object_filter(
        &mut self,
        connection: &str,
        key: &str,
        filter: Option<ObjectFilter>,
        cx: &mut Context<Self>,
    ) {
        self.connections.update(cx, |connections, cx| {
            connections.set_navigator_filter(connection, key, filter, cx)
        });
        self.sync(cx);
    }

    /// Runs a Filter ▸ entry on the selected node; says so when there is none.
    pub fn filter_selected(
        &mut self,
        action: FilterAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((connection, node)) = self.selected_node(window, cx) {
            self.perform_filter(
                action,
                &model::object_row(&connection, &node.id),
                window,
                cx,
            );
        }
    }

    /// What a row shows after its name: the node's hint, and on a filtered
    /// folder how many objects its filter hides.
    fn row_hint(tree: &Tree, node: &Node) -> Option<String> {
        let hidden = tree
            .hidden_below(Some(&node.id))
            .map(|hidden| match hidden {
                0 => "filtered".to_string(),
                hidden => format!("{hidden} hidden"),
            });
        match (node.hint.clone(), hidden) {
            (Some(hint), Some(hidden)) => Some(format!("{hint} · {hidden}")),
            (hint, hidden) => hint.or(hidden),
        }
    }

    /// The hint of the row of a node of the current connection, by its id
    /// in that connection's tree; for tests.
    #[cfg(test)]
    pub fn hint_of(&self, id: &str) -> Option<String> {
        let tree = self.tree(self.current.as_deref()?)?;
        tree.node(id).and_then(|node| Self::row_hint(tree, node))
    }

    /// Whether the engine creates databases with options (see
    /// [`database_dialog`]).
    fn offers_database_options(&self, connection: &str, cx: &App) -> bool {
        self.meta(connection, cx)
            .is_some_and(|meta| meta.dialect().database_options_sql().is_some())
    }

    /// Opens the form of a new database with its options; what it writes
    /// is confirmed, then run on the connection's own session.
    pub fn create_database_form(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<database_dialog::CreateDatabaseForm>> {
        let (connection, node) = self.object(id)?;
        if self
            .unavailable(&connection, Action::CreateDatabase, &node, cx)
            .is_some()
        {
            return None;
        }
        let connections = self.connections.read(cx);
        let (saved, meta) = (
            connections.get(&connection)?,
            connections.meta_session(&connection)?,
        );
        let (connection_name, environment) =
            (saved.config.name.clone(), saved.config.environment.clone());
        let view = cx.entity();
        let form = database_dialog::open(meta, window, cx, move |sql, window, cx| {
            let (view, connection, node) = (view.clone(), connection.clone(), node.id.clone());
            sql_confirm::open(
                Confirmation {
                    title: "Create database".into(),
                    explanation: "A new, empty database on this server.".into(),
                    statement: Statement::Fixed(sql),
                    action: "Create database",
                    destructive: false,
                    connection: connection_name.clone(),
                    environment: environment.clone(),
                    writes: true,
                },
                window,
                cx,
                move |sql, window, cx| {
                    let plan = Plan {
                        connection: connection.clone(),
                        database: None,
                        release: None,
                        after: After::Reload,
                        done: "Created the database.".into(),
                        node: node.clone(),
                        doing: "Creating the database…".into(),
                        failed: "The database was not created.".into(),
                    };
                    view.update(cx, |this, cx| this.execute(sql, plan, window, cx));
                },
            );
        });
        Some(form)
    }

    /// Why a menu entry cannot be used on this node of `connection`, when
    /// it cannot.
    pub fn unavailable(
        &self,
        connection: &str,
        action: Action,
        node: &Node,
        cx: &App,
    ) -> Option<&'static str> {
        let connections = self.connections.read(cx);
        let saved = connections.get(connection)?;
        let meta = connections.meta_session(connection)?;
        let dialect = meta.dialect();
        if action.writes() && saved.config.read_only {
            return Some("read-only connection");
        }
        let object = match &node.kind {
            NodeKind::Object(object) => Some(object),
            _ => None,
        };
        match (action, object) {
            (Action::Rename, Some(object)) if dialect.rename_sql(object, "name").is_err() => {
                Some("not supported by this database")
            }
            (Action::Drop | Action::DropCascade, Some(object))
                if dialect.drop_sql(object).is_err() =>
            {
                Some("not supported by this database")
            }
            (Action::Truncate, Some(object))
                if !dialect
                    .maintenance_tools(object)
                    .iter()
                    .any(|tool| matches!(tool, Maintenance::Truncate(_))) =>
            {
                Some("not supported by this database")
            }
            (
                Action::AddColumn | Action::EditColumn | Action::RenameColumn | Action::DropColumn,
                _,
            ) if !self
                .relation_above(connection, &node.id)
                .is_some_and(|(_, kind, _)| kind.is_table()) =>
            {
                Some("a view's columns come from its query")
            }
            (Action::EditSource, Some(object))
                if dialect.replace_source_sql(object, "").is_err() =>
            {
                Some("not supported by this database")
            }
            // SQLite: a database is a file, and has no schemas.
            (Action::CreateDatabase, _) if dialect.create_database_sql("name").is_err() => {
                Some("a SQLite database is a file: use New file… in a new connection")
            }
            (Action::CreateSchema, _) if dialect.create_schema_sql("name").is_err() => {
                Some("this database has no schemas")
            }
            // The compare has no SQLite side yet (K30): said here, not a
            // menu entry that does nothing.
            (Action::CompareWith, _) if saved.config.engine.is_file() => {
                Some("comparing SQLite schemas is not supported yet")
            }
            (Action::CompareData, _) if saved.config.engine.is_file() => {
                Some("comparing the rows of SQLite tables is not supported yet")
            }
            // PostgreSQL cannot drop the database a session is in.
            (Action::Drop, Some(ObjectRef::Database(name)))
                if dialect.has_schemas() && meta.database().as_deref() == Some(name.as_str()) =>
            {
                Some("the connection is using it")
            }
            (Action::SetDefault, _) => match &*self.tab_target.borrow() {
                None => Some("no query tab is active"),
                Some(tab) if tab.connection != connection => {
                    Some("the active tab is on another connection")
                }
                Some(_) => None,
            },
            _ => None,
        }
    }

    /// Opens the maintenance tools of the node `id` on the tool like
    /// `wanted` (the first of its tools when `None`), or says why it cannot.
    /// Once a tool ran, the node's parent is read again (row counts, a
    /// refreshed view, a trigger switched).
    pub fn open_tools(
        &mut self,
        id: &str,
        wanted: Option<&Maintenance>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<MaintenanceDialog>> {
        let (connection, node) = self.object(id)?;
        let meta = self.meta(&connection, cx)?;
        let warn = |text: String, window: &mut Window, cx: &mut Context<Self>| {
            window.push_notification(Notification::warning(SharedString::from(text)), cx);
        };
        let available = tools(&node, meta.dialect());
        let (NodeKind::Object(object), false) = (&node.kind, available.is_empty()) else {
            warn(NO_TOOLS.into(), window, cx);
            return None;
        };
        if let Some(reason) = self.unavailable(&connection, Action::Tools, &node, cx) {
            warn(format!("Maintenance tools: {reason}."), window, cx);
            return None;
        }
        // A database is the root of the tree: nothing above it to read again.
        let above = model::parent_of(&node.id)
            .filter(|_| !matches!(object, ObjectRef::Database(_)))
            .map(str::to_string);
        let navigator = cx.entity().downgrade();
        let opening = maintenance_dialog::Opening {
            connection: connection.clone(),
            database: node.database.clone(),
            object: object.clone(),
            kind: kind_name(object),
            name: plain_name(object),
            selected: maintenance_dialog::position(&available, wanted),
            tools: available,
        };
        Some(maintenance_dialog::open(
            self.connections.clone(),
            opening,
            window,
            cx,
            move |_, cx| {
                if let Some(above) = &above {
                    let _ = navigator.update(cx, |this, cx| this.refresh(&connection, above, cx));
                }
            },
        ))
    }

    /// Tools ▸ of `node` for its kind (see [`tool_entries`]).
    fn tool_entries_of(&self, connection: &str, node: &Node, cx: &App) -> ToolGroups {
        let Some(meta) = self.meta(connection, cx) else {
            return Vec::new();
        };
        let offered = tools(node, meta.dialect());
        tool_entries(Some(node), meta.engine(), &offered)
    }

    /// Tools ▸ of `node` as its menu shows it: each entry with why it
    /// cannot be used now, if it cannot.
    pub fn tool_menu(&self, connection: &str, node: &Node, cx: &App) -> ToolGroups {
        let read_only = self
            .connections
            .read(cx)
            .get(connection)
            .is_some_and(|saved| saved.config.read_only);
        // The entries that were in the plain menu keep its rules.
        let moved = |entry: &ToolEntry| match entry {
            ToolEntry::Backup => Some(Action::Backup),
            ToolEntry::Restore => Some(Action::Restore),
            ToolEntry::Maintain(Maintenance::Truncate(_)) => Some(Action::Truncate),
            _ => None,
        };
        tools_now(self.tool_entries_of(connection, node, cx), true, read_only)
            .into_iter()
            .map(|group| {
                group
                    .into_iter()
                    .map(|(entry, reason)| {
                        let reason = reason.or_else(|| {
                            moved(&entry)
                                .and_then(|action| self.unavailable(connection, action, node, cx))
                        });
                        (entry, reason)
                    })
                    .collect()
            })
            .collect()
    }

    /// Does what a Tools ▸ entry says for the node `id`, or says why it
    /// cannot. Maintenance tools open here; the rest opens in the window.
    pub fn perform_tool(
        &mut self,
        entry: ToolEntry,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((connection, node)) = self.object(id) else {
            return;
        };
        let listed = self
            .tool_menu(&connection, &node, cx)
            .concat()
            .into_iter()
            .find(|(listed, _)| same_tool(listed, &entry));
        match listed {
            None => return,
            Some((_, Some(reason))) => {
                let text = format!("{}: {reason}.", entry.label().trim_end_matches('…'));
                window.push_notification(Notification::warning(SharedString::from(text)), cx);
                return;
            }
            Some(_) => {}
        }
        if let ToolEntry::Maintain(tool) = &entry {
            self.open_tools(id, Some(tool), window, cx);
            return;
        }
        // A table's backup is of that table; a schema's starts with it
        // ticked.
        let tables = match (&entry, &node.kind) {
            (
                ToolEntry::Backup | ToolEntry::NewTask | ToolEntry::CopyTables,
                NodeKind::Object(ObjectRef::Relation(table, _)),
            ) => vec![table.clone()],
            _ => Vec::new(),
        };
        let in_schema = matches!(node.kind, NodeKind::Object(ObjectRef::Schema(_)));
        let target = Self::target_of(&connection, &node, in_schema);
        cx.emit(NavigatorEvent::Tool {
            target,
            entry,
            tables,
        });
    }

    /// What "Maintenance Tools…" does: the tools of the selected node.
    pub fn tools_on_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((connection, node)) = self.selected_node(window, cx) {
            self.open_tools(&model::object_row(&connection, &node.id), None, window, cx);
        }
    }

    /// Opens a query tab with a statement to complete: nothing runs.
    fn open_template(
        &mut self,
        connection: &str,
        action: Action,
        node: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A query on a PostgreSQL database runs in no schema in particular.
        let Some(schema) = node
            .schema
            .clone()
            .or_else(|| (action == Action::NewQueryHere).then(|| node.database.clone()))
        else {
            return;
        };
        let target = Some(Self::target_of(
            connection,
            node,
            action == Action::NewQueryHere,
        ));
        // A trigger or column made from a table's folder is on that table.
        let table = self
            .relation_above(connection, &node.id)
            .map(|(table, _, _)| table);
        // The template needs the engine's dialect only: nothing is read.
        let Some(meta) = self.meta(connection, cx) else {
            window.push_notification(
                crate::error_notification("The connection is not open.".into()),
                cx,
            );
            return;
        };
        let dialect = meta.dialect();
        let (title, sql) = match action {
            Action::CreateView => (
                "New view".to_string(),
                format!(
                    "CREATE VIEW {} AS\nSELECT 1 AS one;\n",
                    dialect.table_name(&TableRef::new(schema, "new_view"))
                ),
            ),
            // An empty tab that runs in this schema.
            Action::NewQueryHere => (schema.clone(), String::new()),
            _ => match create_template(action, dialect, &schema, table.as_ref()) {
                Some(template) => template,
                None => return,
            },
        };
        cx.emit(NavigatorEvent::OpenQuery { title, sql, target });
    }

    /// Writes a statement for a table into a new query tab, which runs it
    /// when `run` says so.
    fn generate(
        &mut self,
        connection: &str,
        template: Template,
        node: &Node,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let NodeKind::Object(ObjectRef::Relation(table, _)) = &node.kind else {
            return;
        };
        let table = table.clone();
        let target = Some(Self::target_of(connection, node, false));
        let page_size = AppState::get(cx).settings.page_size;
        let read = table.clone();
        let writing = self.read_on(connection, &node.database, cx, move |session| async move {
            let structure = session.table_structure(&read).await?;
            let dialect = session.dialect();
            Ok(match template {
                Template::Select => {
                    select_template(dialect, &read, &structure.columns, u64::from(page_size))
                }
                Template::Insert => insert_template(dialect, &read, &structure.columns),
                Template::Update => update_template(dialect, &read, &structure),
                Template::Delete => delete_template(dialect, &read, &structure),
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            let written = writing.await;
            let _ = this.update_in(cx, |_, window, cx| match written {
                Ok(sql) if run => cx.emit(NavigatorEvent::RunQuery {
                    title: table.name.clone(),
                    sql,
                    target,
                }),
                Ok(sql) => cx.emit(NavigatorEvent::OpenQuery {
                    title: table.name.clone(),
                    sql,
                    target,
                }),
                Err(error) => window.push_notification(
                    crate::error_notification(format!(
                        "The statement for {} could not be written: {error}",
                        table.name,
                        error = describe(&error)
                    )),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Writes the call of a routine into a new query tab, which runs it
    /// (asking for its parameters) when `run` says so. The arguments are
    /// read from the server, as the tree lists them.
    fn call(
        &mut self,
        connection: &str,
        node: &Node,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let NodeKind::Object(ObjectRef::Routine {
            schema,
            name,
            kind,
            identity_arguments,
        }) = &node.kind
        else {
            return;
        };
        let (schema, name, kind) = (schema.clone(), name.clone(), *kind);
        let identity = identity_arguments.clone();
        let target = Some(Self::target_of(connection, node, false));
        let routine = name.clone();
        let writing = self.read_on(connection, &node.database, cx, move |session| async move {
            let objects = session.objects(&schema).await?;
            let arguments = objects
                .routines
                .iter()
                .find(|found| {
                    found.name == routine
                        && found.kind == kind
                        && found.identity_arguments == identity
                })
                .map(|found| found.arguments.clone())
                .ok_or_else(|| kerno_core::Error::Config(format!("{routine} is gone")))?;
            Ok(call_routine(
                session.dialect(),
                &schema,
                &routine,
                kind,
                &arguments,
            ))
        });
        cx.spawn_in(window, async move |this, cx| {
            let written = writing.await;
            let _ = this.update_in(cx, |_, window, cx| match written {
                Ok(sql) => {
                    let title = name.clone();
                    cx.emit(if run {
                        NavigatorEvent::RunQuery { title, sql, target }
                    } else {
                        NavigatorEvent::OpenQuery { title, sql, target }
                    })
                }
                Err(error) => window.push_notification(
                    crate::error_notification(format!(
                        "The call of {name} could not be written: {error}",
                        error = describe(&error)
                    )),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Shows what the catalogs say of a database or a schema: owner,
    /// encoding, collation, size, comment (DBeaver's object editor, F4).
    fn show_properties(
        &mut self,
        connection: &str,
        node: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let NodeKind::Object(object) = &node.kind else {
            return;
        };
        let object = object.clone();
        let title = format!("{} · properties", node.label);
        let reading = self.read_on(connection, &node.database, cx, move |session| async move {
            let d = session.dialect();
            let sql = match &object {
                ObjectRef::Database(name) => d.database_properties_sql(name),
                ObjectRef::Schema(name) => d.schema_properties_sql(name),
                _ => None,
            }
            .ok_or_else(|| kerno_core::Error::Unsupported("properties of this object".into()))?;
            exec::collect(session, sql).await
        });
        cx.spawn_in(window, async move |_, cx| {
            let read = reading.await;
            let _ = cx.update(|window, cx| match read {
                Ok(collected) => crate::structure::tab::open_properties(
                    title,
                    crate::structure::tab::properties_of(collected),
                    window,
                    cx,
                ),
                Err(error) => window.push_notification(
                    crate::error_notification(format!(
                        "The properties could not be read: {error}",
                        error = describe(&error)
                    )),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Opens the `CREATE` statement of an object in a query tab.
    fn show_ddl(
        &mut self,
        connection: &str,
        node: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let NodeKind::Object(object) = &node.kind else {
            return;
        };
        let (object, name) = (object.clone(), node.label.clone());
        let target = Some(Self::target_of(connection, node, false));
        let reading = self.read_on(connection, &node.database, cx, move |session| async move {
            session.ddl(&object).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let ddl = reading.await;
            let _ = this.update_in(cx, |_, window, cx| match ddl {
                Ok(sql) => cx.emit(NavigatorEvent::OpenQuery {
                    title: format!("DDL {name}"),
                    sql,
                    target,
                }),
                Err(error) => window.push_notification(
                    crate::error_notification(format!(
                        "The DDL of {name} could not be read: {error}",
                        error = describe(&error)
                    )),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Opens the script that replaces a view or routine by its edited
    /// definition, in a query tab in source mode. Nothing runs.
    fn edit_source(
        &mut self,
        connection: &str,
        node: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (NodeKind::Object(object), Some(schema)) = (&node.kind, node.schema.clone()) else {
            return;
        };
        let (object, name) = (object.clone(), node.label.clone());
        let target = Some(Self::target_of(connection, node, false));
        let reading = self.read_on(connection, &node.database, cx, move |session| async move {
            let ddl = session.ddl(&object).await?;
            session.dialect().replace_source_sql(&object, &ddl)
        });
        cx.spawn_in(window, async move |this, cx| {
            let script = reading.await;
            let _ = this.update_in(cx, |_, window, cx| match script {
                Ok(sql) => cx.emit(NavigatorEvent::OpenSource {
                    title: name,
                    sql,
                    target,
                    schema,
                }),
                Err(error) => window.push_notification(
                    crate::error_notification(format!(
                        "The source of {name} could not be read: {error}",
                        error = describe(&error)
                    )),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Reads again the objects of `schema` after a source was applied
    /// from a tab on `target`, when this tree shows that connection.
    pub fn refresh_source(&mut self, target: &Target, schema: &str, cx: &mut Context<Self>) {
        self.refresh_tables(&target.connection, target.database.as_deref(), schema, cx);
    }

    /// Shows the statement a destructive or creating action would run and
    /// runs it once confirmed.
    fn confirm(
        &mut self,
        connection: &str,
        action: Action,
        node: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connections = self.connections.read(cx);
        let (Some(saved), Some(meta)) = (
            connections.get(connection),
            connections.meta_session(connection),
        ) else {
            return;
        };
        if self.unavailable(connection, action, node, cx).is_some() {
            return;
        }
        let (connection_name, environment) =
            (saved.config.name.clone(), saved.config.environment.clone());
        let object = match &node.kind {
            NodeKind::Object(object) => Some(object.clone()),
            _ => None,
        };
        let name = node.label.clone();
        let kind = match (&node.kind, &object) {
            (NodeKind::Extension { .. }, _) => "extension",
            (NodeKind::Column { .. }, _) => "column",
            (_, Some(object)) => kind_name(object),
            _ => "object",
        };
        // The statement is written by the dialect of the connection; the
        // session it runs on is found when it runs.
        let dialect_of = meta.clone();
        let above = model::parent_of(&node.id).map(str::to_string);
        // Databases are listed by the connection's own session, and a
        // database is dropped from outside it.
        let on_root = matches!(object, Some(ObjectRef::Database(_)));
        // The table a column belongs to, read again once it changed.
        let relation = self.relation_above(connection, &node.id);
        let (title, explanation, statement, button, destructive, after, done) = match action {
            Action::Drop | Action::DropCascade => {
                let sql = match (&node.kind, &object) {
                    (NodeKind::Extension { name, .. }, _) => {
                        dialect_of.dialect().drop_extension_sql(name)
                    }
                    (_, Some(object)) => dialect_of.dialect().drop_sql(object),
                    _ => return,
                };
                let Ok(sql) = sql else {
                    return;
                };
                // The dialect's DROP never ends with CASCADE or a semicolon.
                let cascade = action == Action::DropCascade;
                (
                    if cascade {
                        format!("Drop {kind} “{name}” with cascade?")
                    } else {
                        format!("Drop {kind} “{name}”?")
                    },
                    if cascade {
                        format!(
                            "The {kind}, what it holds and every object that depends on it (views, foreign keys, …) are removed for good. This cannot be undone."
                        )
                    } else {
                        format!(
                            "The {kind} and what it holds are removed for good. This cannot be undone."
                        )
                    },
                    Statement::Fixed(if cascade { sql + " CASCADE" } else { sql }),
                    if cascade { "Drop with cascade" } else { "Drop" },
                    true,
                    if on_root {
                        After::Reload
                    } else {
                        After::Refresh(above.clone())
                    },
                    format!("Dropped {kind} {name}."),
                )
            }
            Action::Rename => {
                let Some(object) = &object else {
                    return;
                };
                let (renamed, build_for) = (object.clone(), dialect_of.clone());
                let current = plain_name(object);
                (
                    format!("Rename {kind} “{name}”"),
                    format!(
                        "Statements that use the old name of the {kind} have to be changed too."
                    ),
                    Statement::Named {
                        label: "New name",
                        initial: plain_name(object),
                        build: Rc::new(move |new_name| {
                            if new_name == current {
                                return Err("Type a different name.".into());
                            }
                            build_for
                                .dialect()
                                .rename_sql(&renamed, new_name)
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Rename",
                    false,
                    if on_root {
                        After::Reload
                    } else {
                        After::Refresh(above.clone())
                    },
                    format!("Renamed {kind} {name}."),
                )
            }
            Action::RenameColumn | Action::DropColumn => {
                let (
                    NodeKind::Column {
                        table,
                        name: column,
                    },
                    Some((_, _, relation)),
                ) = (&node.kind, relation)
                else {
                    return;
                };
                let (table, current, build_for) =
                    (table.clone(), column.clone(), dialect_of.clone());
                let alter = move |change: TableChange| {
                    build_for
                        .dialect()
                        .alter_table_sql(&table, &[change])
                        .map(|statements| statements.join(";\n"))
                        .map_err(|error| error.to_string())
                };
                if action == Action::DropColumn {
                    let Ok(sql) = alter(TableChange::DropColumn(current)) else {
                        return;
                    };
                    (
                        format!("Drop column “{name}”?"),
                        "The column and the values it holds are removed for good. This cannot be undone.".to_string(),
                        Statement::Fixed(sql),
                        "Drop column",
                        true,
                        After::Refresh(Some(relation)),
                        format!("Dropped column {name}."),
                    )
                } else {
                    (
                        format!("Rename column “{name}”"),
                        "Statements, views and functions that use the old name of the column have to be changed too.".to_string(),
                        Statement::Named {
                            label: "New name",
                            initial: column.clone(),
                            build: Rc::new(move |new_name| {
                                if new_name == current {
                                    return Err("Type a different name.".into());
                                }
                                alter(TableChange::RenameColumn {
                                    from: current.clone(),
                                    to: new_name.to_string(),
                                })
                            }),
                        },
                        "Rename column",
                        false,
                        After::Refresh(Some(relation)),
                        format!("Renamed column {name}."),
                    )
                }
            }
            Action::CreateSchema => {
                let build_for = dialect_of.clone();
                (
                    "Create schema".to_string(),
                    format!("A new, empty schema in the database “{}”.", node.database),
                    Statement::Named {
                        label: "Name",
                        initial: String::new(),
                        build: Rc::new(move |name| {
                            build_for
                                .dialect()
                                .create_schema_sql(name)
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Create schema",
                    false,
                    // The schemas are the children of the database node.
                    After::Refresh(Some(format!("db:{}", node.database))),
                    "Created the schema.".to_string(),
                )
            }
            Action::CreateDatabase => {
                let build_for = dialect_of.clone();
                (
                    "Create database".to_string(),
                    "A new, empty database on this server.".to_string(),
                    Statement::Named {
                        label: "Name",
                        initial: String::new(),
                        build: Rc::new(move |name| {
                            build_for
                                .dialect()
                                .create_database_sql(name)
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Create database",
                    false,
                    After::Reload,
                    "Created the database.".to_string(),
                )
            }
            Action::CreateEnum => {
                let Some(schema) = node.schema.clone() else {
                    return;
                };
                let build_for = dialect_of.clone();
                (
                    "Create enum".to_string(),
                    format!("A new enum type in the schema “{schema}”, with its values in order."),
                    Statement::NamedList {
                        list_label: "Values, one per line",
                        build: Rc::new(move |name, values| {
                            build_for
                                .dialect()
                                .create_enum_sql(&TableRef::new(schema.clone(), name), values)
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Create enum",
                    false,
                    After::Refresh(Some(node.id.clone())),
                    "Created the enum.".to_string(),
                )
            }
            Action::AddEnumValue => {
                // On a value: the new one goes before it.
                let (ty, before) = match &node.kind {
                    NodeKind::Object(ObjectRef::Type(ty)) => (ty.clone(), None),
                    NodeKind::EnumValue { ty, value } => (ty.clone(), Some(value.clone())),
                    _ => return,
                };
                let explanation = match &before {
                    Some(value) => format!("The new value goes just before “{value}”."),
                    None => "The new value goes after the others.".to_string(),
                };
                let (title, build_for) =
                    (format!("Add a value to “{}”", ty.name), dialect_of.clone());
                (
                    title,
                    explanation,
                    Statement::Named {
                        label: "Value",
                        initial: String::new(),
                        build: Rc::new(move |value| {
                            build_for
                                .dialect()
                                .add_enum_value_sql(&ty, value, before.as_deref())
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Add value",
                    false,
                    After::Refresh(Some(node.id.clone())),
                    "Added the value.".to_string(),
                )
            }
            Action::RenameEnumValue => {
                let NodeKind::EnumValue { ty, value } = &node.kind else {
                    return;
                };
                let (ty, current, build_for) = (ty.clone(), value.clone(), dialect_of.clone());
                (
                    format!("Rename value “{value}”"),
                    format!("Rows holding “{value}” hold the new value from then on."),
                    Statement::Named {
                        label: "New value",
                        initial: value.clone(),
                        build: Rc::new(move |new_value| {
                            if new_value == current {
                                return Err("Type a different value.".into());
                            }
                            build_for
                                .dialect()
                                .rename_enum_value_sql(&ty, &current, new_value)
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Rename value",
                    false,
                    After::Refresh(Some(node.id.clone())),
                    "Renamed the value.".to_string(),
                )
            }
            _ => return,
        };
        // Creating a database and anything done to a database runs on the
        // connection's own session.
        let database = if on_root || action == Action::CreateDatabase {
            None
        } else {
            Some(node.database.clone())
        };
        // A session of the navigator's own inside a database would keep
        // PostgreSQL from dropping or renaming it.
        let release = match (&object, action) {
            (Some(ObjectRef::Database(name)), Action::Drop | Action::Rename) => Some(name.clone()),
            _ => None,
        };
        let (doing, failed) = match action {
            Action::Drop | Action::DropCascade | Action::DropColumn => (
                format!("Dropping {kind} {name}…"),
                format!("The {kind} “{name}” was not dropped."),
            ),
            Action::Rename | Action::RenameColumn => (
                format!("Renaming {kind} {name}…"),
                format!("The {kind} “{name}” was not renamed."),
            ),
            Action::RenameEnumValue => (
                "Renaming the value…".into(),
                "The value was not renamed.".into(),
            ),
            Action::AddEnumValue => (
                "Adding the value…".into(),
                "The value was not added.".into(),
            ),
            Action::CreateSchema => (
                "Creating the schema…".into(),
                "The schema was not created.".into(),
            ),
            Action::CreateDatabase => (
                "Creating the database…".into(),
                "The database was not created.".into(),
            ),
            Action::CreateEnum => (
                "Creating the enum…".into(),
                "The enum was not created.".into(),
            ),
            _ => (
                "Running the statement…".into(),
                "The statement failed.".into(),
            ),
        };
        let plan = Plan {
            connection: connection.to_string(),
            database,
            release,
            after,
            done,
            node: node.id.clone(),
            doing,
            failed,
        };
        // What else it reaches, read from the catalog while the dialog is
        // open (C11). Not for a database: a session of the tree's inside it
        // would keep PostgreSQL from dropping or renaming it.
        let cascade = action == Action::DropCascade;
        let change = match (action, &node.kind, &object) {
            (_, _, Some(ObjectRef::Database(_))) => None,
            (Action::Drop | Action::DropCascade, NodeKind::Extension { name, .. }, _) => {
                Some(Change::DropExtension {
                    name: name.clone(),
                    cascade,
                })
            }
            (Action::Drop | Action::DropCascade, _, Some(object)) => Some(Change::Drop {
                object: object.clone(),
                cascade,
            }),
            (Action::Rename, _, Some(object)) => Some(Change::Rename(object.clone())),
            _ => None,
        };
        let reach = change.map(|change| {
            // MySQL's information_schema shows only what the user may see:
            // the empty line says so.
            let empty = match (&change, dialect_of.engine()) {
                (Change::Rename(_), Engine::Postgres) => {
                    "No function or procedure body names it; views, defaults and triggers follow a rename on their own."
                }
                (Change::Rename(_), _) => "No view, routine, trigger or event this user can see names it.",
                (_, Engine::Postgres) => "Nothing else depends on it.",
                _ => "Nothing this user can see depends on it.",
            };
            Reach {
                reading: self.dependents(connection, &node.database, change, cx),
                empty: empty.into(),
                // The list is what the cascade destroys: not confirmed blind.
                waits: cascade,
            }
        });
        let confirmation = Confirmation {
            title,
            explanation,
            statement,
            action: button,
            destructive,
            connection: connection_name,
            environment,
            writes: true,
        };
        let view = cx.entity();
        let run = move |sql, window: &mut Window, cx: &mut App| {
            let plan = plan.clone();
            view.update(cx, |this, cx| this.execute(sql, plan, window, cx));
        };
        match reach {
            Some(reach) => sql_confirm::open_with_reach(confirmation, reach, window, cx, run),
            None => sql_confirm::open(confirmation, window, cx, run),
        }
    }

    /// What else `change` reaches (C11), read on a session of its own
    /// (C3): a slow read holds none of the tree's loads, and the dialog
    /// going (closed, answered) stops it on the server and closes the
    /// session; its clock running out leaves it to end on its own session.
    fn dependents(
        &mut self,
        connection: &str,
        database: &str,
        change: Change,
        cx: &mut Context<Self>,
    ) -> Task<kerno_core::Result<Vec<Dependent>>> {
        let database = self
            .has_schemas(connection, cx)
            .then(|| database.to_string());
        read_dependents(&self.connections, connection, database, change, cx)
    }

    /// Runs a confirmed statement on a session of its own, closed once it
    /// ends, and shows what came of it. The node says it runs, and Cancel
    /// (its menu, the notification) stops it on the server (C3): a session
    /// of the tree's would also hold the loads queued behind a statement
    /// waiting for a lock, and a cancel would fail them too.
    fn execute(&mut self, sql: String, plan: Plan, window: &mut Window, cx: &mut Context<Self>) {
        let Plan {
            connection,
            database,
            release,
            after,
            done,
            node,
            doing,
            failed,
        } = plan;
        let Some(browse) = self.browse.get_mut(&connection) else {
            return;
        };
        let generation = browse.generation;
        // The tree's session on a database to drop would hold the drop.
        if let Some(released) = release
            && self.has_schemas(&connection, cx)
        {
            self.stop_session(&connection, &released, Halt::Cancelled, cx);
        }
        let opening = self
            .connections
            .read(cx)
            .open_session(&connection, database, cx);
        self.last_write += 1;
        let key = self.last_write;
        self.writes.push(Write {
            key,
            connection: connection.clone(),
            node,
            doing: doing.clone(),
            session: None,
            cancelled: false,
        });
        let view = cx.entity().downgrade();
        window.push_notification(
            Notification::info(SharedString::from(doing))
                .id1::<WriteNote>(key as usize)
                .action(move |_, _, _| {
                    let view = view.clone();
                    Button::new(("navigator-write-cancel", key as usize))
                        .small()
                        .label("Cancel")
                        .on_click(move |_, _, cx| {
                            if let Some(view) = view.upgrade() {
                                view.update(cx, |this, cx| this.cancel_write(key, cx));
                            }
                        })
                }),
            cx,
        );
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = match opening.await {
                Err(error) => Err(error),
                Ok(session) => {
                    // A Cancel that came while it opened: nothing is sent.
                    let go_on = this.update(cx, |this, _| {
                        match this.writes.iter_mut().find(|write| write.key == key) {
                            Some(write) if !write.cancelled => {
                                write.session = Some(session.clone());
                                true
                            }
                            _ => false,
                        }
                    });
                    let go_on = matches!(go_on, Ok(true));
                    let running = cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            let outcome = if go_on {
                                exec::collect(session.clone(), sql).await.map(drop)
                            } else {
                                Err(Error::Cancelled)
                            };
                            session.close().await;
                            outcome
                        })
                    });
                    match running {
                        Ok(running) => running.await,
                        Err(_) => return,
                    }
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let cancelled = this.end_write(key, window, cx);
                match outcome {
                    Ok(()) => {
                        window
                            .push_notification(Notification::success(SharedString::from(done)), cx);
                        if this.live(&connection, generation).is_none() {
                            return;
                        }
                        match after {
                            After::Reload => this.reload(&connection, cx),
                            After::Refresh(Some(id)) => this.refresh(&connection, &id, cx),
                            After::Refresh(None) => {}
                        }
                    }
                    // The server words a cancel its own way (an error on
                    // PostgreSQL); the navigator knows it asked. One
                    // statement, in auto-commit: it did not take effect.
                    Err(_) if cancelled => window.push_notification(
                        Notification::info("Cancelled: nothing was changed."),
                        cx,
                    ),
                    // In plain words, then the server's own (C14): its hint
                    // often holds the fix ("Use DROP … CASCADE …").
                    Err(error) => window.push_notification(
                        crate::error_notification(format!("{failed}\n{}", describe(&error))),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    /// Stops the write `key` on the server (C3).
    pub fn cancel_write(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(write) = self.writes.iter_mut().find(|write| write.key == key) else {
            return;
        };
        write.cancelled = true;
        if let Some(session) = write.session.clone() {
            bridge::spawn(cx, async move {
                if let Err(error) = session.cancel().await {
                    tracing::debug!(%error, "the statement could not be cancelled");
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Forgets the write `key`, which ended; returns whether it was
    /// cancelled.
    fn end_write(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(at) = self.writes.iter().position(|write| write.key == key) else {
            return false;
        };
        window.remove_notification1::<WriteNote>(key as usize, cx);
        cx.notify();
        self.writes.remove(at).cancelled
    }

    /// The write running on `node` of `connection`, if one is.
    fn write_on(&self, connection: &str, node: &str) -> Option<&Write> {
        self.writes
            .iter()
            .find(|write| write.connection == connection && write.node == node)
    }

    /// The databases the tree of `connection` holds a session in or reads
    /// from now, sorted; for tests (K62).
    #[cfg(test)]
    pub fn sessions_of(&self, connection: &str) -> Vec<String> {
        let Some(browse) = self.browse.get(connection) else {
            return Vec::new();
        };
        let reading = browse.loading.values().map(|loading| &loading.session);
        (browse.sessions.keys().chain(reading).cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The open nodes of the tree of `connection`; for tests.
    #[cfg(test)]
    pub fn open_nodes(&self, connection: &str) -> BTreeSet<String> {
        (self.tree(connection).map(Tree::open_ids))
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    /// Whether the tree of `connection` is being read or put back (K77);
    /// for tests.
    #[cfg(test)]
    pub fn settling(&self, connection: &str) -> bool {
        self.restore.contains_key(connection)
            || self.browse.get(connection).is_none_or(|browse| {
                browse.restoring.is_some()
                    || browse.root == Root::Loading
                    || !browse.loading.is_empty()
            })
    }

    /// The id of the selected row; for tests.
    #[cfg(test)]
    pub fn selected_id(&self, cx: &App) -> Option<String> {
        let state = self.tree_state.read(cx);
        state.selected_item().map(|item| item.id.to_string())
    }

    /// The writes running, by key, with what each says; for tests.
    #[cfg(test)]
    pub fn writes(&self) -> Vec<(u64, String)> {
        self.writes
            .iter()
            .map(|write| (write.key, write.doing.clone()))
            .collect()
    }

    /// Shows the folders, the connections and what each open one holds in
    /// the tree widget, keeping the selection.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let connections = self.connections.read(cx);
        let filter = self.filter_text.as_str();
        // Each tree through its connection's object filters and the
        // filter, unless the connection's name matches it: then all of it.
        for saved in connections.saved() {
            if let Some(browse) = self.browse.get_mut(&saved.id) {
                browse
                    .tree
                    .set_object_filters(saved.config.navigator_filters.clone());
                // PostgreSQL only: the others list what the account sees (K62).
                browse.tree.set_databases_shown(match saved.config.engine {
                    Engine::Postgres => saved.config.databases_shown,
                    _ => DatabasesShown::All,
                });
                let matched =
                    !filter.is_empty() && saved.config.name.to_lowercase().contains(filter);
                browse.tree.set_filter(if matched { "" } else { filter });
            }
        }
        let saved: Vec<Listed> = connections
            .saved()
            .iter()
            .map(|saved| {
                (
                    saved.id.clone(),
                    saved.config.name.clone(),
                    folder_of(&saved.config),
                )
            })
            .collect();
        let items = model::sidebar_items(
            &connections.folders(cx),
            &saved,
            &AppState::get(cx).settings.collapsed_folders,
            &self.expanded,
            filter,
            |id| self.subtree(id, connections),
        );
        let items: Vec<TreeItem> = items.into_iter().map(tree_item).collect();
        // Where the user was in a tree read again (K77).
        let places: Vec<(String, Option<String>, Option<_>)> = (self.browse.iter())
            .filter_map(|(connection, browse)| {
                let restore = browse.restoring.as_ref()?;
                Some((connection.clone(), restore.selected.clone(), restore.scroll))
            })
            .collect();
        let (last, reselected) = self.tree_state.update(cx, |state, cx| {
            let selected = state.selected_item().map(|item| item.id.clone());
            state.set_items(items, cx);
            let index = selected.as_ref().and_then(|id| state.index_of(id));
            state.set_selected_index(index, cx);
            let mut reselected = Vec::new();
            for (connection, row, scroll) in places {
                // Unless another row was selected meanwhile.
                if let Some(row) = row
                    && state.selected_index().is_none()
                    && let Some(index) = state.index_of(&row.into())
                {
                    state.set_selected_index(Some(index), cx);
                    reselected.push(connection);
                }
                if let Some(scroll) = scroll {
                    state
                        .scroll_handle()
                        .0
                        .borrow()
                        .base_handle
                        .set_offset(scroll);
                }
            }
            (selected, reselected)
        });
        if let Some(last) = last {
            self.last_selected = Some(last.to_string());
        }
        for (connection, browse) in &mut self.browse {
            if let Some(restore) = &mut browse.restoring {
                if reselected.contains(connection) {
                    restore.selected = None;
                }
                if restore.done {
                    browse.restoring = None;
                }
            }
        }
        cx.notify();
    }

    /// What a connection shows under it, ids local to it: its databases
    /// once listed, otherwise what is happening or what went wrong.
    fn subtree(&self, connection: &str, connections: &Connections) -> Vec<Item> {
        let line = |id: &str, label: &str| Item {
            id: id.to_string(),
            label: label.to_string(),
            expanded: false,
            children: Vec::new(),
        };
        match (&connections.status(connection), self.browse.get(connection)) {
            (Status::Connected { .. }, Some(browse)) => match &browse.root {
                Root::Loading => vec![line(LINE_STATUS, "Loading databases…")],
                Root::Failed(message) => vec![
                    line(LINE_ERROR, "The databases could not be listed."),
                    line(LINE_DETAIL, message),
                    line(LINE_RETRY, "Try again"),
                ],
                Root::Loaded => {
                    let items = browse.tree.items();
                    if items.is_empty() {
                        vec![line(LINE_STATUS, "No databases")]
                    } else {
                        items
                    }
                }
            },
            (Status::Connected { .. }, None) => vec![line(LINE_STATUS, "Connected")],
            (Status::Connecting, _) if connections.reconnecting(connection) => {
                vec![line(LINE_STATUS, "Connection lost: reconnecting…")]
            }
            (Status::Connecting, _) => vec![line(LINE_STATUS, "Connecting…")],
            (Status::WaitingForInput, _) => vec![line(LINE_STATUS, "Waiting for your answer…")],
            // In plain words first, then the server's own (CLAUDE.md §3 item 5).
            (Status::Failed { message }, _) => {
                let Some(saved) = connections.get(connection) else {
                    return vec![line(LINE_DETAIL, message)];
                };
                let explained = kerno_core::error::explain(message, &saved.config);
                let mut lines = vec![line(LINE_ERROR, &explained.headline)];
                if !explained.next_step.is_empty() {
                    lines.push(line(LINE_NEXT, &explained.next_step));
                }
                lines.push(line(LINE_DETAIL, &explained.raw));
                lines.push(line(LINE_RETRY, "Try again"));
                lines
            }
            (Status::Disconnected, _) => vec![line(LINE_STATUS, "Not connected")],
        }
    }

    // Clicks and keys --------------------------------------------------------

    fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let connection = self
            .tree_state
            .read(cx)
            .entry(index)
            .and_then(|entry| connection_of(&entry.item().id).map(str::to_string));
        if connection.is_some() {
            self.current = connection;
        }
        // The user's choice: no place put back over it (K77).
        let restores = (self.browse.values_mut()).filter_map(|browse| browse.restoring.as_mut());
        for restore in restores.chain(self.restore.values_mut()) {
            (restore.selected, restore.scroll) = (None, None);
        }
        self.tree_state
            .update(cx, |state, cx| state.set_selected_index(Some(index), cx));
    }

    /// Opens the row, or closes it.
    fn toggle_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((id, open)) = self
            .tree_state
            .read(cx)
            .entry(index)
            .filter(|entry| entry.is_folder())
            .map(|entry| (entry.item().id.to_string(), entry.is_expanded()))
        else {
            return;
        };
        self.set_open(&id, !open, cx);
    }

    /// What a double click or Enter does: on an object, opens it (see
    /// [`default_action`]) or opens or closes the row; on a connection,
    /// opens and connects it (never closes or disconnects it); on a folder,
    /// opens or closes it.
    fn open_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .tree_state
            .read(cx)
            .entry(index)
            .map(|entry| entry.item().id.to_string())
        else {
            return;
        };
        match model::parse_row(&id) {
            Some(RowId::Connection(connection)) => self.open_connection(connection, cx),
            // The databases the connection's choice hides: its settings (K62).
            Some(RowId::Object { connection, local }) if local == model::HIDDEN_NOTE => {
                Sidebar::edit(self.connections.clone(), connection.to_string(), window, cx)
            }
            Some(RowId::Object { .. }) => {
                let Some((connection, node)) = self.object(&id) else {
                    return;
                };
                let table = AppState::get(cx).settings.table_double_click;
                match default_action(&node, self.has_schemas(&connection, cx), table) {
                    Some(action) => self.run_entry(&connection, action, &node, window, cx),
                    None => self.toggle_row(index, cx),
                }
            }
            _ => self.toggle_row(index, cx),
        }
    }

    /// Runs `action` on the selected node, as its menu entry does: what a
    /// key of the tree does (F2, Delete, F5, Cmd+C…).
    pub fn perform_on_selected(
        &mut self,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected_node(window, cx) {
            // Any node of a database leads to its extensions (K27); not one
            // this account may not open (K62).
            Some((connection, node)) if action == Action::ManageExtensions => {
                if !node.is_closed() {
                    self.manage_extensions(&connection, &node, cx)
                }
            }
            Some((connection, node)) => self.run_entry(&connection, action, &node, window, cx),
            None => {}
        }
    }

    /// K53: AI ▸ of `node`, as its menu shows it: a table's, a view's or a
    /// materialized view's entries, or a column's.
    fn ai_entries(&self, connection: &str, node: &Node, cx: &App) -> Option<Vec<AiShown>> {
        let (entries, view): (&[AiEntry], bool) = match &node.kind {
            NodeKind::Object(ObjectRef::Relation(_, kind)) => (&AiEntry::TABLE, !kind.is_table()),
            NodeKind::Column { .. } => (&[AiEntry::ExplainColumns], false),
            _ => return None,
        };
        let on = AppState::get(cx).settings.ai.enabled;
        let caution = ai_ask::caution(self.connections.read(cx), connection, cx);
        let shown = |&entry| {
            let (label, enabled) = ai_ask::shown(entry, on, view, caution.as_deref());
            (entry, label, enabled)
        };
        Some(entries.iter().map(shown).collect())
    }

    /// K53: an AI ▸ entry on the node `id`: its structure read, then the
    /// question asked in a query tab on its schema.
    pub fn ask_ai(
        &mut self,
        entry: AiEntry,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((connection, node)) = self.object(id) else {
            return;
        };
        let usable = self.ai_entries(&connection, &node, cx);
        if !usable.is_some_and(|shown| shown.iter().any(|(e, _, on)| *e == entry && *on)) {
            return;
        }
        let (table, kind, columns) = match &node.kind {
            NodeKind::Object(ObjectRef::Relation(table, kind)) => (table.clone(), *kind, vec![]),
            NodeKind::Column { table, name } => {
                (table.clone(), RelationKind::Table, vec![name.clone()])
            }
            _ => return,
        };
        let target = Self::target_of(&connection, &node, true);
        let read = table.clone();
        let reading = self.read_on(&connection, &node.database, cx, move |session| async move {
            session.table_structure(&read).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let read = reading.await;
            let _ = this.update_in(cx, |_, window, cx| match read {
                Ok(structure) => {
                    let ask = ai_ask::of_table(entry, table, kind, &structure, &columns);
                    if let Some(ask) = ask {
                        cx.emit(NavigatorEvent::AskAi { target, ask });
                    }
                }
                Err(error) => window.push_notification(
                    crate::error_notification(format!(
                        "The structure of {} could not be read: {}",
                        table.name,
                        describe(&error)
                    )),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// K53: an AI ▸ entry on the node selected in the tree (its palette
    /// command); `false` when that node has no such entry.
    pub fn ask_ai_on_selected(
        &mut self,
        entry: AiEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let selected =
            (self.tree_state.read(cx).selected_item()).and_then(|item| self.object(&item.id));
        let Some((connection, node)) = selected else {
            return false;
        };
        let shown = self.ai_entries(&connection, &node, cx);
        if !shown.is_some_and(|shown| shown.iter().any(|(e, _, on)| *e == entry && *on)) {
            return false;
        }
        let id = model::object_row(&connection, &node.id);
        self.ask_ai(entry, &id, window, cx);
        true
    }

    /// What F4 does: opens the selected node's editor (see [`edit_action`]).
    pub fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((connection, node)) = self.selected_node(window, cx)
            && let Some(action) = edit_action(&node, self.has_schemas(&connection, cx))
        {
            self.run_entry(&connection, action, &node, window, cx);
        }
    }

    /// The folder or connection of the selected row, when it is one (or a
    /// line under one): what the sidebar's menus act on.
    pub fn selected_subject(&self, cx: &App) -> Option<Subject> {
        let id = self.tree_state.read(cx).selected_item()?.id.clone();
        match model::parse_row(&id)? {
            RowId::Folder(path) | RowId::FolderNote(path) => Some(Subject::Folder(path.into())),
            RowId::Connection(connection) => Some(Subject::Connection(connection.into())),
            RowId::Object { connection, local } if local.starts_with("line:") => {
                Some(Subject::Connection(connection.into()))
            }
            RowId::Object { .. } => None,
        }
    }

    /// The connection and node of the selected row; says so when there is
    /// none.
    fn selected_node(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<(String, Node)> {
        let selected = self
            .tree_state
            .read(cx)
            .selected_item()
            .and_then(|item| self.object(&item.id));
        if selected.is_none() {
            window.push_notification(
                Notification::warning("Select an object in the schema tree first."),
                cx,
            );
        }
        selected
    }

    /// Runs a menu entry of `node` from a key or a double click: only one
    /// the node's menu holds, and one that cannot be used now says why.
    fn run_entry(
        &mut self,
        connection: &str,
        action: Action,
        node: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Backup, Restore and Truncate are under Tools ▸; their keys work
        // where the entry applies.
        let tool = match action {
            Action::Backup => Some(ToolEntry::Backup),
            Action::Restore => Some(ToolEntry::Restore),
            Action::Truncate => Some(ToolEntry::Maintain(Maintenance::Truncate(
                Default::default(),
            ))),
            _ => None,
        };
        let in_tools = tool.is_some_and(|tool| {
            self.tool_entries_of(connection, node, cx)
                .concat()
                .iter()
                .any(|(entry, reason)| same_tool(entry, &tool) && reason.is_none())
        });
        if !in_tools
            && !actions(node, self.has_schemas(connection, cx))
                .concat()
                .contains(&action)
        {
            return;
        }
        let id = model::object_row(connection, &node.id);
        let reason = self.unavailable(connection, action, node, cx);
        match (action, reason) {
            // A source that cannot be edited here (read-only connection)
            // is shown as DDL.
            (Action::EditSource, Some(_)) => self.perform(Action::ShowDdl, &id, window, cx),
            // A view's column has no form: its structure is shown.
            (Action::EditColumn, Some(_)) => self.perform(Action::OpenStructure, &id, window, cx),
            (_, Some(reason)) => window.push_notification(
                Notification::warning(SharedString::from(format!(
                    "{}: {reason}.",
                    action.label().trim_end_matches('…')
                ))),
                cx,
            ),
            (_, None) => self.perform(action, &id, window, cx),
        }
    }

    /// What "Try again" under a connection does: connects it again, or
    /// lists its databases again.
    fn retry(&mut self, connection: &str, cx: &mut Context<Self>) {
        if self.browse.contains_key(connection) {
            self.reload(connection, cx);
        } else {
            self.open_connection(connection, cx);
        }
    }

    /// Right: opens the selected row, or goes to its first child when open.
    fn key_right(&mut self, cx: &mut Context<Self>) {
        let state = self.tree_state.read(cx);
        let Some(index) = state.selected_index() else {
            return;
        };
        let Some(entry) = state.entry(index) else {
            return;
        };
        if !entry.is_folder() {
            return;
        }
        if !entry.is_expanded() {
            self.toggle_row(index, cx);
        } else if state
            .entry(index + 1)
            .is_some_and(|child| child.depth() > entry.depth())
        {
            self.select_row(index + 1, cx);
        }
    }

    /// Left: closes the selected row, or goes to its parent when closed.
    fn key_left(&mut self, cx: &mut Context<Self>) {
        let state = self.tree_state.read(cx);
        let Some(index) = state.selected_index() else {
            return;
        };
        let Some(entry) = state.entry(index) else {
            return;
        };
        if entry.is_folder() && entry.is_expanded() {
            self.toggle_row(index, cx);
            return;
        }
        let depth = entry.depth();
        let parent = (0..index)
            .rev()
            .find(|&above| state.entry(above).is_some_and(|row| row.depth() < depth));
        if let Some(parent) = parent {
            self.select_row(parent, cx);
        }
    }

    /// Scrolls the row of a connection into view; for tests.
    #[cfg(test)]
    pub fn scroll_to_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = SharedString::from(model::connection_row(id));
        self.tree_state.update(cx, |state, _| {
            if let Some(index) = state.index_of(&id) {
                state.scroll_to_item(index, gpui_kit::ScrollStrategy::Top);
            }
        });
    }

    /// The selected row; for tests.
    #[cfg(test)]
    pub fn selected_row(&self, cx: &App) -> Option<usize> {
        self.tree_state.read(cx).selected_index()
    }

    /// The rows under the row of `connection` (absolute index, depth below
    /// it, label), without the lines that say what the connection is
    /// doing; for tests and the bench.
    fn rows_under(&self, connection: &str, cx: &App) -> Vec<(usize, usize, SharedString)> {
        let state = self.tree_state.read(cx);
        let Some(start) = state.index_of(&model::connection_row(connection).into()) else {
            return Vec::new();
        };
        let base = state.entry(start).map_or(0, |entry| entry.depth());
        (start + 1..)
            .map_while(|index| state.entry(index).map(|entry| (index, entry)))
            .take_while(|(_, entry)| entry.depth() > base)
            .filter(|(_, entry)| {
                !matches!(
                    model::parse_row(&entry.item().id),
                    Some(RowId::Object { local, .. }) if local.starts_with("line:")
                )
            })
            .map(|(index, entry)| (index, entry.depth() - base - 1, entry.item().label.clone()))
            .collect()
    }

    /// The tree's session of `connection` kept under `key` (a database on
    /// PostgreSQL, `""` on MySQL); for tests.
    #[cfg(test)]
    pub fn tree_session(&self, connection: &str, key: &str) -> Option<Arc<dyn Session>> {
        self.browse.get(connection)?.sessions.get(key).cloned()
    }

    /// The rows now shown under `connection`, indented by depth; for tests.
    #[cfg(test)]
    pub fn outline_of(&self, connection: &str, cx: &App) -> Vec<String> {
        self.rows_under(connection, cx)
            .into_iter()
            .map(|(_, depth, label)| format!("{}{label}", "  ".repeat(depth)))
            .collect()
    }

    /// The rows now shown under the current connection; for tests.
    #[cfg(test)]
    pub fn outline(&self, cx: &App) -> Vec<String> {
        self.current
            .as_deref()
            .map(|connection| self.outline_of(connection, cx))
            .unwrap_or_default()
    }

    /// The id of the row at `row`; for tests and the bench.
    pub fn id_at(&self, row: usize, cx: &App) -> String {
        self.tree_state
            .read(cx)
            .entry(row)
            .expect("a row")
            .item()
            .id
            .to_string()
    }

    /// Whether a row is open; for tests.
    #[cfg(test)]
    pub fn is_open(&self, row: usize, cx: &App) -> bool {
        self.tree_state
            .read(cx)
            .entry(row)
            .is_some_and(|entry| entry.is_expanded())
    }

    /// Why an action cannot be used on the node of a row; for tests.
    #[cfg(test)]
    pub fn why_not(&self, action: Action, row: usize, cx: &App) -> Option<&'static str> {
        let (connection, node) = self.object(&self.id_at(row, cx))?;
        self.unavailable(&connection, action, &node, cx)
    }

    /// The maintenance tools of the node on a row, by label; for tests.
    #[cfg(test)]
    pub fn tools_at(&self, row: usize, cx: &App) -> Vec<&'static str> {
        let Some((connection, node)) = self.object(&self.id_at(row, cx)) else {
            return Vec::new();
        };
        match self.meta(&connection, cx) {
            Some(meta) => tools(&node, meta.dialect())
                .iter()
                .map(maintenance_dialog::label)
                .collect(),
            None => Vec::new(),
        }
    }

    /// Tools ▸ of the node on a row, as the menu shows it (a group per
    /// line, a disabled entry with its reason); for tests.
    #[cfg(test)]
    pub fn tool_menu_at(&self, row: usize, cx: &App) -> Vec<String> {
        let Some((connection, node)) = self.object(&self.id_at(row, cx)) else {
            return Vec::new();
        };
        self.tool_menu(&connection, &node, cx)
            .into_iter()
            .map(|group| {
                group
                    .into_iter()
                    .map(|(entry, reason)| match reason {
                        Some(reason) => format!("{} ({reason})", entry.label()),
                        None => entry.label(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .collect()
    }

    /// The row reached by these labels from the current connection's
    /// first level down; for tests and the bench.
    pub fn row_of(&self, label_path: &[&str], cx: &App) -> Option<usize> {
        let rows = self.rows_under(self.current.as_deref()?, cx);
        let mut depth = 0;
        let mut found = None;
        for (index, row_depth, label) in rows {
            if row_depth < depth {
                return None;
            }
            if row_depth == depth && label.as_ref() == label_path[depth] {
                found = Some(index);
                depth += 1;
                if depth == label_path.len() {
                    return found;
                }
            }
        }
        found
    }

    // Rendering ---------------------------------------------------------------

    fn render_row(
        &self,
        view: &Entity<Self>,
        index: usize,
        entry: &TreeEntry,
        cx: &App,
    ) -> ListItem {
        // Every row has the height of the first: the list is virtualized.
        let row = |id: ElementId| {
            ListItem::new(id)
                .w_full()
                .h(px(ROW_HEIGHT))
                .py_0()
                .pr_2()
                .pl(px(14.) * entry.depth() + px(8.))
                .text_sm()
        };
        let id = entry.item().id.clone();
        match model::parse_row(&id) {
            Some(RowId::Folder(path)) => self.render_folder(view, row, index, entry, path, cx),
            Some(RowId::Connection(connection)) => {
                let saved = self.connections.read(cx).saved();
                let Some(at) = saved.iter().position(|saved| saved.id == connection) else {
                    return row(("node", index).into());
                };
                self.render_connection(view, row(("connection", at).into()), index, entry, at, cx)
            }
            Some(RowId::Object { connection, local }) => {
                let base = row(("node", index).into());
                match self.tree(connection).and_then(|tree| tree.node(local)) {
                    Some(node) => Self::clicks(view, base, index)
                        .child(self.node_content(view, index, entry, connection, node, cx)),
                    None => self.render_line(view, base, index, connection, entry, cx),
                }
            }
            Some(RowId::FolderNote(_)) | None => row(("node", index).into()).child(line_text(
                entry.item().label.clone(),
                cx.theme().muted_foreground,
            )),
        }
    }

    /// The chevron of a row that has children: opens and closes it, and
    /// nothing else.
    fn chevron(
        view: &Entity<Self>,
        id: ElementId,
        index: usize,
        entry: &TreeEntry,
        cx: &App,
    ) -> AnyElement {
        let view = view.clone();
        div()
            .id(id)
            .test_support()
            .flex_none()
            .child(
                Icon::new(if entry.is_expanded() {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size_3()
                .text_color(cx.theme().muted_foreground),
            )
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                view.update(cx, |this, cx| {
                    this.focus_tree(window, cx);
                    this.select_row(index, cx);
                    this.toggle_row(index, cx);
                });
            })
            .into_any_element()
    }

    /// Gives the tree the keyboard, and notes what has it then.
    fn focus_tree(&mut self, window: &mut Window, cx: &mut App) {
        self.tree_state
            .update(cx, |state, cx| state.focus(window, cx));
        self.tree_focus = window.focused(cx).map(|focus| focus.downgrade());
    }

    /// Whether `focus` is the tree's: the keyboard is in the tree (K56).
    pub fn is_tree_focus(&self, focus: &gpui_kit::FocusHandle) -> bool {
        self.tree_focus
            .as_ref()
            .and_then(gpui_kit::WeakFocusHandle::upgrade)
            .is_some_and(|tree| &tree == focus)
    }

    /// The connection of the selected row, when a row is selected; not the
    /// one opened last (K56).
    pub fn selected_connection(&self, cx: &App) -> Option<String> {
        let item = self.tree_state.read(cx).selected_item()?;
        connection_of(&item.id).map(str::to_string)
    }

    /// Clicks are the navigator's: the tree's own would open or close the
    /// row on a single click. One selects, two open (see [`Self::open_row`]).
    fn clicks(view: &Entity<Self>, row: ListItem, index: usize) -> ListItem {
        let view = view.clone();
        row.on_click(move |event, window, cx| {
            cx.stop_propagation();
            let count = event.click_count();
            view.update(cx, |this, cx| {
                this.focus_tree(window, cx);
                this.select_row(index, cx);
                if count >= 2 {
                    this.open_row(index, window, cx);
                }
            });
        })
    }

    /// A folder's row: its look's picture, or its glyph in its colour (a
    /// small dot beside a picture), never a bar. Its place in the list of
    /// folders names its row and its look for tests.
    fn render_folder(
        &self,
        view: &Entity<Self>,
        row: impl Fn(ElementId) -> ListItem,
        index: usize,
        entry: &TreeEntry,
        path: &str,
        cx: &App,
    ) -> ListItem {
        let at = self
            .connections
            .read(cx)
            .folders(cx)
            .iter()
            .position(|folder| folder == path)
            .unwrap_or_default();
        let theme = cx.theme();
        let state = AppState::get(cx);
        let look = state
            .settings
            .folder_looks
            .get(path)
            .cloned()
            .unwrap_or_default();
        let color = look
            .color
            .as_deref()
            .and_then(|key| theme::tag_color(key, cx));
        let picture = look
            .image
            .filter(|file| is_icon_name(file))
            .map(|file| state.paths.folder_icons().join(file));
        let icon = match &picture {
            Some(picture) => div()
                .id(("folder-picture", at))
                .test_support()
                .flex_none()
                .child(img(picture.clone()).size(px(16.)))
                .into_any_element(),
            None => div()
                .id(("folder-glyph", at))
                .test_support()
                .flex_none()
                .child(
                    Icon::new(folder_glyph(look.glyph.as_deref()))
                        .size_3p5()
                        .text_color(color.unwrap_or(theme.muted_foreground)),
                )
                .into_any_element(),
        };
        let dot = color.filter(|_| picture.is_some()).map(|color| {
            div()
                .id(("folder-dot", at))
                .test_support()
                .flex_none()
                .size(px(6.))
                .rounded_full()
                .bg(color)
        });
        let row = row(("conn-folder", at).into());
        let count = self.connections.read(cx).count_in(path);
        let (connections, target) = (self.connections.clone(), path.to_string());
        let chevron_id = ElementId::from(("folder-chevron", index));
        Self::clicks(view, row, index)
            .drag_over::<DraggedConnection>(|style, _, _, cx| style.bg(cx.theme().drop_target))
            .on_drop(move |dragged: &DraggedConnection, _, cx| {
                connections.update(cx, |c, cx| {
                    c.move_connection(&dragged.id, Some(&target), cx)
                });
            })
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1p5()
                    .child(Self::chevron(view, chevron_id, index, entry, cx))
                    .child(icon)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(entry.item().label.clone()),
                    )
                    .children(dot)
                    .when(count > 0, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(count.to_string())),
                        )
                    }),
            )
    }

    fn render_connection(
        &self,
        view: &Entity<Self>,
        row: ListItem,
        index: usize,
        entry: &TreeEntry,
        at: usize,
        cx: &App,
    ) -> ListItem {
        let theme = cx.theme();
        let connections = self.connections.read(cx);
        let saved = &connections.saved()[at];
        let config = &saved.config;
        let password_origin = connections.password_origin(&saved.id);
        // Its environment, seen before connecting (K47): the name in the
        // status bar's colours, so it reads as the chrome it will tint. One
        // that tints nothing (local) adds nothing.
        let environment = crate::theme::environment_chrome(config, cx).map(|chrome| {
            let state = crate::state::AppState::get(cx);
            let name = kerno_core::environment::environment(
                &state.settings.environments,
                &config.environment,
            )
            .name
            .clone();
            (chrome, SharedString::from(name))
        });
        let (state_label, state_color) = match connections.status(&saved.id) {
            // The tag takes the engine's place: the logo says it already.
            Status::Disconnected if environment.is_some() => ("", theme.muted_foreground),
            Status::Disconnected => (config.engine.label(), theme.muted_foreground),
            Status::Connecting if connections.reconnecting(&saved.id) => {
                ("reconnecting…", theme.warning)
            }
            Status::Connecting => ("connecting…", theme.muted_foreground),
            Status::WaitingForInput => ("waiting for you", theme.warning),
            Status::Connected { .. } => ("connected", theme.success),
            Status::Failed { .. } => ("failed", theme.danger),
        };
        // A colour tag is a small dot; no colour, no marker.
        let dot = theme::connection_color(config, cx);
        let dragged = DraggedConnection {
            id: saved.id.clone(),
            name: SharedString::from(config.name.clone()),
        };
        Self::clicks(view, row, index)
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1p5()
                    .child(Self::chevron(
                        view,
                        ("connection-chevron", at).into(),
                        index,
                        entry,
                        cx,
                    ))
                    .when_some(dot, |this, color| {
                        this.child(
                            div()
                                .id(("connection-dot", at))
                                .test_support()
                                .flex_none()
                                .size(px(8.))
                                .rounded_full()
                                .bg(color),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .child(engine_logo(config.engine, px(16.), cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(entry.item().label.clone()),
                    )
                    // Opened from a URL (`kerno <url>`): gone at quit unless
                    // its menu saves it.
                    .when(connections.is_unsaved(&saved.id), |this| {
                        this.child(
                            div()
                                .id(("connection-unsaved", at))
                                .test_support()
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(theme.border)
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("unsaved")
                                .tooltip(tip("Opened from a URL, not saved: its menu saves it")),
                        )
                    })
                    .when_some(environment, |this, (chrome, name)| {
                        let hint = format!("{} · {name} environment", config.engine.label());
                        this.child(
                            div()
                                .id(("connection-env", at))
                                .test_support()
                                .flex_none()
                                .px_1()
                                .rounded_sm()
                                .bg(chrome.bar)
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(chrome.bar_text)
                                .tooltip(tip(hint))
                                .child(name.clone())
                                .test_text(name),
                        )
                    })
                    .when(!state_label.is_empty(), |this| {
                        this.child(
                            div()
                                .id(("connection-state", at))
                                .flex_none()
                                .text_xs()
                                .text_color(state_color)
                                .child(state_label)
                                // Where the password came from (J3).
                                .when_some(password_origin, |this, origin| {
                                    this.tooltip(tip(origin))
                                }),
                        )
                    }),
            )
    }

    /// A line under a connection: what it is doing, what went wrong, or the
    /// button that tries again; or a line of a connection's tree (loading,
    /// empty, or an error), which the tree's own click selects.
    fn render_line(
        &self,
        view: &Entity<Self>,
        row: ListItem,
        index: usize,
        connection: &str,
        entry: &TreeEntry,
        cx: &App,
    ) -> ListItem {
        let row_id = entry.item().id.to_string();
        let local = match model::parse_row(&row_id) {
            Some(RowId::Object { local, .. }) => local,
            _ => "",
        };
        let theme = cx.theme();
        let label = entry.item().label.clone();
        let at = self
            .connections
            .read(cx)
            .saved()
            .iter()
            .position(|saved| saved.id == connection)
            .unwrap_or_default();
        match local {
            LINE_RETRY => {
                // The rows above cannot wrap: a failed connection's whole
                // message is one click away.
                let failed = matches!(
                    self.connections.read(cx).status(connection),
                    Status::Failed { .. }
                );
                let details = failed.then(|| {
                    let (connections, connection) =
                        (self.connections.clone(), connection.to_string());
                    Button::new(("connection-details", at))
                        .ghost()
                        .xsmall()
                        .label("Details…")
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            failure_details(connections.clone(), connection.clone(), window, cx);
                        })
                });
                let (view, connection) = (view.clone(), connection.to_string());
                row.child(
                    h_flex()
                        .pl(px(20.))
                        .gap_1()
                        .child(
                            Button::new(("connection-retry", at))
                                .outline()
                                .xsmall()
                                .label(label)
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    view.update(cx, |this, cx| this.retry(&connection, cx));
                                }),
                        )
                        .children(details),
                )
            }
            // What the connection's choice hides: a click (and Enter) opens
            // its settings, where the choice is (K62).
            model::HIDDEN_NOTE => {
                let (connections, connection) = (self.connections.clone(), connection.to_string());
                row.child(
                    div()
                        .id(("databases-hidden", at))
                        .test_support()
                        .min_w_0()
                        .cursor_pointer()
                        .child(line_text(label, theme.muted_foreground))
                        .tooltip(tip("Edit the connection to choose the databases shown"))
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            Sidebar::edit(connections.clone(), connection.clone(), window, cx);
                        }),
                )
            }
            LINE_ERROR => row.child(line_text(label, theme.danger)),
            LINE_NEXT => row.child(line_text(label, theme.foreground)),
            LINE_DETAIL => row.child(
                div()
                    .id(("connection-detail", at))
                    .min_w_0()
                    // One line in the row; all of it (a detail, a hint) in the tooltip.
                    .child(line_text(
                        SharedString::from(label.lines().next().unwrap_or_default().to_string()),
                        theme.muted_foreground,
                    ))
                    .tooltip(tip(label)),
            ),
            _ => {
                let failed = model::parent_of(local)
                    .and_then(|parent| self.tree(connection)?.node(parent))
                    .and_then(|parent| match &parent.children {
                        Children::Failed(message) => Some(SharedString::from(message.clone())),
                        _ => None,
                    });
                match failed {
                    // A read the user cancelled did not fail: said muted.
                    Some(message) if message == CANCELLED_LOAD => row.child(
                        div()
                            .id(("node-cancelled", at))
                            .test_support()
                            .min_w_0()
                            .child(line_text(label, theme.muted_foreground)),
                    ),
                    // The row shows the first line; the server's detail and
                    // hint are in its tooltip (C14).
                    Some(message) => row.child(
                        div()
                            .id(("node-failure", at))
                            .test_support()
                            .min_w_0()
                            .child(line_text(label, theme.danger))
                            .tooltip(tip(message)),
                    ),
                    // "Loading…": a Cancel beside it while the read runs (C3).
                    None if self.load_of_row(&row_id).is_some() => {
                        let view = view.clone();
                        row.child(
                            h_flex()
                                .gap_2()
                                .child(line_text(label, theme.muted_foreground))
                                .child(
                                    Button::new(("node-load-cancel", index))
                                        .ghost()
                                        .xsmall()
                                        .label("Cancel")
                                        .tooltip("Stop the read on the server (Cmd+.)")
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            view.update(cx, |this, cx| {
                                                this.cancel_load(&row_id, cx);
                                            });
                                        }),
                                ),
                        )
                    }
                    None => row.child(line_text(label, theme.muted_foreground)),
                }
            }
        }
    }

    fn node_content(
        &self,
        view: &Entity<Self>,
        index: usize,
        entry: &TreeEntry,
        connection: &str,
        node: &Node,
        cx: &App,
    ) -> impl IntoElement {
        let tree = self.tree(connection);
        let theme = cx.theme();
        let doing = self
            .write_on(connection, &node.id)
            .map(|write| write.doing.clone());
        let chevron = if entry.is_folder() {
            Self::chevron(view, ("node-chevron", index).into(), index, entry, cx)
        } else {
            div().flex_none().size_3().into_any_element()
        };
        let default = matches!(&node.kind, NodeKind::Object(ObjectRef::Database(name))
            if tree.and_then(Tree::default_database) == Some(name.as_str()));
        h_flex()
            .w_full()
            .min_w_0()
            .gap_1p5()
            .child(chevron)
            .child(
                Icon::new(glyph(&node.kind))
                    .flex_none()
                    .size_3p5()
                    .text_color(theme.muted_foreground),
            )
            .child(
                // The name keeps its room: the hint only gets what is
                // left, and the name is cut only when alone too long.
                div()
                    .min_w_0()
                    .truncate()
                    .when(node.muted, |this| this.text_color(theme.muted_foreground))
                    .child(entry.item().label.clone()),
            )
            // The database the connection opens on (CLAUDE.md §3 item 8).
            .when(default, |this| {
                this.child(
                    div()
                        .id(("node-default", index))
                        .test_support()
                        .flex_none()
                        .px_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("default"),
                )
            })
            // A statement running on it says so in place of the hint (C3).
            .when_some(doing.clone(), |this, doing| {
                this.child(
                    div()
                        .id(("node-doing", index))
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_right()
                        .text_xs()
                        .text_color(theme.warning)
                        .child(SharedString::from(doing)),
                )
            })
            .when_some(
                tree.and_then(|tree| Self::row_hint(tree, node))
                    .filter(|_| doing.is_none()),
                |this, hint| {
                    this.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_right()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(hint)),
                    )
                },
            )
    }
}

/// What "Maintenance Tools…" says on a node that has none.
const NO_TOOLS: &str = "No maintenance tool for this object: they act on tables, materialized \
                        views, PostgreSQL triggers and PostgreSQL databases.";

/// How a confirmed statement is run.
#[derive(Debug, Clone)]
struct Plan {
    /// The connection it runs on.
    connection: String,
    /// The database whose session runs it; the connection's own session
    /// when `None`.
    database: Option<String>,
    /// A database the navigator must not be connected to meanwhile.
    release: Option<String>,
    after: After,
    /// What to say once it ran.
    done: String,
    /// The node it acts on, which says it runs meanwhile.
    node: String,
    /// What runs, in a few words ("Dropping table t…").
    doing: String,
    /// What a failure says first, in plain words ("The table “t” was not
    /// dropped."), before the server's message, detail and hint (C14).
    failed: String,
}

/// What to show again once a statement ran.
#[derive(Debug, Clone)]
enum After {
    /// The list of databases.
    Reload,
    /// The node with this id (nothing when there is none).
    Refresh(Option<String>),
}

/// What a kind of object is called in a sentence.
fn kind_name(object: &ObjectRef) -> &'static str {
    match object {
        ObjectRef::Relation(_, RelationKind::View) => "view",
        ObjectRef::Relation(_, RelationKind::MaterializedView) => "materialized view",
        ObjectRef::Relation(..) => "table",
        ObjectRef::Routine {
            kind: RoutineKind::Procedure,
            ..
        } => "procedure",
        ObjectRef::Routine { .. } => "function",
        ObjectRef::Sequence(_) => "sequence",
        ObjectRef::Type(_) => "type",
        ObjectRef::Trigger { .. } => "trigger",
        ObjectRef::Event(_) => "event",
        ObjectRef::Index { .. } => "index",
        ObjectRef::Schema(_) => "schema",
        ObjectRef::Database(_) => "database",
    }
}

/// What a failed read of the tree says on its row (C14). A session that
/// could not open in `database` (a privilege, a server's rule), or was
/// lost (K75), is said in plain words first, then the server's own
/// (CLAUDE.md §3 item 5, K62); a statement's error is said as it is.
fn read_failure(
    error: &Error,
    reading: &str,
    database: &str,
    config: Option<&kerno_core::config::ConnectionConfig>,
) -> String {
    let raw = format!("Could not read {reading}: {}", describe(error));
    let explained = match (error, config) {
        (Error::Connect(_) | Error::Closed(_), Some(config)) => kerno_core::error::explain(
            &error.to_string(),
            &kerno_core::config::ConnectionConfig {
                database: Some(database.to_string()),
                ..config.clone()
            },
        ),
        _ => return raw,
    };
    if explained.kind == kerno_core::error::FailureKind::Other {
        return raw;
    }
    [explained.headline, explained.next_step, raw]
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The name of an object without its schema: what a rename starts from.
fn plain_name(object: &ObjectRef) -> String {
    object.name().to_string()
}

/// The glyph of a kind of node.
fn glyph(kind: &NodeKind) -> Glyph {
    match kind {
        NodeKind::Object(object) => match object {
            ObjectRef::Database(_) => Glyph::Database,
            ObjectRef::Schema(_) => Glyph::Boxes,
            ObjectRef::Relation(_, RelationKind::View) => Glyph::Eye,
            ObjectRef::Relation(_, RelationKind::MaterializedView) => Glyph::Layers,
            ObjectRef::Relation(..) => Glyph::Table,
            ObjectRef::Routine {
                kind: RoutineKind::Procedure,
                ..
            } => Glyph::Workflow,
            ObjectRef::Routine { .. } => Glyph::SquareFunction,
            ObjectRef::Sequence(_) => Glyph::ListOrdered,
            ObjectRef::Type(_) => Glyph::Shapes,
            ObjectRef::Trigger { .. } => Glyph::Zap,
            ObjectRef::Event(_) => Glyph::CalendarClock,
            ObjectRef::Index { .. } => Glyph::KeyRound,
        },
        NodeKind::Group(Group::Extensions) | NodeKind::Extension { .. } => Glyph::Puzzle,
        NodeKind::EnumValue { .. } => Glyph::ListOrdered,
        NodeKind::Group(_) => Glyph::Folder,
        NodeKind::Column { .. } => Glyph::Columns3,
        NodeKind::Constraint { .. } => Glyph::ShieldCheck,
        NodeKind::ForeignKey { .. } => Glyph::Link2,
    }
}

/// The name to copy for a node, without its schema (DBeaver's Copy; "Copy
/// qualified name" adds the schema).
fn name_of(node: &Node) -> String {
    match &node.kind {
        NodeKind::Object(object) => plain_name(object),
        NodeKind::Column { name, .. }
        | NodeKind::Extension { name, .. }
        | NodeKind::Constraint { name, .. }
        | NodeKind::ForeignKey { name, .. }
        | NodeKind::EnumValue { value: name, .. } => name.clone(),
        NodeKind::Group(group) => group.label().to_string(),
    }
}

/// The height of every row of the tree.
const ROW_HEIGHT: f32 = 24.;

/// The folders made, the folders closed and their looks.
type FolderSettings = (
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeMap<String, FolderLook>,
);

/// The folders made, the folders closed and their looks, as the settings
/// hold them.
fn folder_settings(cx: &App) -> FolderSettings {
    let settings = &AppState::get(cx).settings;
    (
        settings.connection_folders.clone(),
        settings.collapsed_folders.clone(),
        settings.folder_looks.clone(),
    )
}

/// The connection a row of the tree is or is under.
fn connection_of(id: &str) -> Option<&str> {
    match model::parse_row(id)? {
        RowId::Connection(connection) | RowId::Object { connection, .. } => Some(connection),
        RowId::Folder(_) | RowId::FolderNote(_) => None,
    }
}

/// What else `change` reaches (C11), read on a session of its own on
/// `database` (C3); the task dropped (the dialog closed, answered) stops it
/// on the server and closes the session. The tree's drops and the
/// extensions manager's (K27) read it this way.
pub fn read_dependents(
    connections: &Entity<Connections>,
    connection: &str,
    database: Option<String>,
    change: Change,
    cx: &mut App,
) -> Task<kerno_core::Result<Vec<Dependent>>> {
    let opening = connections.read(cx).open_session(connection, database, cx);
    cx.spawn(async move |cx| {
        let session = opening.await?;
        // Dropped with this task, when the dialog goes first.
        let (_waiting, gone) = tokio::sync::oneshot::channel::<()>();
        cx.update(|cx| {
            bridge::spawn(cx, async move {
                let found = tokio::select! {
                    found = session.dependents(&change) => found,
                    _ = gone => Err(Error::Cancelled),
                };
                session.close().await;
                found
            })
        })
        .await
    })
}

/// A row of the forest as the tree widget takes it. What a closed row
/// holds is left out (one empty child keeps its chevron): the widget
/// copies each shown row's children.
fn tree_item(item: Item) -> TreeItem {
    let row = TreeItem::new(item.id.clone(), item.label).expanded(item.expanded);
    if item.expanded {
        row.children(item.children.into_iter().map(tree_item))
    } else if item.children.is_empty() {
        row
    } else {
        row.child(TreeItem::new(format!("{}\u{1e}…", item.id), ""))
    }
}

/// A line of text in a row.
fn line_text(text: SharedString, color: gpui_kit::Hsla) -> impl IntoElement {
    div()
        .pl(px(20.))
        .min_w_0()
        .truncate()
        .text_xs()
        .text_color(color)
        .child(text)
}

/// An AI ▸ entry as the menu shows it: what it asks, its words, usable.
type AiShown = (AiEntry, String, bool);

/// The menu of the object on the row `id`: each entry says why it cannot
/// be used, when it cannot.
fn object_menu(
    view: &Entity<Navigator>,
    id: &str,
    mut menu: PopupMenu,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let navigator = view.read(cx);
    // A read filling this row, or the node of this "Loading…" line, can be
    // stopped from here (C3).
    if navigator.load_of_row(id).is_some() {
        let (view, id) = (view.clone(), id.to_string());
        menu = menu
            .item(
                PopupMenuItem::new("Cancel loading").on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| this.cancel_load(&id, cx));
                }),
            )
            .separator();
    }
    let Some((connection, node)) = navigator.object(id) else {
        return menu;
    };
    // Read first; the Tools submenu is built afterwards.
    let groups: Vec<Vec<(Action, Option<&'static str>)>> =
        actions(&node, navigator.has_schemas(&connection, cx))
            .into_iter()
            .map(|group| {
                group
                    .into_iter()
                    .map(|action| {
                        (
                            action,
                            navigator.unavailable(&connection, action, &node, cx),
                        )
                    })
                    .collect()
            })
            .collect();
    let tools = navigator.tool_menu(&connection, &node, cx);
    let filters: Vec<(FilterAction, Option<&'static str>)> = filter_actions(&node)
        .into_iter()
        .map(|action| {
            (
                action,
                navigator.filter_unavailable(&connection, action, &node, cx),
            )
        })
        .collect();
    // AI ▸ of a table, a view, a materialized view or a column (K53).
    let ai = navigator.ai_entries(&connection, &node, cx);
    let entry = |label: &str, reason: Option<&str>| match reason {
        Some(reason) => SharedString::from(format!("{label} ({reason})")),
        None => SharedString::from(label.to_string()),
    };
    // A statement running on it can be stopped from here (C3).
    if let Some(key) = navigator
        .write_on(&connection, &node.id)
        .map(|write| write.key)
    {
        let view = view.clone();
        menu = menu
            .item(
                PopupMenuItem::new("Cancel the running statement").on_click(move |_, _, cx| {
                    view.update(cx, |this, cx| this.cancel_write(key, cx))
                }),
            )
            .separator();
    }
    let id = id.to_string();
    let last = groups.len().saturating_sub(1);
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            menu = menu.separator();
        }
        // Tools ▸ comes just before Refresh, in a group of its own.
        if index == last && !tools.is_empty() {
            let (view, id, tools) = (view.clone(), id.clone(), tools.clone());
            let submenu = PopupMenu::build(window, cx, move |mut submenu, _, _| {
                for (index, group) in tools.into_iter().enumerate() {
                    if index > 0 {
                        submenu = submenu.separator();
                    }
                    for (tool, reason) in group {
                        let (view, id) = (view.clone(), id.clone());
                        submenu = submenu.item(
                            PopupMenuItem::new(entry(&tool.label(), reason))
                                .disabled(reason.is_some())
                                .on_click(move |_, window, cx| {
                                    let tool = tool.clone();
                                    view.update(cx, |this, cx| {
                                        this.perform_tool(tool, &id, window, cx)
                                    })
                                }),
                        );
                    }
                }
                submenu
            });
            menu = menu
                .item(PopupMenuItem::submenu(Action::Tools.label(), submenu))
                .separator();
        }
        // Filter ▸ too, before Refresh.
        if index == last && !filters.is_empty() {
            let (view, id, filters) = (view.clone(), id.clone(), filters.clone());
            let submenu = PopupMenu::build(window, cx, move |mut submenu, _, _| {
                for (action, reason) in filters {
                    let (view, id) = (view.clone(), id.clone());
                    submenu = submenu.item(
                        PopupMenuItem::new(entry(action.label(), reason))
                            .disabled(reason.is_some())
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.perform_filter(action, &id, window, cx)
                                })
                            }),
                    );
                }
                submenu
            });
            menu = menu
                .item(PopupMenuItem::submenu("Filter", submenu))
                .separator();
        }
        // AI ▸ last, before Refresh (K53).
        if index == last
            && let Some(ai) = &ai
        {
            let (view, id, ai) = (view.clone(), id.clone(), ai.clone());
            let submenu = PopupMenu::build(window, cx, move |mut submenu, _, _| {
                for (ask, label, enabled) in ai {
                    let (view, id) = (view.clone(), id.clone());
                    submenu = submenu.item(
                        PopupMenuItem::new(SharedString::from(label))
                            .disabled(!enabled)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| this.ask_ai(ask, &id, window, cx))
                            }),
                    );
                }
                submenu
            });
            menu = menu.item(PopupMenuItem::submenu("AI", submenu)).separator();
        }
        for (action, reason) in group {
            let (view, id) = (view.clone(), id.clone());
            menu = menu.item(
                PopupMenuItem::new(entry(action.label(), reason))
                    .disabled(reason.is_some())
                    .on_click(move |_, window, cx| {
                        view.update(cx, |this, cx| this.perform(action, &id, window, cx))
                    }),
            );
        }
    }
    menu
}

/// The sidebar's menu of `subject`, built when it opens.
fn sidebar_menu(
    view: &Entity<Navigator>,
    subject: Subject,
    menu: PopupMenu,
    window: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let Some(sidebar) = view.read(cx).sidebar.upgrade() else {
        return menu;
    };
    let entries = sidebar.read(cx).menu_of(&subject, cx);
    Sidebar::fill_menu(sidebar, subject, entries, menu, window, cx)
}

impl Render for Navigator {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let (view, menu_view, keys, area) = (cx.entity(), cx.entity(), cx.entity(), cx.entity());
        // Each row's menu is the one of what it is; a menu attached to the
        // row itself would stack under the kit's.
        let tree_view = tree(&self.tree_state, move |index, entry, _, _, cx| {
            view.read(cx).render_row(&view, index, entry, cx)
        })
        .context_menu(move |_, entry, menu, window, cx| {
            let id = entry.item().id.to_string();
            let subject = match model::parse_row(&id) {
                None => return menu,
                Some(RowId::Object { local, .. }) if !local.starts_with("line:") => {
                    return object_menu(&menu_view, &id, menu, window, cx);
                }
                Some(RowId::Folder(path) | RowId::FolderNote(path)) => {
                    Subject::Folder(path.to_string())
                }
                Some(RowId::Connection(connection) | RowId::Object { connection, .. }) => {
                    Subject::Connection(connection.to_string())
                }
            };
            sidebar_menu(&menu_view, subject, menu, window, cx)
        })
        .size_full();
        // The filter looks only at what is loaded: when nothing there matches, say so and
        // where the rest is, rather than an empty sidebar.
        let nothing = (!self.filter_text.is_empty() && self.tree_state.read(cx).entry(0).is_none())
            .then(|| {
                format!(
                    "Nothing loaded matches “{}”. The filter looks at what the tree has already \
                     opened: expand a schema, or use Quick Open ({}).",
                    self.filter.read(cx).value().trim(),
                    crate::actions::shown("secondary-p")
                )
            });
        v_flex()
            .id("navigator")
            .test_support()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(
                div().px_3().child(
                    Input::new(&self.filter)
                        .id("navigator-filter")
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).size_3p5().text_color(muted)),
                ),
            )
            .children(nothing.map(|line| {
                div()
                    .id("navigator-filter-empty")
                    .test_support()
                    .px_3()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(line.clone()))
                    .test_text(line)
            }))
            // Right and Left also move to the first child and the parent;
            // Enter does what a double click does. They run before the
            // tree's own handling of the keys. The other keys are commands
            // bound in the `Navigator` context (see `actions.rs`). A
            // connection dropped below the rows leaves its folder.
            .child(
                div()
                    .id("navigator-tree")
                    .test_support()
                    .key_context("Navigator")
                    .flex_1()
                    .min_h_0()
                    .capture_action(cx.listener(|this, _: &SelectRight, _, cx| {
                        cx.stop_propagation();
                        this.key_right(cx);
                    }))
                    .capture_action(cx.listener(|this, _: &SelectLeft, _, cx| {
                        cx.stop_propagation();
                        this.key_left(cx);
                    }))
                    // Cmd+. on a row being read stops the read (C3); else
                    // it goes on to the active tab.
                    // Edit ▸ Copy does what Cmd+C does here: the selected
                    // object's name (K61).
                    .on_action(|_: &gpui_kit::base::input::Copy, window, cx| {
                        window.dispatch_action(Box::new(crate::actions::CopyObjectName), cx)
                    })
                    .capture_action(cx.listener(|this, _: &crate::actions::CancelQuery, _, cx| {
                        if this.cancel_selected_load(cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .on_key_down(move |event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified() {
                            cx.stop_propagation();
                            keys.update(cx, |this, cx| {
                                if let Some(index) = this.tree_state.read(cx).selected_index() {
                                    this.open_row(index, window, cx);
                                }
                            });
                        }
                    })
                    .on_drop(cx.listener(|this, dragged: &DraggedConnection, _, cx| {
                        this.connections
                            .update(cx, |c, cx| c.move_connection(&dragged.id, None, cx));
                    }))
                    .child(tree_view),
            )
            // Every empty area of the sidebar answers with the list's menu.
            .context_menu(move |menu, window, cx| {
                sidebar_menu(&area, Subject::List, menu, window, cx)
            })
    }
}

/// Why a connection failed, whole: what happened, what to do, and the
/// driver's message (selectable), with Copy and "Edit connection…". The
/// tree's rows show them cut to one line each.
fn failure_details(
    connections: Entity<Connections>,
    id: String,
    window: &mut Window,
    cx: &mut App,
) {
    let (config, message) = {
        let manager = connections.read(cx);
        let (Some(saved), Status::Failed { message }) = (manager.get(&id), manager.status(&id))
        else {
            return;
        };
        (saved.config.clone(), message)
    };
    let explained = kerno_core::error::explain(&message, &config);
    let copied = [
        explained.headline.as_str(),
        explained.next_step.as_str(),
        explained.raw.as_str(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let title = SharedString::from(format!("“{}” could not connect", config.name));
    window.open_dialog(cx, move |dialog, _, _| {
        let (explained, copied) = (explained.clone(), copied.clone());
        let (connections, id) = (connections.clone(), id.clone());
        dialog
            .title(title.clone())
            .w(px(560.))
            .content(move |content, _, cx| {
                let copied = copied.clone();
                let (connections, id) = (connections.clone(), id.clone());
                content.child(
                    v_flex()
                        .id("connection-failure-dialog")
                        .test_support()
                        .gap_3()
                        .child(failure_parts(&explained, cx))
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("connection-failure-copy")
                                        .outline()
                                        .label("Copy")
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copied.clone(),
                                            ))
                                        }),
                                )
                                .child(
                                    Button::new("connection-failure-edit")
                                        .outline()
                                        .label("Edit connection…")
                                        .on_click(move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let (connections, id) =
                                                (connections.clone(), id.clone());
                                            window.defer(cx, move |window, cx| {
                                                Sidebar::edit(connections, id, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("connection-failure-close")
                                        .primary()
                                        .label("Close")
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                ),
                        ),
                )
            })
    });
}

/// The three parts of a failure, each selectable: what happened (strong),
/// what to do next, the driver's message (muted).
fn failure_parts(explained: &kerno_core::error::Explained, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let part = |id: &'static str, text: &str| {
        div().id(id).test_support().child(
            TextView::markdown(
                SharedString::from(format!("{id}-text")),
                plain_markdown(text),
            )
            .selectable(true),
        )
    };
    v_flex()
        .gap_1()
        .text_sm()
        .child(
            part("connection-failure-headline", &explained.headline)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.danger),
        )
        .when(!explained.next_step.is_empty(), |this| {
            this.child(part("connection-failure-next", &explained.next_step))
        })
        .child(
            part("connection-failure-raw", &explained.raw)
                .text_xs()
                .text_color(theme.muted_foreground),
        )
        .into_any_element()
}

/// `text` as Markdown that renders as itself: every ASCII punctuation mark
/// escaped, line breaks kept.
fn plain_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        match c {
            '\n' => out.push_str("  \n"),
            c if c.is_ascii_punctuation() => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use kerno_core::DbError;
    use kerno_core::config::ConnectionConfig;

    use super::*;

    /// K62: a session refused in a database is said in plain words first,
    /// then the server's line; a statement's error with the same code, or
    /// a failure with no plain words, is said as it is.
    #[test]
    fn a_refused_session_is_said_in_plain_words_first() {
        let config = ConnectionConfig::new(Engine::Postgres);
        let refused = Error::Connect("permission denied for database \"billing\" [42501]".into());
        let text = read_failure(&refused, "the schemas", "billing", Some(&config));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "You may not connect to database “billing”.");
        assert_eq!(
            lines.last(),
            Some(
                &"Could not read the schemas: could not connect: permission denied for \
                  database \"billing\" [42501]"
            )
        );
        let statement = Error::Db(Box::new(DbError {
            code: Some("42501".into()),
            ..DbError::message("permission denied for schema shop")
        }));
        assert_eq!(
            read_failure(&statement, "the objects of shop", "billing", Some(&config)),
            "Could not read the objects of shop: permission denied for schema shop [42501]"
        );
        let other = Error::Connect("something unheard of".into());
        assert_eq!(
            read_failure(&other, "the schemas", "billing", Some(&config)),
            "Could not read the schemas: could not connect: something unheard of"
        );
    }

    /// K75: a read whose session was lost (once read again) says so in plain words first, with
    /// what to do, then the driver's own words.
    #[test]
    fn a_lost_session_is_said_in_plain_words_first() {
        let config = ConnectionConfig {
            name: "pg test".into(),
            ..ConnectionConfig::new(Engine::Postgres)
        };
        let lost = Error::Closed("the session was closed".into());
        assert_eq!(
            read_failure(&lost, "the objects of shop", "kerno", Some(&config)),
            "The connection to “pg test” was lost.\nReconnect it (its menu in the sidebar), then \
             try again.\nCould not read the objects of shop: connection closed: the session was \
             closed"
        );
    }
}
