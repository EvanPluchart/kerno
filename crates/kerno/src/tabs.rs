//! Tabs of the central area: what is remembered of them between runs, the
//! menus of the tab bar, the editor and the status bar, and what closing a
//! tab would lose.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{App, SharedString, Window, px};
use kerno_core::TxState;
use serde::{Deserialize, Serialize};

/// Where a query tab runs its statements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// Id of the saved connection.
    pub connection: String,
    /// Database; the connection's own when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    /// Schema put first in the search path (PostgreSQL); the default when
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

/// What is stored of an open tab, so it comes back after a restart with
/// its unsaved text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TabSnapshot {
    /// A query tab.
    Query {
        /// Tab title.
        title: String,
        /// Editor text, saved or not.
        sql: String,
        /// The connection the tab runs on, when one was chosen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<Target>,
        /// The `.sql` file the tab was opened from or saved to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file: Option<PathBuf>,
        /// The text differs from the file's.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unsaved: bool,
        /// Source mode: the schema of the object the text replaces.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    /// The data of a table or view.
    Data {
        /// The connection and database the table is in.
        target: Target,
        /// PostgreSQL schema or MySQL database.
        schema: String,
        table: String,
        /// A view: its rows are read-only.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        view: bool,
    },
    /// The structure of a table or view.
    Structure {
        /// The connection and database the table is in.
        target: Target,
        /// PostgreSQL schema or MySQL database.
        schema: String,
        table: String,
        /// A view.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        view: bool,
        /// A materialized view: its DDL is not a view's.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        materialized: bool,
    },
    /// The administration of a server.
    Admin {
        /// The connection, and the database its tables are listed from.
        target: Target,
    },
    /// An ER diagram (J18): of a schema, or of a table and its neighbours.
    Diagram {
        /// The connection and database the tables are in.
        target: Target,
        /// PostgreSQL schema or MySQL database.
        schema: String,
        /// The table whose neighbours are drawn; the whole schema when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        table: Option<String>,
    },
}

impl TabSnapshot {
    /// The tab as it opens again when its connection is gone (deleted, or
    /// opened from a URL and never saved): a query tab keeps its text and
    /// runs nowhere until a connection is chosen; a tab that only shows
    /// something of that connection is not opened. `known` says whether a
    /// connection id is a saved connection.
    pub fn without_gone_connection(self, known: impl Fn(&str) -> bool) -> Option<Self> {
        match self {
            Self::Query {
                title,
                sql,
                target,
                file,
                unsaved,
                source,
            } => {
                let gone = target.as_ref().is_some_and(|t| !known(&t.connection));
                Some(Self::Query {
                    title,
                    sql,
                    target: target.filter(|_| !gone),
                    file,
                    unsaved,
                    // Source mode belongs to an object of that connection.
                    source: source.filter(|_| !gone),
                })
            }
            Self::Data { ref target, .. }
            | Self::Structure { ref target, .. }
            | Self::Admin { ref target }
            | Self::Diagram { ref target, .. } => known(&target.connection).then_some(self),
        }
    }
}

/// An entry of the menus of the tabs, the editor and the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabAction {
    // A tab.
    Close,
    CloseOthers,
    CloseToRight,
    CloseAll,
    Rename,
    Duplicate,
    CopyPath,
    Reveal,
    // The empty area of the tab bar.
    NewQuery,
    ReopenClosed,
    // The SQL editor.
    ApplySource,
    RunStatement,
    RunInNewTab,
    RunFromCursor,
    RunScript,
    /// Runs the script on connections the user picks (J14).
    RunOnConnections,
    Explain,
    ExplainAnalyze,
    CountRows,
    Format,
    NextStatement,
    PreviousStatement,
    ToggleResults,
    MaximizeResults,
    SwitchFocus,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    SelectAllOccurrences,
    ToggleComment,
    ToggleBlockComment,
    UpperCase,
    LowerCase,
    ToggleSoftWrap,
    UnfoldAll,
    Find,
    Replace,
    Save,
    SaveAs,
    SaveQuery,
    RevertFile,
    OpenFile,
    FetchAllRows,
    ExportFromQuery,
    /// Makes the tab run on this saved connection: id and name.
    UseConnection(String, String),
    UseSchema(String),
    // The status bar.
    Disconnect,
    Reconnect,
    Administration,
    EditConnection,
    UseDatabase(String),
    Commit,
    Rollback,
    /// Lists the statements of the open transaction.
    TransactionLog,
    /// Statements run in a transaction the user ends (`true`), or not.
    ManualCommit(bool),
    Cancel,
    ReviewChanges,
    DiscardChanges,
    /// Copies this text.
    CopyText(String),
    AboutReadOnly,
    AboutMemoryOnly,
    /// Shows the messages of the query tab's last run.
    ShowMessages,
    NewConnection,
    DisconnectAll,
    // A result tab of a query tab.
    CloseToLeft,
    Pin,
    Unpin,
    UnpinAll,
    CopyStatement,
    /// Runs the statement of an incomplete result again.
    RunAgain,
    // The update item of the status bar.
    Update,
    OpenReleasePage,
    SkipVersion,
    TurnOffUpdateChecks,
}

