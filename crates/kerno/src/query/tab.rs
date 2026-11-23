//! A query tab: a SQL editor bound to a connection, and what its
//! statements returned.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Glyph;
use gpui_kit::base::input;
use gpui_kit::base::input::Diagnostic;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    CompletionProvider, Editor, EditorState, Input, InputEvent, InputState, Position, RopeExt as _,
    TabSize,
};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, DragMoveEvent, EmptyView, Entity,
    EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement, KeyDownEvent,
    ListAlignment, ListState, MouseButton, ParentElement as _, Pixels, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div, list, px,
};
use kerno_core::browse::{
    Filter, Operator, Query, Source as QuerySource, and_where, count_from, described, distinct_sql,
    one_of_sql, select_from,
};
use kerno_core::editable::{self, Editable, ReadOnly, Source as EditSource};
use kerno_core::environment;
use kerno_core::schema::{RelationKind, TableRef, TableStructure};
use kerno_core::sql::{
    ApplyFailure, CellValue, RowChange, RowSql, StatementClass, StatementKind, TxCommand,
    apply_edits, apply_edits_in_transaction, classify,
};
use kerno_core::sql::{
    LexRules, Param, ParamKey, ParamValue, bind_params, change_case, find_params, format_cleanly,
    format_with, matching_bracket, split_statements, statement_at, toggle_block_comment,
};
use kerno_core::storage::{HistoryEntry, HistoryStatus};
use kerno_core::{
    DbError, Described, Dialect, Error, ExecEvent, IsolationLevel, Notice, Plan, ResultStore,
    RowLimit, Session, TxState, describe, exec,
};

use super::comment;
use super::completion::{self, SharedCatalog, SqlCompletion};
use super::distinct;
use super::edit;
use super::find;
use super::grid::{CopyFormat, EditTarget, Grid, GridAction, Owner, table_size};
use super::occurrences::Occurrences;
use super::params;
use super::plan_view;
use super::run_marks::{self, Mark, Trace};
use super::tx_log::{self, TxLogEntry};
use super::value_panel::{self, ValuePanel};
use crate::actions::{
    self, CountStatementRows, ExplainAnalyze, ExplainStatement, FindInEditor, FormatSql,
    MaximizeResultsPanel, NextStatement, PreviousStatement, ReplaceInEditor, RunAll, RunFromCursor,
    RunStatement, RunStatementInNewTab, SaveFile, SaveFileAs, SaveQuery, ShowHistory,
    ShowSavedQueries, SwitchResultsFocus, ToggleComment, ToggleResultsPanel,
};
use crate::bridge;
use crate::connections::manager::{Connections, ConnectionsEvent, Status};
use crate::data::tab::{
    AutoRefresh, FILTER_MIN_WIDTH, auto_refresh_button, history_button, related,
};
use crate::engine_logo::engine_logo;
use crate::export_dialog::{self, ExportDialog, Source};
use crate::multi_run::{ScriptRun, Stage};
use crate::sql_confirm::{self, Confirmation, Statement as ConfirmStatement};
use crate::state::AppState;
use crate::tabs::{self, Choices, EditorFacts, ResultFacts, TabAction, TabSnapshot, Target};

/// A statement to run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Statement {
    sql: String,
    /// The bytes of the editor's text the statement was read from. With
    /// parameters bound, `sql` is no longer that text: the server's error
    /// positions are then not marked (`mark_error` compares), the margin's
    /// marks still are (K37).
    place: Option<std::ops::Range<usize>>,
    /// Why it is not sent: a write on a read-only target of a run on
    /// several connections (J14), shown in its place.
    refused: Option<Failure>,
}

/// How a statement ended.
struct Ended {
    started_at_ms: i64,
    elapsed: Duration,
    status: HistoryStatus,
    failure: Option<Failure>,
    row_count: Option<u64>,
}

/// Line and column of a byte offset, as the editor counts them.
fn position_of(text: &str, offset: usize) -> Position {
    let before = &text[..offset.min(text.len())];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    Position {
        line: before.matches('\n').count() as u32,
        character: before[line_start..].encode_utf16().count() as u32,
    }
}

/// Which statements a run takes from the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The selection if there is one, otherwise the statement the cursor
    /// is in.
    AtCursor,
    /// The selection if there is one, otherwise the whole text.
    All,
    /// The statement the cursor is in and every one after it (DBeaver's
    /// `run.scriptFromPosition`); the selection does not count.
    FromCursor,
}

/// What the user asked for while the tab's connection was closed: done
/// once it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Request {
    Launch {
        scope: Scope,
        explain: Option<bool>,
        keep: bool,
    },
    ApplySource,
    CountRows,
}

/// Which parts of the tab are on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panels {
    Both,
    /// The results are hidden.
    Editor,
    /// The editor is hidden.
    Results,
}

/// Why a statement failed, as the server said it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub code: Option<String>,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// 1-based character position in the statement, when the server gave one.
    pub position: Option<u32>,
}

impl From<&Error> for Failure {
    fn from(error: &Error) -> Self {
        match error {
            Error::Db(db) => Self {
                message: db.message.clone(),
                code: db.code.clone(),
                detail: db.detail.clone(),
                hint: db.hint.clone(),
                position: db.position,
            },
            other => Self {
                message: other.to_string(),
                code: None,
                detail: None,
                hint: None,
                position: None,
            },
        }
    }
}

impl Failure {
    /// The failure as every error surface shows it (C14): the message with
    /// its code, then the detail and the hint.
    pub fn described(&self) -> Described {
        let message = match &self.code {
            Some(code) => format!("{} [{code}]", self.message),
            None => self.message.clone(),
        };
        Described::new(message, self.detail.as_deref(), self.hint.as_deref())
    }
}

/// How a COMMIT or ROLLBACK ended.
enum Ending {
    Done,
    /// The session closed under a ROLLBACK: its transaction is rolled back.
    Closed,
    /// The server had closed the session: the COMMIT was not sent (C15).
    Gone(DbError),
    /// What the failure says.
    Failed(String),
}

/// What a COMMIT or ROLLBACK that failed says (C14, C15): first what came
/// of the transaction, from the session's state after the failure (`after`),
/// then the server's message, detail and hint.
pub fn ending_failed(commit: bool, error: &Error, after: TxState) -> String {
    let outcome = match (commit, error, after) {
        // The server may have saved it before the connection went.
        (true, Error::Closed(_), _) => {
            "The connection closed during the commit: whether the server saved the transaction \
             is not known. Check before you run it again."
        }
        (true, _, TxState::Idle) => {
            "The commit failed: the transaction was rolled back by the server; nothing it \
             changed was saved."
        }
        (true, _, TxState::Open | TxState::Failed) => {
            "The commit failed; the transaction is still open."
        }
        (false, _, TxState::Idle) => {
            "The rollback failed, but the transaction is no longer open: nothing it changed was \
             saved."
        }
        (false, _, TxState::Open | TxState::Failed) => {
            "The rollback failed; the transaction is still open."
        }
    };
    format!("{outcome}\n{}", describe(error))
}

/// What a statement cut off by a lost connection says: it is never run
/// again by Kerno (a write could happen twice).
pub const LOST_WHILE_RUNNING: &str = "The connection closed while this statement ran. Kerno does \
                                      not run it again: whether the server applied it is not \
                                      known, so check before you run it again.";

/// The failure of a statement cut off by a lost connection, the driver's
/// words as the detail.
fn lost_failure(error: Option<&Error>) -> Failure {
    Failure {
        message: LOST_WHILE_RUNNING.into(),
        code: None,
        detail: error.map(|error| error.to_string()),
        hint: Some(
            "The next statement opens a new session, and the connection again if it was lost."
                .into(),
        ),
        position: None,
    }
}

/// How often a tab holding a transaction asks whether the server closed
/// its session (C15). Never on that session: a command would restart the
/// server's idle timers, the guard a DBA sets against forgotten
/// transactions. Free on PostgreSQL (its client reads the server's FATAL as
/// it comes), one short connection of its own on MySQL.
pub(crate) const SESSION_CHECK: Duration = Duration::from_secs(15);

/// How long a Commit waits to learn whether the server closed the session
/// before it is sent anyway.
const GONE_CHECK: Duration = Duration::from_secs(3);

/// What a tab says when the server closed its session and took the
/// transaction with it (C15).
const SESSION_CLOSED: &str = "The server closed this tab's session while its transaction was \
                              open: the server rolled it back, and nothing of it was committed. \
                              The next statement opens a new session.";

/// The same, found by a statement of the tab: the session closed under it,
/// by the server or with the connection.
const SESSION_LOST: &str = "This tab's session closed while its transaction was open: the server \
                            rolled it back, and nothing of it was committed.";

/// What the connection's loss says (its own session did not answer).
const CONNECTION_LOST: &str = "The connection closed while this tab's transaction was open: the \
                               server rolled it back, and nothing of it was committed.";

/// Why the server closed a session: plain words first, when its code says
/// it, then the server's own words (C14).
pub fn closed_why(why: &DbError) -> String {
    let plain = match why.code.as_deref() {
        Some("25P03") => Some(
            "It stayed idle inside a transaction for longer than the server allows \
             (idle_in_transaction_session_timeout).",
        ),
        Some("25P04") => {
            Some("Its transaction lasted longer than the server allows (transaction_timeout).")
        }
        Some("57P05") => {
            Some("It stayed idle for longer than the server allows (idle_session_timeout).")
        }
        Some("57P01") => Some(
            "An administrator ended it (pg_terminate_backend), or the server is shutting down.",
        ),
        _ => None,
    };
    match plain {
        Some(plain) => format!("{plain}\nThe server said: {why}"),
        None => format!("The server said: {why}"),
    }
}

/// What a statement produced.
pub enum Outcome {
    /// Rows, in a grid.
    Rows(Entity<TableState<Grid>>),
    /// The plan of the statement (EXPLAIN).
    Plan {
        /// The plan as a tree; `None` when the output could not be read.
        plan: Option<Plan>,
        /// What the server returned, as it is.
        raw: String,
        /// Why there is no tree, when there is none.
        problem: Option<String>,
        /// The raw output is shown instead of the tree.
        show_raw: bool,
    },
    /// It ran and returned no rows.
    Done,
    /// The server refused it.
    Failed(Failure),
    /// It was cancelled.
    Cancelled,
}

/// One statement of a run and what came of it.
pub struct ResultPane {
    /// Stays the same while the pane lives: what a run, a menu and the
    /// result tab bar refer to, whatever closes around it.
    id: u64,
    pub statement: String,
    pub outcome: Outcome,
    /// Count the server reported: rows changed, or rows returned.
    pub rows_affected: Option<u64>,
    /// From sending the statement to its end (or to now, while it runs).
    pub elapsed: Option<Duration>,
    /// The name given to its tab.
    name: Option<String>,
    /// Kept by the next runs until unpinned or closed.
    pinned: bool,
    /// The height its grid was dragged to among stacked results (J11).
    stack_height: Option<Pixels>,
    /// How the result of a SELECT is read again on the server.
    pub requery: Option<Requery>,
    /// Rows of the whole result, once counted.
    pub total: Option<u64>,
    /// Whether its rows take changes as rows of one table (RES-51): `None`
    /// while that is being found out, and for rows that are no result.
    editable: Option<Result<(), ReadOnly>>,
    /// Why the last apply of its changes changed nothing.
    apply_error: Option<String>,
    /// The connection and database it was read on: its changes are
    /// applied there or nowhere.
    place: Option<(String, Option<String>)>,
    /// The bytes of the editor's text a statement the server refused was read from, for "Fix
    /// with AI" (K52); `None` for other outcomes, and when it did not come from the editor.
    source: Option<std::ops::Range<usize>>,
    /// The grid's selection and double click; dropped with the pane.
    _grid_events: Option<Subscription>,
    /// Finds out whether its rows take changes; dropped with the pane.
    _lookup: Option<Task<()>>,
}

/// What the result of a SELECT is read again with: its own statement,
/// filtered and ordered by the server, as DBeaver's filter panel over an
/// editor's result (`ResultSetFilterPanel`, `getWrappedFilterQuery`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Requery {
    /// The statement as it ran, its parameters written in.
    pub statement: String,
    /// The body of the `WHERE`, as typed.
    pub filter: String,
    /// The column the server orders on, and whether descending.
    pub sort: Option<(String, bool)>,
    /// Auto-refresh reads this result again.
    pub auto: bool,
    /// The server ignored the statement's own `ORDER BY` (MySQL and
    /// MariaDB in a derived table without a `LIMIT`).
    pub order_dropped: bool,
    /// The table columns the result of the statement itself showed:
    /// read again inside a derived table, MariaDB names the derived table
    /// instead.
    pub origins: Option<Arc<[Option<kerno_core::ColumnOrigin>]>>,
}

/// A count or a list of values read for the result on screen.
struct SideWork {
    /// The session it runs on once open: the tab's, or one of its own
    /// while the tab's holds a result being paged.
    session: Option<Arc<dyn Session>>,
    _task: Task<()>,
}

impl ResultPane {
    /// The grid of the pane, when it holds rows.
    fn grid(&self) -> Option<Entity<TableState<Grid>>> {
        match &self.outcome {
            Outcome::Rows(grid) => Some(grid.clone()),
            _ => None,
        }
    }
}

/// What the status bar shows of the active tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabStatus {
    pub connection: Option<String>,
    pub read_only: bool,
    pub database: Option<String>,
    pub transaction: TxState,
    pub running: bool,
    /// Rows of the result on screen.
    pub rows: Option<usize>,
    /// Duration of the statement whose result is on screen.
    pub elapsed: Option<Duration>,
    /// Messages the server sent during the last run.
    pub notices: usize,
    /// Rows with changes waiting to be applied (a table's data).
    pub pending: usize,
}

/// Why the changes of a result are not applied after the tab moved.
const MOVED_AWAY: &str = "The tab now runs on another connection or database than these rows were \
                          read on: discard their changes, or switch back.";

/// Why a statement that writes was not sent on the read-only connection
/// named `connection`.
fn read_only_failure(connection: &str) -> Failure {
    Failure {
        message: format!(
            "“{connection}” is a read-only connection: this statement would change the database and was not run."
        ),
        code: None,
        detail: None,
        hint: Some("Untick “Read-only” in the connection's settings to allow changes.".into()),
        position: None,
    }
}

/// A statement on one line, cut short: enough to tell which one it is.
fn one_line(statement: &str) -> String {
    let mut line = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 120 {
        line = line.chars().take(119).collect::<String>() + "…";
    }
    line
}

/// `12 ms` or `1.25 s`.
pub fn duration_text(elapsed: Duration) -> String {
    let millis = elapsed.as_secs_f64() * 1000.;
    if millis < 1000. {
        format!("{millis:.0} ms")
    } else {
        format!("{:.2} s", millis / 1000.)
    }
}

/// What the tab is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Idle,
    /// Waiting for the server: statement `index` of `total`.
    Running {
        index: usize,
        total: usize,
        since: Instant,
    },
}

/// How often a tab is drawn again while a statement runs: its time in the
/// status bar goes up, its mark in the margin breathes (K37).
const RUN_TICK: Duration = Duration::from_millis(250);

/// Rows a stacked result shows before its grid scrolls (J11).
const STACK_ROWS: usize = 12;
/// The height of a stacked result's header.
const STACK_HEADER: Pixels = px(28.);

/// The lower edge of a stacked result's grid being dragged: its pane.
struct StackResize(u64);

/// The bar of an unbound query tab (C6).
pub const UNBOUND_PICK: &str = "Pick a connection to run this script.";
/// The bar of a query tab whose connection was deleted (C6).
pub const UNBOUND_DELETED: &str =
    "Its connection was deleted: pick another one to run this script.";

pub struct QueryTab {
    title: SharedString,
    editor: Entity<EditorState>,
    connections: Entity<Connections>,
    /// The connection, database and schema statements run on; `None`: the
    /// tab is unbound and runs nothing until the user picks one (C6).
    target: Option<Target>,
    /// Unbound: the connection the pickers list first (a script's last one,
    /// or the one being browsed when it opened).
    proposed: Option<String>,
    /// Unbound: why, in the bar above the editor; the usual words if `None`.
    unbound_why: Option<&'static str>,
    /// This tab's own session, opened at the first run.
    session: Option<Arc<dyn Session>>,
    /// The database and schema the session was last seen in (C6).
    session_place: (Option<String>, Option<String>),
    /// Databases of the connection and schemas of the database, for the
    /// selectors.
    databases: Vec<String>,
    schemas: Vec<String>,
    results: Vec<ResultPane>,
    active_result: usize,
    notices: Vec<Notice>,
    activity: Activity,
    /// The run in progress. Dropping it stops the statement it waits on.
    run: Option<Task<()>>,
    /// The user asked to cancel the running statement: however the server
    /// words its end, it was cancelled.
    cancel_requested: bool,
    /// Statements run in a transaction that the user commits or rolls back.
    manual_commit: bool,
    /// The isolation level chosen for this tab's connection (the main
    /// toolbar), given again to each session the tab opens on it.
    chosen_isolation: Option<IsolationLevel>,
    /// The isolation level the server gives the session's transactions, as
    /// read back when it opened or was last set.
    isolation: Option<IsolationLevel>,
    /// The statements of the open transaction, in manual commit.
    tx_log: Vec<TxLogEntry>,
    /// When the open transaction began, on the executor's clock (K4); a failed
    /// one keeps the start of the transaction it was. See `note_transaction`.
    tx_started: Option<Instant>,
    /// What was typed for each parameter last time, by its label.
    param_values: HashMap<String, String>,
    /// The editor shows where the last statement failed.
    error_marked: bool,
    /// The `.sql` file the text was read from or written to.
    file: Option<PathBuf>,
    /// The text was changed since it was read or written.
    unsaved: bool,
    /// The run being prepared asks for a plan: `Some(analyze)`.
    explain_mode: Option<bool>,
    /// The run being prepared adds its results after the ones shown,
    /// instead of replacing those that are not pinned.
    keep_results: bool,
    /// What runs once the connection, being opened, is open.
    pending: Option<Request>,
    /// Id the next result pane gets.
    next_pane: u64,
    /// The pane whose rows are arriving, or waiting to be paged.
    receiving: Option<u64>,
    results_scroll: ScrollHandle,
    /// The pane the result tab bar was last scrolled to.
    revealed_result: Option<u64>,
    panels: Panels,
    /// The results area, focused when the result on screen is no grid.
    results_focus: FocusHandle,
    /// Bumped at every run and target change, so late answers are dropped.
    generation: u64,
    /// Bumped at every target change only: the lists of the selectors are
    /// read for a target, and a run does not make them late (a tab of a
    /// closed connection connects, then runs, while they are read).
    placed: u64,
    /// What completion offers; empty until loaded.
    catalog: SharedCatalog,
    /// The catalog being loaded. Replacing it drops the older load.
    catalog_load: Option<Task<()>>,
    /// Source mode: the text replaces an object of this schema (MySQL
    /// database), and "Apply source" runs all of it.
    source: Option<String>,
    /// The settings the editor follows: soft wrap, line numbers, tab size.
    editor_options: (bool, bool, u8),
    /// What the run being prepared reads again, and the run in progress.
    next_requery: Option<Requery>,
    requery: Option<Requery>,
    /// The column widths of the result being read again, kept by the
    /// run being prepared, and by the run in progress.
    next_widths: HashMap<String, gpui_kit::Pixels>,
    widths: HashMap<String, gpui_kit::Pixels>,
    /// The find bar of the result being read again, carried as its widths
    /// are (by the run being prepared, then the run in progress).
    next_find: Option<find::Carried>,
    carried_find: Option<find::Carried>,
    /// How far its columns were scrolled, carried the same way (K40).
    next_scroll: Option<gpui_kit::Pixels>,
    carried_scroll: Option<gpui_kit::Pixels>,
    /// "Open in its own tab" showed the results in tabs while they are
    /// stacked (J11): until the next run.
    unstacked: bool,
    /// The "Stack results" setting as this tab last saw it: when it changes
    /// (Settings, the View menu, the results menu), the result on screen is
    /// scrolled into view again.
    stacking: bool,
    /// The stacked results, virtual: only those in view are laid out.
    stack_list: ListState,
    /// The panes (and `u64::MAX` for the messages) the list holds.
    stack_items: RefCell<Vec<u64>>,
    /// The filter of the result on screen, and the pane it shows.
    result_filter: Entity<InputState>,
    result_filter_for: Option<u64>,
    /// The statement and filter the field was last given: a result read
    /// again with the same ones (auto-refresh) leaves what is typed alone.
    result_filter_shown: Option<(String, String)>,
    /// The filters applied to results before, by statement.
    result_history: HashMap<String, Vec<String>>,
    /// A count or a list of values being read.
    side: Option<SideWork>,
    /// Reads the result marked `auto` again every so often (RES-25).
    auto_refresh: Option<AutoRefresh>,
    /// Why auto-refresh stopped by itself, until it is started again.
    auto_refresh_stopped: Option<&'static str>,
    /// The frames around the bracket at the cursor and its match.
    brackets: input::RangeDecorationCollection,
    /// Where the last run from the editor stands, drawn in its margin (K37).
    trace: Rc<RefCell<Trace>>,
    /// The tab is drawn again every [`RUN_TICK`] while a statement runs.
    ticking: bool,
    /// The other occurrences of the word or selection at the cursor.
    occurrences: Occurrences,
    /// The rollback of a transaction left idle, armed for the length of
    /// its log and the environment's period (see `watch_idle_transaction`).
    idle_rollback: Option<((usize, u32), Task<()>)>,
    /// The check of the session the tab's transaction is on (see
    /// `watch_session`).
    session_watch: Option<(Arc<dyn Session>, Task<()>)>,
    /// The transaction's log is of a transaction the server rolled back
    /// when it closed the session (C15): shown until the next statement.
    tx_lost: bool,
    /// The run on several connections this tab took part in (J14).
    script: Option<ScriptRun>,
    /// The AI assistant's panel at the side, while open (J17).
    ai: Option<Entity<crate::ai_view::AiPanel>>,
    /// The field asking the assistant at a place of the editor, and its proposal (K52).
    inline: Option<Entity<super::ai_inline::AiInline>>,
    /// The failed result whose error is marked in the editor, and where (K52's chip).
    error_fix: Option<(u64, usize)>,
    /// The value of the selected cell beside the results, while shown (K17).
    value_panel: Option<Entity<ValuePanel>>,
    /// Vim mode (J19), only while Settings ▸ Editor turns it on.
    pub(super) vim: Option<super::vim::VimEditor>,
    _subscriptions: Vec<Subscription>,
}

/// What a query tab tells the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryTabEvent {
    /// A source was applied, whether all of it ran or not: the objects of
    /// its schema may have changed.
    SourceApplied,
    /// The user chose another connection, database or schema while a
    /// transaction is open: the window asks how to end it, then switches.
    SwitchTarget(Option<Target>),
    /// The user chose auto-commit while a transaction is open (C15): the
    /// window asks how to end it, then switches.
    SwitchToAutoCommit,
    /// Open a query tab holding `sql`, on this tab's connection, database
    /// and schema (statements written from rows of a result).
    OpenQuery { title: String, sql: String },
    /// The tab's session moved to another database or schema (`USE`, `SET
    /// search_path`), and the tab's target with it: the tab is stored again.
    Moved,
    /// Show the rows of a table that `filter` keeps, on this tab's
    /// connection: the other end of a foreign key of a result (K49).
    OpenData { table: TableRef, filter: Filter },
}

impl EventEmitter<QueryTabEvent> for QueryTab {}