impl TabAction {
    pub fn label(&self) -> String {
        match self {
            Self::Close => "Close",
            Self::CloseOthers => "Close others",
            Self::CloseToRight => "Close tabs to the right",
            Self::CloseAll => "Close all",
            Self::Rename => "Rename…",
            Self::Duplicate => "Duplicate",
            Self::CopyPath => "Copy file path",
            Self::Reveal if cfg!(target_os = "macos") => "Reveal in Finder",
            Self::Reveal => "Show in folder",
            Self::NewQuery => "New query tab",
            Self::ReopenClosed => "Reopen closed tab",
            Self::ApplySource => "Apply source…",
            Self::RunStatement => "Run statement",
            Self::RunInNewTab => "Run statement in new result tab",
            Self::RunFromCursor => "Run script from the cursor",
            Self::RunScript => "Run script",
            Self::RunOnConnections => "Run script on connections…",
            Self::Explain => "Explain plan",
            Self::ExplainAnalyze => "Explain analyze",
            Self::CountRows => "Count rows of statement",
            Self::Format => "Format SQL",
            Self::NextStatement => "Go to next statement",
            Self::PreviousStatement => "Go to previous statement",
            Self::ToggleResults => "Toggle results panel",
            Self::MaximizeResults => "Maximize results panel",
            Self::SwitchFocus => "Focus results / editor",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::Cut => "Cut",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::SelectAll => "Select all",
            Self::SelectAllOccurrences => "Select all occurrences",
            Self::ToggleComment => "Toggle line comment",
            Self::ToggleBlockComment => "Toggle block comment",
            Self::UpperCase => "Upper case",
            Self::LowerCase => "Lower case",
            Self::ToggleSoftWrap => "Toggle word wrap",
            Self::UnfoldAll => "Unfold all",
            Self::Find => "Find…",
            Self::Replace => "Replace…",
            Self::Save => "Save",
            Self::SaveAs => "Save as…",
            Self::SaveQuery => "Save as query…",
            Self::RevertFile => "Revert to saved file",
            Self::OpenFile => "Open SQL file…",
            Self::FetchAllRows => "Fetch all rows",
            Self::ExportFromQuery => "Export from query…",
            Self::UseConnection(_, name) => return name.clone(),
            Self::UseSchema(schema) => return schema.clone(),
            Self::Disconnect => "Disconnect",
            Self::Reconnect => "Reconnect",
            Self::Administration => "Administration",
            Self::EditConnection => "Edit connection…",
            Self::UseDatabase(database) => return database.clone(),
            Self::Commit => "Commit transaction",
            Self::Rollback => "Roll back transaction",
            Self::TransactionLog => "Transaction log…",
            Self::ManualCommit(true) => "Switch to manual commit",
            Self::ManualCommit(false) => "Switch to auto-commit",
            Self::Cancel => "Cancel running statement",
            Self::ReviewChanges => "Review SQL…",
            Self::DiscardChanges => "Discard changes",
            Self::CopyText(_) => "Copy",
            Self::AboutReadOnly => "What read-only means…",
            Self::AboutMemoryOnly => "What this means…",
            Self::ShowMessages => "Show messages",
            Self::NewConnection => "New connection…",
            Self::DisconnectAll => "Disconnect all",
            Self::CloseToLeft => "Close tabs to the left",
            Self::Pin => "Pin tab",
            Self::Unpin => "Unpin tab",
            Self::UnpinAll => "Unpin all tabs",
            Self::CopyStatement => "Copy statement",
            Self::RunAgain => "Run again",
            Self::Update => "Update",
            Self::OpenReleasePage => "Open release page",
            Self::SkipVersion => "Skip this version",
            Self::TurnOffUpdateChecks => "Turn off update checks",
        }
        .to_string()
    }
}

/// One line of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// An action, and why it cannot be used now when it cannot.
    Item(TabAction, Option<&'static str>),
    Submenu(&'static str, Vec<Entry>),
    Separator,
}

use Entry::{Item, Separator, Submenu};

fn enabled(action: TabAction) -> Entry {
    Item(action, None)
}

/// What a tab is, for its menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    Query {
        file: bool,
    },
    /// A table's data or structure, or a table form.
    Other,
}

/// The menu of the tab at `index` of `count`.
pub fn tab_menu(kind: TabKind, index: usize, count: usize) -> Vec<Entry> {
    let (rename, duplicate) = match kind {
        TabKind::Query { file: false } => (None, None),
        TabKind::Query { file: true } => (Some("named after its file"), None),
        TabKind::Other => (Some("query tabs only"), Some("query tabs only")),
    };
    let mut entries = vec![
        enabled(TabAction::Close),
        Item(
            TabAction::CloseOthers,
            (count < 2).then_some("no other tab"),
        ),
        Item(
            TabAction::CloseToRight,
            (index + 1 >= count).then_some("none to the right"),
        ),
        enabled(TabAction::CloseAll),
        Separator,
        Item(TabAction::Rename, rename),
        Item(TabAction::Duplicate, duplicate),
    ];
    if kind == (TabKind::Query { file: true }) {
        entries.extend([
            Separator,
            enabled(TabAction::CopyPath),
            enabled(TabAction::Reveal),
        ]);
    }
    entries
}

/// The menu of the empty area of the tab bar: `open` tabs, `closed` query
/// tabs that can come back.
pub fn bar_menu(open: usize, closed: usize) -> Vec<Entry> {
    vec![
        enabled(TabAction::NewQuery),
        Item(
            TabAction::ReopenClosed,
            (closed == 0).then_some("none closed"),
        ),
        Separator,
        Item(TabAction::CloseAll, (open == 0).then_some("no tab open")),
    ]
}

/// What the editor menu depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EditorFacts {
    pub connected: bool,
    pub running: bool,
    pub selection: bool,
    pub empty: bool,
    /// The tab is a file whose text was saved as it is.
    pub saved_file: bool,
    /// Source mode: the text replaces an object.
    pub source: bool,
    /// The tab is a file.
    pub file: bool,
    /// The result on screen has rows left to fetch.
    pub more_rows: bool,
}

/// What the editor menu offers to run on: the saved connections (id, name,
/// connected), and the databases and schemas of the tab's, with the ones in
/// use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choices {
    pub connections: Vec<(String, String, bool)>,
    pub connection: Option<String>,
    pub databases: Vec<String>,
    pub database: Option<String>,
    pub schemas: Vec<String>,
    pub schema: Option<String>,
}

/// The menu of the SQL editor, grouped as DBeaver's (`SQLEditor` context
/// menu: Execute ▸, Format ▸, File ▸); enabled as the toolbar's buttons are.
/// The three ways to run used most stay on top.
pub fn editor_menu(facts: EditorFacts, choices: &Choices) -> Vec<Entry> {
    let run = if !facts.connected {
        Some("not connected")
    } else {
        facts.running.then_some("running")
    };
    let no_selection = (!facts.selection).then_some("no selection");
    let empty = facts.empty.then_some("empty");
    let current =
        |is: bool, reason: Option<&'static str>| if is { Some("current") } else { reason };
    let connections: Vec<Entry> = choices
        .connections
        .iter()
        .map(|(id, name, connected)| {
            Item(
                TabAction::UseConnection(id.clone(), name.clone()),
                current(
                    choices.connection.as_ref() == Some(id),
                    (!connected).then_some("not connected"),
                ),
            )
        })
        .collect();
    let list = |names: &[String], chosen: &Option<String>, action: fn(String) -> TabAction| {
        names
            .iter()
            .map(|name| {
                Item(
                    action(name.clone()),
                    current(chosen.as_ref() == Some(name), None),
                )
            })
            .collect()
    };
    // The lists to run on, those that have something to choose from.
    let targets: Vec<Entry> = [
        ("Active connection", connections),
        (
            "Active database",
            list(
                &choices.databases,
                &choices.database,
                TabAction::UseDatabase,
            ),
        ),
        (
            "Active schema",
            list(&choices.schemas, &choices.schema, TabAction::UseSchema),
        ),
    ]
    .into_iter()
    .filter(|(_, items)| !items.is_empty())
    .map(|(label, items)| Submenu(label, items))
    .collect();
    let mut entries = Vec::new();
    if facts.source {
        entries.extend([Item(TabAction::ApplySource, run), Separator]);
    }
    entries.extend([
        Item(TabAction::RunStatement, run),
        Item(TabAction::RunScript, run),
        Item(TabAction::Explain, run),
        Submenu(
            "Execute",
            vec![
                Item(TabAction::RunInNewTab, run),
                Item(TabAction::RunFromCursor, run),
                Item(TabAction::RunOnConnections, empty),
                Item(TabAction::ExplainAnalyze, run),
                Separator,
                Item(TabAction::CountRows, run),
                Item(
                    TabAction::FetchAllRows,
                    (!facts.more_rows).then_some("no rows left"),
                ),
                Item(TabAction::ExportFromQuery, run.or(empty)),
            ]
            .into_iter()
            .chain((!targets.is_empty()).then_some(Separator))
            .chain(targets)
            .collect(),
        ),
        Separator,
        Item(TabAction::NextStatement, empty),
        Item(TabAction::PreviousStatement, empty),
        Separator,
        enabled(TabAction::ToggleResults),
        enabled(TabAction::MaximizeResults),
        enabled(TabAction::SwitchFocus),
        Separator,
        enabled(TabAction::Undo),
        enabled(TabAction::Redo),
        Separator,
        Item(TabAction::Cut, no_selection),
        Item(TabAction::Copy, no_selection),
        Item(TabAction::CopyStatement, empty),
        enabled(TabAction::Paste),
        Item(TabAction::SelectAll, empty),
        Item(TabAction::SelectAllOccurrences, empty),
        Separator,
        Submenu(
            "Format",
            vec![
                Item(TabAction::Format, empty),
                Item(TabAction::UpperCase, no_selection),
                Item(TabAction::LowerCase, no_selection),
                Separator,
                enabled(TabAction::ToggleComment),
                enabled(TabAction::ToggleBlockComment),
                Separator,
                enabled(TabAction::ToggleSoftWrap),
                enabled(TabAction::UnfoldAll),
            ],
        ),
        enabled(TabAction::Find),
        enabled(TabAction::Replace),
        Separator,
        Submenu(
            "File",
            vec![
                Item(
                    TabAction::Save,
                    if facts.saved_file {
                        Some("saved")
                    } else {
                        empty
                    },
                ),
                Item(TabAction::SaveAs, empty),
                Item(TabAction::SaveQuery, empty),
                Separator,
                Item(
                    TabAction::Rename,
                    facts.file.then_some("named after its file"),
                ),
                Item(
                    TabAction::RevertFile,
                    if !facts.file {
                        Some("no file")
                    } else {
                        facts.saved_file.then_some("saved")
                    },
                ),
                enabled(TabAction::OpenFile),
            ],
        ),
    ]);
    entries
}

/// The toolbar's Run dropdown: the other ways to run, and to move between
/// statements and panels; enabled as the editor menu is.
pub fn run_menu(facts: EditorFacts) -> Vec<Entry> {
    let run = if !facts.connected {
        Some("not connected")
    } else {
        facts.running.then_some("running")
    };
    let empty = facts.empty.then_some("empty");
    vec![
        Item(TabAction::RunStatement, run),
        Item(TabAction::RunInNewTab, run),
        Item(TabAction::RunFromCursor, run),
        Item(TabAction::RunScript, run),
        Item(TabAction::CountRows, run),
        Item(TabAction::RunOnConnections, empty),
        Separator,
        Item(TabAction::NextStatement, empty),
        Item(TabAction::PreviousStatement, empty),
        Separator,
        enabled(TabAction::ToggleResults),
        enabled(TabAction::MaximizeResults),
        enabled(TabAction::SwitchFocus),
    ]
}

/// What the menu of a result tab depends on. The bulk closes leave pinned
/// results alone, as DBeaver's do (`SQLEditor`, result tab menu); Close
/// closes the tab it is on, pinned or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResultFacts {
    pub pinned: bool,
    /// Unpinned results before and after this one.
    pub left: usize,
    pub right: usize,
    /// Pinned results, this one included.
    pub pinned_count: usize,
    /// The result came from a statement (not from a failed connection).
    pub statement: bool,
    /// Its rows stopped arriving before the end: another statement took
    /// the session while it was waiting to be paged.
    pub incomplete: bool,
    /// Its statement writes: running it again would write again.
    pub writes: bool,
}

/// The menu of a result tab.
pub fn result_menu(facts: ResultFacts) -> Vec<Entry> {
    let others = facts.left + facts.right;
    let mut entries = vec![
        enabled(TabAction::Close),
        Item(
            TabAction::CloseOthers,
            (others == 0).then_some("no other unpinned result"),
        ),
        Item(
            TabAction::CloseToLeft,
            (facts.left == 0).then_some("none unpinned to the left"),
        ),
        Item(
            TabAction::CloseToRight,
            (facts.right == 0).then_some("none unpinned to the right"),
        ),
        Item(
            TabAction::CloseAll,
            (others == 0 && facts.pinned).then_some("all pinned"),
        ),
        Separator,
        enabled(if facts.pinned {
            TabAction::Unpin
        } else {
            TabAction::Pin
        }),
        Item(
            TabAction::UnpinAll,
            (facts.pinned_count == 0).then_some("none pinned"),
        ),
        enabled(TabAction::Rename),
        Separator,
        Item(
            TabAction::CopyStatement,
            (!facts.statement).then_some("no statement"),
        ),
    ];
    if facts.incomplete {
        entries.push(Item(
            TabAction::RunAgain,
            facts.writes.then_some("the statement writes"),
        ));
    }
    entries
}

/// An item of the status bar, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusItem {
    /// How many connections are open, without a tab on one.
    Summary,
    Connection,
    Database,
    ReadOnly,
    Transaction,
    Running,
    Pending,
    Rows,
    Timing,
    Messages,
    /// Passwords are kept in memory only.
    MemoryOnly,
    /// A newer Kerno is out (CLAUDE.md §3 item 12).
    Update,
}

impl StatusItem {
    /// Id of its element, for tests.
    pub fn id(self) -> &'static str {
        match self {
            Self::Summary => "status-summary",
            Self::Connection => "status-connection",
            Self::Database => "status-database",
            Self::ReadOnly => "status-read-only",
            Self::Transaction => "status-transaction",
            Self::Running => "status-running",
            Self::Pending => "status-pending",
            Self::Rows => "status-rows",
            Self::Timing => "status-timing",
            Self::Messages => "status-messages",
            Self::MemoryOnly => "status-memory-only",
            Self::Update => "status-update",
        }
    }
}