impl QueryTab {
    /// A query tab titled `title` with `sql` in its editor, running on
    /// `target`.
    pub fn new(
        title: impl Into<SharedString>,
        sql: &str,
        target: Option<Target>,
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sql = sql.to_string();
        let editor_options = Self::editor_settings(cx);
        let (soft_wrap, line_numbers, tab_size) = editor_options;
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(line_numbers)
                .soft_wrap(soft_wrap)
                .tab_size(TabSize {
                    tab_size: tab_size.into(),
                    hard_tabs: false,
                })
                .default_value(sql)
                .placeholder("Write SQL here")
        });
        let settings = cx.observe_global_in::<AppState>(window, Self::follow_settings);
        let brackets = editor.update(cx, |editor, cx| {
            editor
                .lsp_mut()
                .set_completion_highlighter(Rc::new(completion::highlight));
            // The strip of a run's marks (K37), kept for good: one made at
            // the first run would push the text aside then.
            editor.create_gutter_decorations_collection(Vec::new(), cx);
            editor.set_gutter_decorations_width(run_marks::STRIP, cx);
            editor.create_range_decorations_collection(Vec::new(), cx)
        });
        let occurrences = Occurrences::new(&editor, cx);
        // An edit makes a file's tab differ from its file. (Moving the
        // cursor also notifies observers; only a change of text counts.)
        let edits = cx.subscribe(&editor, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.occurrences.text_changed();
                // A run's marks describe the text it ran: an edit ends them,
                // and a run still going marks nothing more (K37).
                this.trace.borrow_mut().clear();
                // The error's place moved: "Fix with AI" stays in the result (K52).
                if this.error_fix.take().is_some() {
                    cx.notify();
                }
                if let Some(vim) = this.vim.as_mut() {
                    vim.text_changed();
                }
            }
            if matches!(event, InputEvent::Change) && this.file.is_some() && !this.unsaved {
                this.unsaved = true;
                cx.notify();
            }
        });
        // Every change of the editor (text, cursor, blink) re-marks the
        // brackets: the scan is bounded, and nothing is redrawn when the
        // marks stay the same. The occurrences are computed again only when
        // the text or the selection changed.
        let cursor_moves = cx.observe(&editor, |this: &mut Self, editor, cx| {
            this.mark_brackets(cx);
            this.occurrences.refresh(&editor, cx);
            if let Some(vim) = this.vim.as_mut() {
                vim.refresh_block(&editor, cx);
            }
        });
        let subscription = cx.subscribe_in(
            &connections,
            window,
            |this, _, event: &ConnectionsEvent, window, cx| {
                if let ConnectionsEvent::StatusChanged { id } = event
                    && this
                        .target
                        .as_ref()
                        .is_some_and(|target| target.connection == *id)
                {
                    this.say_if_rolled_back(window, cx);
                    this.follow_connection(cx);
                    this.resume(window, cx);
                }
            },
        );
        // The connection's commit mode: its own, else its environment's.
        let environments = &AppState::get(cx).settings.environments;
        let manual_commit = target
            .as_ref()
            .and_then(|target| connections.read(cx).get(&target.connection))
            .is_some_and(|saved| saved.config.starts_in_manual_commit(environments));
        let result_filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                "Filter the result on the server: a SQL condition, for example total > 100",
            )
        });
        let filtering = cx.subscribe_in(
            &result_filter,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.apply_result_filter(window, cx);
                }
            },
        );
        let mut tab = Self {
            title: title.into(),
            editor,
            connections,
            target,
            proposed: None,
            unbound_why: None,
            session: None,
            session_place: (None, None),
            databases: Vec::new(),
            schemas: Vec::new(),
            results: Vec::new(),
            active_result: 0,
            notices: Vec::new(),
            activity: Activity::Idle,
            run: None,
            cancel_requested: false,
            manual_commit,
            chosen_isolation: None,
            isolation: None,
            tx_log: Vec::new(),
            tx_started: None,
            param_values: HashMap::new(),
            error_marked: false,
            file: None,
            unsaved: false,
            explain_mode: None,
            keep_results: false,
            pending: None,
            next_pane: 0,
            receiving: None,
            results_scroll: ScrollHandle::new(),
            revealed_result: None,
            panels: Panels::Both,
            results_focus: cx.focus_handle(),
            generation: 0,
            placed: 0,
            catalog: SharedCatalog::default(),
            catalog_load: None,
            source: None,
            editor_options,
            next_requery: None,
            next_widths: HashMap::new(),
            widths: HashMap::new(),
            next_find: None,
            carried_find: None,
            next_scroll: None,
            carried_scroll: None,
            unstacked: false,
            stacking: AppState::get(cx).settings.stack_results,
            stack_list: ListState::new(0, ListAlignment::Top, px(200.)),
            stack_items: RefCell::new(Vec::new()),
            requery: None,
            result_filter,
            result_filter_for: None,
            result_filter_shown: None,
            result_history: HashMap::new(),
            side: None,
            auto_refresh: None,
            auto_refresh_stopped: None,
            brackets,
            trace: Rc::default(),
            ticking: false,
            occurrences,
            idle_rollback: None,
            session_watch: None,
            tx_lost: false,
            script: None,
            ai: None,
            inline: None,
            error_fix: None,
            value_panel: None,
            vim: None,
            _subscriptions: vec![subscription, edits, cursor_moves, settings, filtering],
        };
        // Every change of the tab may start, restart or stop the wait for
        // an idle transaction to end.
        let idle = cx.observe_in(&cx.entity(), window, |this: &mut Self, _, window, cx| {
            this.watch_idle_transaction(window, cx);
            this.watch_session(window, cx);
            this.tick_while_running(cx);
        });
        tab._subscriptions.push(idle);
        tab.follow_connection(cx);
        super::vim::follow_setting(&mut tab, cx);
        tab
    }

    /// Soft wrap, line numbers and tab size, as the settings have them.
    fn editor_settings(cx: &App) -> (bool, bool, u8) {
        let settings = &AppState::get(cx).settings;
        (settings.soft_wrap, settings.line_numbers, settings.tab_size)
    }

    /// The editor takes a changed setting at once.
    fn follow_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        super::vim::follow_setting(self, cx);
        let stacking = AppState::get(cx).settings.stack_results;
        if stacking != self.stacking {
            self.stacking = stacking;
            self.unstacked = false;
            self.revealed_result = None;
            cx.notify();
        }
        let wanted = Self::editor_settings(cx);
        if wanted == self.editor_options {
            return;
        }
        self.editor_options = wanted;
        let (soft_wrap, line_numbers, tab_size) = wanted;
        self.editor.update(cx, |editor, cx| {
            editor.set_soft_wrap(soft_wrap, window, cx);
            editor.set_line_number(line_numbers, window, cx);
            editor.set_tab_size(
                TabSize {
                    tab_size: tab_size.into(),
                    hard_tabs: false,
                },
                cx,
            );
        });
    }

    /// Whether the editor wraps long lines, shows line numbers, and its tab
    /// size; for tests.
    #[cfg(test)]
    pub fn editor_options(&self) -> (bool, bool, u8) {
        self.editor_options
    }

    /// The tab's title.
    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// Names the tab.
    pub fn set_title(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.title = title.into();
        cx.notify();
    }

    /// The editor, to observe its changes.
    pub fn editor(&self) -> &Entity<EditorState> {
        &self.editor
    }

    /// The editor's text.
    pub fn sql(&self, cx: &App) -> String {
        self.editor.read(cx).text().to_string()
    }

    /// What to store of this tab.
    pub fn snapshot(&self, cx: &App) -> TabSnapshot {
        TabSnapshot::Query {
            title: self.title.to_string(),
            sql: self.sql(cx),
            target: self.target.clone(),
            file: self.file.clone(),
            unsaved: self.unsaved,
            source: self.source.clone(),
        }
    }

    /// Puts the tab in source mode for an object of `schema`, or out of it.
    pub fn set_source(&mut self, schema: Option<String>, cx: &mut Context<Self>) {
        self.source = schema;
        cx.notify();
    }

    /// The schema of the object whose source the tab holds, in source mode.
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    /// The `.sql` file of this tab, when it has one.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// `:q!` in vim mode: the text is let go without asking.
    pub(super) fn forget_unsaved(&mut self, cx: &mut Context<Self>) {
        self.unsaved = false;
        cx.notify();
    }

    /// The vim mode the editor is in, when vim mode is on (J19).
    pub fn vim_mode(&self) -> Option<kerno_core::vim::Mode> {
        self.vim.as_ref().map(|vim| vim.mode())
    }

    /// Whether the text differs from its file's (always, without a file).
    pub fn unsaved(&self) -> bool {
        self.unsaved
    }

    /// The tab is the file at `path` from now on, named after it; its text
    /// is what the file holds unless `unsaved`.
    pub fn set_file(&mut self, path: PathBuf, unsaved: bool, cx: &mut Context<Self>) {
        if let Some(name) = path.file_name() {
            self.title = SharedString::from(name.to_string_lossy().into_owned());
        }
        self.file = Some(path);
        self.unsaved = unsaved;
        cx.notify();
    }

    /// The connection the tab runs on.
    pub fn connection_id(&self) -> Option<&str> {
        self.target
            .as_ref()
            .map(|target| target.connection.as_str())
    }

    /// Where the tab runs its statements.
    pub fn target(&self) -> Option<&Target> {
        self.target.as_ref()
    }

    #[cfg(test)]
    pub fn activity(&self) -> Activity {
        self.activity
    }

    /// What the status bar says about this tab.
    pub fn status(&self, cx: &App) -> TabStatus {
        let connections = self.connections.read(cx);
        let saved = self
            .target
            .as_ref()
            .and_then(|target| connections.get(&target.connection));
        let database = self.target.as_ref().and_then(|target| {
            target.database.clone().or_else(|| {
                connections
                    .meta_session(&target.connection)
                    .and_then(|meta| meta.database())
            })
        });
        let pane = self
            .results
            .get(self.active_result.min(self.results.len().saturating_sub(1)));
        TabStatus {
            connection: saved.map(|saved| saved.config.name.clone()),
            read_only: saved.is_some_and(|saved| saved.config.read_only),
            database,
            transaction: self
                .session
                .as_ref()
                .map_or(TxState::Idle, |session| session.tx_state()),
            running: matches!(self.activity, Activity::Running { .. }),
            rows: pane
                .and_then(ResultPane::grid)
                .map(|grid| grid.read(cx).delegate().row_count()),
            elapsed: pane.and_then(|pane| pane.elapsed),
            notices: self.notices.len(),
            pending: self.pending_count(cx),
        }
    }

    /// The messages of the failed statements of the last run; for tests.
    #[cfg(test)]
    pub fn failures(&self) -> Vec<String> {
        self.results
            .iter()
            .filter_map(|pane| match &pane.outcome {
                Outcome::Failed(failure) => Some(failure.message.clone()),
                _ => None,
            })
            .collect()
    }

    /// What each statement of the last run produced, in words; for tests.
    #[cfg(test)]
    pub fn outcomes(&self, cx: &App) -> Vec<String> {
        self.results
            .iter()
            .map(|pane| match &pane.outcome {
                Outcome::Rows(grid) => {
                    let grid = grid.read(cx).delegate();
                    format!(
                        "{} rows{}",
                        grid.row_count(),
                        if grid.is_complete() {
                            ""
                        } else if grid.has_more() {
                            ", more"
                        } else {
                            ", incomplete"
                        }
                    )
                }
                Outcome::Done => match pane.rows_affected {
                    Some(rows) => format!("done, {rows} affected"),
                    None => "done".into(),
                },
                Outcome::Failed(failure) => {
                    format!("failed [{}]", failure.code.clone().unwrap_or_default())
                }
                Outcome::Cancelled => "cancelled".into(),
                Outcome::Plan {
                    plan: Some(plan), ..
                } => {
                    format!("plan, {} steps", plan_view::rows(plan).len())
                }
                Outcome::Plan { problem, .. } => {
                    format!("plan unread: {}", problem.clone().unwrap_or_default())
                }
            })
            .collect()
    }

    /// The labels of the result tabs, `*` before a pinned one; for tests.
    #[cfg(test)]
    pub fn result_tabs(&self) -> Vec<String> {
        (0..self.results.len())
            .map(|index| {
                let pin = if self.results[index].pinned { "*" } else { "" };
                format!("{pin}{}", self.result_label(index))
            })
            .collect()
    }

    /// The result on screen, from 0; for tests.
    #[cfg(test)]
    pub fn active_result(&self) -> usize {
        self.active_result
    }

    /// How far the result tab bar is scrolled; for tests.
    #[cfg(test)]
    pub fn result_bar_offset(&self) -> gpui_kit::Pixels {
        self.results_scroll.offset().x
    }

    /// Which parts of the tab are on screen; for tests.
    #[cfg(test)]
    pub fn panels(&self) -> Panels {
        self.panels
    }

    /// How many places the editor marks as errors; for tests.
    #[cfg(test)]
    pub fn error_marks(&self, cx: &App) -> usize {
        self.editor
            .read(cx)
            .diagnostics()
            .map_or(0, |diagnostics| diagnostics.len())
    }

    /// The grid of a result; for tests and the bench mode.
    pub fn grid_of(&self, result: usize) -> Option<Entity<TableState<Grid>>> {
        self.results.get(result).and_then(ResultPane::grid)
    }

    /// Whether it is known if the rows of result `result` take changes
    /// (an editable result is ordered by the server); for the bench.
    pub fn edit_decided(&self, result: usize) -> bool {
        self.results
            .get(result)
            .is_some_and(|pane| pane.editable.is_some())
    }

    /// The server's messages of the last run; for tests.
    #[cfg(test)]
    pub fn messages(&self) -> Vec<String> {
        self.notices
            .iter()
            .map(|notice| format!("{}: {}", notice.severity, notice.message))
            .collect()
    }

    /// The details of the messages that have one; for tests.
    #[cfg(test)]
    pub fn message_details(&self) -> Vec<String> {
        self.notices
            .iter()
            .filter_map(|notice| notice.detail.clone())
            .collect()
    }

    /// Column names and the first `limit` rows of a result; for tests.
    #[cfg(test)]
    pub fn rows_of(
        &self,
        result: usize,
        limit: usize,
        cx: &App,
    ) -> (Vec<String>, Vec<Vec<Option<String>>>) {
        let Some(grid) = self.results.get(result).and_then(ResultPane::grid) else {
            return Default::default();
        };
        let store = grid.read(cx).delegate().store();
        let columns: Vec<String> = store
            .columns()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        let rows = (0..store.row_count().min(limit))
            .map(|row| {
                (0..columns.len())
                    .map(|column| store.text(row, column).map(str::to_string))
                    .collect()
            })
            .collect();
        (columns, rows)
    }

    /// Whether the tab's connection is open.
    pub fn connected(&self, cx: &App) -> bool {
        self.target.as_ref().is_some_and(|target| {
            matches!(
                self.connections.read(cx).status(&target.connection),
                Status::Connected { .. }
            )
        })
    }

    /// What the toolbar's selectors do: switches to `target` now, or, with
    /// a transaction open, lets the window ask how to end it first.
    pub fn request_target(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        if self.target == target {
            return;
        }
        if self.tx_state() == TxState::Idle {
            self.set_target(target, cx);
        } else {
            cx.emit(QueryTabEvent::SwitchTarget(target));
        }
    }

    /// Unbound (C6): the pickers list `connection` first.
    pub fn propose(&mut self, connection: Option<String>, cx: &mut Context<Self>) {
        self.proposed = connection;
        cx.notify();
    }

    /// The connection the pickers list first while unbound; for tests.
    #[cfg(test)]
    pub fn proposed(&self) -> Option<&str> {
        self.proposed.as_deref()
    }

    /// Its connection was deleted (C6): the tab keeps its text and file and
    /// runs nowhere until the user picks another one; nothing is bound in
    /// its place. What was running is stopped, as closing the tab does.
    pub fn unbind_deleted(&mut self, cx: &mut Context<Self>) {
        self.set_target(None, cx);
        self.proposed = None;
        self.unbound_why = Some(UNBOUND_DELETED);
        cx.notify();
    }

    /// What the bar of an unbound tab says, and a run of it; `None` when
    /// the tab is bound.
    pub fn unbound_text(&self) -> Option<&'static str> {
        self.target
            .is_none()
            .then(|| self.unbound_why.unwrap_or(UNBOUND_PICK))
    }

    /// Runs on `target` from now on. What was running is stopped. On
    /// another connection without a transaction open, the tab takes that
    /// connection's commit mode. A file's tab remembers the connection for
    /// the next time the file opens (C6).
    pub fn set_target(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        if self.target == target {
            return;
        }
        if let (Some(file), Some(target)) = (&self.file, &target) {
            let (file, connection) = (file.clone(), target.connection.clone());
            AppState::update_settings(cx, |settings| {
                settings.remember_script(file, Some(connection))
            });
        }
        if target.is_some() {
            self.proposed = None;
            self.unbound_why = None;
        }
        let connection = |target: &Option<Target>| target.as_ref().map(|t| t.connection.clone());
        if connection(&self.target) != connection(&target) {
            // A level is chosen for one server.
            self.chosen_isolation = None;
        }
        if connection(&self.target) != connection(&target) && self.tx_state() == TxState::Idle {
            let environments = &AppState::get(cx).settings.environments;
            self.manual_commit = target
                .as_ref()
                .and_then(|target| self.connections.read(cx).get(&target.connection))
                .is_some_and(|saved| saved.config.starts_in_manual_commit(environments));
        }
        self.stop(cx);
        self.pending = None;
        self.target = target;
        // The source replaces an object where it was read from.
        self.source = None;
        self.close_session(cx);
        self.databases.clear();
        self.schemas.clear();
        self.generation += 1;
        self.placed += 1;
        *self.catalog.borrow_mut() = Arc::default();
        self.catalog_load = None;
        self.complete_with(None, cx);
        self.follow_connection(cx);
        cx.notify();
    }

    /// The connection went away with this tab's transaction open: lost, or
    /// closed by something that could not ask (Kerno's own disconnects end
    /// the transaction first). The server rolled it back; the tab says so.
    fn say_if_rolled_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The tab's own record: the session's state may already say `Idle`
        // (a PostgreSQL session the server closed, C15).
        if self.connected(cx) || self.tx_started.is_none() {
            return;
        }
        self.report_rolled_back(CONNECTION_LOST, None, window, cx);
    }

    /// A statement of this tab found its session gone: an open transaction
    /// is reported lost and the session dropped. Nothing runs again. The
    /// connection is checked, and opened again only if it is lost
    /// (CLAUDE.md §4 A5): the server may have closed this session alone,
    /// and the other tabs' transactions stay (C15).
    fn lost_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The session says `Idle` once closed: the tab's record says
        // whether it held a transaction.
        if self.tx_started.is_some() {
            self.report_rolled_back(SESSION_LOST, None, window, cx);
        }
        self.activity = Activity::Idle;
        self.close_session(cx);
        if let Some(id) = self.connection_id().map(str::to_string) {
            self.connections
                .update(cx, |connections, cx| connections.verify(&id, cx));
        }
    }

    /// Says in the tab, and in a notification, that its transaction was
    /// rolled back by the server: `first`, then what was kept (C15), then
    /// why when the server said.
    fn report_rolled_back(
        &mut self,
        first: &str,
        why: Option<&DbError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let message = self.note_rolled_back(first, why, cx);
        window.push_notification(
            Notification::warning(SharedString::from(format!("“{}”: {message}", self.title))),
            cx,
        );
    }

    /// [`Self::report_rolled_back`] in the tab only; returns its text.
    fn note_rolled_back(
        &mut self,
        first: &str,
        why: Option<&DbError>,
        cx: &mut Context<Self>,
    ) -> String {
        let mut message = first.to_string();
        // Its statements stay in the log, marked rolled back.
        if !self.tx_log.is_empty() {
            self.tx_lost = true;
            message.push_str(" Its statements are in the transaction log.");
        }
        if self.pending_count(cx) > 0 {
            message.push_str(
                " The pending changes of the results are kept: Apply runs them in a new \
                 transaction.",
            );
        }
        let why = why.map(closed_why);
        self.notices.push(Notice {
            severity: "Warning".into(),
            message: message.clone(),
            code: None,
            detail: why.clone(),
        });
        // The messages, after the results.
        self.active_result = self.results.len();
        self.show_results();
        cx.notify();
        match why {
            Some(why) => format!("{message}\n{why}"),
            None => message,
        }
    }

    /// While the tab holds a transaction and nothing runs, asks every
    /// [`SESSION_CHECK`] whether the server closed its session (C15),
    /// without sending anything on it (see [`Session::gone`]). Armed on
    /// the tab's record of its transaction, not on the session's state,
    /// which turns `Idle` the moment a PostgreSQL client reads the close.
    fn watch_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let due = self
            .session
            .clone()
            .filter(|_| self.tx_started.is_some() && !self.running());
        let same = match (&due, &self.session_watch) {
            (Some(due), Some((watched, _))) => Arc::ptr_eq(due, watched),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.session_watch = due.map(|session| {
            let watched = session.clone();
            let watch = cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(SESSION_CHECK).await;
                    let checking = {
                        let session = session.clone();
                        cx.update(|_, cx| bridge::spawn(cx, async move { session.gone().await }))
                    };
                    let Ok(checking) = checking else {
                        return;
                    };
                    if let Some(why) = checking.await {
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.session_gone(&session, why, window, cx)
                        });
                        return;
                    }
                }
            });
            (watched, watch)
        });
    }

    /// The server closed `session` (C15): if it is still the tab's, the
    /// transaction went with it. The tab says so once and drops the
    /// session; the next statement opens a new one, in the same database
    /// and schema (the target follows the session, C6) and commit mode.
    fn session_gone(
        &mut self,
        session: &Arc<dyn Session>,
        why: DbError,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Kerno closed it, or the tab moved on: nothing to say.
        if !self
            .session
            .as_ref()
            .is_some_and(|own| Arc::ptr_eq(own, session))
        {
            return;
        }
        if self.tx_started.is_some() {
            self.report_rolled_back(SESSION_CLOSED, Some(&why), window, cx);
        }
        self.close_session(cx);
        cx.notify();
    }

    /// Loads the lists of the selectors when the connection is open; drops
    /// the session when it closed.
    fn follow_connection(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        if !self.connected(cx) {
            // A statement still running is cut off: said, never run again.
            if self.run.is_some() && matches!(self.activity, Activity::Running { .. }) {
                self.push_pane(
                    String::new(),
                    Outcome::Failed(lost_failure(None)),
                    None,
                    None,
                );
                self.active_result = self.results.len() - 1;
                self.show_results();
                self.trace.borrow_mut().stopped(Mark::Failed);
            }
            self.stop(cx);
            self.close_session(cx);
            self.complete_with(None, cx);
            cx.notify();
            return;
        }
        let Some(meta) = self.connections.read(cx).meta_session(&target.connection) else {
            return;
        };
        self.complete_with(Some(meta.clone()), cx);
        self.load_catalog(cx);
        // The connection's own session lists the databases, and the
        // schemas when the tab is in its database.
        let own_database = target.database.is_none() || target.database == meta.database();
        let wants_schemas = meta.dialect().has_schemas() && own_database;
        let placed = self.placed;
        let listing = bridge::spawn(cx, async move {
            let databases = meta.databases().await?;
            let schemas = if wants_schemas {
                meta.schemas().await?
            } else {
                Vec::new()
            };
            Ok::<_, Error>((databases, schemas))
        });
        cx.spawn(async move |this, cx| {
            if let Ok((databases, schemas)) = listing.await {
                let _ = this.update(cx, |this, cx| {
                    if this.placed != placed {
                        return;
                    }
                    this.databases = databases
                        .into_iter()
                        .filter(|database| !database.is_system)
                        .map(|database| database.name)
                        .collect();
                    if wants_schemas {
                        this.schemas = schemas
                            .into_iter()
                            .filter(|schema| !schema.is_system)
                            .map(|schema| schema.name)
                            .collect();
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Completion in the editor proposes from the catalog, with the dialect
    /// of `session`; none without a session.
    fn complete_with(&self, session: Option<Arc<dyn Session>>, cx: &mut Context<Self>) {
        let catalog = self.catalog.clone();
        self.editor.update(cx, |editor, _| {
            editor.lsp_mut().completion_provider = session.map(|session| {
                Rc::new(SqlCompletion {
                    catalog,
                    session,
                    prefix: String::new(),
                }) as Rc<dyn CompletionProvider>
            });
        });
    }

    /// Loads (again) what completion offers: from the connection's own
    /// session when the tab is in its database, else from the tab's.
    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(meta) = self.connections.read(cx).meta_session(&target.connection) else {
            return;
        };
        let session = if target.database.is_none() || target.database == meta.database() {
            Task::ready(Ok(meta))
        } else {
            self.session(cx)
        };
        self.catalog_load = Some(cx.spawn(async move |this, cx| {
            let Ok(session) = session.await else {
                return;
            };
            let loading = cx
                .update(|cx| bridge::spawn(cx, async move { session.completion_catalog().await }));
            match loading.await {
                Ok(mut catalog) => {
                    // The tab's schema is searched first.
                    if let Some(schema) = target.schema {
                        catalog.search_path.retain(|name| *name != schema);
                        catalog.search_path.insert(0, schema);
                    }
                    let _ = this.update(cx, |this, cx| {
                        *this.catalog.borrow_mut() = Arc::new(catalog);
                        this.tell_ai_catalog(cx);
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "the completion catalog was not loaded");
                    let _ = this.update(cx, |this, cx| this.tell_ai_catalog(cx));
                }
            }
        }));
    }

    /// K53: a conversation opened before the catalog was read is put together again from it
    /// (or stops waiting for it, when it could not be read).
    fn tell_ai_catalog(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.ai.clone() {
            let facts = self.ai_facts(cx);
            panel.update(cx, |panel, cx| panel.catalog_read(facts, cx));
        }
    }

    /// Whether the completion catalog has arrived; for tests.
    #[cfg(test)]
    pub fn catalog_ready(&self) -> bool {
        !self.catalog.borrow().schemas.is_empty()
    }

    /// Labels and inserted texts completion proposes at byte `cursor` of
    /// `sql`; for tests.
    #[cfg(test)]
    pub fn completions(&self, sql: &str, cursor: usize, cx: &App) -> Vec<(String, String)> {
        let Some(session) = self
            .target
            .as_ref()
            .and_then(|target| self.connections.read(cx).meta_session(&target.connection))
        else {
            return Vec::new();
        };
        let provider = SqlCompletion {
            catalog: self.catalog.clone(),
            session,
            prefix: String::new(),
        };
        provider
            .items(&gpui_kit::component::input::Rope::from(sql), cursor)
            .into_iter()
            .map(|item| {
                let insert = match item.text_edit {
                    Some(lsp_types::CompletionTextEdit::Edit(edit)) => edit.new_text,
                    _ => String::new(),
                };
                (item.label, insert)
            })
            .collect()
    }

    /// This tab's session, opened (and put in the tab's schema) if needed.
    fn session(&mut self, cx: &mut Context<Self>) -> Task<kerno_core::Result<Arc<dyn Session>>> {
        if let Some(session) = &self.session {
            return Task::ready(Ok(session.clone()));
        }
        let Some(target) = self.target.clone() else {
            return Task::ready(Err(Error::Config(UNBOUND_PICK.into())));
        };
        if !self.connected(cx) {
            let Some(saved) = self.connections.read(cx).get(&target.connection) else {
                return Task::ready(Err(Error::Config(UNBOUND_DELETED.into())));
            };
            return Task::ready(Err(Error::Closed(format!(
                "“{}” is not connected. Double-click it in the sidebar, then run again.",
                saved.config.name
            ))));
        }
        let opening =
            self.connections
                .read(cx)
                .open_session(&target.connection, target.database.clone(), cx);
        let generation = self.generation;
        let chosen = self.chosen_isolation;
        cx.spawn(async move |this, cx| {
            let session = opening.await?;
            // The level chosen for the tab, or the server's, for the tick
            // of the toolbar's transaction mode.
            let reading = {
                let session = session.clone();
                cx.update(|cx| {
                    bridge::spawn(cx, async move {
                        match chosen {
                            Some(level) => exec::set_isolation(session, level).await,
                            None => exec::isolation(session).await,
                        }
                    })
                })
            };
            let isolation = reading.await.ok().flatten();
            if session.dialect().has_schemas() {
                if let Some(schema) = &target.schema {
                    let sql = format!(
                        "SET search_path TO {}, public",
                        session.dialect().quote_ident(schema)
                    );
                    let entering =
                        cx.update(|cx| bridge::spawn(cx, exec::collect(session.clone(), sql)));
                    entering.await?;
                }
                // The schemas of a database other than the connection's own
                // can only be listed from inside it.
                let listing = {
                    let session = session.clone();
                    cx.update(|cx| bridge::spawn(cx, async move { session.schemas().await }))
                };
                if let Ok(schemas) = listing.await {
                    let _ = this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.schemas = schemas
                                .into_iter()
                                .filter(|schema| !schema.is_system)
                                .map(|schema| schema.name)
                                .collect();
                            cx.notify();
                        }
                    });
                }
            }
            // One session per tab: one opened at the same time as the tab's
            // own (a run and the completion list) is closed at once, and the
            // tab's is used.
            let session = this.update(cx, |this, cx| match &this.session {
                _ if this.generation != generation => session,
                Some(kept) if !Arc::ptr_eq(kept, &session) => {
                    let kept = kept.clone();
                    bridge::spawn(cx, async move { session.close().await }).detach();
                    kept
                }
                _ => {
                    this.session = Some(session.clone());
                    this.session_place = (session.database(), session.schema());
                    this.isolation = isolation;
                    session
                }
            });
            session.map_err(|_| Error::Closed("the tab was closed".into()))
        })
    }

    /// Closes the tab's session now: the tab is closing, moving to another
    /// target, or its connection closed. Results on screen stay.
    pub fn close_session(&mut self, cx: &mut Context<Self>) {
        self.isolation = None;
        // Its transaction ends with it: the next one starts its own age.
        self.tx_started = None;
        if let Some(session) = self.session.take() {
            bridge::spawn(cx, async move {
                // A transaction is rolled back before, not cancelled by, the
                // close (the server would roll it back too).
                if session.tx_state() != TxState::Idle {
                    let _ = exec::collect(session.clone(), "ROLLBACK").await;
                }
                session.close().await
            })
            .detach();
        }
    }

    /// Whether the result of `pane` can be exported: rows of a statement
    /// that only reads, since an export runs it again.
    fn exportable(&self, pane: &ResultPane, cx: &App) -> bool {
        matches!(pane.outcome, Outcome::Rows(_))
            && !classify(&pane.statement, self.lex_rules(cx)).writes
    }

    /// Opens the export of the result on screen; its statement runs again.
    pub fn export_result(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ExportDialog>> {
        let pane = self.results.get(self.active_result)?;
        if !self.exportable(pane, cx) {
            return None;
        }
        let sql = pane.statement.clone();
        self.export_statement(sql, window, cx)
    }

    /// Opens the export of the rows of a statement that only reads.
    fn export_statement(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ExportDialog>> {
        export_dialog::open(
            self.connections.clone(),
            self.target.clone()?,
            Source::Query { sql },
            window,
            cx,
        )
    }

    /// The statements a run takes: the selection or, without one, the
    /// statement at the cursor or the whole text.
    pub(super) fn lex_rules(&self, cx: &App) -> LexRules {
        self.target
            .as_ref()
            .and_then(|target| self.connections.read(cx).meta_session(&target.connection))
            .map_or(LexRules::POSTGRES, |meta| meta.dialect().lex_rules())
    }

    /// Frames the bracket beside the cursor and the one that matches it
    /// (C2 bracket matching), or nothing.
    fn mark_brackets(&mut self, cx: &mut Context<Self>) {
        // ponytail: lexes 8 KiB each way from the cursor, from a line
        // start, so a long script costs no more than a short one. In a
        // script longer than that, a window that begins inside a multi-line
        // string or comment can misread the brackets after it; cache the
        // whole text's tokens per edit if that is ever seen.
        const WINDOW: usize = 8 * 1024;
        let editor = self.editor.read(cx);
        let cursor = editor.cursor();
        let text = editor.text();
        let start = text.floor_char_boundary(cursor.saturating_sub(WINDOW));
        let end = text.ceil_char_boundary((cursor + WINDOW).min(text.len()));
        let window = text.slice(start..end).to_string();
        let from = start
            + if start == 0 {
                0
            } else {
                window[..cursor - start]
                    .find('\n')
                    .map_or(0, |line| line + 1)
            };
        let marks: Vec<_> =
            matching_bracket(&window[from - start..], cursor - from, self.lex_rules(cx))
                .map(|(at, other)| {
                    vec![
                        at.start + from..at.end + from,
                        other.start + from..other.end + from,
                    ]
                })
                .unwrap_or_default();
        // Setting them notifies the editor, which calls this again: set
        // only what changed.
        if self.brackets.get_ranges(cx) != marks {
            let marks = marks.into_iter().map(input::RangeDecoration::new).collect();
            self.brackets.set(marks, cx);
        }
    }

    /// The ranges framed as the bracket at the cursor and its match; for
    /// tests.
    #[cfg(test)]
    pub fn marked_brackets(&self, cx: &App) -> Vec<std::ops::Range<usize>> {
        self.brackets.get_ranges(cx)
    }

    fn statements(&self, scope: Scope, cx: &App) -> Vec<Statement> {
        let rules = self.lex_rules(cx);
        let editor = self.editor.read(cx);
        let text = editor.text().to_string();
        let selection = editor.selected_range();
        let from = |source: &str, base: usize, span: &kerno_core::sql::StatementSpan| Statement {
            sql: span.text(source).to_string(),
            place: Some(base + span.range.start..base + span.range.end),
            refused: None,
        };
        if scope != Scope::FromCursor
            && let Some(selected) = text.get(selection.clone())
            && !selected.trim().is_empty()
        {
            return split_statements(selected, rules)
                .iter()
                .map(|span| from(selected, selection.start, span))
                .collect();
        }
        let spans = split_statements(&text, rules);
        match scope {
            Scope::All => spans.iter().map(|span| from(&text, 0, span)).collect(),
            Scope::AtCursor => statement_at(&spans, editor.cursor())
                .map(|span| vec![from(&text, 0, span)])
                .unwrap_or_default(),
            Scope::FromCursor => {
                // As `statement_at` finds the statement of the cursor.
                let cursor = editor.cursor();
                let first = spans
                    .iter()
                    .position(|span| cursor <= span.reach)
                    .unwrap_or(spans.len().saturating_sub(1));
                spans
                    .iter()
                    .skip(first)
                    .map(|span| from(&text, 0, span))
                    .collect()
            }
        }
    }

    /// Moves the cursor to the start of the next statement, or of the one
    /// before the statement it is in (DBeaver's `query.next` / `query.prev`).
    pub fn go_to_statement(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let rules = self.lex_rules(cx);
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let spans = split_statements(&text, rules);
            let cursor = editor.cursor();
            let target = if forward {
                spans.iter().find(|span| span.range.start > cursor)
            } else {
                // The statement the cursor is in or before, then the one ahead.
                let here = spans
                    .iter()
                    .position(|span| span.range.end >= cursor)
                    .unwrap_or(spans.len());
                here.checked_sub(1).map(|previous| &spans[previous])
            };
            if let Some(span) = target {
                editor.set_cursor_position(position_of(&text, span.range.start), window, cx);
            }
        });
    }

    /// Opens the editor's search panel, with its replace row when asked.
    pub fn find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.focus_handle(cx).focus(window, cx);
            editor.open_search(replace, cx);
        });
    }

    /// Whether the editor's search panel is open, and with its replace row.
    #[cfg(test)]
    pub fn searching(&self, cx: &App) -> Option<bool> {
        let session = self.editor.read(cx).search_session();
        session.open.then_some(session.replace_mode)
    }

    /// Comments the lines of the selection (or the cursor's line) out, or
    /// back in. One step to undo.
    pub fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let selection = editor.selected_range();
            if text.get(selection.clone()).is_none() {
                return;
            }
            let lines = comment::lines_of(&text, selection.clone());
            let toggled = comment::toggle(&text[lines.clone()]);
            let cursor = position_of(&text, lines.start);
            let rewritten = format!("{}{toggled}{}", &text[..lines.start], &text[lines.end..]);
            editor.replace_all(rewritten, window, cx);
            editor.set_cursor_position(cursor, window, cx);
        });
    }

    /// Lays out the selection, or the whole text without one: keywords in
    /// upper case, one clause per line. One step to undo; text already
    /// laid out is left alone.
    pub fn format(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rules = self.lex_rules(cx);
        let options = AppState::get(cx).settings.format;
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let mut range = editor.selected_range();
            let Some(selected) = text.get(range.clone()) else {
                return;
            };
            if selected.trim().is_empty() {
                range = 0..text.len();
            }
            let part = &text[range.clone()];
            let mut formatted = format_with(part, rules, &options);
            // A piece of the text keeps what followed it on its line.
            if range.len() < text.len() && !part.ends_with('\n') && formatted.ends_with('\n') {
                formatted.pop();
            }
            if formatted == part {
                return;
            }
            let rewritten = format!("{}{formatted}{}", &text[..range.start], &text[range.end..]);
            editor.replace_all(rewritten, window, cx);
            let cursor = editor.text().offset_to_position(range.start);
            editor.set_cursor_position(cursor, window, cx);
        });
    }

    /// Format on save: lays the whole text out when it does so cleanly
    /// (one step to undo), keeping the cursor on its line; a script that
    /// does not is left as it is.
    pub fn format_for_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rules = self.lex_rules(cx);
        let options = AppState::get(cx).settings.format;
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let Some(formatted) = format_cleanly(&text, rules, &options) else {
                return;
            };
            if formatted == text {
                return;
            }
            let cursor = editor.cursor_position();
            editor.replace_all(formatted, window, cx);
            editor.set_cursor_position(cursor, window, cx);
        });
    }

    /// Selects every occurrence of the word or selection at the cursor
    /// (⌘⇧L, DBeaver's ⌥J): typing changes them all.
    pub fn select_all_occurrences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        Occurrences::select_all(&self.editor, cx);
        self.editor.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// Opens every folded region. The kit folds from the gutter only, and
    /// opens a fold through a line it hides.
    pub fn unfold_all(&mut self, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            let folded: Vec<usize> = editor
                .display_map()
                .folded_ranges()
                .iter()
                .map(|fold| fold.start_line)
                .collect();
            for line in folded {
                editor.unfold_at(Position::new(line as u32 + 1, 0), cx);
            }
        });
    }

    /// The highlighted occurrences of the word or selection at the cursor;
    /// for tests.
    #[cfg(test)]
    pub fn marked_occurrences(&self, cx: &App) -> Vec<std::ops::Range<usize>> {
        self.occurrences.marked(cx)
    }

    /// Puts the selection in upper (`upper`) or lower case, strings, quoted
    /// names and parameters aside. One step to undo; the selection stays.
    pub fn change_case(&mut self, upper: bool, window: &mut Window, cx: &mut Context<Self>) {
        let rules = self.lex_rules(cx);
        self.rewrite_selection(window, cx, |text, range| {
            change_case(text, range, upper, rules)
        });
    }

    /// Wraps the selection (or the cursor's line) in `/* */`, or takes the
    /// markers off. One step to undo.
    pub fn toggle_block_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rewrite_selection(window, cx, |text, range| toggle_block_comment(&text[range]));
    }

    /// Replaces the selection, or the cursor's whole line without one, by
    /// what `rewrite` makes of it, and selects the result.
    fn rewrite_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        rewrite: impl FnOnce(&str, std::ops::Range<usize>) -> String,
    ) {
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let mut range = editor.selected_range();
            if text.get(range.clone()).is_none() {
                return;
            }
            if range.is_empty() {
                range = comment::lines_of(&text, range);
            }
            let replaced = rewrite(&text, range.clone());
            if replaced == text[range.clone()] {
                return;
            }
            let rewritten = format!("{}{replaced}{}", &text[..range.start], &text[range.end..]);
            editor.replace_all(rewritten, window, cx);
            editor.set_selected_range(range.start..range.start + replaced.len(), cx);
        });
    }

    /// Copies the statement at the cursor, or the selected text (DBeaver's
    /// "Copy selected query"). Returns whether there was one.
    pub fn copy_statement(&mut self, cx: &mut Context<Self>) -> bool {
        let statements = self.statements(Scope::AtCursor, cx);
        let text = statements
            .iter()
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>()
            .join(";\n");
        if text.trim().is_empty() {
            return false;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// Reads again what completion knows of the tab's database: tables
    /// made or dropped elsewhere since the tab opened.
    pub fn refresh_completion(&mut self, cx: &mut Context<Self>) {
        self.load_catalog(cx);
    }

    /// Opens the export of the rows of the statement at the cursor (or the
    /// selection), which runs again for it (DBeaver's "Export from query").
    pub fn export_from_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rules = self.lex_rules(cx);
        let Some(statement) = self.statements(Scope::AtCursor, cx).into_iter().next() else {
            return;
        };
        if classify(&statement.sql, rules).writes {
            window.push_notification(
                Notification::warning(
                    "Export from query: the statement writes; only one that reads can be exported.",
                ),
                cx,
            );
            return;
        }
        if self.target.is_none() {
            window.push_notification(
                Notification::warning("Export from query: choose a connection first."),
                cx,
            );
            return;
        }
        self.export_statement(statement.sql, window, cx);
    }

    /// Puts back the text of the tab's file, read off the UI thread. One
    /// step to undo.
    pub fn revert_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.file.clone() else {
            return;
        };
        let source = path.clone();
        let reading = bridge::spawn_blocking(cx, move || std::fs::read_to_string(&source));
        cx.spawn_in(window, async move |this, cx| {
            let outcome = reading.await;
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok(sql) => {
                    this.editor
                        .update(cx, |editor, cx| editor.replace_all(sql, window, cx));
                    // Once the edit's own event has marked the text changed.
                    cx.defer_in(window, move |this, _, cx| this.set_file(path, false, cx));
                }
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "{} could not be read: {error}",
                        path.display()
                    ))),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Stops what is running or waiting to be paged, keeping what arrived.
    fn stop(&mut self, cx: &mut Context<Self>) {
        // Dropping the run drops the execution, which stops the statement.
        if self.run.take().is_some() {
            self.trace.borrow_mut().stopped(Mark::Cancelled);
            // A target of a run on several connections stops with it (J14).
            self.end_script(Stage::Cancelled, None, cx);
            for pane in &mut self.results {
                // A result kept from an earlier run stays complete.
                if let Outcome::Rows(grid) = &pane.outcome
                    && !grid.read(cx).delegate().is_complete()
                {
                    grid.update(cx, |grid, cx| {
                        grid.delegate_mut().finish(false);
                        // A result that stopped arriving can be sorted: its
                        // columns are described again.
                        grid.refresh(cx);
                        cx.notify();
                    });
                }
            }
        }
        self.receiving = None;
        self.activity = Activity::Idle;
    }

    /// Asks the server to cancel the running statement. It ends with the
    /// server's cancellation error and keeps the rows that arrived.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        // A request to the assistant asked in the editor stops first (K52).
        if let Some(inline) = self.inline.clone()
            && inline.update(cx, |inline, cx| inline.stop(cx))
        {
            return;
        }
        // A count or a list of values: its statement is cancelled, and it
        // says so when it ends.
        if let Some(side) = &self.side {
            if let Some(session) = side.session.clone() {
                bridge::spawn(cx, async move { session.cancel().await }).detach();
            }
            return;
        }
        // A run waiting for its connection is simply forgotten.
        if self.pending.take().is_some() {
            self.end_script(Stage::Cancelled, None, cx);
            cx.notify();
            return;
        }
        if !matches!(self.activity, Activity::Running { .. }) {
            // Nothing is being computed; a result waiting to be paged is
            // simply let go. A target not yet running stops too (J14).
            self.stop(cx);
            self.end_script(Stage::Cancelled, None, cx);
            cx.notify();
            return;
        }
        if let Some(session) = self.session.clone() {
            self.cancel_requested = true;
            bridge::spawn(cx, async move { session.cancel().await }).detach();
        }
    }

    /// Runs the statements of `scope`, one after the other, stopping at
    /// the first that fails.
    pub fn run(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        self.launch(scope, None, false, window, cx);
    }

    /// Runs the statement at the cursor (or the selection) and adds its
    /// results after the ones on screen (DBeaver's `run.statementNew`).
    pub fn run_in_new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.launch(Scope::AtCursor, None, true, window, cx);
    }

    /// Shows the plan of the statement at the cursor. With `analyze` the
    /// statement is run and the plan carries what really happened.
    pub fn explain(&mut self, analyze: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.launch(Scope::AtCursor, Some(analyze), false, window, cx);
    }

    /// Runs the whole text as one target of a run on several connections
    /// (J14), through the usual path (connect, confirmations, history). Two
    /// differences: on a read-only connection each write is refused in its
    /// place and the other statements run; with `continue_on_error` an error
    /// does not stop the statements after it.
    // ponytail: a run the user starts in this tab while it is still a target
    // counts as part of the script; mark the user's own runs if that matters.
    pub(crate) fn run_script(
        &mut self,
        continue_on_error: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.script = Some(ScriptRun::new(continue_on_error));
        self.launch(Scope::All, None, false, window, cx);
        cx.notify();
    }

    /// Where this tab's part of a run on several connections stands.
    pub(crate) fn script(&self) -> Option<&ScriptRun> {
        self.script.as_ref()
    }

    /// The run on several connections in progress here, if any.
    fn scripting(&mut self) -> Option<&mut ScriptRun> {
        self.script.as_mut().filter(|script| script.active())
    }

    /// Ends this tab's part of a run on several connections.
    fn end_script(&mut self, stage: Stage, error: Option<String>, cx: &mut Context<Self>) {
        if let Some(script) = self.scripting() {
            script.end(stage, error);
            cx.notify();
        }
    }

    /// When the tab's saved connection is closed: opens it, with its usual
    /// prompts, and keeps `request` for when it is open (DBeaver connects
    /// on execute, `EDITOR_CONNECT_ON_EXECUTE`). Whether it waits.
    fn connect_first(&mut self, request: Request, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.target.as_ref().map(|target| target.connection.clone()) else {
            return false;
        };
        let connections = self.connections.read(cx);
        if connections.get(&id).is_none() {
            return false;
        }
        match connections.status(&id) {
            Status::Connected { .. } => return false,
            // Already on its way, or asking the user: wait for it.
            Status::Connecting | Status::WaitingForInput => self.pending = Some(request),
            Status::Disconnected | Status::Failed { .. } => {
                self.pending = Some(request);
                self.connections
                    .update(cx, |connections, cx| connections.connect(&id, cx));
            }
        }
        // The results area says it is connecting.
        self.show_results();
        cx.notify();
        true
    }

    /// Does what was asked while the connection was opening, once it is
    /// open; says why nothing ran when it did not open.
    fn resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(request) = self.pending else {
            return;
        };
        let Some(target) = &self.target else {
            return;
        };
        let connections = self.connections.read(cx);
        let saved = connections.get(&target.connection);
        let name = saved
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default();
        let mut detail = None;
        let message = match connections.status(&target.connection) {
            Status::Connecting | Status::WaitingForInput => return,
            Status::Connected { .. } => {
                self.pending = None;
                match request {
                    Request::Launch {
                        scope,
                        explain,
                        keep,
                    } => self.launch(scope, explain, keep, window, cx),
                    Request::ApplySource => self.apply_source(window, cx),
                    Request::CountRows => self.count_rows(window, cx),
                }
                return;
            }
            Status::Failed { message } => {
                // In plain words first, the raw message as the detail.
                let words = match saved {
                    Some(saved) => {
                        let explained = kerno_core::error::explain(&message, &saved.config);
                        format!("{} {}", explained.headline, explained.next_step)
                    }
                    None => "The connection failed.".to_string(),
                };
                detail = Some(message);
                format!(
                    "“{name}” could not connect, and nothing was run. {}",
                    words.trim_end()
                )
            }
            Status::Disconnected => {
                format!("“{name}” was not connected, and nothing was run.")
            }
        };
        self.pending = None;
        if let Request::Launch { keep, .. } = request {
            self.keep_results = keep;
        }
        self.refuse(
            String::new(),
            Failure {
                message,
                code: None,
                detail,
                hint: Some("Run again to connect again.".into()),
                position: None,
            },
            cx,
        );
    }

    fn launch(
        &mut self,
        scope: Scope,
        explain: Option<bool>,
        keep: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Unbound (C6): nothing runs; the bar above the editor has the
        // picker.
        if let Some(text) = self.unbound_text() {
            self.end_script(Stage::Failed, Some(text.to_string()), cx);
            let message = format!("Nothing ran. {text}");
            if self
                .notices
                .last()
                .is_none_or(|last| last.message != message)
            {
                self.notices.push(Notice {
                    severity: "Warning".into(),
                    message,
                    code: None,
                    detail: None,
                });
            }
            self.active_result = self.results.len();
            self.show_results();
            cx.notify();
            return;
        }
        if self.connect_first(
            Request::Launch {
                scope,
                explain,
                keep,
            },
            cx,
        ) {
            return;
        }
        // An old server has no such plan (K22): said before anything runs,
        // here so that a plan asked while the connection was opening is
        // checked once it is open too.
        let unavailable = explain.and_then(|analyze| {
            self.target
                .as_ref()
                .and_then(|target| self.connections.read(cx).meta_session(&target.connection))
                .and_then(|meta| meta.dialect().explain_unavailable(analyze))
        });
        if let Some(reason) = unavailable {
            self.end_script(Stage::Failed, Some(reason.clone()), cx);
            window.push_notification(Notification::warning(SharedString::from(reason)), cx);
            return;
        }
        let mut statements = self.statements(scope, cx);
        if explain.is_some() {
            // One plan at a time: the first statement of a selection.
            statements.truncate(1);
        }
        self.explain_mode = explain;
        self.keep_results = keep;
        if statements.is_empty() {
            self.end_script(Stage::Done, None, cx);
            return;
        }
        self.clear_error_mark(cx);
        // Parameters are asked for before anything runs.
        let rules = self.lex_rules(cx);
        let params: Vec<Vec<Param>> = statements
            .iter()
            .map(|statement| find_params(&statement.sql, rules))
            .collect();
        if params.iter().all(Vec::is_empty) {
            self.execute(statements, true, window, cx);
        } else {
            self.ask_params(statements, params, window, cx);
        }
    }

    /// Asks for the value of each parameter, then runs the statements with
    /// the values written in.
    fn ask_params(
        &mut self,
        statements: Vec<Statement>,
        params: Vec<Vec<Param>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(meta) = self
            .target
            .as_ref()
            .and_then(|target| self.connections.read(cx).meta_session(&target.connection))
        else {
            // No connection to run on: the run says so.
            self.execute(statements, true, window, cx);
            return;
        };
        // A named or numbered parameter is one value for the whole run; a
        // `?` is its own, in its own statement.
        let several = statements.len() > 1;
        let label = move |statement: usize, key: &ParamKey| match key {
            ParamKey::Positional(_) if several => {
                format!("{} · statement {}", key.label(), statement + 1)
            }
            key => key.label(),
        };
        let mut labels: Vec<String> = Vec::new();
        for (statement, params) in params.iter().enumerate() {
            for param in params {
                let label = label(statement, &param.key);
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
        }
        let remembered = labels
            .iter()
            .map(|label| self.param_values.get(label).cloned().unwrap_or_default())
            .collect();
        let view = cx.entity();
        params::open(
            labels.clone(),
            remembered,
            window,
            cx,
            move |values, window, cx| {
                let bound: Vec<Statement> = statements
                    .iter()
                    .zip(&params)
                    .enumerate()
                    .map(|(index, (statement, params))| Statement {
                        sql: bind_params(&statement.sql, params, |key| {
                            let position = labels
                                .iter()
                                .position(|known| *known == label(index, key))?;
                            Some(values[position].to_sql(meta.dialect()))
                        }),
                        place: statement.place.clone(),
                        refused: None,
                    })
                    .collect();
                let typed: Vec<(String, String)> = labels
                    .iter()
                    .zip(&values)
                    .filter_map(|(label, value)| match value {
                        ParamValue::Auto(text) => Some((label.clone(), text.clone())),
                        _ => None,
                    })
                    .collect();
                view.update(cx, |this, cx| {
                    this.param_values.extend(typed);
                    this.execute(bound, true, window, cx);
                });
                // Back to the editor once the prompt is gone, so the
                // keyboard keeps working without a click.
                let editor = view.read(cx).focus_handle(cx);
                window.defer(cx, move |window, cx| editor.focus(window, cx));
            },
        );
    }

    /// Runs `statements` one after the other, stopping at the first that
    /// fails. `from_editor`: the user ran them (not a result read again),
    /// and an environment that confirms every run asks first.
    fn execute(
        &mut self,
        statements: Vec<Statement>,
        from_editor: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.free_to_run(window, cx) {
            return;
        }
        let rules = self.lex_rules(cx);
        let connections = self.connections.read(cx);
        let saved = self
            .target
            .as_ref()
            .and_then(|target| connections.get(&target.connection));
        let meta = self
            .target
            .as_ref()
            .and_then(|target| connections.meta_session(&target.connection));
        let (Some(saved), Some(meta)) = (saved, meta) else {
            // Nothing to run on: the run itself says so.
            self.start(statements, false, window, cx);
            return;
        };
        // What will be sent decides: EXPLAIN ANALYZE runs the statement.
        let explain = self.explain_mode;
        let classes: Vec<StatementClass> = statements
            .iter()
            .map(|statement| match explain {
                Some(analyze) => classify(&meta.dialect().explain(&statement.sql, analyze), rules),
                None => classify(&statement.sql, rules),
            })
            .collect();
        let name = saved.config.name.clone();
        let mut statements = statements;
        if saved.config.read_only
            && let Some(blocked) = classes.iter().position(|class| class.writes)
        {
            if self.script.as_ref().is_none_or(|script| !script.active()) {
                self.refuse_read_only(&statements[blocked].sql, &name, cx);
                return;
            }
            // One target of a run on several connections (J14): each write
            // is refused in its place, and the reads still run.
            for (statement, class) in statements.iter_mut().zip(&classes) {
                if class.writes {
                    statement.refused = Some(read_only_failure(&name));
                }
            }
        }
        // Statements that destroy something are shown and confirmed first;
        // so is anything that writes where the environment asks twice, and
        // every statement run from the editor where it confirms each run.
        let settings = &AppState::get(cx).settings;
        let asks =
            &environment::environment(&settings.environments, &saved.config.environment).rules;
        let ask_destructive = settings.confirm_destructive;
        let ask_all = from_editor && asks.confirm_execute;
        let to_confirm: Vec<(&Statement, &StatementClass)> = statements
            .iter()
            .zip(&classes)
            .filter(|(statement, class)| {
                statement.refused.is_none()
                    && (ask_all
                        || (class.destructive && ask_destructive)
                        || (asks.confirm_writes_twice && class.writes))
            })
            .collect();
        if to_confirm.is_empty() {
            self.start(statements, false, window, cx);
            return;
        }
        let destructive = to_confirm.iter().any(|(_, class)| class.destructive);
        let writes = to_confirm.iter().any(|(_, class)| class.writes);
        let confirmation = Confirmation {
            title: match to_confirm.len() {
                1 => "Run this statement?".to_string(),
                count => format!("Run these {count} statements?"),
            },
            explanation: if destructive {
                "It removes data or objects for good (a DROP, a TRUNCATE, an ALTER that drops, a MERGE \
                 that deletes, or a DELETE or UPDATE without a WHERE). This cannot be undone."
                    .to_string()
            } else if writes {
                "It changes the database.".to_string()
            } else {
                "This connection's environment asks before every statement runs.".to_string()
            },
            statement: ConfirmStatement::Fixed(
                to_confirm
                    .iter()
                    .map(|(statement, _)| statement.sql.as_str())
                    .collect::<Vec<_>>()
                    .join(";\n\n"),
            ),
            action: "Run",
            destructive,
            connection: name,
            environment: saved.config.environment.clone(),
            writes,
        };
        let view = cx.entity();
        let declined = cx.weak_entity();
        if let Some(script) = self.scripting() {
            script.stage = Stage::Asking;
        }
        sql_confirm::open_or_decline(
            confirmation,
            window,
            cx,
            move |_, window, cx| {
                let statements = statements.clone();
                view.update(cx, |this, cx| this.start(statements, false, window, cx));
                let editor = view.read(cx).focus_handle(cx);
                window.defer(cx, move |window, cx| editor.focus(window, cx));
            },
            // A run on several connections skips this target (J14).
            move |cx| {
                let _ = declined.update(cx, |this, cx| {
                    if this
                        .script
                        .as_ref()
                        .is_some_and(|s| s.stage == Stage::Asking)
                    {
                        this.end_script(Stage::Declined, None, cx);
                    }
                });
            },
        );
    }

    /// Whether a run may replace the results. With changes waiting in one
    /// it may not: they are about the rows on screen (as a table's page
    /// keeps its rows while changes wait).
    fn free_to_run(&self, window: &mut Window, cx: &mut App) -> bool {
        if self.pending_count(cx) == 0 {
            return true;
        }
        window.push_notification(
            Notification::warning("Apply or discard the pending changes of the result first."),
            cx,
        );
        false
    }

    /// Shows why a statement was not run on a read-only connection.
    fn refuse_read_only(&mut self, statement: &str, connection: &str, cx: &mut Context<Self>) {
        self.refuse(statement.to_string(), read_only_failure(connection), cx);
    }

    /// Shows why a run did not happen, as its result.
    fn refuse(&mut self, statement: String, failure: Failure, cx: &mut Context<Self>) {
        self.stop(cx);
        self.requery = None;
        self.show_results();
        self.clear_results();
        self.notices.clear();
        let message = failure.message.clone();
        self.push_pane(statement, Outcome::Failed(failure), None, None);
        self.active_result = self.results.len() - 1;
        self.end_script(Stage::Failed, Some(message), cx);
        cx.notify();
    }

    /// The results come back on screen when they were hidden.
    fn show_results(&mut self) {
        if self.panels == Panels::Editor {
            self.panels = Panels::Both;
        }
    }

    /// Shows the messages of the last run, after the result tabs (the
    /// status bar's messages item).
    pub fn show_messages(&mut self, cx: &mut Context<Self>) {
        self.show_results();
        self.active_result = self.results.len();
        cx.notify();
    }

    /// Before a run: the results that are not pinned go, unless the run
    /// keeps them. Their rows are freed with them.
    fn clear_results(&mut self) {
        if !self.keep_results {
            self.results.retain(|pane| pane.pinned);
        }
    }

    /// Adds a result pane at the end; returns its id.
    fn push_pane(
        &mut self,
        statement: String,
        outcome: Outcome,
        rows_affected: Option<u64>,
        elapsed: Option<Duration>,
    ) -> u64 {
        let id = self.next_pane;
        self.next_pane += 1;
        self.results.push(ResultPane {
            id,
            statement,
            outcome,
            rows_affected,
            elapsed,
            name: None,
            pinned: false,
            stack_height: None,
            requery: self.requery.clone(),
            total: None,
            editable: None,
            apply_error: None,
            place: self.place(),
            source: None,
            _grid_events: None,
            _lookup: None,
        });
        id
    }

    fn pane_index(&self, id: u64) -> Option<usize> {
        self.results.iter().position(|pane| pane.id == id)
    }

    fn pane_mut(&mut self, id: u64) -> Option<&mut ResultPane> {
        self.results.iter_mut().find(|pane| pane.id == id)
    }

    fn pane_grid(&self, id: u64) -> Option<Entity<TableState<Grid>>> {
        self.results
            .iter()
            .find(|pane| pane.id == id)
            .and_then(ResultPane::grid)
    }

    /// Runs the whole text, the script of a source, once its SQL was
    /// shown and confirmed. Parameters are not asked for: a source is
    /// sent as it is written. Whatever came of it, the window is told.
    pub fn apply_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source.is_none() || matches!(self.activity, Activity::Running { .. }) {
            return;
        }
        if !self.free_to_run(window, cx) {
            return;
        }
        if self.connect_first(Request::ApplySource, cx) {
            return;
        }
        self.keep_results = false;
        let rules = self.lex_rules(cx);
        let text = self.sql(cx);
        let statements: Vec<Statement> = split_statements(&text, rules)
            .iter()
            .map(|span| Statement {
                sql: span.text(&text).to_string(),
                place: Some(span.range.clone()),
                refused: None,
            })
            .collect();
        if statements.is_empty() {
            return;
        }
        self.explain_mode = None;
        self.clear_error_mark(cx);
        let connections = self.connections.read(cx);
        let Some(saved) = self
            .target
            .as_ref()
            .and_then(|target| connections.get(&target.connection))
        else {
            // Nothing to run on: the run itself says so.
            self.start(statements, true, window, cx);
            return;
        };
        let (name, environment) = (saved.config.name.clone(), saved.config.environment.clone());
        let classes: Vec<StatementClass> = statements
            .iter()
            .map(|statement| classify(&statement.sql, rules))
            .collect();
        if saved.config.read_only {
            let blocked = classes.iter().position(|class| class.writes).unwrap_or(0);
            self.refuse_read_only(&statements[blocked].sql, &name, cx);
            return;
        }
        let destructive = classes.iter().any(|class| class.destructive);
        let in_transaction = classes
            .iter()
            .any(|class| class.kind == StatementKind::Transaction(TxCommand::Begin));
        let confirmation = Confirmation {
            title: format!("Apply the source of “{}”?", self.title),
            explanation: match (destructive, in_transaction) {
                (true, true) => "The object is dropped, then created from this definition, in one \
                                 transaction: if the definition fails, the old object stays."
                    .to_string(),
                (true, false) => {
                    "The object is dropped, then created from this definition. If the \
                                  definition fails, the object stays dropped until it is fixed and \
                                  applied again."
                        .to_string()
                }
                _ => "The object is replaced by this definition.".to_string(),
            },
            statement: ConfirmStatement::Fixed(
                statements
                    .iter()
                    .map(|statement| statement.sql.as_str())
                    .collect::<Vec<_>>()
                    .join(";\n\n"),
            ),
            action: "Apply source",
            destructive,
            connection: name,
            environment,
            writes: true,
        };
        let view = cx.entity();
        sql_confirm::open(confirmation, window, cx, move |_, window, cx| {
            let statements = statements.clone();
            view.update(cx, |this, cx| this.start(statements, true, window, cx));
            let editor = view.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| editor.focus(window, cx));
        });
    }

    /// Runs the statements, guards passed. `applying` a source: the window
    /// is told once it ran.
    fn start(
        &mut self,
        statements: Vec<Statement>,
        applying: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop(cx);
        // A run from the editor replaces the marks of the last one, and marks
        // its own (an edit clears them in place); a result read again (no
        // place in the text) marks nothing and leaves them (K37).
        let trace = statements
            .iter()
            .any(|statement| statement.place.is_some())
            .then(|| {
                let places = statements
                    .iter()
                    .enumerate()
                    .filter_map(|(index, statement)| Some((index, statement.place.clone()?)));
                self.trace = Rc::new(RefCell::new(Trace::new(
                    self.editor.read(cx).text(),
                    places,
                )));
                self.trace.clone()
            });
        let mark = move |index: usize, mark: Mark| {
            if let Some(trace) = &trace {
                trace.borrow_mut().set(index, mark);
            }
        };
        self.generation += 1;
        self.requery = self.next_requery.take();
        self.widths = std::mem::take(&mut self.next_widths);
        self.carried_find = self.next_find.take();
        self.carried_scroll = self.next_scroll.take();
        self.unstacked = false;
        self.clear_results();
        self.notices.clear();
        // The first result of this run is shown; the editor and the
        // results are both on screen again, as DBeaver does on execute.
        self.active_result = self.results.len();
        self.panels = Panels::Both;
        self.cancel_requested = false;
        let total = statements.len();
        self.activity = Activity::Running {
            index: 0,
            total,
            since: Instant::now(),
        };
        let page = u64::from(AppState::get(cx).settings.page_size).max(1);
        let manual = self.manual_commit;
        let explain = self.explain_mode;
        let rules = self.lex_rules(cx);
        let session = self.session(cx);
        // A target of a run on several connections (J14) may go on after an
        // error, and frees its session when its last result is paged.
        let script = self.scripting().map(|script| {
            script.stage = Stage::Running;
            script.continue_on_error
        });
        let continue_on_error = script == Some(true);
        self.run = Some(cx.spawn_in(window, async move |this, cx| {
            let session = match session.await {
                Ok(session) => session,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.push_pane(
                            String::new(),
                            Outcome::Failed(Failure::from(&error)),
                            None,
                            None,
                        );
                        this.activity = Activity::Idle;
                        this.end_script(Stage::Failed, Some(error.to_string()), cx);
                        cx.notify();
                    });
                    return;
                }
            };
            // In manual-commit mode the first statement opens a transaction.
            if manual && session.tx_state() == TxState::Idle {
                let begin = session.dialect().begin_sql();
                let Ok(beginning) =
                    cx.update(|_, cx| bridge::spawn(cx, exec::collect(session.clone(), begin)))
                else {
                    return;
                };
                if let Err(error) = beginning.await {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.push_pane(
                            begin.to_string(),
                            Outcome::Failed(Failure::from(&error)),
                            None,
                            None,
                        );
                        this.activity = Activity::Idle;
                        this.run = None;
                        this.end_script(Stage::Failed, Some(error.to_string()), cx);
                        // The session is gone (the server closed it, C15):
                        // dropped, so the next run opens a new one.
                        if error.is_disconnect() {
                            this.lost_connection(window, cx);
                        }
                        cx.notify();
                    });
                    return;
                }
                // A new transaction: its log starts empty, whatever ended
                // the last one (a lost connection, a rollback on close).
                // Its age starts here, before its first statement ends.
                let _ = this.update(cx, |this, cx| {
                    this.tx_log.clear();
                    this.note_transaction(cx);
                    cx.notify();
                });
            }
            let mut changed_schema = false;
            let mut any_failed = false;
            let mut cancelled = false;
            for (index, statement) in statements.into_iter().enumerate() {
                // A write refused on a read-only target is shown, not sent.
                if let Some(refusal) = statement.refused.clone() {
                    let _ = this.update(cx, |this, cx| {
                        this.push_pane(statement.sql.clone(), Outcome::Failed(refusal), None, None);
                        mark(index, Mark::Refused);
                        if let Some(script) = this.scripting() {
                            script.refused += 1;
                        }
                        cx.notify();
                    });
                    continue;
                }
                let started = Instant::now();
                let started_at_ms = kerno_core::storage::now_ms();
                // How long the statement took: until its first page when its rows
                // come by pages. Later pages come when the user scrolls, and the
                // time spent reading is not the statement's.
                let mut took: Option<Duration> = None;
                // How the statement ended, for the history and the editor.
                let mut ended: Option<(HistoryStatus, Option<Failure>)> = None;
                let mut row_count: Option<u64> = None;
                let kind = classify(&statement.sql, rules).kind;
                // Only statements that change rows have a count worth showing.
                let counts_rows = matches!(
                    kind,
                    StatementKind::Insert
                        | StatementKind::Update
                        | StatementKind::Delete
                        | StatementKind::Merge
                        | StatementKind::Copy
                );
                // The pane the statement's rows went to, for its plan.
                let mut rows_pane: Option<u64> = None;
                let _ = this.update(cx, |this, cx| {
                    this.activity = Activity::Running {
                        index,
                        total,
                        since: started,
                    };
                    mark(index, Mark::Running);
                    cx.notify();
                });
                // A plan is asked for by wrapping the statement.
                let sent = match explain {
                    Some(analyze) => session.dialect().explain(&statement.sql, analyze),
                    None => statement.sql.clone(),
                };
                let starting = {
                    let (session, sql) = (session.clone(), sent.clone());
                    cx.update(|_, cx| {
                        crate::bench::sent(cx);
                        // `exec::run` spawns the statement: it needs the runtime.
                        bridge::spawn(
                            cx,
                            async move { exec::run(session, sql, RowLimit::Rows(page)) },
                        )
                    })
                };
                let Ok(starting) = starting else { return };
                let mut execution = starting.await;
                let demand = execution.demand();
                // The pane of the result set being received.
                let mut pane: Option<u64> = None;
                let mut failed = false;
                while let Some(event) = execution.next().await {
                    let last = index + 1 == total;
                    let keep_going = this.update_in(cx, |this, window, cx| {
                        match event {
                            ExecEvent::Columns(columns) => {
                                crate::bench::columns(cx);
                                let grid = Grid::new(
                                    ResultStore::new(columns),
                                    demand.clone(),
                                    page,
                                    session.clone(),
                                );
                                pane = Some(this.open_grid(&statement.sql, grid, window, cx));
                                rows_pane = pane;
                                this.receiving = pane;
                            }
                            ExecEvent::Rows(batch) => {
                                crate::bench::rows(window, cx);
                                if let Some(id) = pane
                                    && let Some(grid) = this.pane_grid(id)
                                {
                                    let paused = grid.update(cx, |grid, cx| {
                                        grid.delegate_mut().push(batch);
                                        let paused = grid.delegate().has_more();
                                        // The first page sets the widths of the
                                        // columns (more pages keep them).
                                        if paused && grid.delegate().row_count() as u64 <= page {
                                            grid.refresh(cx);
                                        }
                                        cx.notify();
                                        paused
                                    });
                                    if paused {
                                        // The rows asked for are here: the tab is not
                                        // waiting for the server any more.
                                        this.activity = Activity::Idle;
                                        mark(index, Mark::Done);
                                        let took = *took.get_or_insert_with(|| started.elapsed());
                                        if let Some(pane) = this.pane_mut(id) {
                                            pane.elapsed = Some(took);
                                        }
                                        // In a script the next statement needs the
                                        // session: this one stops at its first page.
                                        // So does the last of a run on several
                                        // connections, which then ends.
                                        if !last || script.is_some() {
                                            if script.is_some() {
                                                row_count = Some(
                                                    grid.read(cx).delegate().row_count() as u64,
                                                );
                                            }
                                            grid.update(cx, |grid, cx| {
                                                grid.delegate_mut().finish(false);
                                                grid.refresh(cx);
                                            });
                                            this.receiving = None;
                                            cx.notify();
                                            return false;
                                        }
                                    }
                                }
                            }
                            ExecEvent::Complete { rows_affected } => match pane.take() {
                                Some(id) => {
                                    this.receiving = None;
                                    if let Some(grid) = this.pane_grid(id) {
                                        grid.update(cx, |grid, cx| {
                                            grid.delegate_mut().finish(true);
                                            grid.refresh(cx);
                                            cx.notify();
                                        });
                                        row_count = rows_affected
                                            .or(Some(grid.read(cx).delegate().row_count() as u64));
                                    }
                                    if let Some(pane) = this.pane_mut(id) {
                                        pane.rows_affected = rows_affected;
                                        pane.elapsed =
                                            Some(*took.get_or_insert_with(|| started.elapsed()));
                                    }
                                }
                                None => {
                                    row_count = rows_affected;
                                    this.push_pane(
                                        statement.sql.clone(),
                                        Outcome::Done,
                                        rows_affected.filter(|_| counts_rows),
                                        Some(started.elapsed()),
                                    );
                                }
                            },
                            ExecEvent::Notice(notice) => this.notices.push(notice),
                            ExecEvent::Finished(Ok(())) => {
                                crate::bench::complete(cx);
                                // Some statements end quietly when cancelled
                                // (MySQL's SLEEP returns early): it was still a
                                // cancellation, and the script stops there.
                                if std::mem::take(&mut this.cancel_requested) {
                                    failed = true;
                                    ended = Some((HistoryStatus::Cancelled, None));
                                    this.push_pane(
                                        statement.sql.clone(),
                                        Outcome::Cancelled,
                                        None,
                                        Some(started.elapsed()),
                                    );
                                    this.active_result = this.results.len() - 1;
                                }
                            }
                            ExecEvent::Finished(Err(error)) => {
                                failed = true;
                                this.receiving = None;
                                // Rows that arrived before the failure stay; a
                                // result without any goes.
                                if let Some(id) = pane
                                    && let Some(grid) = this.pane_grid(id)
                                {
                                    let empty = grid.update(cx, |grid, cx| {
                                        grid.delegate_mut().finish(false);
                                        grid.refresh(cx);
                                        grid.delegate().row_count() == 0
                                    });
                                    if empty {
                                        this.results.retain(|pane| pane.id != id);
                                    }
                                }
                                // The server words a cancellation its own way
                                // (an error on PostgreSQL); the tab knows it asked.
                                let cancelled = std::mem::take(&mut this.cancel_requested)
                                    || matches!(error, Error::Cancelled);
                                let lost = error.is_disconnect();
                                let failure = if lost {
                                    lost_failure(Some(&error))
                                } else {
                                    Failure::from(&error)
                                };
                                ended = Some(if cancelled {
                                    (HistoryStatus::Cancelled, None)
                                } else {
                                    (HistoryStatus::Error, Some(failure.clone()))
                                });
                                let id = this.push_pane(
                                    statement.sql.clone(),
                                    if cancelled {
                                        Outcome::Cancelled
                                    } else {
                                        Outcome::Failed(failure)
                                    },
                                    None,
                                    Some(started.elapsed()),
                                );
                                // What the server refused, where the editor has it (K52).
                                if !cancelled
                                    && !lost
                                    && let Some(pane) = this.pane_mut(id)
                                {
                                    // Not an EXPLAIN's: its error is of the wrapped text.
                                    pane.source =
                                        statement.place.clone().filter(|_| explain.is_none());
                                }
                                this.active_result = this.results.len() - 1;
                                if lost {
                                    this.lost_connection(window, cx);
                                }
                            }
                        }
                        cx.notify();
                        true
                    });
                    if !matches!(keep_going, Ok(true)) {
                        break;
                    }
                }
                if explain.is_some()
                    && let Some(pane) = rows_pane
                {
                    let dialect_of = session.clone();
                    let _ = this.update(cx, |this, cx| {
                        this.show_plan(pane, dialect_of.dialect(), explain == Some(true), cx)
                    });
                }
                let (status, failure) = ended.unwrap_or((HistoryStatus::Ok, None));
                // A table or view created, changed or dropped changes what
                // completion offers.
                changed_schema |= kind == StatementKind::Ddl
                    && explain.is_none()
                    && matches!(status, HistoryStatus::Ok);
                let _ = this.update(cx, |this, cx| {
                    mark(
                        index,
                        match status {
                            HistoryStatus::Ok => Mark::Done,
                            HistoryStatus::Error => Mark::Failed,
                            HistoryStatus::Cancelled => Mark::Cancelled,
                        },
                    );
                    let ended = Ended {
                        started_at_ms,
                        elapsed: took.unwrap_or_else(|| started.elapsed()),
                        status,
                        failure,
                        // The lines of a plan are not rows of data.
                        row_count: row_count.filter(|_| explain.is_none()),
                    };
                    // The history holds what was sent: an EXPLAIN is one.
                    let recorded = Statement {
                        sql: sent.clone(),
                        place: statement.place.clone().filter(|_| explain.is_none()),
                        refused: None,
                    };
                    this.statement_ended(&recorded, ended, cx)
                });
                if failed {
                    any_failed = true;
                    cancelled |= status == HistoryStatus::Cancelled;
                    if cancelled || !continue_on_error {
                        break;
                    }
                }
            }
            // A source that failed inside its own transaction leaves nothing
            // half done, and no lock held: the transaction is rolled back.
            if applying && any_failed && !manual && session.tx_state() == TxState::Failed {
                let rolling = cx
                    .update(|_, cx| bridge::spawn(cx, exec::collect(session.clone(), "ROLLBACK")));
                if let Ok(rolling) = rolling
                    && let Err(error) = rolling.await
                {
                    tracing::warn!(%error, "the failed source was not rolled back");
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.run = None;
                // After the rollback of a failed source, too.
                this.note_transaction(cx);
                let ended = if cancelled {
                    Stage::Cancelled
                } else if any_failed && !continue_on_error {
                    Stage::Failed
                } else {
                    Stage::Done
                };
                this.end_script(ended, None, cx);
                if changed_schema {
                    this.load_catalog(cx);
                }
                if applying {
                    cx.emit(QueryTabEvent::SourceApplied);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Records a statement in the history and, when the server said where
    /// it failed, marks the place in the editor.
    fn statement_ended(&mut self, statement: &Statement, ended: Ended, cx: &mut Context<Self>) {
        let Ended {
            started_at_ms,
            elapsed,
            status,
            failure,
            row_count,
        } = ended;
        if let Some(script) = self.scripting() {
            script.record(
                elapsed,
                row_count,
                failure.as_ref().map(|f| f.message.as_str()),
            );
        }
        // Auto-refresh reads the same statement again and again: the
        // history and the transaction's log keep the first run only.
        let auto = self.requery.as_ref().is_some_and(|requery| requery.auto);
        if let Some(target) = self.target.as_ref().filter(|_| !auto) {
            let entry = HistoryEntry {
                id: 0,
                connection_id: target.connection.clone(),
                database: target.database.clone(),
                sql: statement.sql.clone(),
                started_at_ms,
                duration_ms: elapsed.as_millis() as i64,
                status,
                row_count: row_count.map(|rows| rows as i64),
                error: failure.as_ref().map(|failure| failure.message.clone()),
            };
            let (storage, rules) = (AppState::get(cx).storage.clone(), self.lex_rules(cx));
            bridge::spawn_blocking(cx, move || {
                if let Err(error) = storage.add_history(&entry, rules) {
                    tracing::warn!(%error, "the statement was not added to the history");
                }
            })
            .detach();
        }
        // A statement run in a transaction joins its log, in either commit
        // mode (a `BEGIN` typed in auto-commit begins one, which the idle
        // rule watches too, C15); one that ended the transaction (COMMIT
        // typed, MySQL's DDL) empties it.
        if !auto {
            // A lost transaction's log is shown until the next statement.
            self.tx_lost = false;
            if self.tx_state() == TxState::Idle {
                self.tx_log.clear();
            } else {
                // This statement began it (none was dated yet): nothing of
                // an earlier one, ended out of sight (a lost session, a
                // failed source rolled back), stays in its log.
                if self.tx_started.is_none() {
                    self.tx_log.clear();
                }
                self.tx_log.push(TxLogEntry::new(
                    &statement.sql,
                    self.lex_rules(cx),
                    started_at_ms,
                    elapsed,
                    row_count,
                    status,
                    failure.as_ref().map(|failure| failure.message.clone()),
                ));
            }
        }
        if let (Some(place), Some(failure)) = (&statement.place, &failure)
            && let Some(position) = failure.position
        {
            self.mark_error(place.start, &statement.sql, position, &failure.message, cx);
        }
        // A `BEGIN` typed in auto-commit opens one; a `COMMIT` typed, or
        // MySQL's DDL, ends it.
        self.note_transaction(cx);
        self.follow_session(cx);
    }

    /// C6: the session moved since it was last seen (`USE`, `SET
    /// search_path`, the end of a transaction undoing a `SET LOCAL`): the
    /// tab's target follows it and says so, so that the picker shows where
    /// statements run and a new session (a reconnect, a toggle, a restart)
    /// opens there again. Nothing is known when the session has no database
    /// or no schema (no schema of the path exists): the target stays.
    fn follow_session(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let now = (session.database(), session.schema());
        let before = std::mem::replace(&mut self.session_place, now.clone());
        let Some(target) = self.target.as_mut() else {
            return;
        };
        let mut moves = Vec::new();
        if let Some(database) = now.0
            && before.0.as_ref() != Some(&database)
            && target.database.as_ref() != Some(&database)
        {
            moves.push(format!("Now in database {database} (USE)."));
            target.database = Some(database);
        }
        if let Some(schema) = now.1
            && before.1.as_ref() != Some(&schema)
            && target.schema.as_ref() != Some(&schema)
        {
            moves.push(format!("Now in schema {schema} (search_path)."));
            target.schema = Some(schema);
        }
        if moves.is_empty() {
            return;
        }
        for message in moves {
            self.notices.push(Notice {
                severity: "Info".into(),
                message,
                code: None,
                detail: None,
            });
        }
        // Completion searches the new place first.
        self.load_catalog(cx);
        cx.emit(QueryTabEvent::Moved);
        cx.notify();
    }

    /// The statements run in the open transaction; none when no
    /// transaction is open.
    pub fn transaction_log(&self) -> &[TxLogEntry] {
        if self.tx_state() == TxState::Idle && !self.tx_lost {
            &[]
        } else {
            &self.tx_log
        }
    }

    /// Shows the statements of the open transaction (DBeaver's
    /// "Transaction log"), or of the one the server rolled back (C15).
    pub fn show_transaction_log(&self, window: &mut Window, cx: &mut App) {
        let state = if self.tx_state() != TxState::Idle {
            tx_log::Shown::Open
        } else if self.tx_lost {
            tx_log::Shown::Lost
        } else {
            tx_log::Shown::None
        };
        tx_log::open(self.transaction_log().to_vec(), state, window, cx);
    }

    /// Underlines the place of an error in the editor. `position` is the
    /// 1-based character the server pointed at in `statement`, which starts
    /// at byte `origin` of the editor's text.
    fn mark_error(
        &mut self,
        origin: usize,
        statement: &str,
        position: u32,
        message: &str,
        cx: &mut Context<Self>,
    ) {
        let at = statement
            .char_indices()
            .nth(position.saturating_sub(1) as usize)
            .map_or(statement.len(), |(byte, _)| byte);
        // The word (or single character) the server pointed at.
        let word = statement[at..]
            .char_indices()
            .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
            .map_or(statement.len() - at, |(end, _)| end)
            .max(statement[at..].chars().next().map_or(0, char::len_utf8));
        let message = SharedString::from(message.to_string());
        // "Fix with AI" beside the mark, for the failed result last added (K52).
        let failed = (self.results.last())
            .filter(|pane| pane.source.is_some() && matches!(pane.outcome, Outcome::Failed(_)))
            .map(|pane| pane.id);
        let marked = self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let (start, end) = (origin + at, (origin + at + word).min(text.len()));
            // The text changed since the run started, or the statement sent
            // is not the one written (parameters bound): nothing sure to mark.
            if text.get(origin..origin + statement.len()) != Some(statement) {
                return false;
            }
            let range = position_of(&text, start)..position_of(&text, end);
            if let Some(diagnostics) = editor.diagnostics_mut() {
                diagnostics.clear();
                diagnostics.push(
                    // An error: the danger colour, not the hint's.
                    Diagnostic::new(range, message).with_severity(input::DiagnosticSeverity::Error),
                );
            }
            cx.notify();
            true
        });
        self.error_marked = true;
        self.error_fix = failed.filter(|_| marked).map(|id| (id, origin + at));
    }

    fn clear_error_mark(&mut self, cx: &mut Context<Self>) {
        self.error_fix = None;
        if std::mem::take(&mut self.error_marked) {
            self.editor.update(cx, |editor, cx| {
                if let Some(diagnostics) = editor.diagnostics_mut() {
                    diagnostics.clear();
                }
                cx.notify();
            });
        }
    }

    /// Whether statements run in a transaction the user ends.
    pub fn manual_commit(&self) -> bool {
        self.manual_commit
    }

    /// The state of the tab's transaction.
    pub fn tx_state(&self) -> TxState {
        self.session
            .as_ref()
            .map_or(TxState::Idle, |session| session.tx_state())
    }

    /// Records when the tab's transaction began (K4): now, when one is open
    /// and none was recorded; nothing once none is open. A transaction that
    /// failed, or went back to a savepoint, keeps its start. The executor's
    /// clock, so tests move it.
    fn note_transaction(&mut self, cx: &App) {
        if self.tx_state() == TxState::Idle {
            self.tx_started = None;
        } else if self.tx_started.is_none() {
            self.tx_started = Some(cx.background_executor().now());
        }
    }

    /// How long the tab's transaction has been open; `None` without one.
    pub fn transaction_age(&self, cx: &App) -> Option<Duration> {
        let started = self
            .tx_started
            .filter(|_| self.tx_state() != TxState::Idle)?;
        Some(
            cx.background_executor()
                .now()
                .saturating_duration_since(started),
        )
    }

    /// Whether a statement holds the tab's session: running, or a result
    /// waiting to be paged; for tests.
    #[cfg(test)]
    pub fn holds_session(&self) -> bool {
        self.run.is_some()
    }

    /// Whether a statement is running.
    pub fn running(&self) -> bool {
        matches!(self.activity, Activity::Running { .. })
    }

    /// What the status bar says while the tab waits for the server: which
    /// statement of the run, and for how long it has been running (K37).
    pub fn running_text(&self) -> Option<String> {
        let Activity::Running {
            index,
            total,
            since,
        } = self.activity
        else {
            return None;
        };
        let time = duration_text(since.elapsed());
        Some(if total > 1 {
            format!("running statement {} of {total} · {time}", index + 1)
        } else {
            format!("running · {time}")
        })
    }

    /// What the marks of the last run from the editor say, in words, once
    /// it ended (K37).
    pub fn trace_text(&self, cx: &App) -> Option<String> {
        if self.running() {
            return None;
        }
        self.trace
            .borrow()
            .summary(self.editor.read(cx).text().len())
    }

    /// The marks of the last run from the editor, in the order of the
    /// text; for tests.
    #[cfg(test)]
    pub fn run_marks(&self) -> Vec<Option<Mark>> {
        self.trace.borrow().marks()
    }

    /// The marks of the last run, to read or set; for tests.
    #[cfg(test)]
    pub fn trace_for_tests(&self) -> &Rc<RefCell<Trace>> {
        &self.trace
    }

    /// Draws the tab again every [`RUN_TICK`] while a statement runs.
    fn tick_while_running(&mut self, cx: &mut Context<Self>) {
        if self.ticking || !self.running() {
            return;
        }
        self.ticking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(RUN_TICK).await;
                let going = this.update(cx, |this, cx| {
                    this.ticking = this.running();
                    if this.ticking {
                        cx.notify();
                    }
                    this.ticking
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// The databases the tab can switch to.
    pub fn databases(&self) -> &[String] {
        &self.databases
    }

    /// The schemas the tab can switch to; for tests.
    #[cfg(test)]
    pub fn schemas(&self) -> &[String] {
        &self.schemas
    }

    /// What the editor's menu depends on.
    fn editor_facts(&self, cx: &App) -> EditorFacts {
        let editor = self.editor.read(cx);
        EditorFacts {
            // Running connects a saved connection first.
            connected: self
                .target
                .as_ref()
                .is_some_and(|target| self.connections.read(cx).get(&target.connection).is_some()),
            running: self.running(),
            selection: !editor.selected_range().is_empty(),
            empty: editor.text().len() == 0,
            saved_file: self.file.is_some() && !self.unsaved,
            source: self.source.is_some(),
            file: self.file.is_some(),
            more_rows: self
                .active_grid()
                .is_some_and(|grid| grid.read(cx).delegate().has_more()),
        }
    }

    /// What the editor's menu (and the main toolbar's pickers) offer to
    /// run on.
    pub(crate) fn choices(&self, cx: &App) -> Choices {
        let connections = self.connections.read(cx);
        let target = self.target.as_ref();
        Choices {
            connections: self.connection_choices(cx),
            connection: target.map(|target| target.connection.clone()),
            databases: self.databases.clone(),
            database: target.and_then(|target| {
                target.database.clone().or_else(|| {
                    connections
                        .meta_session(&target.connection)
                        .and_then(|meta| meta.database())
                })
            }),
            schemas: self.schemas.clone(),
            schema: target.and_then(|target| target.schema.clone()),
        }
    }

    /// The editor's menu, built when it opens. Its entries run as their
    /// commands do: through the editor, which has the focus.
    fn editor_menu(
        &self,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.entity();
        move |menu, window, cx| {
            let tab = view.read(cx);
            let entries = tabs::editor_menu(tab.editor_facts(cx), &tab.choices(cx));
            let view = view.clone();
            let menu = tabs::fill_menu(
                menu,
                entries,
                Rc::new(move |action, window, cx| {
                    let view = view.clone();
                    // Once the menu has given the focus back to the editor.
                    window.defer(cx, move |window, cx| {
                        run_editor_action(&view, action, window, cx)
                    });
                }),
                window,
                cx,
            );
            let menu = crate::ai_view::editor_entries(menu, window, cx);
            let menu = super::ai_inline::editor_entries(menu, cx);
            tabs::fit_window(menu, window)
        }
    }

    /// The isolation level of the tab's session, as the server gave it;
    /// `None` before the session opens.
    pub fn isolation(&self) -> Option<IsolationLevel> {
        self.isolation
    }

    /// Why the isolation level cannot be changed now, when it cannot: the
    /// session must be free and outside a transaction.
    pub fn isolation_why_not(&self, cx: &App) -> Option<&'static str> {
        if self.target.is_none() {
            Some("no connection")
        } else if !self.connected(cx) {
            Some("not connected")
        } else if self.tx_state() != TxState::Idle {
            Some("a transaction is open")
        } else if self.running() {
            Some("running")
        } else {
            // A result still being paged holds the session.
            self.run.is_some().then_some("a result is being read")
        }
    }

    /// Gives the tab's session `level` (opened first if needed), for this
    /// session only, then shows the level the server reads back. Its next
    /// sessions on the same connection take it too.
    pub fn set_isolation(
        &mut self,
        level: IsolationLevel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(reason) = self.isolation_why_not(cx) {
            window.push_notification(
                Notification::warning(SharedString::from(format!("Isolation level: {reason}."))),
                cx,
            );
            return;
        }
        self.chosen_isolation = Some(level);
        let opening = self.session(cx);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = match opening.await {
                Ok(session) => match cx
                    .update(|_, cx| bridge::spawn(cx, exec::set_isolation(session, level)))
                {
                    Ok(setting) => setting.await,
                    Err(_) => return,
                },
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                match outcome {
                    Ok(isolation) => this.isolation = isolation,
                    Err(error) => window.push_notification(
                        crate::error_notification(format!("Isolation level: {}", describe(&error))),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Statements run in a transaction the user ends (`true`), or each is
    /// committed as it runs. Switches at once: what the user picks goes
    /// through [`Self::request_manual_commit`].
    pub fn set_manual_commit(&mut self, manual: bool, cx: &mut Context<Self>) {
        self.manual_commit = manual;
        cx.notify();
    }

    /// What every switch of the commit mode does (both toolbars, the status
    /// bar, the palette and its key): to auto-commit with a transaction
    /// open, lets the window ask how to end it first (C15), since later
    /// statements would join it unseen; otherwise switches now. To manual
    /// commit nothing is lost: an open transaction goes on.
    pub fn request_manual_commit(&mut self, manual: bool, cx: &mut Context<Self>) {
        if !manual && self.manual_commit && self.tx_state() != TxState::Idle {
            cx.emit(QueryTabEvent::SwitchToAutoCommit);
        } else {
            self.set_manual_commit(manual, cx);
        }
    }

    /// The period after which the environment rolls back this tab's open
    /// transaction: in either commit mode (a `BEGIN` typed in auto-commit
    /// too, C15), while nothing runs, once a statement of the transaction
    /// changed something (DBeaver's condition, ported from
    /// `org.jkiss.dbeaver.model/.../runtime/jobs/DataSourceMonitorJob.java`:
    /// a transaction that only read is left alone).
    fn idle_period(&self, cx: &App) -> Option<u32> {
        if self.running() || self.tx_state() == TxState::Idle {
            return None;
        }
        let rules = self.lex_rules(cx);
        if !self
            .tx_log
            .iter()
            .any(|entry| classify(&entry.sql, rules).writes)
        {
            return None;
        }
        let saved = self
            .connections
            .read(cx)
            .get(&self.target.as_ref()?.connection)?;
        let environments = &AppState::get(cx).settings.environments;
        environment::environment(environments, &saved.config.environment)
            .rules
            .close_idle_transactions
            .filter(|seconds| *seconds > 0)
    }

    /// Arms the rollback of an idle transaction when one is due, restarts
    /// it after each statement (the log grows), and drops it when the
    /// transaction ends or a statement runs.
    fn watch_idle_transaction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let due = self
            .idle_period(cx)
            .map(|period| (self.tx_log.len(), period));
        if self.idle_rollback.as_ref().map(|(armed, _)| *armed) == due {
            return;
        }
        self.idle_rollback = due.map(|armed| {
            let wait = Duration::from_secs(armed.1.into());
            let rollback = cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.end_idle_transaction(armed, window, cx)
                });
            });
            (armed, rollback)
        });
    }

    /// Rolls back the transaction left idle for the environment's period,
    /// and says so in the tab and in a notification (DBeaver's
    /// `EndIdleTransactionsJob` rolls back too).
    fn end_idle_transaction(
        &mut self,
        armed: (usize, u32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A statement ran meanwhile: its own wait is armed.
        if self
            .idle_period(cx)
            .map(|period| (self.tx_log.len(), period))
            != Some(armed)
        {
            return;
        }
        let environment = self
            .target
            .as_ref()
            .and_then(|target| self.connections.read(cx).get(&target.connection))
            .map(|saved| {
                let environments = &AppState::get(cx).settings.environments;
                environment::environment(environments, &saved.config.environment)
                    .name
                    .clone()
            })
            .unwrap_or_default();
        let idle = match armed.1 {
            seconds if seconds % 60 == 0 => {
                let minutes = seconds / 60;
                format!("{minutes} minute{}", if minutes == 1 { "" } else { "s" })
            }
            seconds => format!("{seconds} second{}", if seconds == 1 { "" } else { "s" }),
        };
        let message = format!(
            "The transaction was left idle for {idle}: it was rolled back, as the {environment} \
             environment asks, and nothing of it was committed."
        );
        let ending = self.finish_transaction(false, None, cx);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = ending.await;
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok(()) => {
                    this.notices.push(Notice {
                        severity: "Warning".into(),
                        message: message.clone(),
                        code: None,
                        detail: None,
                    });
                    // The messages, after the results.
                    this.active_result = this.results.len();
                    this.show_results();
                    window.push_notification(
                        Notification::warning(SharedString::from(format!(
                            "“{}”: {message}",
                            this.title
                        ))),
                        cx,
                    );
                    cx.notify();
                }
                Err(error) => window.push_notification(crate::error_notification(error), cx),
            });
        })
        .detach();
    }

    /// Commits or rolls back the tab's open transaction.
    pub fn end_transaction(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ending = self.finish_transaction(commit, None, cx);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = ending.await {
                let _ = cx.update(|window, cx| {
                    window.push_notification(crate::error_notification(error), cx)
                });
            }
        })
        .detach();
    }

    /// Commits or rolls back the tab's open transaction; done at once when
    /// none is open. The error says which statement failed, or that the
    /// server did not answer within `limit`.
    pub fn finish_transaction(
        &mut self,
        commit: bool,
        limit: Option<Duration>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(session) = self.session.clone() else {
            return Task::ready(Ok(()));
        };
        // Nothing open, and nothing the tab held ended out of its sight (a
        // session the server closed says `Idle`, C15).
        if session.tx_state() == TxState::Idle && self.tx_started.is_none() {
            return Task::ready(Ok(()));
        }
        // A result still being paged belongs to the transaction.
        self.stop(cx);
        let sql = if commit { "COMMIT" } else { "ROLLBACK" };
        let checked = session.clone();
        let ending = bridge::spawn(cx, async move {
            let ending = async {
                // A COMMIT is not sent on a session the server closed: the
                // transaction is gone, and saying so beats "whether it was
                // saved is not known" (C15). A failed check says nothing.
                if commit
                    && let Ok(Some(why)) = tokio::time::timeout(GONE_CHECK, session.gone()).await
                {
                    return Ending::Gone(why);
                }
                match exec::collect(session.clone(), sql).await {
                    Ok(_) => Ending::Done,
                    // A closed session's transaction is rolled back: what a
                    // ROLLBACK asks for.
                    Err(Error::Closed(_)) if !commit => Ending::Closed,
                    // What came of the transaction is the session's state
                    // after the failure (C14).
                    Err(error) => Ending::Failed(ending_failed(commit, &error, session.tx_state())),
                }
            };
            match limit {
                Some(limit) => tokio::time::timeout(limit, ending)
                    .await
                    .unwrap_or_else(|_| {
                        Ending::Failed(format!("{sql} got no answer within {} s", limit.as_secs()))
                    }),
                None => ending.await,
            }
        });
        cx.spawn(async move |this, cx| {
            let ended = ending.await;
            this.update(cx, |this, cx| {
                let outcome = match ended {
                    Ending::Done => Ok(()),
                    Ending::Failed(text) => Err(text),
                    Ending::Closed | Ending::Gone(_) => {
                        let own = this
                            .session
                            .as_ref()
                            .is_some_and(|own| Arc::ptr_eq(own, &checked));
                        let outcome = match &ended {
                            Ending::Gone(why) => {
                                let said = if own {
                                    this.note_rolled_back(SESSION_CLOSED, Some(why), cx)
                                } else {
                                    format!("{SESSION_CLOSED}\n{}", closed_why(why))
                                };
                                Err(format!("The commit was not sent. {said}"))
                            }
                            _ => Ok(()),
                        };
                        if own {
                            this.close_session(cx);
                        }
                        outcome
                    }
                };
                // A failed COMMIT the server rolled back ended it too.
                if (outcome.is_ok() || this.tx_state() == TxState::Idle) && !this.tx_lost {
                    this.tx_log.clear();
                }
                this.note_transaction(cx);
                // A `SET LOCAL search_path` ended with the transaction.
                this.follow_session(cx);
                cx.notify();
                outcome
            })
            .unwrap_or(Ok(()))
        })
    }

    /// Replaces the rows an EXPLAIN returned by its plan as a tree.
    fn show_plan(
        &mut self,
        pane: u64,
        dialect: &dyn Dialect,
        analyze: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.pane_grid(pane) else {
            return;
        };
        // The output is one cell, or one line per row for text plans; a row
        // of several columns (SQLite's id, parent, notused, detail) is one
        // line of its cells separated by tabs.
        let raw = {
            let grid = grid.read(cx).delegate();
            (0..grid.row_count())
                .filter_map(|row| {
                    let cells: Vec<Option<&[u8]>> = (1..)
                        .map_while(|column| grid.value_at(row, column))
                        .map(|(_, value)| value)
                        .collect();
                    cells.iter().any(Option::is_some).then(|| {
                        cells
                            .iter()
                            .map(|cell| String::from_utf8_lossy(cell.unwrap_or_default()))
                            .collect::<Vec<_>>()
                            .join("\t")
                    })
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (plan, problem) = match dialect.parse_plan(&raw, analyze) {
            Ok(plan) => (Some(plan), None),
            Err(error) => (None, Some(error.to_string())),
        };
        if let Some(pane) = self.pane_mut(pane) {
            pane.outcome = Outcome::Plan {
                show_raw: plan.is_none(),
                plan,
                raw,
                problem,
            };
        }
        cx.notify();
    }

    /// Adds the pane of a result set that starts to arrive; returns its id.
    fn open_grid(
        &mut self,
        statement: &str,
        grid: Grid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> u64 {
        // The pane this grid goes in: `push_pane` gives it this id.
        let id = self.next_pane;
        let class = classify(statement, self.lex_rules(cx));
        // A SELECT's result can be read again on the server, filtered and
        // ordered: a re-run keeps what it was read with.
        // A plan's rows are no result to read again.
        let requery = self.requery.clone().or_else(|| {
            (class.kind == StatementKind::Select && !class.writes && self.explain_mode.is_none())
                .then(|| Requery {
                    statement: statement.to_string(),
                    ..Requery::default()
                })
        });
        // The menu of a cell exports the result of its own statement (it
        // says when that statement writes, and does not run it), and
        // filters it.
        let view = cx.weak_entity();
        let sql = statement.to_string();
        let mut grid = grid.with_widths(self.widths.clone()).with_owner(Owner {
            act: Rc::new(move |action, row, column, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    // Among stacked results, the menu of a cell acts on its
                    // own result, which takes the focus (J11).
                    this.focus_stacked(id, cx);
                    match action {
                        GridAction::Export => {
                            this.export_statement(sql.clone(), window, cx);
                        }
                        GridAction::Follow(index, _) => this.follow(id, index, row, cx),
                        GridAction::Paste
                        | GridAction::EditCell
                        | GridAction::SetNull
                        | GridAction::SetDefault
                        | GridAction::CopyFromRow(_)
                        | GridAction::DuplicateRow
                        | GridAction::DeleteRow
                        | GridAction::RevertRow
                        | GridAction::AddRow
                        | GridAction::Generate(_) => {
                            this.edit_result(action, row, column, window, cx)
                        }
                        GridAction::Ai(entry) => {
                            this.ask_about_result(id, entry, column, window, cx)
                        }
                        action => this.result_action(id, action, row, column, window, cx),
                    }
                });
            }),
            // Where a row's foreign keys lead, once the result is known
            // to be one table's rows (K49).
            related: Some(Rc::new(|grid: &Grid, row| {
                grid.structure().map_or_else(Vec::new, |structure| {
                    related(structure, grid, row)
                        .into_iter()
                        .map(|related| related.label)
                        .collect()
                })
            })),
            export: class.writes.then_some("the statement writes"),
            filtered: requery
                .as_ref()
                .is_some_and(|requery| !requery.filter.trim().is_empty()),
            keyless: false,
            structure: None,
        });
        if let Some(requery) = &requery {
            let sorted = requery.sort.as_ref().and_then(|(name, descending)| {
                let column = grid
                    .store()
                    .columns()
                    .iter()
                    .position(|column| column.name == *name)?;
                Some((column, *descending))
            });
            let view = cx.weak_entity();
            grid = grid.with_server_order(
                Rc::new(move |sort, window, cx| {
                    let _ = view.update(cx, |this, cx| this.reorder(id, sort, window, cx));
                }),
                sorted,
            );
            if !requery.filter.trim().is_empty() && !requery.auto {
                self.remember_result_filter(&requery.statement, &requery.filter, cx);
            }
        }
        let scrolled = self.carried_scroll.take();
        let table = cx.new(|cx| {
            let table = TableState::new(grid, window, cx)
                .col_resizable(true)
                .col_movable(true)
                .sortable(true)
                .row_selectable(true)
                .col_selectable(true)
                .cell_selectable(true)
                // The grid numbers its rows itself.
                .row_header(false);
            // Read again, its columns stay scrolled as they were (K40).
            if let Some(x) = scrolled {
                table
                    .horizontal_scroll_handle
                    .set_offset(gpui_kit::point(x, gpui_kit::px(0.)));
            }
            table
        });
        // Whether the rows take changes: what the statement and the
        // columns say at once, then what the table says.
        let verdict = self
            .explain_mode
            .is_none()
            .then(|| self.edit_check(statement, requery.as_ref(), &table, cx));
        let requery = requery.map(|requery| Requery {
            origins: requery.origins.clone().or_else(|| {
                let columns = table.read(cx).delegate().store().columns().clone();
                Some(columns.iter().map(|column| column.origin.clone()).collect())
            }),
            ..requery
        });
        // A double click on a cell types in it, or shows its whole value.
        let events = cx.subscribe_in(
            &table,
            window,
            |_, table, event: &TableEvent, window, cx| {
                // The grid stretches ranges from what the table has selected.
                if matches!(
                    event,
                    TableEvent::SelectCell(..)
                        | TableEvent::SelectRow(_)
                        | TableEvent::SelectColumn(_)
                        | TableEvent::ClearSelection
                ) {
                    table.update(cx, |table, _| {
                        let selection = table.selection();
                        table.delegate_mut().set_anchor(selection);
                    });
                }
                if let TableEvent::DoubleClickedCell(row, column) = event {
                    edit::double_click(table, *row, *column, window, cx);
                }
            },
        );
        // The find bar of the result this one reads again stays open.
        if let Some(carried) = self.carried_find.take() {
            find::restore(&table, carried, window, cx);
        }
        let id = self.push_pane(
            statement.to_string(),
            Outcome::Rows(table.clone()),
            None,
            None,
        );
        if let Some(pane) = self.pane_mut(id) {
            pane._grid_events = Some(events);
            pane.requery = requery;
        }
        match verdict {
            Some(Ok((columns, source))) => self.look_up_table(id, columns, source, window, cx),
            Some(Err(why)) => self.decided(id, Err(why), cx),
            None => {}
        }
        id
    }

    /// Opens the other end of the `index`-th foreign key of a row of the
    /// result in pane `id`, in a table's tab, as the menu of a table's cell
    /// does (K49).
    fn follow(&mut self, id: u64, index: usize, row: usize, cx: &mut Context<Self>) {
        let found = self.pane_grid(id).and_then(|grid| {
            let grid = grid.read(cx).delegate();
            related(grid.structure()?, grid, row).into_iter().nth(index)
        });
        if let Some(related) = found {
            cx.emit(QueryTabEvent::OpenData {
                table: related.table,
                filter: related.filter,
            });
        }
    }

    // Editing a result (RES-51) ---------------------------------------------

    /// What is known at once of whether the rows of a result take changes
    /// (see [`editable::check`]): the connection takes writes, the
    /// statement is a plain SELECT, the columns from a table are one
    /// table's. With the columns as their origins say (those of the
    /// statement's first run when it is read again).
    fn edit_check(
        &self,
        statement: &str,
        requery: Option<&Requery>,
        table: &Entity<TableState<Grid>>,
        cx: &App,
    ) -> Result<(Arc<[kerno_core::Column]>, EditSource), ReadOnly> {
        let saved = self
            .target
            .as_ref()
            .and_then(|target| self.connections.read(cx).get(&target.connection));
        if let Some(saved) = saved.filter(|saved| saved.config.read_only) {
            return Err(ReadOnly::Connection(saved.config.name.clone()));
        }
        let mut columns = table.read(cx).delegate().store().columns().clone();
        if let Some(origins) = requery
            .and_then(|requery| requery.origins.as_ref())
            .filter(|origins| origins.len() == columns.len())
        {
            columns = columns
                .iter()
                .zip(origins.iter())
                .map(|(column, origin)| column.clone().with_origin(origin.clone()))
                .collect();
        }
        let statement = requery.map_or(statement, |requery| requery.statement.as_str());
        let source = editable::check(statement, self.lex_rules(cx), &columns)?;
        Ok((columns, source))
    }

    /// Finds out from the table whether the rows of pane `id` take
    /// changes: one small catalog query and the table's structure, off the
    /// UI thread, on the connection's own session when it is in the
    /// result's database, else on a session opened for it (OIDs belong to
    /// a database). Never on the tab's: a result being paged holds it, and
    /// its transaction is the user's.
    fn look_up_table(
        &mut self,
        id: u64,
        columns: Arc<[kerno_core::Column]>,
        source: EditSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(target), Some(grid)) = (self.target.clone(), self.pane_grid(id)) else {
            return;
        };
        let database = grid.read(cx).delegate().session().database();
        let connections = self.connections.read(cx);
        let meta = connections
            .meta_session(&target.connection)
            .filter(|meta| !meta.dialect().has_schemas() || meta.database() == database);
        let opening = match &meta {
            Some(_) => None,
            None => Some(connections.open_session(&target.connection, database, cx)),
        };
        // One piece of work on the runtime, which goes on when the pane is
        // gone: a session opened for the lookup is always closed by it.
        let looking = bridge::spawn(cx, async move {
            let (session, close) = match (meta, opening) {
                (Some(meta), _) => (meta, false),
                (None, Some(opening)) => match opening.await {
                    Ok(session) => (session, true),
                    Err(error) => return Err(ReadOnly::Unread(error.to_string())),
                },
                (None, None) => return Err(ReadOnly::Unread("no session".into())),
            };
            let decided = editable::lookup(&session, &columns, &source).await;
            if close {
                session.close().await;
            }
            decided
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let decided = looking.await;
            let _ = this.update(cx, |this, cx| this.decided(id, decided, cx));
        });
        if let Some(pane) = self.pane_mut(id) {
            pane._lookup = Some(task);
        }
    }

    /// Makes the rows of pane `id` editable as rows of their table, or says
    /// why they stay read-only.
    fn decided(
        &mut self,
        id: u64,
        decided: Result<(Editable, TableStructure), ReadOnly>,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.pane_grid(id) else {
            return;
        };
        let (verdict, target) = match decided {
            Ok((editable, structure)) => (
                Ok(()),
                Ok(EditTarget {
                    table: editable.table,
                    structure: Arc::new(structure),
                    identity: editable.identity,
                    names: editable.columns,
                }),
            ),
            Err(why) => {
                let short = why.short();
                (Err(why), Err(short))
            }
        };
        grid.update(cx, |table, cx| {
            table.delegate_mut().set_edit_target(target);
            table.refresh(cx);
            cx.notify();
        });
        if let Some(pane) = self.pane_mut(id) {
            pane.editable = Some(verdict);
        }
        cx.notify();
    }

    /// How many rows of the results have changes waiting.
    pub fn pending_count(&self, cx: &App) -> usize {
        self.results
            .iter()
            .filter_map(ResultPane::grid)
            .map(|grid| grid.read(cx).delegate().pending().map_or(0, |p| p.count()))
            .sum()
    }

    /// Why the rows of the result on screen take no change, in one
    /// sentence; `None` while that is being found out or when they do. For
    /// tests.
    #[cfg(test)]
    pub fn result_read_only(&self) -> Option<String> {
        match self.results.get(self.active_result)?.editable.as_ref()? {
            Ok(()) => None,
            Err(why) => Some(why.to_string()),
        }
    }

    /// The connection and database the tab runs on.
    fn place(&self) -> Option<(String, Option<String>)> {
        let target = self.target.as_ref()?;
        Some((target.connection.clone(), target.database.clone()))
    }

    /// Whether the tab runs elsewhere than the rows of pane `id` were read
    /// on: its changes are not applied there.
    fn moved_away(&self, id: u64) -> bool {
        self.results
            .iter()
            .find(|pane| pane.id == id)
            .is_some_and(|pane| pane.place != self.place())
    }

    /// Whether the rows of the result on screen take changes, and why not
    /// when they do not.
    fn result_edits(&self) -> edit::Allowed {
        let pane = self.results.get(self.active_result);
        if pane.is_some_and(|pane| self.moved_away(pane.id)) {
            return Err(Some(MOVED_AWAY.into()));
        }
        match pane.map(|pane| &pane.editable) {
            Some(Some(Ok(()))) => Ok(()),
            Some(Some(Err(why))) => Err(Some(SharedString::from(why.to_string()))),
            Some(None) => Err(Some(
                "Wait a moment: the table of the result is being read.".into(),
            )),
            None => Err(None),
        }
    }

    /// The grid of the result on screen, when its rows take changes; says
    /// why not when they do not.
    fn editing_grid(
        &self,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<(u64, Entity<TableState<Grid>>)> {
        let pane = self.results.get(self.active_result)?;
        let grid = pane.grid()?;
        match self.result_edits() {
            Ok(()) => Some((pane.id, grid)),
            Err(why) => {
                if let Some(why) = why {
                    window.push_notification(Notification::warning(why), cx);
                }
                None
            }
        }
    }

    /// Runs a change of the edit layer on the result on screen, when its
    /// rows take changes, and redraws.
    fn on_result_rows(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        act: impl FnOnce(&Entity<TableState<Grid>>, &mut App),
    ) {
        let Some((id, grid)) = self.editing_grid(window, cx) else {
            return;
        };
        act(&grid, cx);
        if let Some(pane) = self.pane_mut(id) {
            pane.apply_error = None;
        }
        // Auto-refresh would read the rows again over the changes.
        if self.auto_refresh.is_some() && self.pending_count(cx) > 0 {
            self.stop_auto_refresh(Some("changes are pending"), cx);
        }
        cx.notify();
    }

    /// New rows go after the last row: every row must be here first.
    fn rows_all_here(&self, window: &mut Window, cx: &mut App) -> bool {
        let more = self
            .active_grid()
            .is_some_and(|grid| grid.read(cx).delegate().has_more());
        if more {
            window.push_notification(
                Notification::warning(
                    "Fetch all rows first: new rows go after the last row of the result.",
                ),
                cx,
            );
        }
        !more
    }

    /// Adds an empty row at the end of the result on screen.
    pub fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rows_all_here(window, cx) {
            self.on_result_rows(window, cx, edit::add_row);
        }
    }

    /// Adds copies of the selected rows (or of `row`) at the end.
    pub fn duplicate_rows(
        &mut self,
        row: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rows_all_here(window, cx) {
            self.on_result_rows(window, cx, |grid, cx| edit::duplicate_rows(grid, row, cx));
        }
    }

    /// Marks the selected rows (or `row`) for deletion.
    pub fn delete_rows(&mut self, row: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.on_result_rows(window, cx, |grid, cx| edit::delete_rows(grid, row, cx));
    }

    /// Gives every selected cell `value` (`NULL` or the column's default).
    pub fn set_selected(&mut self, value: CellValue, window: &mut Window, cx: &mut Context<Self>) {
        self.on_result_rows(window, cx, |grid, cx| {
            edit::set_cells(grid, value, false, cx)
        });
    }

    /// Gives the selected cells the values of the row above or below.
    pub fn copy_from_row(&mut self, above: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.on_result_rows(window, cx, |grid, cx| edit::copy_from_row(grid, above, cx));
    }

    /// Drops the pending values of the selected cells (Esc). Returns
    /// whether there was one.
    pub fn revert_cells(&mut self, cx: &mut Context<Self>) -> bool {
        self.active_grid()
            .filter(|grid| grid.read(cx).delegate().pending().is_some())
            .is_some_and(|grid| edit::revert_cells(&grid, cx))
    }

    /// Takes the last change back (Cmd+Z). Returns whether there was one.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        self.active_grid()
            .filter(|grid| grid.read(cx).delegate().pending().is_some())
            .is_some_and(|grid| edit::undo(&grid, cx))
    }

    /// Forgets every pending change of the result on screen.
    pub fn discard(&mut self, cx: &mut Context<Self>) {
        if let Some(grid) = self.active_grid() {
            edit::discard(&grid, cx);
        }
        if let Some(pane) = self.results.get_mut(self.active_result) {
            pane.apply_error = None;
        }
        cx.notify();
    }

    /// Forgets the pending changes of every result: the session they were
    /// to be applied on is ending.
    pub fn discard_results(&mut self, cx: &mut Context<Self>) {
        for grid in self.results.iter().filter_map(ResultPane::grid) {
            if grid
                .read(cx)
                .delegate()
                .pending()
                .is_some_and(|p| !p.is_empty())
            {
                edit::discard(&grid, cx);
            }
        }
        cx.notify();
    }

    /// Runs an entry of a grid menu that changes the rows of pane `id`.
    fn edit_result(
        &mut self,
        action: GridAction,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            GridAction::Paste => {
                let text = cx.read_from_clipboard().and_then(|item| item.text());
                if let Some(text) = text.filter(|text| !text.is_empty()) {
                    self.on_result_rows(window, cx, |grid, cx| edit::paste(grid, &text, cx));
                }
            }
            GridAction::EditCell => {
                if let Some(grid) = self.active_grid() {
                    edit::edit_cell(&grid, row, column, window, cx);
                }
            }
            // Cells of NOT NULL columns are left alone, as with Delete.
            GridAction::SetNull => self.on_result_rows(window, cx, |grid, cx| {
                edit::set_cells(grid, CellValue::Null, true, cx)
            }),
            GridAction::SetDefault => {
                self.set_selected(CellValue::Expression("DEFAULT".into()), window, cx)
            }
            GridAction::CopyFromRow(above) => self.copy_from_row(above, window, cx),
            GridAction::DuplicateRow => self.duplicate_rows(Some(row), window, cx),
            GridAction::DeleteRow => self.delete_rows(Some(row), window, cx),
            GridAction::RevertRow => {
                self.on_result_rows(window, cx, |grid, cx| edit::revert_row(grid, row, cx))
            }
            GridAction::AddRow => self.add_row(window, cx),
            GridAction::Generate(kind) => self.generate_from_result(kind, row, window, cx),
            _ => {}
        }
    }

    /// Writes `kind` statements for the selected rows (or `row`) of the
    /// result on screen into a new query tab: nothing runs.
    fn generate_from_result(
        &mut self,
        kind: RowSql,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, grid)) = self.editing_grid(window, cx) else {
            return;
        };
        let shown = edit::selection(&grid, cx).map_or_else(|| vec![row], |(rows, _)| rows);
        let grid = grid.read(cx).delegate();
        let rows: Vec<usize> = shown
            .into_iter()
            .map(|row| grid.source_row(row))
            .filter(|&row| row < grid.row_count())
            .collect();
        if rows.is_empty() {
            return;
        }
        let title = grid
            .table()
            .map(|table| table.name.clone())
            .unwrap_or_default();
        match grid.rows_sql(kind, &rows) {
            Ok(sql) => cx.emit(QueryTabEvent::OpenQuery { title, sql }),
            Err(error) => window.push_notification(
                Notification::error(SharedString::from(format!(
                    "The {} could not be written: {error}",
                    kind.label()
                ))),
                cx,
            ),
        }
    }

    /// Shows the statements of the pending changes of the result on screen
    /// and, once confirmed, applies them.
    pub fn review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, grid)) = self.editing_grid(window, cx) else {
            return;
        };
        // What is being typed counts.
        grid.update(cx, |table, cx| {
            table.delegate_mut().commit_edit(cx);
            cx.notify();
        });
        let (changes, statements) = match grid.read(cx).delegate().pending_sql() {
            Ok(written) => written,
            Err(error) => {
                if let Some(pane) = self.pane_mut(id) {
                    pane.apply_error = Some(error.to_string());
                }
                cx.notify();
                return;
            }
        };
        if statements.is_empty() {
            return;
        }
        let saved = self
            .target
            .as_ref()
            .and_then(|target| self.connections.read(cx).get(&target.connection));
        let environment = saved
            .map(|saved| saved.config.environment.clone())
            .unwrap_or_default();
        let connection = saved
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default();
        let count = grid.read(cx).delegate().pending().map_or(0, |p| p.count());
        let table = grid
            .read(cx)
            .delegate()
            .table()
            .map(|table| table.name.clone())
            .unwrap_or_default();
        let in_transaction = self.manual_commit || self.tx_state() != TxState::Idle;
        let view = cx.entity();
        let run = statements.clone();
        sql_confirm::open(
            Confirmation {
                title: format!(
                    "Apply {count} {} to {table}",
                    if count == 1 { "change" } else { "changes" }
                ),
                explanation: if in_transaction {
                    "These statements join the transaction open in this tab, all of them or none: nothing is committed until you commit it."
                } else {
                    "These statements run in one transaction: all of them take effect, or none does."
                }
                .into(),
                statement: ConfirmStatement::Fixed(
                    statements
                        .iter()
                        .map(|statement| format!("{statement};"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                action: "Apply",
                destructive: changes
                    .iter()
                    .any(|change| matches!(change, RowChange::Delete { .. })),
                connection,
                environment,
                writes: true,
            },
            window,
            cx,
            move |_, window, cx| {
                view.update(cx, |this, cx| this.apply(id, run.clone(), window, cx));
            },
        );
    }

    /// Runs the reviewed statements of pane `id` on the tab's session, all
    /// of them or none: in a transaction of their own in auto-commit, in
    /// the tab's transaction (under a savepoint, nothing committed) in
    /// manual commit or while one is open. The rows are then read again.
    fn apply(
        &mut self,
        id: u64,
        statements: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running() || self.side.is_some() {
            window.push_notification(
                Notification::warning("Wait for the running statement to end."),
                cx,
            );
            return;
        }
        // Never on another server or database than the rows were read on.
        if self.moved_away(id) {
            window.push_notification(Notification::warning(MOVED_AWAY), cx);
            return;
        }
        let manual = self.manual_commit;
        if self.receiving.is_some() {
            // The statement being paged holds the session; stopping it
            // cancels it, which would end a transaction on PostgreSQL.
            if manual || self.tx_state() != TxState::Idle {
                window.push_notification(
                    Notification::warning(
                        "Fetch all rows of the result first: its statement holds the session \
                         of this tab's open transaction.",
                    ),
                    cx,
                );
                return;
            }
            self.stop(cx);
        }
        let opening = self.session(cx);
        let rules = self.lex_rules(cx);
        let started = Instant::now();
        let started_at_ms = kerno_core::storage::now_ms();
        self.cancel_requested = false;
        self.activity = Activity::Running {
            index: 0,
            total: 1,
            since: started,
        };
        self.run = Some(cx.spawn_in(window, async move |this, cx| {
            // Every failure comes as one that changed nothing (C14).
            let whole = |error| ApplyFailure {
                statement: None,
                error,
            };
            let applied = match opening.await {
                Err(error) => Err(whole(error)),
                Ok(session) => {
                    let applying = cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            let began = manual && session.tx_state() == TxState::Idle;
                            if began {
                                let begin = session.dialect().begin_sql();
                                exec::collect(session.clone(), begin).await.map_err(whole)?;
                            }
                            let ran = if session.tx_state() == TxState::Idle {
                                apply_edits(&session, &statements).await
                            } else {
                                apply_edits_in_transaction(&session, &statements).await
                            };
                            ran.map(|()| (statements, began))
                        })
                    });
                    let Ok(applying) = applying else {
                        return;
                    };
                    applying.await
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.run = None;
                this.activity = Activity::Idle;
                // The session is gone (the server closed it, C15): dropped,
                // the changes kept for an Apply on a new one.
                if matches!(&applied, Err(failure) if failure.error.is_disconnect()) {
                    this.lost_connection(window, cx);
                }
                // In manual commit the apply may have begun the transaction.
                this.note_transaction(cx);
                let Some(pane) = this.pane_mut(id) else {
                    return;
                };
                match applied {
                    Ok((statements, began)) => {
                        let count = pane.grid().map_or(0, |grid| {
                            grid.read(cx).delegate().pending().map_or(0, |p| p.count())
                        });
                        pane.apply_error = None;
                        if let Some(grid) = pane.grid() {
                            edit::discard(&grid, cx);
                        }
                        let requery = pane.requery.clone().unwrap_or_default();
                        // The statements join the transaction's log, which a
                        // transaction begun here starts empty.
                        if began {
                            this.tx_log.clear();
                        }
                        if this.tx_state() != TxState::Idle {
                            let elapsed = started.elapsed();
                            for statement in &statements {
                                this.tx_log.push(TxLogEntry::new(
                                    statement,
                                    rules,
                                    started_at_ms,
                                    elapsed,
                                    Some(1),
                                    HistoryStatus::Ok,
                                    None,
                                ));
                            }
                        }
                        window.push_notification(
                            Notification::success(SharedString::from(format!(
                                "{count} {} applied.",
                                if count == 1 { "change" } else { "changes" }
                            ))),
                            cx,
                        );
                        // What the server now holds, read as before.
                        this.read_again(id, requery.filter, requery.sort, window, cx);
                    }
                    // Nothing was written; the changes are still there.
                    Err(failure) => {
                        pane.apply_error = Some(crate::data::tab::apply_failure_text(&failure));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Why the last apply of the result on screen changed nothing; for
    /// tests.
    #[cfg(test)]
    pub fn result_apply_error(&self) -> Option<String> {
        self.results.get(self.active_result)?.apply_error.clone()
    }

    /// Copies the selection of the result on screen (`None`: its column
    /// names). Returns whether there was something to copy.
    pub fn copy_selection(&mut self, format: Option<CopyFormat>, cx: &mut Context<Self>) -> bool {
        let Some(grid) = self
            .results
            .get(self.active_result)
            .and_then(ResultPane::grid)
        else {
            return false;
        };
        grid.update(cx, |table, cx| Grid::copy(table, format, None, cx))
    }

    /// Selects every cell of the result on screen.
    pub fn select_all_rows(&mut self, cx: &mut Context<Self>) {
        if let Some(grid) = self
            .results
            .get(self.active_result)
            .and_then(ResultPane::grid)
        {
            grid.update(cx, |table, cx| {
                table.delegate_mut().select_all();
                cx.notify();
            });
        }
    }

    /// The value panel, while shown; for tests.
    #[cfg(test)]
    pub fn value_panel(&self) -> Option<Entity<ValuePanel>> {
        self.value_panel.clone()
    }

    /// The grid of the result on screen, when it holds rows.
    pub fn active_grid(&self) -> Option<Entity<TableState<Grid>>> {
        self.results
            .get(self.active_result)
            .and_then(ResultPane::grid)
    }

    /// Fetches the next page of the result on screen, or all its remaining
    /// rows: the footer's buttons, and DBeaver's keys.
    pub fn fetch(&mut self, all: bool, cx: &mut Context<Self>) {
        let Some(grid) = self
            .active_grid()
            .filter(|grid| Grid::has_more(grid.read(cx).delegate()))
        else {
            return;
        };
        self.activity = Activity::Running {
            index: 0,
            total: 1,
            since: Instant::now(),
        };
        grid.update(cx, |grid, cx| {
            if all {
                grid.delegate_mut().fetch_all();
            } else {
                grid.delegate_mut().fetch_more();
            }
            cx.notify();
        });
        cx.notify();
    }

    /// Shows the next result tab (the server's messages are the last one),
    /// or the previous, round the ends.
    pub fn cycle_result(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.results.len() + usize::from(!self.notices.is_empty());
        if count == 0 {
            return;
        }
        let current = self.active_result.min(count - 1);
        self.active_result = if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        cx.notify();
    }

    /// The table or view named where the editor's cursor is, found in the
    /// completion catalog: a qualified name, or the first of the search path
    /// that has it.
    pub fn relation_at_cursor(&self, cx: &App) -> Option<(TableRef, RelationKind)> {
        let editor = self.editor.read(cx);
        let text = editor.value().to_string();
        let cursor = editor.cursor().min(text.len());
        let is_name = |c: char| c.is_alphanumeric() || matches!(c, '_' | '.' | '"' | '`' | '$');
        let start = text[..cursor].rfind(|c: char| !is_name(c)).map_or(0, |at| {
            at + text[at..].chars().next().map_or(1, char::len_utf8)
        });
        let end = text[cursor..]
            .find(|c: char| !is_name(c))
            .map_or(text.len(), |at| cursor + at);
        let word = text.get(start..end)?.replace(['"', '`'], "");
        let (schema, name) = match word.rsplit_once('.') {
            Some((schema, name)) => (Some(schema.rsplit('.').next().unwrap_or(schema)), name),
            None => (None, word.as_str()),
        };
        let catalog = self.catalog.borrow();
        let same = |a: &str, b: &str| a == b || a.eq_ignore_ascii_case(b);
        let schemas: Vec<&kerno_core::sql::CatalogSchema> = match schema {
            Some(schema) => catalog
                .schemas
                .iter()
                .filter(|s| same(&s.name, schema))
                .collect(),
            None => catalog
                .search_path
                .iter()
                .filter_map(|path| catalog.schemas.iter().find(|s| s.name == *path))
                .collect(),
        };
        schemas.into_iter().find_map(|found| {
            found
                .relations
                .iter()
                .find(|relation| same(&relation.name, name))
                .map(|relation| (TableRef::new(&found.name, &relation.name), relation.kind))
        })
    }

    // Result tabs ---------------------------------------------------------------

    /// The label of the result tab at `index`: its name, or what it is.
    fn result_label(&self, index: usize) -> String {
        let pane = &self.results[index];
        pane.name.clone().unwrap_or_else(|| match &pane.outcome {
            Outcome::Failed(_) => format!("Error {}", index + 1),
            Outcome::Plan { .. } => format!("Plan {}", index + 1),
            _ => format!("Result {}", index + 1),
        })
    }

    /// Whether the rows of `pane` stopped arriving before the end: it was
    /// still being paged when another statement took the session, or it
    /// was cancelled.
    fn incomplete(&self, pane: &ResultPane, cx: &App) -> bool {
        self.receiving != Some(pane.id)
            && pane.grid().is_some_and(|grid| {
                let grid = grid.read(cx).delegate();
                !grid.is_complete() && !grid.has_more()
            })
    }

    fn result_facts(&self, id: u64, cx: &App) -> Option<ResultFacts> {
        let index = self.pane_index(id)?;
        let unpinned = |panes: &[ResultPane]| panes.iter().filter(|pane| !pane.pinned).count();
        Some(ResultFacts {
            pinned: self.results[index].pinned,
            left: unpinned(&self.results[..index]),
            right: unpinned(&self.results[index + 1..]),
            pinned_count: self.results.iter().filter(|pane| pane.pinned).count(),
            statement: !self.results[index].statement.is_empty(),
            incomplete: self.incomplete(&self.results[index], cx),
            writes: classify(&self.results[index].statement, self.lex_rules(cx)).writes,
        })
    }

    /// Runs the statement of an incomplete result again, in its place; the
    /// other results stay. A statement that writes is never run again.
    fn run_again(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.pane_index(id) else {
            return;
        };
        let sql = self.results[index].statement.clone();
        if self.running() || classify(&sql, self.lex_rules(cx)).writes {
            return;
        }
        self.close_results(&[id], cx);
        self.explain_mode = None;
        self.keep_results = true;
        self.execute(
            vec![Statement {
                sql,
                place: None,
                refused: None,
            }],
            false,
            window,
            cx,
        );
    }

    /// The menu of the result tab of pane `id`, built when it opens.
    fn result_menu(
        &self,
        id: u64,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.entity();
        move |menu, window, cx| {
            let Some(facts) = view.read(cx).result_facts(id, cx) else {
                return menu;
            };
            let view = view.clone();
            let stacked = view.read(cx).stacked(cx);
            let toggle = view.clone();
            tabs::fill_menu(
                menu,
                tabs::result_menu(facts),
                Rc::new(move |action, window, cx| {
                    let view = view.clone();
                    // Once the menu is gone: Rename opens a prompt.
                    window.defer(cx, move |window, cx| {
                        view.update(cx, |this, cx| this.perform_result(id, action, window, cx))
                    });
                }),
                window,
                cx,
            )
            // The results of a script in one panel, or one tab each (J11).
            .separator()
            .item(
                PopupMenuItem::new("Stack results in one panel")
                    .checked(stacked)
                    .on_click(move |_, window, cx| {
                        toggle.update(cx, |this, cx| this.stack_results(!stacked, window, cx))
                    }),
            )
        }
    }

    /// Does what an entry of a result tab's menu says, on pane `id`.
    pub fn perform_result(
        &mut self,
        id: u64,
        action: TabAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.pane_index(id) else {
            return;
        };
        // Closing a result with changes waiting would lose them.
        let closes = matches!(
            action,
            TabAction::Close
                | TabAction::CloseOthers
                | TabAction::CloseToLeft
                | TabAction::CloseToRight
                | TabAction::CloseAll
        );
        if closes && !self.free_to_run(window, cx) {
            return;
        }
        // The bulk closes leave pinned results alone.
        let unpinned = |panes: &[ResultPane]| -> Vec<u64> {
            panes
                .iter()
                .filter(|pane| !pane.pinned)
                .map(|pane| pane.id)
                .collect()
        };
        match action {
            TabAction::Close => self.close_results(&[id], cx),
            TabAction::CloseOthers => {
                let mut others = unpinned(&self.results);
                others.retain(|other| *other != id);
                self.close_results(&others, cx)
            }
            TabAction::CloseToLeft => {
                let left = unpinned(&self.results[..index]);
                self.close_results(&left, cx)
            }
            TabAction::CloseToRight => {
                let right = unpinned(&self.results[index + 1..]);
                self.close_results(&right, cx)
            }
            TabAction::CloseAll => {
                let all = unpinned(&self.results);
                self.close_results(&all, cx)
            }
            TabAction::Pin | TabAction::Unpin => {
                self.results[index].pinned = action == TabAction::Pin;
                cx.notify();
            }
            TabAction::UnpinAll => {
                for pane in &mut self.results {
                    pane.pinned = false;
                }
                cx.notify();
            }
            TabAction::Rename => self.rename_result(id, window, cx),
            // A result without a statement (a failed connection) copies
            // nothing: its menu greys the entry, the palette's command lands here.
            TabAction::CopyStatement if self.results[index].statement.is_empty() => {}
            TabAction::CopyStatement => cx.write_to_clipboard(ClipboardItem::new_string(
                self.results[index].statement.clone(),
            )),
            TabAction::RunAgain => self.run_again(id, window, cx),
            other => tracing::warn!(?other, "not an action of a result tab"),
        }
    }

    /// Does `action` on the result on screen, when it is a result (not the
    /// messages).
    pub fn perform_on_active_result(
        &mut self,
        action: TabAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.results.get(self.active_result) else {
            return;
        };
        let action = match action {
            TabAction::Pin if pane.pinned => TabAction::Unpin,
            other => other,
        };
        self.perform_result(pane.id, action, window, cx)
    }

    /// Closes the result panes `ids`, which frees their rows. A pane still
    /// receiving or waiting to be paged stops its statement first.
    fn close_results(&mut self, ids: &[u64], cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        if self.receiving.is_some_and(|id| ids.contains(&id)) {
            self.stop(cx);
        }
        let on_messages = self.active_result >= self.results.len();
        let shown = self.results.get(self.active_result).map(|pane| pane.id);
        let before = self.active_result;
        self.results.retain(|pane| !ids.contains(&pane.id));
        self.active_result = if on_messages {
            self.results.len()
        } else {
            // The same result, or the one that took the place of the closed one.
            shown
                .and_then(|id| self.pane_index(id))
                .unwrap_or(before.min(self.results.len().saturating_sub(1)))
        };
        cx.notify();
    }

    /// Asks for the name of a result tab; an empty name gives back the one
    /// it had.
    fn rename_result(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.pane_index(id) else {
            return;
        };
        let current = self.result_label(index);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let view = cx.weak_entity();
        let submit = {
            let input = input.clone();
            Rc::new(move |cx: &mut App| {
                let name = input.read(cx).value().trim().to_string();
                let _ = view.update(cx, |this, cx| {
                    if let Some(pane) = this.pane_mut(id) {
                        pane.name = (!name.is_empty()).then_some(name);
                    }
                    cx.notify();
                });
            })
        };
        let focus = input.clone();
        // The keyboard goes back to the editor once the prompt is gone.
        let editor = self.editor.read(cx).focus_handle(cx);
        let give_back = Rc::new(move |window: &mut Window, cx: &mut App| {
            let editor = editor.clone();
            window.defer(cx, move |window, cx| editor.focus(window, cx));
        });
        window.open_dialog(cx, move |dialog, _, _| {
            let (input, on_enter, on_click) = (input.clone(), submit.clone(), submit.clone());
            let (after_enter, after_click, after_cancel) =
                (give_back.clone(), give_back.clone(), give_back.clone());
            dialog
                .title("Rename result tab")
                .w(px(380.))
                .on_ok(move |_, window, cx| {
                    on_enter(cx);
                    after_enter(window, cx);
                    true
                })
                .content(move |content, _, _| {
                    let (on_click, after_click, after_cancel) =
                        (on_click.clone(), after_click.clone(), after_cancel.clone());
                    content.child(
                        v_flex()
                            .gap_3()
                            .child(Input::new(&input).id("result-name"))
                            .child(
                                h_flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        Button::new("result-name-cancel")
                                            .ghost()
                                            .label("Cancel")
                                            .on_click(move |_, window, cx| {
                                                window.close_dialog(cx);
                                                after_cancel(window, cx);
                                            }),
                                    )
                                    .child(
                                        Button::new("result-name-ok")
                                            .primary()
                                            .label("Rename")
                                            .on_click(move |_, window, cx| {
                                                on_click(cx);
                                                window.close_dialog(cx);
                                                after_click(window, cx);
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

    /// Counts the rows the statement at the cursor would return (DBeaver's
    /// `run.count`): a SELECT only, wrapped in `SELECT COUNT(*)`, on the
    /// tab's session. The number goes to the messages and a notification.
    pub fn count_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running() {
            return;
        }
        if self.connect_first(Request::CountRows, cx) {
            return;
        }
        let rules = self.lex_rules(cx);
        let Some(statement) = self.statements(Scope::AtCursor, cx).into_iter().next() else {
            return;
        };
        let Some(meta) = self.meta(cx) else {
            return;
        };
        // DBeaver's `SQLQueryTransformerCount`: the statement as a derived
        // table, on lines of its own.
        let counting = Query::new(meta.dialect(), &statement.sql, &[]).and_then(|query| {
            count_from(
                meta.dialect(),
                QuerySource::Query(&query),
                &[],
                &Filter::default(),
            )
        });
        let refusal = if !find_params(&statement.sql, rules).is_empty() {
            Some("the statement has parameters".to_string())
        } else {
            counting.as_ref().err().map(ToString::to_string)
        };
        if let Some(reason) = refusal {
            window.push_notification(
                Notification::warning(SharedString::from(format!(
                    "Count rows of statement: {reason}."
                ))),
                cx,
            );
            return;
        }
        // A result waiting to be paged holds the session.
        self.stop(cx);
        let Ok(sql) = counting else {
            return;
        };
        let session = self.session(cx);
        self.cancel_requested = false;
        self.activity = Activity::Running {
            index: 0,
            total: 1,
            since: Instant::now(),
        };
        self.run = Some(cx.spawn_in(window, async move |this, cx| {
            let counted = match session.await {
                Ok(session) => {
                    match cx.update(|_, cx| bridge::spawn(cx, exec::collect(session, sql))) {
                        Ok(counting) => counting.await,
                        Err(_) => return,
                    }
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let cancelled = std::mem::take(&mut this.cancel_requested);
                let message = match counted {
                    _ if cancelled => "The count was cancelled.".to_string(),
                    Ok(collected) => {
                        let count = collected
                            .results
                            .last()
                            .and_then(|result| result.rows().first().cloned())
                            .and_then(|row| row.into_iter().next().flatten())
                            .unwrap_or_default();
                        format!(
                            "{count} row{} in the result of the statement.",
                            if count == "1" { "" } else { "s" }
                        )
                    }
                    Err(error) => format!("The rows could not be counted: {}", describe(&error)),
                };
                window
                    .push_notification(Notification::info(SharedString::from(message.clone())), cx);
                this.notices.push(Notice {
                    severity: "Count".into(),
                    message,
                    code: None,
                    detail: Some(one_line(&statement.sql)),
                });
                // The messages, after the results.
                this.active_result = this.results.len();
                this.show_results();
                this.activity = Activity::Idle;
                this.run = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    // A SELECT's result read again on the server -----------------------------

    /// The dialect of the tab's connection, while it is open.
    fn meta(&self, cx: &App) -> Option<Arc<dyn Session>> {
        let target = self.target.as_ref()?;
        self.connections.read(cx).meta_session(&target.connection)
    }

    /// The statement that reads `statement` again with `filter` (the body
    /// of a `WHERE`) and `sort`, as a derived table; the statement itself
    /// when neither is given. With it, whether the server ignores the
    /// statement's own order.
    fn requery_sql(
        dialect: &dyn Dialect,
        statement: &str,
        columns: &[kerno_core::Column],
        filter: &str,
        sort: Option<&(String, bool)>,
    ) -> kerno_core::Result<(String, bool)> {
        if filter.trim().is_empty() && sort.is_none() {
            return Ok((statement.to_string(), false));
        }
        let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
        let query = Query::new(dialect, statement, &names)?;
        let dropped = sort.is_none() && query.drops_own_order(dialect);
        let sql = select_from(
            dialect,
            QuerySource::Query(&query),
            &described(columns),
            &Filter {
                conditions: Vec::new(),
                raw: filter.to_string(),
            },
            sort.map(|(column, descending)| (column.as_str(), *descending)),
        )?;
        Ok((sql, dropped))
    }

    /// The columns of the rows of pane `id`; none when it holds no rows.
    fn pane_columns(&self, id: u64, cx: &App) -> Vec<kerno_core::Column> {
        self.pane_grid(id).map_or_else(Vec::new, |grid| {
            grid.read(cx).delegate().store().columns().to_vec()
        })
    }

    /// Runs the statement of pane `id` again on the server with `filter`
    /// and `sort`, in the pane's place (DBeaver's filter panel and server
    /// ordering of an editor's result). What cannot be read again says why.
    pub fn read_again(
        &mut self,
        id: u64,
        filter: String,
        sort: Option<(String, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.running() || self.side.is_some() {
            window.push_notification(
                Notification::warning("Wait for the running statement to end."),
                cx,
            );
            return;
        }
        if !self.free_to_run(window, cx) {
            return;
        }
        let (Some(requery), Some(meta)) = (
            self.pane_index(id)
                .and_then(|index| self.results[index].requery.clone()),
            self.meta(cx),
        ) else {
            return;
        };
        let columns = self.pane_columns(id, cx);
        let sql = Self::requery_sql(
            meta.dialect(),
            &requery.statement,
            &columns,
            &filter,
            sort.as_ref(),
        );
        let (sql, order_dropped) = match sql {
            Ok(sql) => sql,
            Err(error) => {
                window.push_notification(
                    Notification::warning(SharedString::from(format!(
                        "The result cannot be filtered or ordered on the server: {error}."
                    ))),
                    cx,
                );
                return;
            }
        };
        // Read again, the result keeps its column widths and its find bar.
        self.next_widths = self
            .pane_grid(id)
            .map(|grid| Grid::widths(grid.read(cx)))
            .unwrap_or_default();
        self.next_find = self.pane_grid(id).and_then(|grid| find::carried(&grid, cx));
        self.next_scroll = self
            .pane_grid(id)
            .map(|grid| grid.read(cx).horizontal_scroll_handle.offset().x);
        self.next_requery = Some(Requery {
            filter,
            sort,
            order_dropped,
            ..requery
        });
        self.close_results(&[id], cx);
        self.explain_mode = None;
        self.keep_results = true;
        self.execute(
            vec![Statement {
                sql,
                place: None,
                refused: None,
            }],
            false,
            window,
            cx,
        );
    }

    /// The order a header asked of a result not wholly fetched: a column
    /// of its rows and whether descending, or none.
    fn reorder(
        &mut self,
        id: u64,
        sort: Option<(usize, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(requery) = self
            .pane_index(id)
            .and_then(|index| self.results[index].requery.clone())
        else {
            return;
        };
        let columns = self.pane_columns(id, cx);
        let sort = sort
            .and_then(|(column, descending)| Some((columns.get(column)?.name.clone(), descending)));
        self.read_again(id, requery.filter, sort, window, cx);
    }

    /// Reads the result on screen again with the filter typed above it.
    pub fn apply_result_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.results.get(self.active_result) else {
            return;
        };
        let (Some(requery), true) = (
            pane.requery.clone(),
            self.result_filter_for == Some(pane.id),
        ) else {
            return;
        };
        let filter = self.result_filter.read(cx).value().trim().to_string();
        self.read_again(pane.id, filter, requery.sort, window, cx);
    }

    /// Puts `filter` in the filter field of pane `id`, to be finished and
    /// applied there.
    fn prefill_result_filter(
        &mut self,
        id: u64,
        filter: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.result_filter_for != Some(id) {
            return;
        }
        self.result_filter.update(cx, |input, cx| {
            input.set_value(filter, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// The SQL of a condition on the cell at `row` and `column` of pane
    /// `id`'s grid, its value as a literal.
    fn cell_condition(
        &self,
        id: u64,
        row: usize,
        column: usize,
        operator: Operator,
        cx: &App,
    ) -> Option<String> {
        let grid = self.pane_grid(id)?;
        let grid = grid.read(cx).delegate();
        let condition = crate::data::tab::condition_at(grid, row, column, operator)?;
        let filter = Filter {
            conditions: vec![condition],
            raw: String::new(),
        };
        let meta = self.meta(cx)?;
        filter
            .where_sql(meta.dialect(), &described(grid.store().columns()))
            .ok()
            .flatten()
    }

    /// Runs an entry of a grid menu that filters the result of pane `id`.
    fn result_action(
        &mut self,
        id: u64,
        action: GridAction,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(requery) = self
            .pane_index(id)
            .and_then(|index| self.results[index].requery.clone())
        else {
            return;
        };
        let filtered = |condition: &str| and_where(&requery.filter, condition);
        match action {
            GridAction::FilterByValue | GridAction::ExcludeValue => {
                let operator = if action == GridAction::FilterByValue {
                    Operator::Equals
                } else {
                    Operator::NotEquals
                };
                if let Some(condition) = self.cell_condition(id, row, column, operator, cx) {
                    self.read_again(id, filtered(&condition), requery.sort, window, cx);
                }
            }
            GridAction::FilterWith(operator) => {
                if let Some(condition) = self.cell_condition(id, row, column, operator, cx) {
                    self.prefill_result_filter(id, filtered(&condition), window, cx);
                }
            }
            GridAction::FilterColumn => {
                let name = self.pane_grid(id).and_then(|grid| {
                    let grid = grid.read(cx).delegate();
                    let source = grid.source_column(column)?;
                    Some(grid.store().columns()[source].name.clone())
                });
                if let (Some(name), Some(meta)) = (name, self.meta(cx)) {
                    let condition = format!("{} = ", meta.dialect().quote_ident(&name));
                    self.prefill_result_filter(id, filtered(&condition), window, cx);
                }
            }
            GridAction::DistinctValues => self.distinct_result_values(id, column, window, cx),
            GridAction::ClearFilters => {
                self.read_again(id, String::new(), requery.sort, window, cx)
            }
            _ => {}
        }
    }

    /// K53: the AI assistant asked about the result of pane `id`, on this
    /// tab's conversation; `column` is the header's or the current cell's.
    fn ask_about_result(
        &mut self,
        id: u64,
        entry: crate::ai_view::ask::Entry,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ask = self
            .results
            .iter()
            .find(|pane| pane.id == id)
            .and_then(|pane| {
                let grid = pane.grid()?;
                crate::ai_view::ask::of_grid(entry, &grid, &pane.statement, None, Some(column), cx)
            });
        let tab = cx.entity();
        if let Some(ask) = ask {
            window.defer(cx, move |window, cx| {
                crate::ai_view::ask::open(&tab, ask, None, window, cx)
            });
        }
    }

    /// What the filter history of the results of `statement` is stored
    /// under: the connection and the statement on one line.
    fn result_history_key(&self, statement: &str) -> Option<String> {
        let target = self.target.as_ref()?;
        Some(format!(
            "query:{}/{}/{}",
            target.connection,
            target.database.as_deref().unwrap_or_default(),
            statement.split_whitespace().collect::<Vec<_>>().join(" ")
        ))
    }

    /// Puts a filter applied to a result of `statement` first in its
    /// history.
    fn remember_result_filter(&mut self, statement: &str, filter: &str, cx: &mut Context<Self>) {
        let Some(key) = self.result_history_key(statement) else {
            return;
        };
        let (storage, rules) = (AppState::get(cx).storage.clone(), self.lex_rules(cx));
        let (stored_key, kept) = (key.clone(), filter.to_string());
        let saving =
            bridge::spawn_blocking(cx, move || storage.add_filter(&stored_key, &kept, rules));
        let filter = filter.to_string();
        cx.spawn(async move |this, cx| {
            if !matches!(saving.await, Ok(true)) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                let history = this.result_history.entry(key).or_default();
                history.retain(|known| *known != filter);
                history.insert(0, filter);
                history.truncate(kerno_core::storage::FILTERS_KEPT);
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads the filter history of the results of `statement`, once.
    fn load_result_history(&mut self, statement: &str, cx: &mut Context<Self>) {
        let Some(key) = self.result_history_key(statement) else {
            return;
        };
        if self.result_history.contains_key(&key) {
            return;
        }
        self.result_history.insert(key.clone(), Vec::new());
        let storage = AppState::get(cx).storage.clone();
        let stored_key = key.clone();
        let reading = bridge::spawn_blocking(cx, move || storage.filters(&stored_key));
        cx.spawn(async move |this, cx| {
            let Ok(history) = reading.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.result_history.insert(key, history);
                cx.notify();
            });
        })
        .detach();
    }

    /// The filters applied before to results of the statement on screen.
    pub fn result_filter_history(&self) -> Vec<String> {
        self.results
            .get(self.active_result)
            .and_then(|pane| pane.requery.as_ref())
            .and_then(|requery| self.result_history_key(&requery.statement))
            .and_then(|key| self.result_history.get(&key).cloned())
            .unwrap_or_default()
    }

    /// A session for a count or a list of values while the tab's holds a
    /// result being paged: the tab's connection, database and schema.
    fn own_session(&self, cx: &App) -> Task<kerno_core::Result<Arc<dyn Session>>> {
        let Some(target) = self.target.clone() else {
            return Task::ready(Err(Error::Config(UNBOUND_PICK.into())));
        };
        let opening =
            self.connections
                .read(cx)
                .open_session(&target.connection, target.database.clone(), cx);
        cx.spawn(async move |cx| {
            let session = opening.await?;
            // As the tab's own session (`session`): its schema first.
            if session.dialect().has_schemas()
                && let Some(schema) = &target.schema
            {
                let sql = format!(
                    "SET search_path TO {}, public",
                    session.dialect().quote_ident(schema)
                );
                cx.update(|cx| bridge::spawn(cx, exec::collect(session.clone(), sql)))
                    .await?;
            }
            Ok(session)
        })
    }

    /// Runs `sql`, a read statement about the result on screen, without
    /// losing it: on the tab's session when it is free, as a run of the tab
    /// (Cancel and the next run stop it); on a session of its own while the
    /// tab's holds the result being paged. A transaction open on the tab's
    /// session would not be seen from another one: then nothing runs, and
    /// the notice says why.
    fn side_query(
        &mut self,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(
            &mut Self,
            kerno_core::Result<exec::Collected>,
            &mut Window,
            &mut Context<Self>,
        ) + 'static,
    ) {
        if self.running() || self.side.is_some() {
            window.push_notification(
                Notification::warning("Wait for the running statement to end."),
                cx,
            );
            return;
        }
        let paged = self.receiving.is_some();
        if paged && self.tx_state() != TxState::Idle {
            window.push_notification(
                Notification::warning(
                    "A transaction is open in this tab while its result is still being read: \
                     another session would not see its changes. Fetch all rows, or end the \
                     transaction, then try again.",
                ),
                cx,
            );
            return;
        }
        let opening = if paged {
            self.own_session(cx)
        } else {
            self.session(cx)
        };
        let task = cx.spawn_in(window, async move |this, cx| {
            let ran = match opening.await {
                Ok(session) => {
                    let _ = this.update(cx, |this, _| {
                        if let Some(side) = &mut this.side {
                            side.session = Some(session.clone());
                        }
                    });
                    let running = cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            let ran = exec::collect(session.clone(), sql).await;
                            if paged {
                                let _ = session.close().await;
                            }
                            ran
                        })
                    });
                    match running {
                        Ok(running) => running.await,
                        Err(_) => return,
                    }
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if paged {
                    this.side = None;
                } else {
                    this.run = None;
                    this.activity = Activity::Idle;
                }
                done(this, ran, window, cx);
                cx.notify();
            });
        });
        if paged {
            self.side = Some(SideWork {
                session: None,
                _task: task,
            });
        } else {
            self.cancel_requested = false;
            self.activity = Activity::Running {
                index: 0,
                total: 1,
                since: Instant::now(),
            };
            self.run = Some(task);
        }
        cx.notify();
    }

    /// Types `filter` above the result on screen and applies it; for tests.
    #[cfg(test)]
    pub fn filter_result(&mut self, filter: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.type_result_filter(filter, window, cx);
        self.apply_result_filter(window, cx);
    }

    /// Types `filter` above the result on screen; for tests.
    #[cfg(test)]
    pub fn type_result_filter(
        &mut self,
        filter: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.result_filter.update(cx, |input, cx| {
            input.set_value(filter.to_string(), window, cx)
        });
    }

    /// What the filter field above the result holds; for tests.
    #[cfg(test)]
    pub fn typed_result_filter(&self, cx: &App) -> String {
        self.result_filter.read(cx).value().to_string()
    }

    /// How the result on screen is read again, and its count; for tests.
    #[cfg(test)]
    pub fn result_requery(&self) -> (Option<Requery>, Option<u64>) {
        self.results
            .get(self.active_result)
            .map_or((None, None), |pane| (pane.requery.clone(), pane.total))
    }

    /// Counts the rows of the result on screen, its filter applied
    /// (DBeaver's "Calculate total row count", `core.resultset.count`); the
    /// footer shows the number.
    pub fn count_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.results.get(self.active_result) else {
            return;
        };
        let (id, Some(requery), Some(meta)) = (pane.id, pane.requery.clone(), self.meta(cx)) else {
            return;
        };
        let columns = self.pane_columns(id, cx);
        let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
        let counting = Query::new(meta.dialect(), &requery.statement, &names).and_then(|query| {
            count_from(
                meta.dialect(),
                QuerySource::Query(&query),
                &described(&columns),
                &Filter {
                    conditions: Vec::new(),
                    raw: requery.filter.clone(),
                },
            )
        });
        let counting = match counting {
            Ok(counting) => counting,
            Err(error) => {
                window.push_notification(
                    Notification::warning(SharedString::from(format!(
                        "The rows of the result cannot be counted: {error}."
                    ))),
                    cx,
                );
                return;
            }
        };
        self.side_query(
            counting,
            window,
            cx,
            move |this, counted, window, cx| match counted {
                Ok(counted) => {
                    let total = crate::data::tab::first_count(Ok(counted));
                    if let Some(pane) = this.pane_mut(id) {
                        pane.total = total;
                    }
                }
                Err(error) => window.push_notification(
                    crate::error_notification(format!(
                        "The rows could not be counted: {}",
                        describe(&error)
                    )),
                    cx,
                ),
            },
        );
    }

    /// Lists the distinct values of a column of pane `id`'s result, its
    /// filter applied, to filter on some (DBeaver's "Filter by distinct
    /// values").
    fn distinct_result_values(
        &mut self,
        id: u64,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(requery), Some(meta), Some(grid)) = (
            self.pane_index(id)
                .and_then(|index| self.results[index].requery.clone()),
            self.meta(cx),
            self.pane_grid(id),
        ) else {
            return;
        };
        let Some(source) = grid.read(cx).delegate().source_column(column) else {
            return;
        };
        let columns = self.pane_columns(id, cx);
        let infos = described(&columns);
        let name = columns[source].name.clone();
        let names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
        let listing = Query::new(meta.dialect(), &requery.statement, &names).and_then(|query| {
            distinct_sql(
                meta.dialect(),
                QuerySource::Query(&query),
                &infos,
                &Filter {
                    conditions: Vec::new(),
                    raw: requery.filter.clone(),
                },
                &name,
                distinct::LIMIT,
            )
        });
        let listing = match listing {
            Ok(listing) => listing,
            Err(error) => {
                window.push_notification(
                    Notification::warning(SharedString::from(format!(
                        "The values of {name} cannot be listed: {error}."
                    ))),
                    cx,
                );
                return;
            }
        };
        let info = infos[source].clone();
        self.side_query(listing, window, cx, move |_, listed, window, cx| {
            let values = match listed {
                Ok(listed) => distinct::values(&listed),
                Err(error) => {
                    window.push_notification(
                        crate::error_notification(format!(
                            "The values of {name} could not be read: {}",
                            describe(&error)
                        )),
                        cx,
                    );
                    return;
                }
            };
            let view = cx.weak_entity();
            distinct::open(&name, values, window, cx, move |picked, window, cx| {
                let Some(condition) = one_of_sql(meta.dialect(), &info, &picked) else {
                    return;
                };
                let _ = view.update(cx, |this, cx| {
                    let filter = and_where(&requery.filter, &condition);
                    this.read_again(id, filter, requery.sort.clone(), window, cx)
                });
            });
        });
    }

    /// Reads the result on screen again every `every`, or stops (`None`).
    pub fn set_auto_refresh(
        &mut self,
        every: Option<Duration>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for pane in &mut self.results {
            if let Some(requery) = &mut pane.requery {
                requery.auto = false;
            }
        }
        self.auto_refresh = None;
        self.auto_refresh_stopped = None;
        if let Some(every) = every
            && let Some(Some(requery)) = self
                .results
                .get_mut(self.active_result)
                .map(|pane| pane.requery.as_mut())
        {
            requery.auto = true;
            self.auto_refresh = Some(AutoRefresh::start(every, window, cx, Self::auto_tick));
        }
        cx.notify();
    }

    /// The interval of auto-refresh, while on.
    pub fn auto_refresh(&self) -> Option<Duration> {
        self.auto_refresh.as_ref().map(|auto| auto.every)
    }

    /// Stops auto-refresh; `why` it stopped by itself, shown in the footer.
    pub fn stop_auto_refresh(&mut self, why: Option<&'static str>, cx: &mut Context<Self>) {
        if self.auto_refresh.take().is_some() {
            self.auto_refresh_stopped = why;
            cx.notify();
        }
    }

    /// Another tab is on screen: auto-refresh stops.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.stop_auto_refresh(Some("the tab was hidden"), cx);
        // A request asked in the editor is closed; its answer is not waited for (K52).
        if let Some(inline) = self.inline.clone() {
            inline.update(cx, |inline, cx| inline.left(cx));
        }
    }

    /// One tick of auto-refresh: whether it goes on.
    fn auto_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(pane) = self
            .results
            .iter()
            .find(|pane| pane.requery.as_ref().is_some_and(|requery| requery.auto))
        else {
            // Another run replaced it, or the run that read it again is
            // still on its way.
            if self.running() {
                return true;
            }
            self.stop_auto_refresh(Some("the result was replaced"), cx);
            return false;
        };
        if matches!(pane.outcome, Outcome::Failed(_) | Outcome::Cancelled) {
            self.stop_auto_refresh(Some("the statement failed"), cx);
            return false;
        }
        // Reading it again would bring it on screen in place of another.
        if self.results.get(self.active_result).map(|shown| shown.id) != Some(pane.id) {
            self.stop_auto_refresh(Some("another result is on screen"), cx);
            return false;
        }
        if self.pending_count(cx) > 0 {
            self.stop_auto_refresh(Some("changes are pending"), cx);
            return false;
        }
        // Never two runs at once: the next tick tries again.
        if !self.running() && self.side.is_none() && self.pending.is_none() {
            let (id, requery) = (pane.id, pane.requery.clone().unwrap_or_default());
            self.read_again(id, requery.filter, requery.sort, window, cx);
        }
        true
    }

    // Panels ------------------------------------------------------------------

    /// Hides the results, or shows them again (DBeaver's
    /// `toggle.result.panel`). The editor keeps the keyboard.
    pub fn toggle_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panels = match self.panels {
            Panels::Editor => Panels::Both,
            Panels::Both | Panels::Results => Panels::Editor,
        };
        if self.panels == Panels::Editor {
            self.focus_editor(window, cx);
        }
        cx.notify();
    }

    /// Shows only the results, or both again (DBeaver's
    /// `maximize.result.panel`).
    pub fn maximize_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panels = match self.panels {
            Panels::Results => Panels::Both,
            Panels::Both | Panels::Editor => Panels::Results,
        };
        if self.panels == Panels::Results {
            self.focus_results(window, cx);
        }
        cx.notify();
    }

    /// Gives the keyboard to the results when the editor has it, and back
    /// (DBeaver's `switch.panel`); the side it goes to is shown.
    pub fn switch_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .editor
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            if self.panels == Panels::Editor {
                self.panels = Panels::Both;
            }
            self.focus_results(window, cx);
        } else {
            if self.panels == Panels::Results {
                self.panels = Panels::Both;
            }
            self.focus_editor(window, cx);
        }
        cx.notify();
    }

    fn focus_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor.read(cx).focus_handle(cx);
        window.defer(cx, move |window, cx| editor.focus(window, cx));
    }

    /// The grid on screen, or the results area.
    fn focus_results(&self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self
            .results
            .get(self.active_result)
            .and_then(ResultPane::grid)
            .map_or_else(
                || self.results_focus.clone(),
                |grid| grid.read(cx).focus_handle(cx),
            );
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }

    /// Whether the results area has the keyboard.
    pub fn results_focused(&self, window: &Window, cx: &App) -> bool {
        self.results_focus.contains_focused(window, cx)
    }

    // Rendering ---------------------------------------------------------------

    /// The connections the pickers offer, by id, name and whether connected:
    /// the proposed one first while unbound (C6).
    fn connection_choices(&self, cx: &App) -> Vec<(String, String, bool)> {
        let connections = self.connections.read(cx);
        let mut choices: Vec<(String, String, bool)> = connections
            .saved()
            .iter()
            .map(|saved| {
                (
                    saved.id.clone(),
                    saved.config.name.clone(),
                    matches!(connections.status(&saved.id), Status::Connected { .. }),
                )
            })
            .collect();
        if self.target.is_none()
            && let Some(at) = (self.proposed.as_ref())
                .and_then(|proposed| choices.iter().position(|(id, _, _)| id == proposed))
        {
            let proposed = choices.remove(at);
            choices.insert(0, proposed);
        }
        choices
    }

    /// Above the editor of an unbound tab (C6): why it runs nowhere, and
    /// the picker one click away.
    fn render_unbound_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let text = SharedString::from(self.unbound_text()?);
        let theme = cx.theme();
        Some(
            h_flex()
                .id("unbound-bar")
                .test_support()
                .aria_label(text.clone())
                .flex_none()
                .px_2()
                .py_1()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.warning.opacity(0.12))
                .text_sm()
                .child(div().flex_1().min_w_0().child(text))
                .child(
                    Button::new("unbound-pick")
                        .outline()
                        .small()
                        .label("Pick a connection")
                        .dropdown_caret(true)
                        .dropdown_menu(connection_menu(self.connection_choices(cx), cx.entity())),
                ),
        )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let connections = self.connections.read(cx);
        let saved = self
            .target
            .as_ref()
            .and_then(|target| connections.get(&target.connection));
        let connection_label = saved.map_or_else(
            || SharedString::from("Choose a connection"),
            |saved| SharedString::from(saved.config.name.clone()),
        );
        let engine = saved.map(|saved| saved.config.engine);
        let has_schemas = self
            .target
            .as_ref()
            .and_then(|target| connections.meta_session(&target.connection))
            .is_some_and(|meta| meta.dialect().has_schemas());
        let database_label = self
            .target
            .as_ref()
            .and_then(|target| {
                target.database.clone().or_else(|| {
                    connections
                        .meta_session(&target.connection)
                        .and_then(|meta| meta.database())
                })
            })
            .unwrap_or_else(|| "No database".into());
        let schema_label = self
            .target
            .as_ref()
            .and_then(|target| target.schema.clone())
            .unwrap_or_else(|| "default schema".into());

        // What the menus offer, captured when they open.
        let choices = self.connection_choices(cx);
        let (databases, schemas) = (self.databases.clone(), self.schemas.clone());
        let running = matches!(self.activity, Activity::Running { .. });
        // Stop while a statement runs or rows are on their way; a result
        // that only waits for "Fetch more" has nothing to stop.
        let fetching = self
            .receiving
            .and_then(|id| self.pane_grid(id))
            .is_some_and(|grid| {
                let grid = grid.read(cx).delegate();
                !grid.is_complete() && !grid.has_more()
            });
        let waiting = running || self.pending.is_some() || (self.run.is_some() && fetching);
        let transaction = self
            .session
            .as_ref()
            .map_or(TxState::Idle, |session| session.tx_state());
        let view = cx.entity();
        let chrome = saved.and_then(|saved| crate::theme::environment_chrome(&saved.config, cx));

        h_flex()
            .flex_none()
            // A group of a split is often narrower than the toolbar: its
            // buttons go to a second line instead of off the edge (K19).
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .when_some(chrome, |this, chrome| this.bg(chrome.wash))
            .child(
                Button::new("tab-connection")
                    .ghost()
                    .small()
                    // The engine's logo, then the name.
                    .child(
                        h_flex()
                            .gap_1p5()
                            .children(engine.map(|engine| {
                                div().flex_none().child(engine_logo(engine, px(14.), cx))
                            }))
                            .child(connection_label),
                    )
                    .dropdown_caret(true)
                    .dropdown_menu(connection_menu(choices, view.clone())),
            )
            .when(self.target.is_some(), |this| {
                this.child(
                    Button::new("tab-database")
                        .ghost()
                        .small()
                        .label(SharedString::from(database_label))
                        .dropdown_caret(true)
                        .dropdown_menu({
                            let view = view.clone();
                            move |mut menu, _, _| {
                                for database in &databases {
                                    let (view, database) = (view.clone(), database.clone());
                                    menu = menu.item(
                                        PopupMenuItem::new(SharedString::from(database.clone()))
                                            .on_click(move |_, _, cx| {
                                                view.update(cx, |this, cx| {
                                                    let target =
                                                        this.target.clone().map(|target| Target {
                                                            database: Some(database.clone()),
                                                            schema: None,
                                                            ..target
                                                        });
                                                    this.request_target(target, cx)
                                                })
                                            }),
                                    );
                                }
                                menu
                            }
                        }),
                )
            })
            .when(self.target.is_some() && has_schemas, |this| {
                this.child(
                    Button::new("tab-schema")
                        .ghost()
                        .small()
                        .label(SharedString::from(schema_label))
                        .dropdown_caret(true)
                        .dropdown_menu({
                            let view = view.clone();
                            move |mut menu, _, _| {
                                for schema in &schemas {
                                    let (view, schema) = (view.clone(), schema.clone());
                                    menu = menu.item(
                                        PopupMenuItem::new(SharedString::from(schema.clone()))
                                            .on_click(move |_, _, cx| {
                                                view.update(cx, |this, cx| {
                                                    let target =
                                                        this.target.clone().map(|target| Target {
                                                            schema: Some(schema.clone()),
                                                            ..target
                                                        });
                                                    this.request_target(target, cx)
                                                })
                                            }),
                                    );
                                }
                                menu
                            }
                        }),
                )
            })
            .child(
                Button::new("tab-commit-mode")
                    .ghost()
                    .small()
                    .label(if self.manual_commit {
                        "Manual commit"
                    } else {
                        "Auto-commit"
                    })
                    .dropdown_caret(true)
                    .dropdown_menu({
                        let view = view.clone();
                        move |menu, _, _| {
                            let (auto, manual) = (view.clone(), view.clone());
                            menu.item(
                                PopupMenuItem::new(
                                    "Auto-commit: every statement is committed as it runs",
                                )
                                .on_click(move |_, _, cx| {
                                    auto.update(cx, |this, cx| this.request_manual_commit(false, cx))
                                }),
                            )
                            .item(
                                PopupMenuItem::new(
                                    "Manual commit: changes wait for Commit or Rollback",
                                )
                                .on_click(move |_, _, cx| {
                                    manual.update(cx, |this, cx| this.request_manual_commit(true, cx))
                                }),
                            )
                        }
                    }),
            )
            .when(transaction != TxState::Idle, |this| {
                this.child(
                    Button::new("tab-commit")
                        .outline()
                        .small()
                        .label("Commit")
                        .disabled(running || transaction == TxState::Failed)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.end_transaction(true, window, cx)
                        })),
                )
                .child(
                    Button::new("tab-rollback")
                        .outline()
                        .small()
                        .label("Rollback")
                        .disabled(running)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.end_transaction(false, window, cx)
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("tab-history")
                    .ghost()
                    .small()
                    .icon(Icon::new(Glyph::Clock))
                    .tooltip("Query history (Cmd+Y)")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowHistory), cx)),
            )
            .child(
                Button::new("tab-saved")
                    .ghost()
                    .small()
                    .icon(Icon::new(Glyph::Bookmark))
                    .tooltip("Saved queries")
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ShowSavedQueries), cx)
                    }),
            )
            .when(waiting, |this| {
                this.child(
                    Button::new("tab-cancel")
                        .outline()
                        .small()
                        .label(if running { "Cancel" } else { "Stop" })
                        .tooltip("Cancel the running statement (Cmd+.)")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .child(
                Button::new("tab-explain")
                    .ghost()
                    .small()
                    .label("Explain")
                    .dropdown_caret(true)
                    .disabled(running)
                    .dropdown_menu({
                        let view = view.clone();
                        move |menu, _, _| {
                            let (plan, analyze) = (view.clone(), view.clone());
                            menu.item(
                                PopupMenuItem::new(
                                    "Explain: the plan, without running the statement",
                                )
                                .on_click(move |_, window, cx| {
                                    plan.update(cx, |this, cx| this.explain(false, window, cx))
                                }),
                            )
                            .item(
                                PopupMenuItem::new(
                                    "Explain analyze: runs the statement and times each step",
                                )
                                .on_click(move |_, window, cx| {
                                    analyze.update(cx, |this, cx| this.explain(true, window, cx))
                                }),
                            )
                        }
                    }),
            )
            .child(
                Button::new("tab-run-all")
                    .ghost()
                    .small()
                    .label("Run all")
                    .tooltip("Run every statement (Cmd+Shift+Enter)")
                    .disabled(running)
                    .on_click(cx.listener(|this, _, window, cx| this.run(Scope::All, window, cx))),
            )
            .when(self.source.is_some(), |this| {
                this.child(
                    Button::new("tab-apply-source")
                        .primary()
                        .small()
                        .label("Apply source")
                        .tooltip("Run the whole script, which replaces the object, once its SQL is confirmed")
                        .disabled(running)
                        .on_click(cx.listener(|this, _, window, cx| this.apply_source(window, cx))),
                )
            })
            // Run and its menu stay together when the toolbar wraps.
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
            .child(
                Button::new("tab-run")
                    .map(|button| {
                        // In source mode, applying is the main action.
                        if self.source.is_some() {
                            button.ghost()
                        } else {
                            button.primary()
                        }
                    })
                    .small()
                    .label("Run")
                    .tooltip("Run the statement at the cursor, or the selection (Cmd+Enter)")
                    .disabled(running)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.run(Scope::AtCursor, window, cx)),
                    ),
            )
            // The other ways to run, and to move between statements and panels.
            .child(
                Button::new("tab-run-menu")
                    .ghost()
                    .small()
                    .dropdown_caret(true)
                    .tooltip("More ways to run")
                    .dropdown_menu({
                        let view = view.clone();
                        move |menu, window, cx| {
                            let entries = tabs::run_menu(view.read(cx).editor_facts(cx));
                            let view = view.clone();
                            tabs::fill_menu(
                                menu,
                                entries,
                                Rc::new(move |action, window, cx| {
                                    let view = view.clone();
                                    window.defer(cx, move |window, cx| {
                                        run_editor_action(&view, action, window, cx)
                                    });
                                }),
                                window,
                                cx,
                            )
                        }
                    }),
            )
            )
    }

    fn render_pane(&self, pane: &ResultPane, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match &pane.outcome {
            // Striped by the grid itself (`Grid::render_tr`).
            Outcome::Rows(grid) => super::grid_view::render(grid, cx),
            Outcome::Done => div()
                .id("result-done")
                .test_support()
                .p_3()
                .text_sm()
                .child(SharedString::from(match pane.rows_affected {
                    Some(1) => "Done. 1 row affected.".to_string(),
                    Some(rows) => format!("Done. {rows} rows affected."),
                    None => "Done.".to_string(),
                }))
                .into_any_element(),
            Outcome::Plan {
                plan,
                raw,
                problem,
                show_raw,
            } => {
                let show_raw = *show_raw || plan.is_none();
                let view = |raw_view: bool, id: &'static str, label: &'static str| {
                    Button::new(id)
                        .ghost()
                        .xsmall()
                        .label(label)
                        .selected(show_raw == raw_view)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let active = this.active_result;
                            if let Some(Outcome::Plan { show_raw, .. }) =
                                this.results.get_mut(active).map(|pane| &mut pane.outcome)
                            {
                                *show_raw = raw_view;
                            }
                            cx.notify();
                        }))
                };
                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .flex_none()
                            .px_2()
                            .py_1()
                            .gap_1()
                            .child(view(false, "plan-as-tree", "Plan").disabled(plan.is_none()))
                            .child(view(true, "plan-as-raw", "Raw output"))
                            .when(AppState::get(cx).settings.ai.enabled, |bar| {
                                bar.child(
                                    Button::new("ai-explain-plan")
                                        .ghost()
                                        .xsmall()
                                        .label("Explain with AI…")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.ai_request(
                                                crate::ai_view::Kind::ExplainPlan,
                                                window,
                                                cx,
                                            )
                                        })),
                                )
                            })
                            .children(problem.clone().map(|problem| {
                                div().pl_2().text_xs().text_color(theme.warning).child(
                                    SharedString::from(format!(
                                        "The plan could not be read as a tree: {problem}"
                                    )),
                                )
                            })),
                    )
                    .child(
                        div().flex_1().min_h_0().child(match plan {
                            Some(plan) if !show_raw => plan_view::render(plan, cx),
                            _ => div()
                                .id("plan-raw")
                                .test_support()
                                .size_full()
                                .p_3()
                                .overflow_y_scroll()
                                .font_family(theme.mono_font_family.clone())
                                .text_sm()
                                .child(SharedString::from(raw.clone()))
                                .into_any_element(),
                        }),
                    )
                    .into_any_element()
            }
            Outcome::Cancelled => div()
                .id("result-cancelled")
                .test_support()
                .p_3()
                .text_sm()
                .text_color(theme.warning)
                .child("Cancelled. Rows that had arrived are kept in their result.")
                .into_any_element(),
            // The message in the danger colour, then its detail and hint.
            Outcome::Failed(failure) => v_flex()
                .id("result-error")
                .test_support()
                .p_3()
                .gap_1()
                .text_sm()
                .children(failure.described().lines().into_iter().enumerate().map(
                    |(index, line)| {
                        div()
                            .when(index == 0, |line| line.text_color(theme.danger))
                            .child(SharedString::from(line))
                    },
                ))
                .when(AppState::get(cx).settings.ai.enabled, |error| {
                    let id = pane.id;
                    error.child(
                        h_flex()
                            .pt_1()
                            .gap_1()
                            // K52: the fix as a diff on the statement, in the editor.
                            .when(pane.source.is_some(), |row| {
                                row.child(
                                    Button::new("ai-fix-error")
                                        .primary()
                                        .xsmall()
                                        .label("Fix with AI")
                                        .tooltip(
                                            "Proposes a corrected statement as a diff in the \
                                             editor; runs nothing",
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.fix_with_ai(Some(id), window, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("ai-explain-error")
                                    .outline()
                                    .xsmall()
                                    .label("Explain error with AI…")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.ai_request(
                                            crate::ai_view::Kind::ExplainError,
                                            window,
                                            cx,
                                        )
                                    })),
                            ),
                    )
                })
                .into_any_element(),
        }
    }

    /// The filter of a SELECT's result, read again by the server
    /// (DBeaver's `ResultSetFilterPanel` over an editor's result).
    fn render_result_filter(&self, requery: &Requery, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let filtered = !requery.filter.trim().is_empty();
        let refined = filtered || requery.sort.is_some();
        let statement = SharedString::from(requery.statement.clone());
        let done = match (filtered, requery.sort.is_some()) {
            (true, true) => "filtered and ordered",
            (true, false) => "filtered",
            _ => "ordered",
        };
        let origin = SharedString::from(if requery.order_dropped {
            format!(
                "{done} on the server, which ignored the statement's ORDER BY: \
                 order by a column header"
            )
        } else {
            format!("{done} on the server")
        });
        h_flex()
            .id("result-filter-bar")
            .test_support()
            .flex_none()
            // In a narrow group, History and Filter go to a second line
            // rather than squeezing the condition to a few letters.
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("WHERE"),
            )
            .child(
                div().flex_1().min_w(FILTER_MIN_WIDTH).child(
                    Input::new(&self.result_filter)
                        .small()
                        .id("result-filter")
                        .font_family(theme.mono_font_family.clone()),
                ),
            )
            .child(history_button(
                "result-filter-history",
                &self.result_filter_history(),
                cx.entity(),
                |this, filter, window, cx| {
                    if let Some(pane) = this.results.get(this.active_result) {
                        let (id, sort) = (
                            pane.id,
                            pane.requery
                                .as_ref()
                                .and_then(|requery| requery.sort.clone()),
                        );
                        this.read_again(id, filter, sort, window, cx);
                    }
                },
            ))
            .child(
                Button::new("result-filter-apply")
                    .outline()
                    .small()
                    .label("Filter")
                    .tooltip("Run the statement again, keeping the rows of this condition (Enter)")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.apply_result_filter(window, cx)),
                    ),
            )
            .when(refined, |bar| {
                bar.child(
                    Button::new("result-filter-clear")
                        .ghost()
                        .small()
                        .label("Clear")
                        .tooltip("Run the statement as it is written")
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(pane) = this.results.get(this.active_result) {
                                this.read_again(pane.id, String::new(), None, window, cx);
                            }
                        })),
                )
                .child(
                    div()
                        .id("result-filter-origin")
                        .test_support()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .aria_label(origin.clone())
                        .child(origin)
                        .tooltip(move |window, cx| {
                            Tooltip::new(statement.clone()).build(window, cx)
                        }),
                )
            })
    }

    /// What the server said on the side during the last run.
    fn render_messages(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("result-messages")
            .test_support()
            .flex_1()
            .min_h_0()
            .p_3()
            .gap_2()
            .text_sm()
            .children(self.notices.iter().map(|notice| {
                let warning = notice.severity.to_uppercase().contains("WARN");
                v_flex()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(if warning {
                                        theme.warning
                                    } else {
                                        theme.muted_foreground
                                    })
                                    .child(SharedString::from(notice.severity.to_uppercase())),
                            )
                            .child(SharedString::from(match &notice.code {
                                Some(code) => format!("{} [{code}]", notice.message),
                                None => notice.message.clone(),
                            })),
                    )
                    .children(notice.detail.clone().map(|detail| {
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(detail))
                    }))
            }))
    }

    /// What a result whose rows take changes adds above them: the table
    /// they are written to, the row commands, and the pending changes with
    /// Discard, Review SQL and Apply (as a table's data toolbar).
    fn render_edit_bar(
        &self,
        pane: &ResultPane,
        grid: &Entity<TableState<Grid>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, warning, danger, border) = (
            theme.muted_foreground,
            theme.warning,
            theme.danger,
            theme.border,
        );
        let state = grid.read(cx);
        let delegate = state.delegate();
        let pending = delegate.pending().map_or(0, |pending| pending.count());
        let table = delegate
            .table()
            .map(|table| format!("{}.{}", table.schema, table.name))
            .unwrap_or_default();
        let selected = delegate.selected(state.selection()).is_some();
        let busy = self.running();
        let action = |id: &'static str, label: &'static str, tip: &'static str| {
            Button::new(id).ghost().xsmall().label(label).tooltip(tip)
        };
        v_flex()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .child(
                h_flex()
                    .id("result-edit-bar")
                    .test_support()
                    // Beside the value panel in a narrow group, the row
                    // commands and Apply go to more lines, never off the edge.
                    .flex_wrap()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .text_xs()
                    .child(
                        div()
                            .id("result-edit-table")
                            .test_support()
                            .max_w_full()
                            .truncate()
                            .px_1()
                            .text_color(muted)
                            .child(SharedString::from(format!("Rows of {table}"))),
                    )
                    .child(
                        action("result-add-row", "Add row", "Add an empty row")
                            .on_click(cx.listener(|this, _, window, cx| this.add_row(window, cx))),
                    )
                    .child(
                        action(
                            "result-delete",
                            "Delete",
                            "Mark the selected rows for deletion",
                        )
                        .disabled(!selected)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.delete_rows(None, window, cx)),
                        ),
                    )
                    .child(
                        action(
                            "result-set-null",
                            "Set NULL",
                            "Set the selected cells to NULL",
                        )
                        .disabled(!selected)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_selected(CellValue::Null, window, cx)
                        })),
                    )
                    .child(div().flex_1())
                    .when(pending > 0, |bar| {
                        bar.child(
                            div()
                                .id("result-pending")
                                .test_support()
                                .px_1()
                                .text_color(warning)
                                .child(SharedString::from(format!(
                                    "{pending} pending {}",
                                    if pending == 1 { "change" } else { "changes" }
                                ))),
                        )
                        .child(
                            action("result-discard", "Discard", "Forget the pending changes")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.discard(cx))),
                        )
                        .child(
                            action(
                                "result-review",
                                "Review SQL",
                                "Show the statements of the pending changes",
                            )
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.review(window, cx))),
                        )
                        .child(
                            Button::new("result-apply")
                                .primary()
                                .xsmall()
                                .label("Apply")
                                .tooltip("Review the SQL, then apply it")
                                .disabled(busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.review(window, cx)),
                                ),
                        )
                    }),
            )
            .children(pane.apply_error.clone().map(|error| {
                div()
                    .id("result-apply-error")
                    .test_support()
                    .px_3()
                    .py_1()
                    .text_sm()
                    .text_color(danger)
                    .child(SharedString::from(error))
            }))
    }

    // Stacked results (J11, DBeaver #6861) -----------------------------------

    /// Shows the results of a run one under the other (`on`), or one result
    /// tab each, in every query tab (Settings ▸ Grid, the results menu, the
    /// palette).
    pub fn set_stacked(on: bool, window: &mut Window, cx: &mut App) {
        AppState::update_settings(cx, |settings| settings.stack_results = on);
        window.refresh();
    }

    /// Whether this tab shows its results stacked.
    pub fn stacked(&self, cx: &App) -> bool {
        AppState::get(cx).settings.stack_results && !self.unstacked
    }

    /// Stacks the results or puts them back in tabs, from this tab, which
    /// follows the setting again after "Open in its own tab".
    pub fn stack_results(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.unstacked = false;
        self.revealed_result = None;
        Self::set_stacked(on, window, cx);
        cx.notify();
    }

    /// Shows stacked result `id` in its own result tab: the results are in
    /// tabs until the next run, nothing runs again.
    fn unstack(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(index) = self.pane_index(id) {
            self.active_result = index;
            self.unstacked = true;
            self.revealed_result = None;
            cx.notify();
        }
    }

    /// The stacked result a click went to has the focus: the find, the
    /// copy, the value viewer, the filter and the footer act on it.
    fn focus_stacked(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(index) = self.pane_index(id)
            && index != self.active_result
        {
            self.active_result = index;
            // Clicked, so in view: the list stays where it is.
            self.revealed_result = Some(id);
            cx.notify();
        }
    }

    /// How tall a stacked grid is: as many rows as it holds, up to
    /// [`STACK_ROWS`], or what its lower edge was dragged to.
    fn stacked_grid_height(
        &self,
        pane: &ResultPane,
        grid: &Entity<TableState<Grid>>,
        cx: &App,
    ) -> Pixels {
        let row = table_size(cx).table_row_height();
        if let Some(height) = pane.stack_height {
            return height.max(row * 3.);
        }
        let rows = grid.read(cx).delegate().row_count().clamp(1, STACK_ROWS);
        let find = if find::carried(grid, cx).is_some() {
            px(36.)
        } else {
            px(0.)
        };
        // The header row, the rows, room for the horizontal scrollbar.
        row * (rows + 1) as f32 + px(14.) + find
    }

    /// Every result of the run in one scrolling panel: per result a header
    /// and its grid (or its status line), the messages last. The filter, the
    /// edit bar and the footer are those of the result that has the focus.
    fn render_stacked(&self, cx: &mut Context<Self>) -> AnyElement {
        let keys: Vec<u64> = self
            .results
            .iter()
            .map(|pane| pane.id)
            .chain((!self.notices.is_empty()).then_some(u64::MAX))
            .collect();
        {
            // What came at the end is measured; the rest keeps its place. A
            // new run starts at the top.
            let mut shown = self.stack_items.borrow_mut();
            if *shown != keys {
                let common = shown.iter().zip(&keys).take_while(|(a, b)| a == b).count();
                if common == 0 {
                    self.stack_list.reset(keys.len());
                } else {
                    self.stack_list
                        .splice(common..shown.len(), keys.len() - common);
                }
                *shown = keys;
            }
        }
        let (filter_bar, edit_bar, footer) = match self.results.get(self.active_result) {
            Some(pane) => {
                let (filter, edit, footer) = self.render_result_bars(pane, cx);
                (filter, edit, Some(footer))
            }
            None => (None, None, None),
        };
        v_flex()
            .size_full()
            .children(filter_bar)
            .children(edit_bar)
            .child(
                div()
                    .id("stacked-results")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(
                        list(
                            self.stack_list.clone(),
                            cx.processor(|this, index: usize, _, cx| {
                                this.render_stacked_item(index, cx)
                            }),
                        )
                        .size_full(),
                    ),
            )
            .children(footer)
            .into_any_element()
    }

    /// One item of the stacked results: a result, or the messages.
    fn render_stacked_item(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let (border, muted, danger, warning, mono, active_bg, header_bg) = {
            let theme = cx.theme();
            (
                theme.border,
                theme.muted_foreground,
                theme.danger,
                theme.warning,
                theme.mono_font_family.clone(),
                theme.list_active,
                theme.tab_bar,
            )
        };
        let header = |id: gpui_kit::ElementId| {
            h_flex()
                .id(id)
                .test_support()
                .flex_none()
                .h(STACK_HEADER)
                .px_2()
                .gap_2()
                .text_xs()
                .border_b_1()
                .border_color(border)
        };
        let Some(pane) = self.results.get(index) else {
            return v_flex()
                .id("stacked-messages")
                .test_support()
                .w_full()
                .child(
                    header("stacked-messages-header".into())
                        .bg(header_bg)
                        .child(SharedString::from(format!(
                            "Messages ({})",
                            self.notices.len()
                        ))),
                )
                .child(
                    div()
                        .id("stacked-messages-body")
                        .max_h(px(240.))
                        .overflow_y_scroll()
                        .child(self.render_messages(cx)),
                )
                .into_any_element();
        };
        let id = pane.id;
        let active = index == self.active_result;
        let grid = pane.grid();
        let facts = match (&pane.outcome, &grid) {
            (_, Some(grid)) => {
                let grid = grid.read(cx).delegate();
                let rows = plan_view::number(grid.row_count() as f64);
                let rows = if grid.is_complete() {
                    format!("{rows} rows")
                } else {
                    format!("first {rows} rows")
                };
                Some(rows)
            }
            (Outcome::Done, _) => pane.rows_affected.map(|rows| format!("{rows} affected")),
            _ => None,
        };
        let exportable = self.exportable(pane, cx);
        let bar = header(("stacked-header", index).into())
            .bg(if active { active_bg } else { header_bg })
            .context_menu(self.result_menu(id, cx))
            .child(
                div()
                    .flex_none()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(SharedString::from(self.result_label(index))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(mono)
                    .text_color(muted)
                    .child(SharedString::from(one_line(
                        pane.statement.lines().next().unwrap_or_default(),
                    ))),
            )
            .children(facts.map(|facts| {
                div()
                    .id(("stacked-facts", index))
                    .test_support()
                    .flex_none()
                    .text_color(muted)
                    .child(SharedString::from(facts))
            }))
            .children(pane.elapsed.map(|elapsed| {
                div()
                    .flex_none()
                    .text_color(muted)
                    .child(SharedString::from(duration_text(elapsed)))
            }))
            .when_some(grid.clone(), |bar, grid| {
                bar.child(
                    Button::new(("stacked-copy", index))
                        .ghost()
                        .xsmall()
                        .label("Copy")
                        .tooltip("Copy every fetched row, with the column names")
                        .on_click(move |_, _, cx| {
                            grid.update(cx, |table, cx| {
                                table.delegate_mut().select_all();
                                Grid::copy(table, Some(CopyFormat::TsvWithHeader), None, cx);
                                cx.notify();
                            });
                        }),
                )
            })
            .when(exportable, |bar| {
                bar.child(
                    Button::new(("stacked-export", index))
                        .ghost()
                        .xsmall()
                        .label("Export…")
                        .tooltip("Run the statement again and write all its rows to a file")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let statement = this
                                .pane_index(id)
                                .map(|index| this.results[index].statement.clone());
                            if let Some(statement) = statement {
                                this.export_statement(statement, window, cx);
                            }
                        })),
                )
            })
            .child(
                Button::new(("stacked-own-tab", index))
                    .ghost()
                    .xsmall()
                    .label("Open in its own tab")
                    .tooltip("Show the results in tabs, this one on screen, until the next run")
                    .on_click(cx.listener(move |this, _, _, cx| this.unstack(id, cx))),
            );
        let status = |text: String, color: Option<gpui_kit::Hsla>| {
            div()
                .id(("stacked-status", index))
                .test_support()
                .px_3()
                .py_2()
                .text_sm()
                .when_some(color, |line, color| line.text_color(color))
                .child(SharedString::from(text))
        };
        let body = match (&pane.outcome, grid) {
            (_, Some(grid)) => {
                let height = self.stacked_grid_height(pane, &grid, cx);
                v_flex()
                    .child(
                        div()
                            .id(("stacked-grid", index))
                            .test_support()
                            .w_full()
                            .h(height)
                            .child(super::grid_view::render(&grid, cx)),
                    )
                    // Its lower edge drags to make it taller or shorter.
                    .child(
                        div()
                            .id(("stacked-resize", index))
                            .test_support()
                            .h(px(5.))
                            .w_full()
                            .border_b_1()
                            .border_color(border)
                            .cursor_row_resize()
                            .hover(|edge| edge.bg(border))
                            .on_drag(StackResize(id), |_, _, _, cx| cx.new(|_| EmptyView)),
                    )
                    .into_any_element()
            }
            (Outcome::Plan { .. }, None) => div()
                .h(px(320.))
                .child(self.render_pane(pane, cx))
                .into_any_element(),
            (Outcome::Done, None) => status(
                match pane.rows_affected {
                    Some(1) => "Done. 1 row affected.".to_string(),
                    Some(rows) => format!("Done. {rows} rows affected."),
                    None => "Done.".to_string(),
                },
                None,
            )
            .into_any_element(),
            (Outcome::Cancelled, None) => status(
                "Cancelled. Rows that had arrived are kept in their result.".into(),
                Some(warning),
            )
            .into_any_element(),
            (Outcome::Failed(failure), None) => {
                status(failure.described().to_string(), Some(danger)).into_any_element()
            }
            (Outcome::Rows(_), None) => div().into_any_element(),
        };
        v_flex()
            .id(("stacked-result", index))
            .test_support()
            .w_full()
            .border_b_1()
            .border_color(border)
            .capture_any_mouse_up(cx.listener(move |this, _, _, cx| this.focus_stacked(id, cx)))
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<StackResize>, _, cx| {
                    if event.drag(cx).0 != id {
                        return;
                    }
                    let height = event.event.position.y - event.bounds.top() - STACK_HEADER;
                    if let Some(index) = this.pane_index(id) {
                        this.results[index].stack_height = Some(height.min(px(2000.)));
                        cx.notify();
                    }
                }),
            )
            .child(bar)
            .child(body)
            .into_any_element()
    }

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (muted, warning) = {
            let theme = cx.theme();
            (theme.muted_foreground, theme.warning)
        };
        // The server's messages get a tab of their own after the results.
        let messages = !self.notices.is_empty();
        let tab_count = self.results.len() + usize::from(messages);
        // While a run starts, its first result is not there yet: kept
        // results stay in their tabs, and the area says it is running.
        let starting =
            (self.run.is_some() || self.pending.is_some()) && self.active_result >= tab_count;
        let active = self.active_result.min(tab_count.saturating_sub(1));
        let tabs = TabBar::new("results")
            .track_scroll(&self.results_scroll)
            .when(!starting, |bar| bar.selected_index(active))
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                this.active_result = *index;
                cx.notify();
            }))
            .children(self.results.iter().enumerate().map(|(index, pane)| {
                let id = pane.id;
                let statement = SharedString::from(pane.statement.clone());
                let incomplete = self.incomplete(pane, cx);
                Tab::new()
                    .when(pane.pinned, |tab| {
                        tab.prefix(Icon::new(Glyph::Bookmark).xsmall())
                    })
                    // A middle click closes the result, as in a browser.
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if this.free_to_run(window, cx) {
                                this.close_results(&[id], cx)
                            }
                        }),
                    )
                    .child(
                        div()
                            .id(("result-tab", index))
                            .test_support()
                            // Which statement it is the result of.
                            .when(!statement.is_empty(), |this| {
                                this.tooltip(move |window, cx| {
                                    Tooltip::new(statement.clone()).build(window, cx)
                                })
                            })
                            .context_menu(self.result_menu(id, cx))
                            .child(SharedString::from(self.result_label(index))),
                    )
                    // Not every row is there: says so, and how to get them.
                    .when(incomplete, |tab| {
                        tab.suffix(
                            div()
                                .id(("result-incomplete", index))
                                .test_support()
                                .pl_1()
                                .text_xs()
                                .text_color(warning)
                                .child("incomplete")
                                .tooltip(|window, cx| {
                                    Tooltip::new(
                                        "Not every row arrived: another statement ran on this \
                                         tab's session while this result was waiting to be \
                                         paged, or it was cancelled. Choose “Run again” in its \
                                         menu for the whole result.",
                                    )
                                    .build(window, cx)
                                }),
                        )
                    })
            }))
            .children(messages.then(|| {
                Tab::new().label(SharedString::from(format!(
                    "Messages ({})",
                    self.notices.len()
                )))
            }));
        let pane = (!starting).then(|| self.results.get(active)).flatten();
        let Some(pane) = pane else {
            if messages && !starting {
                return v_flex()
                    .size_full()
                    .child(tabs)
                    .child(self.render_messages(cx));
            }
            let connecting = self.pending.and(self.target.as_ref()).map(|target| {
                let name = self
                    .connections
                    .read(cx)
                    .get(&target.connection)
                    .map(|saved| saved.config.name.clone())
                    .unwrap_or_default();
                let verb = if self.connections.read(cx).reconnecting(&target.connection) {
                    "Reconnecting"
                } else {
                    "Connecting"
                };
                format!("{verb} to “{name}”… The statements run once it is open.")
            });
            let text = match (connecting, self.activity) {
                (Some(connecting), _) => connecting,
                (None, Activity::Running { .. }) => "Running…".to_string(),
                (None, Activity::Idle) => "Run a statement to see its result here.".to_string(),
            };
            return v_flex()
                .size_full()
                .when(tab_count > 0, |this| this.child(tabs))
                .child(
                    div()
                        .id("results-waiting")
                        .test_support()
                        .p_3()
                        .text_sm()
                        .text_color(muted)
                        .child(SharedString::from(text)),
                );
        };
        let (filter_bar, edit_bar, footer) = self.render_result_bars(pane, cx);
        v_flex()
            .size_full()
            .child(tabs)
            .children(filter_bar)
            .children(edit_bar)
            .child(div().flex_1().min_h_0().child(self.render_pane(pane, cx)))
            .child(footer)
    }

    /// What goes around the result on screen: the filter read again by the
    /// server, the bar of the rows that take changes, and the footer
    /// (counts, fetch, export, the statement and its time). The same for a
    /// result tab and for the result that has the focus among stacked ones.
    fn render_result_bars(
        &self,
        pane: &ResultPane,
        cx: &mut Context<Self>,
    ) -> (Option<AnyElement>, Option<AnyElement>, AnyElement) {
        let (border, muted, warning, mono) = {
            let theme = cx.theme();
            (
                theme.border,
                theme.muted_foreground,
                theme.warning,
                theme.mono_font_family.clone(),
            )
        };
        // What the footer says about the result on screen.
        let (summary, has_more, complete, order) = match &pane.outcome {
            Outcome::Rows(grid) => {
                let grid = grid.read(cx).delegate();
                let rows = grid.row_count();
                let more = grid.has_more();
                let count = plan_view::number(rows as f64);
                // The find shows matching rows only: of how many.
                let shown = super::find::rows_shown(grid);
                let text = match pane.total {
                    _ if shown.is_some() => shown.unwrap_or_default(),
                    _ if grid.is_complete() => {
                        format!("{count} row{}", if rows == 1 { "" } else { "s" })
                    }
                    Some(total) => {
                        format!("first {count} of {} rows", plan_view::number(total as f64))
                    }
                    None if more => format!("{count} rows fetched, more on the server"),
                    // The next page is on its way (K21).
                    None if grid.has_more_coming() => {
                        format!("{count} rows fetched, fetching more…")
                    }
                    None => format!("{count} rows so far"),
                };
                // Which order the rows are in, as DBeaver's smart ordering
                // chose it.
                let order = grid.ordered_by_server().map(|server| {
                    if server {
                        "ordered by the server"
                    } else {
                        "ordered here (every row is fetched)"
                    }
                });
                (text, more, grid.is_complete(), order)
            }
            _ => (String::new(), false, true, None),
        };
        let requery = pane.requery.clone();
        let auto_here = requery.as_ref().is_some_and(|requery| requery.auto);
        let elapsed = pane.elapsed.map(duration_text);
        let grid = match &pane.outcome {
            Outcome::Rows(grid) => Some(grid.clone()),
            _ => None,
        };
        let exportable = self.exportable(pane, cx);
        let filter_bar = requery
            .as_ref()
            .map(|requery| self.render_result_filter(requery, cx).into_any_element());
        let edit_bar = grid
            .as_ref()
            .filter(|_| matches!(pane.editable, Some(Ok(()))))
            .map(|grid| self.render_edit_bar(pane, grid, cx).into_any_element());
        let read_only = match &pane.editable {
            Some(Err(why)) => Some(SharedString::from(why.to_string())),
            _ => None,
        };
        let footer = h_flex()
            .id("result-footer")
            .test_support()
            .flex_none()
            // A narrow result keeps Value, Export and the fetch buttons in
            // reach: they go to a second line instead of off the edge.
            .flex_wrap()
            .min_h(px(26.))
            .px_3()
            .py_0p5()
            .gap_x_3()
            .gap_y_0p5()
            .border_t_1()
            .border_color(border)
            .text_xs()
            .text_color(muted)
            .child({
                let summary = SharedString::from(summary);
                div()
                    .id("result-summary")
                    .test_support()
                    .aria_label(summary.clone())
                    .child(summary)
            })
            .children(order.map(|order| div().id("result-order").test_support().child(order)))
            .when(
                requery.is_some()
                    && grid.is_some()
                    && !complete
                    && pane.total.is_none()
                    && self.side.is_none(),
                |this| {
                    this.child(
                        Button::new("result-count")
                            .ghost()
                            .xsmall()
                            .label("Count")
                            .tooltip("Count every row of the result on the server")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.count_result(window, cx)),
                            ),
                    )
                },
            )
            .when(self.side.is_some(), |this| {
                this.child("reading…").child(
                    Button::new("result-side-cancel")
                        .ghost()
                        .xsmall()
                        .label("Cancel")
                        .tooltip("Stop the statement")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .when(requery.is_some(), |this| {
                this.child(auto_refresh_button(
                    "result-auto-refresh",
                    self.auto_refresh().filter(|_| auto_here),
                    cx.entity(),
                    |this, every, window, cx| this.set_auto_refresh(every, window, cx),
                ))
            })
            .children(self.auto_refresh_stopped.map(|why| {
                div()
                    .id("result-auto-refresh-stopped")
                    .test_support()
                    .text_color(warning)
                    .child(SharedString::from(format!("Auto-refresh stopped: {why}.")))
            }))
            .children(grid.clone().map(|table| {
                Button::new("result-record")
                    .ghost()
                    .xsmall()
                    .label("Record")
                    .tooltip("One row as a list of its columns, or the grid again (Tab)")
                    .on_click(move |_, window, cx| Grid::toggle_record(&table, window, cx))
            }))
            .when(grid.is_some(), |this| {
                this.child(value_panel::button(
                    "result-value-panel",
                    self.value_panel.as_ref(),
                    cx,
                ))
            })
            .children(
                grid.as_ref()
                    .map(|table| super::grid_view::SelectionStats::of(table, cx)),
            )
            .when(grid.is_some() && has_more, |this| {
                this.child(
                    Button::new("fetch-more")
                        .ghost()
                        .xsmall()
                        .label("Fetch more")
                        .on_click(cx.listener(|this, _, _, cx| this.fetch(false, cx))),
                )
                .child(
                    Button::new("fetch-all")
                        .ghost()
                        .xsmall()
                        .label("Fetch all")
                        .on_click(cx.listener(|this, _, _, cx| this.fetch(true, cx))),
                )
            })
            .when(exportable, |this| {
                this.child(
                    Button::new("result-export")
                        .ghost()
                        .xsmall()
                        .label("Export…")
                        .tooltip("Run the statement again and write all its rows to a file")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.export_result(window, cx);
                        })),
                )
            })
            // Why its rows take no change.
            .children(read_only.map(|why| {
                div()
                    .id("result-read-only")
                    .test_support()
                    .min_w_0()
                    .truncate()
                    .child(why.clone())
                    .tooltip(move |window, cx| Tooltip::new(why.clone()).build(window, cx))
            }))
            // Which statement this is the result of.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_right()
                    .font_family(mono)
                    .child(SharedString::from(one_line(&pane.statement))),
            )
            .children(elapsed.map(SharedString::from))
            .into_any_element();
        (filter_bar, edit_bar, footer)
    }
}