/// What the status bar menus depend on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusFacts {
    /// The active tab is a query tab (its database and transaction are its own).
    pub query: bool,
    pub connected: bool,
    /// Databases the tab can switch to, and the one it is in.
    pub databases: Vec<String>,
    pub database: Option<String>,
    /// The tab's own transaction.
    pub transaction: TxState,
    pub manual_commit: bool,
    pub running: bool,
    /// How many connections are open (the summary, without a tab on one).
    pub open_connections: usize,
}

/// The actions of a status bar item showing `text`; none when it has none.
/// Switching or closing a session that holds a transaction asks first
/// ([`end_question`]), so those entries stay enabled.
pub fn status_menu(item: StatusItem, text: &str, facts: &StatusFacts) -> Vec<Entry> {
    let not_connected = (!facts.connected).then_some("not connected");
    match item {
        StatusItem::Summary => vec![
            enabled(if facts.open_connections == 0 {
                TabAction::NewConnection
            } else {
                TabAction::DisconnectAll
            }),
            Separator,
            enabled(TabAction::CopyText(text.into())),
        ],
        StatusItem::Messages => vec![
            enabled(TabAction::ShowMessages),
            enabled(TabAction::CopyText(text.into())),
        ],
        StatusItem::Connection => vec![
            Item(TabAction::Disconnect, not_connected),
            Item(TabAction::Reconnect, not_connected),
            Separator,
            Item(TabAction::Administration, not_connected),
            enabled(TabAction::EditConnection),
        ],
        StatusItem::Database if facts.query => facts
            .databases
            .iter()
            .map(|database| {
                let current = facts.database.as_ref() == Some(database);
                Item(
                    TabAction::UseDatabase(database.clone()),
                    if current {
                        Some("current")
                    } else {
                        not_connected
                    },
                )
            })
            .collect(),
        StatusItem::Database => Vec::new(),
        StatusItem::ReadOnly => vec![
            enabled(TabAction::AboutReadOnly),
            Separator,
            enabled(TabAction::EditConnection),
        ],
        StatusItem::Transaction if facts.query => {
            let running = facts.running.then_some("running");
            vec![
                Item(
                    TabAction::Commit,
                    if facts.transaction == TxState::Failed {
                        Some("failed: roll back")
                    } else {
                        running
                    },
                ),
                Item(TabAction::Rollback, running),
                enabled(TabAction::TransactionLog),
                Separator,
                enabled(TabAction::ManualCommit(!facts.manual_commit)),
            ]
        }
        StatusItem::Transaction => Vec::new(),
        StatusItem::Running => vec![enabled(TabAction::Cancel)],
        StatusItem::Pending => vec![
            enabled(TabAction::ReviewChanges),
            enabled(TabAction::DiscardChanges),
        ],
        StatusItem::Rows | StatusItem::Timing => vec![enabled(TabAction::CopyText(text.into()))],
        StatusItem::MemoryOnly => vec![enabled(TabAction::AboutMemoryOnly)],
        StatusItem::Update => vec![
            enabled(TabAction::Update),
            enabled(TabAction::OpenReleasePage),
            Separator,
            enabled(TabAction::SkipVersion),
            enabled(TabAction::TurnOffUpdateChecks),
        ],
    }
}

/// A menu taller than the window scrolls instead of being cut: the kit
/// moves a menu inside the window but does not shorten it.
pub fn fit_window(menu: PopupMenu, window: &Window) -> PopupMenu {
    // The kit keeps 8 px from each edge; the frame and padding take 10.
    menu.scrollable(true)
        .max_h(window.bounds().size.height - px(26.))
}

/// What runs the entry chosen in a menu.
pub type Pick = Rc<dyn Fn(TabAction, &mut Window, &mut App)>;

/// Fills `menu` with `entries`; `pick` runs the chosen one.
pub fn fill_menu(
    mut menu: PopupMenu,
    entries: Vec<Entry>,
    pick: Pick,
    window: &mut Window,
    cx: &mut gpui_kit::Context<PopupMenu>,
) -> PopupMenu {
    for entry in entries {
        menu = match entry {
            Separator => menu.separator(),
            Submenu(label, items) => {
                let pick = pick.clone();
                menu.submenu(label, window, cx, move |menu, window, cx| {
                    fill_menu(menu, items.clone(), pick.clone(), window, cx)
                })
            }
            Item(action, reason) => {
                // An entry that cannot be used says why.
                let label = match reason {
                    Some(reason) => format!("{} ({reason})", action.label()),
                    None => action.label(),
                };
                let pick = pick.clone();
                menu.item(
                    PopupMenuItem::new(SharedString::from(label))
                        .disabled(reason.is_some())
                        .on_click(move |_, window, cx| pick(action.clone(), window, cx)),
                )
            }
        };
    }
    menu
}

/// What closing a tab would lose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loss {
    /// An open transaction, rolled back.
    Transaction,
    /// A transaction a statement failed in: it can only be rolled back.
    FailedTransaction,
    /// Rows with changes not applied.
    Pending(usize),
    /// Text not written to the tab's file.
    Unsaved,
    /// A table form with changes not applied.
    Form,
}

impl Loss {
    fn is_transaction(&self) -> bool {
        matches!(self, Loss::Transaction | Loss::FailedTransaction)
    }

    /// What happens to it, after the tab's title.
    fn what(&self) -> String {
        match self {
            Loss::Transaction => "its open transaction is rolled back".to_string(),
            Loss::FailedTransaction => "its failed transaction is rolled back".to_string(),
            Loss::Pending(count) => format!(
                "{count} pending change{} {} discarded",
                if *count == 1 { "" } else { "s" },
                if *count == 1 { "is" } else { "are" }
            ),
            Loss::Unsaved => "the changes not saved to its file are lost".to_string(),
            Loss::Form => "the changes not applied to the table are lost".to_string(),
        }
    }
}

/// How what a session holds is ended before it closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ending {
    /// Open transactions: committed or rolled back. Commit is refused, with
    /// why, when one of them failed.
    Transactions { commit: Option<&'static str> },
    /// Only pending changes: discarded.
    Discard,
}

/// The question to ask before `action` (“Disconnect “prod””) closes
/// sessions that hold `losses` (tab title, what it holds): title, text and
/// how they may be ended; `None` to go ahead without asking. Unsaved text
/// and table forms survive a closed session and are not asked about.
pub fn end_question(action: &str, losses: &[(String, Loss)]) -> Option<(String, String, Ending)> {
    question(action, losses, false)
}

/// [`end_question`] for quitting Kerno (C15): a table form with changes
/// not applied is closed with the app, so it is asked about too. Unsaved
/// text is not: the stored tabs keep it for the next start.
pub fn quit_question(losses: &[(String, Loss)]) -> Option<(String, String, Ending)> {
    question("Quit Kerno", losses, true)
}

fn question(
    action: &str,
    losses: &[(String, Loss)],
    forms: bool,
) -> Option<(String, String, Ending)> {
    let asked: Vec<&(String, Loss)> = losses
        .iter()
        .filter(|(_, loss)| {
            loss.is_transaction()
                || matches!(loss, Loss::Pending(_))
                || (forms && *loss == Loss::Form)
        })
        .collect();
    if asked.is_empty() {
        return None;
    }
    let transactions = asked.iter().any(|(_, loss)| loss.is_transaction());
    let mut lines: Vec<String> = asked
        .iter()
        .map(|(tab, loss)| match loss {
            Loss::Transaction => format!("“{tab}”: a transaction is open."),
            Loss::FailedTransaction => {
                format!(
                    "“{tab}”: a statement failed in its transaction, which can only be rolled back."
                )
            }
            other => format!("“{tab}”: {}.", other.what()),
        })
        .collect();
    let ending = if transactions {
        let failed = asked
            .iter()
            .any(|(_, loss)| *loss == Loss::FailedTransaction);
        lines.push(
            "End the open transactions first, or cancel to keep everything as it is.".to_string(),
        );
        Ending::Transactions {
            commit: failed.then_some("a transaction failed"),
        }
    } else {
        lines.push("Cancel to keep everything as it is.".to_string());
        Ending::Discard
    };
    Some((format!("{action}?"), lines.join("\n"), ending))
}

/// The question to ask before closing tabs that would lose `losses`
/// (tab title, what it loses), as title, text and confirm button; `None`
/// to close without asking. Without `confirm` (the setting) only an open
/// transaction asks.
pub fn close_question(
    losses: &[(String, Loss)],
    confirm: bool,
) -> Option<(String, String, &'static str)> {
    let asked: Vec<&(String, Loss)> = losses
        .iter()
        .filter(|(_, loss)| confirm || loss.is_transaction())
        .collect();
    let rolls_back = asked.iter().any(|(_, loss)| loss.is_transaction());
    let title = match asked.as_slice() {
        [] => return None,
        [(_, Loss::Transaction | Loss::FailedTransaction)] => "Close and roll back?".to_string(),
        [(_, Loss::Pending(count))] => format!(
            "Discard {count} pending change{}?",
            if *count == 1 { "" } else { "s" }
        ),
        [(_, Loss::Unsaved)] => "Close without saving?".to_string(),
        [(_, Loss::Form)] => "Close without applying?".to_string(),
        _ => "Close and lose what these tabs hold?".to_string(),
    };
    let lines: Vec<String> = asked
        .iter()
        .map(|(tab, loss)| format!("“{tab}”: {}.", loss.what()))
        .collect();
    let ok = if rolls_back {
        "Roll back and close"
    } else {
        "Discard and close"
    };
    Some((title, lines.join("\n"), ok))
}

/// How long a transaction has been open (K4, C15): seconds under a minute,
/// minutes under an hour, then hours and minutes ("1 h 05 min"). Rounded
/// down: a transaction is never shown older than it is.
pub fn age_text(age: std::time::Duration) -> String {
    let seconds = age.as_secs();
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3600 => format!("{} min", seconds / 60),
        _ => format!("{} h {:02} min", seconds / 3600, seconds / 60 % 60),
    }
}

/// What the status bar says of the active tab's transaction, with its age
/// when it is known; nothing without one.
pub fn transaction_status(state: TxState, age: Option<std::time::Duration>) -> Option<String> {
    let text = match state {
        TxState::Idle => return None,
        TxState::Open => "transaction open",
        TxState::Failed => "transaction failed: roll back",
    };
    Some(match age {
        Some(age) => format!("{text} · {}", age_text(age)),
        None => text.to_string(),
    })
}