// The AI assistant (J17, DBeaver #36951, #20072): the request is put
// together from what the tab shows; `crate::ai_view` does the rest.
impl QueryTab {
    /// Opens the AI panel on a request of `kind`: the sentence to write, the
    /// statement at the cursor (or the selection), the plan or the error on
    /// screen. Off, says how to turn the assistant on and opens nothing.
    pub fn ai_request(
        &mut self,
        kind: crate::ai_view::Kind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::ai_view::Kind;
        use kerno_core::ai::Action as AiAction;
        let say = |text: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            window.push_notification(Notification::warning(SharedString::from(text)), cx)
        };
        if !AppState::get(cx).settings.ai.enabled {
            say(crate::ai_view::OFF_NOTICE, window, cx);
            return;
        }
        let pane = self.results.get(self.active_result);
        let action = match kind {
            Kind::WriteSql => AiAction::WriteSql {
                sentence: String::new(),
            },
            Kind::ExplainQuery => {
                let sql = (self.statements(Scope::AtCursor, cx).into_iter())
                    .map(|statement| statement.sql)
                    .collect::<Vec<_>>()
                    .join(";\n");
                if sql.trim().is_empty() {
                    say("No statement at the cursor to explain.", window, cx);
                    return;
                }
                AiAction::ExplainQuery { sql }
            }
            Kind::ExplainPlan => match pane.map(|pane| (&pane.statement, &pane.outcome)) {
                Some((sql, Outcome::Plan { raw, .. })) => AiAction::ExplainPlan {
                    sql: sql.clone(),
                    plan: raw.clone(),
                },
                _ => {
                    say("No plan on screen: run Explain plan first.", window, cx);
                    return;
                }
            },
            Kind::ExplainError => match pane.map(|pane| (&pane.statement, &pane.outcome)) {
                Some((sql, Outcome::Failed(failure))) => AiAction::ExplainError {
                    sql: sql.clone(),
                    error: failure.described().to_string(),
                },
                _ => {
                    say("No error on screen to explain.", window, cx);
                    return;
                }
            },
        };
        // K51: the panel beside the window, on this tab's conversation, the
        // request ready in its field (sent when the user sends it); once this
        // tab is no longer being updated.
        let (tab, draft) = (
            cx.entity(),
            crate::ai_view::Draft {
                action: Some(action),
                text: String::new(),
            },
        );
        window.defer(cx, move |window, cx| {
            crate::ai_view::open(&tab, draft, window, cx)
        });
    }

    /// This tab's AI conversation, made at its first use (K51).
    pub fn ai_chat(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<crate::ai_view::AiPanel> {
        if let Some(panel) = &self.ai {
            return panel.clone();
        }
        let (tab, editor, facts) = (
            cx.entity().downgrade(),
            self.editor.clone(),
            self.ai_facts(cx),
        );
        let panel = cx.new(|cx| crate::ai_view::AiPanel::new(tab, editor, facts, window, cx));
        self.ai = Some(panel.clone());
        panel
    }

    /// The editor's selection, else the statement at the cursor, with its
    /// place in the editor's text: what the AI panel attaches and replaces.
    pub fn ai_statement(&self, cx: &App) -> Option<(String, std::ops::Range<usize>)> {
        let editor = self.editor.read(cx);
        let selection = editor.selected_range();
        if let Some(selected) = editor.text().to_string().get(selection.clone())
            && !selected.trim().is_empty()
        {
            return Some((selected.to_string(), selection));
        }
        let statement = self.statements(Scope::AtCursor, cx).into_iter().next()?;
        Some((statement.sql, statement.place?))
    }

    /// What the AI assistant is told of this tab: its connection's engine
    /// and version, its database and schema, its completion catalog, the
    /// session to read rows and structures on.
    pub fn ai_facts(&self, cx: &App) -> crate::ai_view::Facts {
        let database = self.choices(cx).database;
        let connections = self.connections.read(cx);
        let connection = self.target.as_ref().map(|target| target.connection.clone());
        let engine = connection
            .as_ref()
            .and_then(|id| connections.get(id))
            .map(|saved| saved.config.engine);
        // The catalog's database, as `load_catalog` read it: the sample
        // rows come from the same one.
        let meta = connection
            .as_ref()
            .and_then(|id| connections.meta_session(id));
        let in_meta = self.target.as_ref().is_some_and(|target| {
            target.database.is_none() || target.database == meta.as_ref().and_then(|m| m.database())
        });
        let session = if in_meta { meta } else { self.session.clone() };
        let mut catalog = self.catalog.borrow().clone();
        // MySQL's schemas are databases: only the tab's goes (§4 J17).
        if matches!(
            engine,
            Some(kerno_core::Engine::MySql | kerno_core::Engine::MariaDb)
        ) && let Some(database) = &database
        {
            catalog = Arc::new(kerno_core::sql::Catalog {
                schemas: (catalog.schemas.iter())
                    .filter(|schema| schema.name == *database)
                    .cloned()
                    .collect(),
                search_path: vec![database.clone()],
            });
        }
        crate::ai_view::Facts {
            catalog,
            engine: engine
                .map(|engine| engine.label().to_string())
                .unwrap_or_default(),
            server_version: match connection.as_ref().map(|id| connections.status(id)) {
                Some(Status::Connected { version }) => Some(version),
                _ => None,
            },
            database,
            schema: self
                .target
                .as_ref()
                .and_then(|target| target.schema.clone()),
            session,
        }
    }

    /// The AI conversation, once made; for tests.
    #[cfg(test)]
    pub fn ai_panel(&self) -> Option<&Entity<crate::ai_view::AiPanel>> {
        self.ai.as_ref()
    }
}

/// Runs an entry of the editor's menu as its command does: through the
/// editor, which has the focus.
pub(crate) fn run_editor_action(
    view: &Entity<QueryTab>,
    action: TabAction,
    window: &mut Window,
    cx: &mut App,
) {
    let focus = view.read(cx).focus_handle(cx);
    focus.focus(window, cx);
    // A change of what the tab runs on, as the toolbar's lists make it.
    let retarget = |change: &dyn Fn(Target) -> Target, cx: &mut App| {
        view.update(cx, |tab, cx| {
            let target = tab.target.clone().map(change);
            tab.request_target(target, cx)
        })
    };
    let command: Box<dyn gpui_kit::Action> = match action {
        TabAction::ApplySource => {
            view.update(cx, |tab, cx| tab.apply_source(window, cx));
            return;
        }
        TabAction::UseConnection(id, _) => {
            view.update(cx, |tab, cx| {
                tab.request_target(
                    Some(Target {
                        connection: id,
                        database: None,
                        schema: None,
                    }),
                    cx,
                )
            });
            return;
        }
        TabAction::UseDatabase(database) => {
            retarget(
                &|target| Target {
                    database: Some(database.clone()),
                    schema: None,
                    ..target
                },
                cx,
            );
            return;
        }
        TabAction::UseSchema(schema) => {
            retarget(
                &|target| Target {
                    schema: Some(schema.clone()),
                    ..target
                },
                cx,
            );
            return;
        }
        TabAction::CopyStatement => Box::new(actions::CopySelectedQuery),
        TabAction::ToggleBlockComment => Box::new(actions::ToggleBlockComment),
        TabAction::UpperCase => Box::new(actions::UpperCase),
        TabAction::LowerCase => Box::new(actions::LowerCase),
        TabAction::ToggleSoftWrap => Box::new(actions::ToggleSoftWrap),
        TabAction::SelectAllOccurrences => Box::new(actions::SelectAllOccurrences),
        TabAction::UnfoldAll => Box::new(actions::UnfoldAll),
        TabAction::FetchAllRows => Box::new(actions::FetchAllRows),
        TabAction::ExportFromQuery => Box::new(actions::ExportFromQuery),
        TabAction::RevertFile => Box::new(actions::RevertFile),
        TabAction::OpenFile => Box::new(actions::OpenSqlFile),
        TabAction::Rename => Box::new(actions::RenameTab),
        TabAction::RunStatement => Box::new(RunStatement),
        TabAction::RunInNewTab => Box::new(RunStatementInNewTab),
        TabAction::RunFromCursor => Box::new(RunFromCursor),
        TabAction::RunScript => Box::new(RunAll),
        TabAction::RunOnConnections => Box::new(actions::RunScriptOnConnections),
        TabAction::Explain => Box::new(ExplainStatement),
        TabAction::ExplainAnalyze => Box::new(ExplainAnalyze),
        TabAction::CountRows => Box::new(CountStatementRows),
        TabAction::Format => Box::new(FormatSql),
        TabAction::NextStatement => Box::new(NextStatement),
        TabAction::PreviousStatement => Box::new(PreviousStatement),
        TabAction::ToggleResults => Box::new(ToggleResultsPanel),
        TabAction::MaximizeResults => Box::new(MaximizeResultsPanel),
        TabAction::SwitchFocus => Box::new(SwitchResultsFocus),
        // The editor's own actions, as its built-in menu does.
        TabAction::Undo => Box::new(input::Undo),
        TabAction::Redo => Box::new(input::Redo),
        TabAction::Cut => Box::new(input::Cut),
        TabAction::Copy => Box::new(input::Copy),
        TabAction::Paste => Box::new(input::Paste),
        TabAction::SelectAll => Box::new(input::SelectAll),
        TabAction::ToggleComment => Box::new(ToggleComment),
        TabAction::Find => Box::new(FindInEditor),
        TabAction::Replace => Box::new(ReplaceInEditor),
        TabAction::Save => Box::new(SaveFile),
        TabAction::SaveAs => Box::new(SaveFileAs),
        TabAction::SaveQuery => Box::new(SaveQuery),
        other => {
            tracing::warn!(?other, "not an editor action");
            return;
        }
    };
    focus.dispatch_action(command.as_ref(), window, cx);
}