/// The tooltip of a tab's transaction badge (K4): how long it has been
/// open and how to end it, with the shortcuts of Commit and Roll back when
/// they have one. A failed transaction can only be rolled back.
pub fn transaction_tip(
    state: TxState,
    age: Option<std::time::Duration>,
    commit: Option<&str>,
    rollback: Option<&str>,
) -> String {
    let with_keys = |name: &str, keys: Option<&str>| match keys {
        Some(keys) => format!("{name} ({keys})"),
        None => name.to_string(),
    };
    let open = match age {
        Some(age) => format!("open for {}", age_text(age)),
        None => "open".to_string(),
    };
    let rollback = with_keys("Roll back", rollback);
    match state {
        TxState::Idle => String::new(),
        TxState::Open => format!(
            "Transaction {open}: {} or {rollback}.",
            with_keys("Commit", commit)
        ),
        TxState::Failed => {
            format!("Transaction failed ({open}): a statement of it failed. {rollback}.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Item(action, None) => action.label(),
                Item(action, Some(reason)) => format!("{} ({reason})", action.label()),
                Submenu(label, items) => format!("{label} ▸ [{}]", labels(items).join(", ")),
                Separator => "—".into(),
            })
            .collect()
    }

    #[test]
    fn a_tab_offers_what_dbeaver_does_and_says_what_does_not_apply() {
        assert_eq!(
            labels(&tab_menu(TabKind::Query { file: false }, 0, 2)),
            [
                "Close",
                "Close others",
                "Close tabs to the right",
                "Close all",
                "—",
                "Rename…",
                "Duplicate",
            ]
        );
        // The last and only tab; a table's data.
        assert_eq!(
            labels(&tab_menu(TabKind::Other, 0, 1)),
            [
                "Close",
                "Close others (no other tab)",
                "Close tabs to the right (none to the right)",
                "Close all",
                "—",
                "Rename… (query tabs only)",
                "Duplicate (query tabs only)",
            ]
        );
        // A file: named after it, and it can be found on disk.
        let file = labels(&tab_menu(TabKind::Query { file: true }, 1, 3));
        assert_eq!(file[5], "Rename… (named after its file)");
        assert_eq!(
            file[7..],
            [
                "—".to_string(),
                TabAction::CopyPath.label(),
                TabAction::Reveal.label()
            ]
        );
    }

    #[test]
    fn the_empty_tab_bar_reopens_only_what_was_closed() {
        assert_eq!(
            labels(&bar_menu(0, 0)),
            [
                "New query tab",
                "Reopen closed tab (none closed)",
                "—",
                "Close all (no tab open)",
            ]
        );
        assert_eq!(
            labels(&bar_menu(2, 1)),
            ["New query tab", "Reopen closed tab", "—", "Close all"]
        );
    }

    #[test]
    fn the_editor_menu_is_enabled_as_the_toolbar_is() {
        let ready = EditorFacts {
            connected: true,
            selection: true,
            ..EditorFacts::default()
        };
        let choices = Choices {
            connections: vec![
                ("a".into(), "local".into(), true),
                ("b".into(), "prod".into(), false),
            ],
            connection: Some("a".into()),
            databases: vec!["kerno".into(), "other".into()],
            database: Some("kerno".into()),
            schemas: vec!["public".into(), "shop".into()],
            schema: None,
        };
        assert_eq!(
            labels(&editor_menu(ready, &choices)),
            [
                "Run statement",
                "Run script",
                "Explain plan",
                "Execute ▸ [Run statement in new result tab, Run script from the cursor, \
                 Run script on connections…, Explain analyze, —, Count rows of statement, Fetch all rows (no rows left), \
                 Export from query…, —, Active connection ▸ [local (current), prod (not connected)], \
                 Active database ▸ [kerno (current), other], Active schema ▸ [public, shop]]",
                "—",
                "Go to next statement",
                "Go to previous statement",
                "—",
                "Toggle results panel",
                "Maximize results panel",
                "Focus results / editor",
                "—",
                "Undo",
                "Redo",
                "—",
                "Cut",
                "Copy",
                "Copy statement",
                "Paste",
                "Select all",
                "Select all occurrences",
                "—",
                "Format ▸ [Format SQL, Upper case, Lower case, —, Toggle line comment, \
                 Toggle block comment, —, Toggle word wrap, Unfold all]",
                "Find…",
                "Replace…",
                "—",
                "File ▸ [Save, Save as…, Save as query…, —, Rename…, \
                 Revert to saved file (no file), Open SQL file…]",
            ]
        );
        // No connection, no selection, nothing written.
        let idle = labels(&editor_menu(
            EditorFacts {
                empty: true,
                ..EditorFacts::default()
            },
            &Choices::default(),
        ));
        assert_eq!(idle[0], "Run statement (not connected)");
        assert_eq!(idle[2], "Explain plan (not connected)");
        assert!(
            idle[3].contains("Count rows of statement (not connected)")
                && idle[3].ends_with("Export from query… (not connected)]"),
            "{}",
            idle[3]
        );
        // Nothing to run on, no list: MySQL has no schemas.
        let mysql = Choices {
            schemas: Vec::new(),
            ..choices.clone()
        };
        let execute = labels(&editor_menu(ready, &mysql))[3].clone();
        assert!(
            execute.contains("Active database ▸") && !execute.contains("Active schema"),
            "{execute}"
        );
        assert_eq!(idle[5], "Go to next statement (empty)");
        assert_eq!(idle[8], "Toggle results panel");
        assert_eq!(idle[15], "Cut (no selection)");
        assert_eq!(idle[17], "Copy statement (empty)");
        assert_eq!(idle[20], "Select all occurrences (empty)");
        assert!(
            idle[22].contains("Upper case (no selection)"),
            "{}",
            idle[22]
        );
        assert!(
            idle[26].contains("Save (empty), Save as… (empty), Save as query… (empty)"),
            "{}",
            idle[26]
        );
        // While a statement runs; a file saved as it is; source mode.
        let busy = labels(&editor_menu(
            EditorFacts {
                running: true,
                saved_file: true,
                file: true,
                more_rows: true,
                source: true,
                ..ready
            },
            &choices,
        ));
        assert_eq!(
            busy[..3],
            ["Apply source… (running)", "—", "Run statement (running)"]
        );
        assert!(busy[5].contains("Fetch all rows,"), "{}", busy[5]);
        assert!(
            busy[28].contains("Save (saved)")
                && busy[28].contains("Rename… (named after its file)")
                && busy[28].contains("Revert to saved file (saved)"),
            "{}",
            busy[28]
        );
        // The toolbar's Run dropdown offers the same commands.
        assert_eq!(
            labels(&run_menu(ready)),
            [
                "Run statement",
                "Run statement in new result tab",
                "Run script from the cursor",
                "Run script",
                "Count rows of statement",
                "Run script on connections…",
                "—",
                "Go to next statement",
                "Go to previous statement",
                "—",
                "Toggle results panel",
                "Maximize results panel",
                "Focus results / editor",
            ]
        );
        assert_eq!(
            labels(&run_menu(EditorFacts::default()))[4],
            "Count rows of statement (not connected)"
        );
    }

    #[test]
    fn a_result_tab_closes_pins_and_names_as_in_dbeaver() {
        // The middle one of three unpinned results.
        let middle = ResultFacts {
            left: 1,
            right: 1,
            statement: true,
            ..ResultFacts::default()
        };
        assert_eq!(
            labels(&result_menu(middle)),
            [
                "Close",
                "Close others",
                "Close tabs to the left",
                "Close tabs to the right",
                "Close all",
                "—",
                "Pin tab",
                "Unpin all tabs (none pinned)",
                "Rename…",
                "—",
                "Copy statement",
            ]
        );
        // The only result, pinned: the bulk closes have nothing to close.
        let alone = labels(&result_menu(ResultFacts {
            pinned: true,
            pinned_count: 1,
            statement: true,
            ..ResultFacts::default()
        }));
        assert_eq!(
            alone[..7],
            [
                "Close",
                "Close others (no other unpinned result)",
                "Close tabs to the left (none unpinned to the left)",
                "Close tabs to the right (none unpinned to the right)",
                "Close all (all pinned)",
                "—",
                "Unpin tab",
            ]
        );
        assert_eq!(alone[7], "Unpin all tabs");
        // An unpinned result alone still closes with Close all; a refusal
        // before anything ran has no statement to copy.
        let refused = labels(&result_menu(ResultFacts::default()));
        assert_eq!(refused[4], "Close all");
        assert_eq!(refused[10], "Copy statement (no statement)");
        assert_eq!(refused.len(), 11);
        // A result whose rows stopped coming can be run again.
        let incomplete = labels(&result_menu(ResultFacts {
            incomplete: true,
            ..middle
        }));
        assert_eq!(incomplete[10..], ["Copy statement", "Run again"]);
        // An INSERT … RETURNING would write a second time.
        let writes = labels(&result_menu(ResultFacts {
            incomplete: true,
            writes: true,
            ..middle
        }));
        assert_eq!(writes[11], "Run again (the statement writes)");
    }

    #[test]
    fn each_status_item_has_its_own_actions_or_none() {
        let facts = StatusFacts {
            query: true,
            connected: true,
            databases: vec!["kerno".into(), "other".into()],
            database: Some("kerno".into()),
            manual_commit: true,
            ..StatusFacts::default()
        };
        let menu = |item, facts: &StatusFacts| labels(&status_menu(item, "12 rows", facts));
        assert_eq!(
            menu(StatusItem::Connection, &facts),
            [
                "Disconnect",
                "Reconnect",
                "—",
                "Administration",
                "Edit connection…"
            ]
        );
        assert_eq!(
            menu(StatusItem::Database, &facts),
            ["kerno (current)", "other"]
        );
        assert_eq!(
            menu(StatusItem::ReadOnly, &facts),
            ["What read-only means…", "—", "Edit connection…"]
        );
        assert_eq!(menu(StatusItem::Rows, &facts), ["Copy"]);
        assert_eq!(
            status_menu(StatusItem::Timing, "4 ms", &facts),
            [Item(TabAction::CopyText("4 ms".into()), None)]
        );
        assert_eq!(
            menu(StatusItem::Running, &facts),
            ["Cancel running statement"]
        );
        assert_eq!(
            menu(StatusItem::Pending, &facts),
            ["Review SQL…", "Discard changes"]
        );
        assert_eq!(menu(StatusItem::MemoryOnly, &facts), ["What this means…"]);
        assert_eq!(
            menu(StatusItem::Update, &facts),
            [
                "Update",
                "Open release page",
                "—",
                "Skip this version",
                "Turn off update checks"
            ]
        );
        assert_eq!(
            menu(StatusItem::Summary, &facts),
            ["New connection…", "—", "Copy"]
        );
        let two_open = StatusFacts {
            open_connections: 2,
            ..StatusFacts::default()
        };
        assert_eq!(
            menu(StatusItem::Summary, &two_open),
            ["Disconnect all", "—", "Copy"]
        );
        assert_eq!(
            menu(StatusItem::Messages, &facts),
            ["Show messages", "Copy"]
        );

        // An open transaction: ended from here; switching or closing the
        // session asks first (`end_question`), so they stay offered.
        let open = StatusFacts {
            transaction: TxState::Open,
            ..facts.clone()
        };
        assert_eq!(
            menu(StatusItem::Transaction, &open),
            [
                "Commit transaction",
                "Roll back transaction",
                "Transaction log…",
                "—",
                "Switch to auto-commit"
            ]
        );
        assert_eq!(
            menu(StatusItem::Connection, &open)[..2],
            ["Disconnect", "Reconnect"]
        );
        assert_eq!(menu(StatusItem::Database, &open)[1], "other");
        let failed = StatusFacts {
            transaction: TxState::Failed,
            ..facts.clone()
        };
        assert_eq!(
            menu(StatusItem::Transaction, &failed)[0],
            "Commit transaction (failed: roll back)"
        );
        // A table's data: its database is fixed.
        let data = StatusFacts {
            query: false,
            ..facts
        };
        assert!(menu(StatusItem::Database, &data).is_empty());
        // A server is administered while it is connected.
        assert_eq!(
            menu(StatusItem::Connection, &StatusFacts::default())[3],
            "Administration (not connected)"
        );
    }

    #[test]
    fn closing_asks_once_for_what_would_be_lost() {
        let tab = |title: &str, loss| (title.to_string(), loss);
        assert_eq!(close_question(&[], true), None);
        assert_eq!(
            close_question(&[tab("Query 1", Loss::Transaction)], true),
            Some((
                "Close and roll back?".into(),
                "“Query 1”: its open transaction is rolled back.".into(),
                "Roll back and close"
            ))
        );
        assert_eq!(
            close_question(&[tab("people", Loss::Pending(2))], true),
            Some((
                "Discard 2 pending changes?".into(),
                "“people”: 2 pending changes are discarded.".into(),
                "Discard and close"
            ))
        );
        let (title, _, _) = close_question(&[tab("a.sql", Loss::Unsaved)], true).unwrap();
        assert_eq!(title, "Close without saving?");
        let (title, _, _) = close_question(&[tab("t · edit", Loss::Form)], true).unwrap();
        assert_eq!(title, "Close without applying?");
        // Several tabs: one question listing each.
        let several = [
            tab("people", Loss::Pending(1)),
            tab("Query 2", Loss::Transaction),
        ];
        assert_eq!(
            close_question(&several, true),
            Some((
                "Close and lose what these tabs hold?".into(),
                "“people”: 1 pending change is discarded.\n\
                 “Query 2”: its open transaction is rolled back."
                    .into(),
                "Roll back and close"
            ))
        );
        // The setting off: only a transaction still asks.
        assert_eq!(
            close_question(&[tab("people", Loss::Pending(1))], false),
            None
        );
        assert_eq!(
            close_question(&several, false).map(|(title, _, _)| title),
            Some("Close and roll back?".into())
        );
    }

    #[test]
    fn ending_a_session_asks_once_for_its_transactions_and_pending_changes() {
        let tab = |title: &str, loss| (title.to_string(), loss);
        // Text and forms survive a closed session: nothing to ask.
        assert_eq!(end_question("Disconnect “pg”", &[]), None);
        assert_eq!(
            end_question(
                "Disconnect “pg”",
                &[tab("a.sql", Loss::Unsaved), tab("t · edit", Loss::Form)]
            ),
            None
        );
        assert_eq!(
            end_question("Disconnect “pg”", &[tab("Query 1", Loss::Transaction)]),
            Some((
                "Disconnect “pg”?".into(),
                "“Query 1”: a transaction is open.\n\
                 End the open transactions first, or cancel to keep everything as it is."
                    .into(),
                Ending::Transactions { commit: None }
            ))
        );
        // A failed transaction can only be rolled back; pending changes
        // are listed with the transactions.
        let (_, text, ending) = end_question(
            "Reconnect “pg”",
            &[
                tab("Query 1", Loss::FailedTransaction),
                tab("people", Loss::Pending(2)),
            ],
        )
        .unwrap();
        assert_eq!(
            text,
            "“Query 1”: a statement failed in its transaction, which can only be rolled back.\n\
             “people”: 2 pending changes are discarded.\n\
             End the open transactions first, or cancel to keep everything as it is."
        );
        assert_eq!(
            ending,
            Ending::Transactions {
                commit: Some("a transaction failed")
            }
        );
        // C15: quitting closes the table forms too, so it asks about them;
        // unsaved text is kept by the stored tabs.
        assert_eq!(
            quit_question(&[tab("a.sql", Loss::Unsaved), tab("t · edit", Loss::Form)]),
            Some((
                "Quit Kerno?".into(),
                "“t · edit”: the changes not applied to the table are lost.\n\
                 Cancel to keep everything as it is."
                    .into(),
                Ending::Discard
            ))
        );
        assert_eq!(quit_question(&[tab("a.sql", Loss::Unsaved)]), None);
        // Only pending changes: discarded, or nothing happens.
        assert_eq!(
            end_question("Disconnect all", &[tab("people", Loss::Pending(1))]),
            Some((
                "Disconnect all?".into(),
                "“people”: 1 pending change is discarded.\n\
                 Cancel to keep everything as it is."
                    .into(),
                Ending::Discard
            ))
        );
        // Closing a tab whose transaction failed rolls it back too.
        assert_eq!(
            close_question(&[tab("Query 1", Loss::FailedTransaction)], false),
            Some((
                "Close and roll back?".into(),
                "“Query 1”: its failed transaction is rolled back.".into(),
                "Roll back and close"
            ))
        );
    }

    #[test]
    fn snapshots_are_tagged_by_kind() {
        let snapshot = TabSnapshot::Query {
            title: "Query 1".into(),
            sql: "select 'unsaved'\n".into(),
            target: None,
            file: None,
            unsaved: false,
            source: None,
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"query","title":"Query 1","sql":"select 'unsaved'\n"}"#
        );
        assert_eq!(
            serde_json::from_str::<TabSnapshot>(&json).unwrap(),
            snapshot
        );
        // A tab remembers where it runs; tabs stored before that still load.
        let bound = TabSnapshot::Query {
            title: "Query 2".into(),
            sql: String::new(),
            target: Some(Target {
                connection: "c1".into(),
                database: Some("kerno".into()),
                schema: None,
            }),
            file: None,
            unsaved: false,
            source: None,
        };
        let json = serde_json::to_string(&bound).unwrap();
        assert!(
            json.ends_with(r#""target":{"connection":"c1","database":"kerno"}}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<TabSnapshot>(&json).unwrap(), bound);
        // A tab in source mode remembers the schema of its object.
        let source = TabSnapshot::Query {
            title: "v".into(),
            sql: String::new(),
            target: None,
            file: None,
            unsaved: false,
            source: Some("shop".into()),
        };
        let json = serde_json::to_string(&source).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"query","title":"v","sql":"","source":"shop"}"#
        );
        assert_eq!(serde_json::from_str::<TabSnapshot>(&json).unwrap(), source);
        // A table's data: where it is, and whether it is a view.
        let data = TabSnapshot::Data {
            target: Target {
                connection: "c1".into(),
                database: None,
                schema: None,
            },
            schema: "shop".into(),
            table: "orders".into(),
            view: false,
        };
        let json = serde_json::to_string(&data).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"data","target":{"connection":"c1"},"schema":"shop","table":"orders"}"#
        );
        assert_eq!(serde_json::from_str::<TabSnapshot>(&json).unwrap(), data);
        // A table's structure, the same way.
        let structure = TabSnapshot::Structure {
            target: Target {
                connection: "c1".into(),
                database: None,
                schema: None,
            },
            schema: "shop".into(),
            table: "product_sales".into(),
            view: true,
            materialized: true,
        };
        let json = serde_json::to_string(&structure).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"structure","target":{"connection":"c1"},"schema":"shop","table":"product_sales","view":true,"materialized":true}"#
        );
        assert_eq!(
            serde_json::from_str::<TabSnapshot>(&json).unwrap(),
            structure
        );
        // A server's administration, the same way.
        let admin = TabSnapshot::Admin {
            target: Target {
                connection: "c1".into(),
                database: Some("kerno".into()),
                schema: None,
            },
        };
        let json = serde_json::to_string(&admin).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"admin","target":{"connection":"c1","database":"kerno"}}"#
        );
        assert_eq!(serde_json::from_str::<TabSnapshot>(&json).unwrap(), admin);
        // An ER diagram of a schema, and of a table and its neighbours.
        let diagram = TabSnapshot::Diagram {
            target: Target {
                connection: "c1".into(),
                database: Some("kerno".into()),
                schema: None,
            },
            schema: "shop".into(),
            table: None,
        };
        let json = serde_json::to_string(&diagram).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"diagram","target":{"connection":"c1","database":"kerno"},"schema":"shop"}"#
        );
        assert_eq!(serde_json::from_str::<TabSnapshot>(&json).unwrap(), diagram);
        let around = TabSnapshot::Diagram {
            target: Target {
                connection: "c1".into(),
                database: None,
                schema: None,
            },
            schema: "shop".into(),
            table: Some("orders".into()),
        };
        let json = serde_json::to_string(&around).unwrap();
        assert_eq!(serde_json::from_str::<TabSnapshot>(&json).unwrap(), around);
        // A kind this version does not know is rejected, and the storage
        // layer then skips that tab instead of failing to start.
        assert!(serde_json::from_str::<TabSnapshot>(r#"{"kind":"from_the_future"}"#).is_err());
    }

    /// C15: the age of a transaction, rounded down, in one wording for the
    /// tab's tooltip and the status bar.
    #[test]
    fn a_transaction_age_reads_in_seconds_minutes_then_hours() {
        use std::time::Duration;
        let age = |seconds| age_text(Duration::from_secs(seconds));
        assert_eq!(age(0), "0 s");
        assert_eq!(age(59), "59 s");
        assert_eq!(age(60), "1 min");
        assert_eq!(age(4 * 60 + 59), "4 min");
        assert_eq!(age(3599), "59 min");
        assert_eq!(age(3600), "1 h 00 min");
        assert_eq!(age(3600 + 5 * 60 + 30), "1 h 05 min");
        assert_eq!(age(26 * 3600 + 59 * 60), "26 h 59 min");
        assert_eq!(age_text(Duration::from_millis(999)), "0 s");
    }

    /// C15: the status bar and the badge's tooltip name the state, the age and
    /// the real shortcuts; nothing without a transaction.
    #[test]
    fn a_transaction_says_its_age_and_how_to_end_it() {
        use std::time::Duration;
        let four = Some(Duration::from_secs(4 * 60 + 2));
        assert_eq!(transaction_status(TxState::Idle, four), None);
        assert_eq!(
            transaction_status(TxState::Open, four).as_deref(),
            Some("transaction open · 4 min")
        );
        assert_eq!(
            transaction_status(TxState::Failed, Some(Duration::from_secs(12))).as_deref(),
            Some("transaction failed: roll back · 12 s")
        );
        assert_eq!(
            transaction_status(TxState::Open, None).as_deref(),
            Some("transaction open")
        );
        assert_eq!(
            transaction_tip(TxState::Open, four, Some("⌃4"), Some("⌃8")),
            "Transaction open for 4 min: Commit (⌃4) or Roll back (⌃8)."
        );
        // A shortcut taken out of the keymap is not invented.
        assert_eq!(
            transaction_tip(TxState::Open, None, None, Some("⌃8")),
            "Transaction open: Commit or Roll back (⌃8)."
        );
        assert_eq!(
            transaction_tip(TxState::Failed, four, Some("⌃4"), Some("⌃8")),
            "Transaction failed (open for 4 min): a statement of it failed. Roll back (⌃8)."
        );
        assert_eq!(transaction_tip(TxState::Idle, four, None, None), "");
    }
}