impl Focusable for QueryTab {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

/// A connection picker's menu: `choices` (id, name, connected) in order; a
/// pick moves the tab there, asking first when a transaction is open. One
/// not connected can be picked too: the tab's first run connects it, as a
/// tab of a closed connection does (C6: a script's last connection is
/// rarely open when the file is opened again).
fn connection_menu(
    choices: Vec<(String, String, bool)>,
    view: Entity<QueryTab>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    move |mut menu, _, _| {
        for (id, name, connected) in &choices {
            let (view, id) = (view.clone(), id.clone());
            let label = if *connected {
                name.clone()
            } else {
                format!("{name} (not connected)")
            };
            menu = menu.item(PopupMenuItem::new(SharedString::from(label)).on_click(
                move |_, _, cx| {
                    view.update(cx, |this, cx| {
                        this.request_target(
                            Some(Target {
                                connection: id.clone(),
                                database: None,
                                schema: None,
                            }),
                            cx,
                        )
                    })
                },
            ));
        }
        menu
    }
}

impl Render for QueryTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The value panel follows the result on screen (the focused one
        // among stacked results).
        let (grid, why) = (self.active_grid(), self.result_edits().err().flatten());
        let lookup = self.target.clone().map(|target| value_panel::Lookup {
            connections: self.connections.clone(),
            target,
        });
        value_panel::sync(
            &mut self.value_panel,
            grid.as_ref(),
            why,
            lookup,
            window,
            cx,
        );
        // The filter field shows the filter of the result on screen.
        let filterable = self
            .results
            .get(self.active_result)
            .and_then(|pane| Some((pane.id, pane.requery.clone()?)));
        if let Some((id, requery)) = filterable
            && self.result_filter_for != Some(id)
        {
            self.result_filter_for = Some(id);
            let shown = (requery.statement.clone(), requery.filter.clone());
            if self.result_filter_shown.as_ref() != Some(&shown) {
                self.result_filter
                    .update(cx, |input, cx| input.set_value(requery.filter, window, cx));
                self.load_result_history(&requery.statement, cx);
                self.result_filter_shown = Some(shown);
            }
        }
        let shows_results = !self.results.is_empty()
            || !self.notices.is_empty()
            || self.run.is_some()
            || self.pending.is_some();
        let panels = if shows_results {
            self.panels
        } else {
            Panels::Editor
        };
        // Stacked results have no result tab bar (J11).
        let stacked = self.stacked(cx) && !self.results.is_empty();
        // The result on screen is scrolled into the tab bar's view.
        let shown = self.results.get(self.active_result).map(|pane| pane.id);
        // Stacked, the list scrolls to it (an error that ended the run).
        if panels != Panels::Editor && stacked && shown.is_some() && self.revealed_result != shown {
            self.stack_list.scroll_to_reveal_item(self.active_result);
            self.revealed_result = shown;
        }
        if panels != Panels::Editor && !stacked && shown.is_some() && self.revealed_result != shown
        {
            self.results_scroll.scroll_to_item(self.active_result);
            // Before the bar's first layout there is no width to scroll in.
            if self.results_scroll.bounds().size.width > px(0.) {
                self.revealed_result = shown;
            } else {
                window.request_animation_frame();
            }
        }
        // The editor's own menu is native and closed to tests: an empty one
        // keeps its cursor placement, and the drawn menu around it shows.
        let editor = div()
            .id("query-editor")
            .test_support()
            .size_full()
            // Tab accepts the highlighted completion as Enter does (J1,
            // DBeaver #2316): the kit's list knows only Enter, and its Tab
            // indents. Before the editor sees the key; otherwise Tab indents.
            .capture_action(cx.listener(|this, _: &input::IndentInline, window, cx| {
                // The list lets the key go on: stopped after it took it.
                if completion::tab_accepts(&this.editor, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .context_menu(self.editor_menu(cx))
            .child(
                Editor::new(&self.editor)
                    .h_full()
                    .bordered(false)
                    .appearance(false)
                    // `>=` stays two signs unless ligatures are chosen.
                    .when(!AppState::get(cx).settings.ligatures, |editor| {
                        editor.font_features(gpui_kit::FontFeatures::disable_ligatures())
                    })
                    .context_menu(|menu, _, _| menu),
            )
            // Where the last run stands, in the margin (K37): after the
            // editor, so that its layout of this frame is the one read.
            .relative()
            .child(run_marks::overlay(self.editor.clone(), self.trace.clone()))
            // The assistant asked at a place, and "Fix with AI" by an error's mark (K52).
            .children(
                (self.inline.as_ref())
                    .map(|inline| super::ai_inline::overlay(&self.editor, inline, cx)),
            )
            .children(self.fix_chip(cx));
        // Vim mode (J19) puts its key handlers and command field around
        // it, only while it is on.
        let editor = if self.vim.is_some() {
            super::vim::render(self, editor, window, cx)
        } else {
            editor.into_any_element()
        };
        let results = div()
            .id("results")
            .test_support()
            .track_focus(&self.results_focus)
            .size_full()
            // With the grid focused: copy what is selected, select everything;
            // in rows that take changes, the keys of a table's data.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                let secondary = keystroke.modifiers.secondary(cx) && !keystroke.modifiers.shift;
                match keystroke.key.as_str() {
                    "c" if secondary => {
                        if this.copy_selection(Some(CopyFormat::Tsv), cx) {
                            cx.stop_propagation();
                        }
                    }
                    "a" if secondary => {
                        this.select_all_rows(cx);
                        cx.stop_propagation();
                    }
                    _ => {
                        let Some(grid) = this.active_grid() else {
                            return;
                        };
                        let edits = this.result_edits();
                        if edits.is_err() || !grid.read(cx).focus_handle(cx).is_focused(window) {
                            return;
                        }
                        if edit::on_key(&grid, keystroke, edits, window, cx) {
                            if let Some(pane) = this.results.get_mut(this.active_result) {
                                pane.apply_error = None;
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }
                }
            }))
            .child(if stacked {
                self.render_stacked(cx)
            } else {
                self.render_results(cx).into_any_element()
            });
        // Beside the results, outside their keys: typing in the panel is
        // not typing in the grid.
        let results =
            value_panel::beside(results.into_any_element(), self.value_panel.as_ref(), cx);
        // The environment's colour frames the editor and the results.
        let chrome = self
            .target
            .as_ref()
            .and_then(|target| self.connections.read(cx).get(&target.connection))
            .and_then(|saved| crate::theme::environment_chrome(&saved.config, cx));
        let area = div().flex_1().min_h_0().map(|area| match panels {
            Panels::Editor => area.child(editor),
            Panels::Results => area.child(results),
            Panels::Both => area.child(
                v_resizable("query-split")
                    .child(resizable_panel().child(editor))
                    .child(resizable_panel().child(results)),
            ),
        });
        v_flex()
            .id("query-tab")
            .key_context("QueryTab")
            .test_support()
            .size_full()
            .on_action(cx.listener(|this, _: &actions::AiWriteSql, window, cx| {
                this.ai_request(crate::ai_view::Kind::WriteSql, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &actions::AiExplainQuery, window, cx| {
                    this.ai_request(crate::ai_view::Kind::ExplainQuery, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &actions::AiExplainPlan, window, cx| {
                this.ai_request(crate::ai_view::Kind::ExplainPlan, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &actions::AiExplainError, window, cx| {
                    this.ai_request(crate::ai_view::Kind::ExplainError, window, cx)
                }),
            )
            .child(self.render_toolbar(cx))
            .children(self.render_unbound_bar(cx))
            .child(match chrome {
                Some(chrome) => v_flex()
                    .id("env-frame")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .border_2()
                    .border_color(chrome.frame)
                    .child(area)
                    .into_any_element(),
                None => area.into_any_element(),
            })
    }
}

// K52 (D-121): the assistant asked at a place of the editor, and a failed statement fixed; the
// proposal is a diff drawn by `super::ai_inline`, never written before Accept.
impl QueryTab {
    /// Whether the assistant is on; off, says how to turn it on.
    fn assistant_on(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let on = AppState::get(cx).settings.ai.enabled;
        if !on {
            window.push_notification(
                Notification::warning(SharedString::from(crate::ai_view::OFF_NOTICE)),
                cx,
            );
        }
        on
    }

    /// "AI: Ask Here…": a field under the selection, else the statement at the cursor, else the
    /// cursor's empty line (nothing of the editor is sent then). A field already open takes the
    /// keyboard again.
    pub fn ask_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.assistant_on(window, cx) {
            return;
        }
        if let Some(inline) = self.inline.clone()
            && !inline.read(cx).stale()
        {
            inline.update(cx, |inline, cx| inline.focus(window, cx));
            return;
        }
        let range = self.ask_place(cx);
        self.open_inline(range, super::ai_inline::Subject::Ask, window, cx);
    }

    /// What "Ask Here" is about: the selection; on an empty line, the cursor alone; else the
    /// statement at the cursor.
    fn ask_place(&self, cx: &App) -> std::ops::Range<usize> {
        let editor = self.editor.read(cx);
        let text = editor.text().to_string();
        let (cursor, selection) = (editor.cursor().min(text.len()), editor.selected_range());
        if text
            .get(selection.clone())
            .is_some_and(|s| !s.trim().is_empty())
        {
            return selection;
        }
        let start = text[..cursor].rfind('\n').map_or(0, |at| at + 1);
        let end = text[cursor..]
            .find('\n')
            .map_or(text.len(), |at| cursor + at);
        if text[start..end].trim().is_empty() {
            return cursor..cursor;
        }
        (self.ai_statement(cx))
            .map(|(_, range)| range)
            .unwrap_or(cursor..cursor)
    }

    /// "Fix with AI" on the failed result `pane` (else the one marked in the editor, else the
    /// result on screen): the statement and the server's error are sent at once, the fix comes
    /// back as a diff on the statement. Nothing runs.
    pub fn fix_with_ai(&mut self, pane: Option<u64>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.assistant_on(window, cx) {
            return;
        }
        let say = |text: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            window.push_notification(Notification::warning(SharedString::from(text)), cx)
        };
        let id = pane
            .or(self.error_fix.map(|(id, _)| id))
            .or(self.results.get(self.active_result).map(|pane| pane.id));
        let Some(pane) = id.and_then(|id| self.results.iter().find(|pane| pane.id == id)) else {
            return say("No failed statement on screen to fix.", window, cx);
        };
        let Outcome::Failed(failure) = &pane.outcome else {
            return say("No failed statement on screen to fix.", window, cx);
        };
        let Some(source) = pane.source.clone() else {
            return say(
                "This error is not of a statement read from the editor: Fix with AI works on \
                 the editor's statements.",
                window,
                cx,
            );
        };
        let text = self.editor.read(cx).text().to_string();
        let mut found = text
            .match_indices(pane.statement.as_str())
            .map(|(at, _)| at);
        let range = if kerno_core::ai::inline::still_there(&text, &source, &pane.statement) {
            source
        } else {
            // Moved by an edit before it, written once in the text: there.
            match (found.next(), found.next()) {
                (Some(at), None) => at..at + pane.statement.len(),
                _ => {
                    return say(
                        "The failed statement is no longer in the editor as it ran: run it \
                         again, then Fix with AI.",
                        window,
                        cx,
                    );
                }
            }
        };
        let subject = super::ai_inline::Subject::Fix {
            error: failure.described().to_string(),
            position: failure.position,
        };
        self.open_inline(range, subject, window, cx);
    }

    fn open_inline(
        &mut self,
        range: std::ops::Range<usize>,
        subject: super::ai_inline::Subject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(old) = self.inline.take() {
            old.update(cx, |old, cx| old.discard(cx));
        }
        let (tab, editor, facts) = (
            cx.entity().downgrade(),
            self.editor.clone(),
            self.ai_facts(cx),
        );
        let inline = cx.new(|cx| {
            super::ai_inline::AiInline::new(tab, editor, facts, range, subject, window, cx)
        });
        let closed = cx.subscribe_in(
            &inline,
            window,
            |this: &mut Self, inline, _: &super::ai_inline::InlineEvent, window, cx| {
                if this.inline.as_ref() == Some(inline) {
                    this.inline = None;
                    this.focus_editor(window, cx);
                    cx.notify();
                }
            },
        );
        self._subscriptions.push(closed);
        self.inline = Some(inline);
        cx.notify();
    }

    /// "Fix with AI" beside the error's mark in the editor, while the assistant is on and no
    /// field is open.
    fn fix_chip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (id, at) = self.error_fix?;
        if self.inline.is_some() || !AppState::get(cx).settings.ai.enabled {
            return None;
        }
        let view = cx.entity().downgrade();
        Some(super::ai_inline::fix_chip(
            &self.editor,
            at,
            move |window, cx| {
                let _ = view.update(cx, |tab, cx| tab.fix_with_ai(Some(id), window, cx));
            },
        ))
    }

    /// The field asking the assistant at a place, while open; for tests.
    #[cfg(test)]
    pub fn ai_inline(&self) -> Option<&Entity<super::ai_inline::AiInline>> {
        self.inline.as_ref()
    }
}
