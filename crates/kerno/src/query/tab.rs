//! A query tab: a SQL editor bound to a connection, and what its
//! statements returned.

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
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, TestSupportExt as _, Window, div, px,
};
use kerno_core::browse::{
    Filter, Operator, Query, Source as QuerySource, and_where, count_from, described, distinct_sql,
    one_of_sql, select_from,
};
use kerno_core::config::Environment;
use kerno_core::schema::{RelationKind, TableRef};
use kerno_core::sql::{
    LexRules, Param, ParamKey, ParamValue, bind_params, change_case, find_params, format_sql,
    split_statements, statement_at, toggle_block_comment,
};
use kerno_core::sql::{StatementClass, StatementKind, TxCommand, classify};
use kerno_core::storage::{HistoryEntry, HistoryStatus};
use kerno_core::{
    Dialect, Error, ExecEvent, Notice, Plan, ResultStore, RowLimit, Session, TxState, exec,
};

use super::comment;
use super::completion::{self, SharedCatalog, SqlCompletion};
use super::distinct;
use super::grid::{CopyFormat, Grid, GridAction, Owner};
use super::params;
use super::tx_log::{self, TxLogEntry};
use super::{plan_view, viewer};
use crate::actions::{
    self, CountStatementRows, ExplainAnalyze, ExplainStatement, FindInEditor, FormatSql,
    MaximizeResultsPanel, NextStatement, PreviousStatement, ReplaceInEditor, RunAll, RunFromCursor,
    RunStatement, RunStatementInNewTab, SaveFile, SaveFileAs, SaveQuery, ShowHistory,
    ShowSavedQueries, SwitchResultsFocus, ToggleComment, ToggleResultsPanel,
};
use crate::bridge;
use crate::connections::manager::{Connections, ConnectionsEvent, Status};
use crate::data::tab::{AutoRefresh, auto_refresh_button, history_button};
use crate::engine_logo::engine_logo;
use crate::export_dialog::{self, ExportDialog, Source};
use crate::sql_confirm::{self, Confirmation, Statement as ConfirmStatement};
use crate::state::AppState;
use crate::tabs::{self, Choices, EditorFacts, ResultFacts, TabAction, TabSnapshot, Target};

/// A statement to run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Statement {
    sql: String,
    /// Byte offset of the statement in the editor's text, when it runs as
    /// written there.
    origin: Option<usize>,
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
    /// How the result of a SELECT is read again on the server.
    pub requery: Option<Requery>,
    /// Rows of the whole result, once counted.
    pub total: Option<u64>,
    /// The grid's selection and double click; dropped with the pane.
    _grid_events: Option<Subscription>,
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

pub struct QueryTab {
    title: SharedString,
    editor: Entity<EditorState>,
    connections: Entity<Connections>,
    /// The connection, database and schema statements run on.
    target: Option<Target>,
    /// This tab's own session, opened at the first run.
    session: Option<Arc<dyn Session>>,
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
    /// The statements of the open transaction, in manual commit.
    tx_log: Vec<TxLogEntry>,
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
        editor.update(cx, |editor, _| {
            editor
                .lsp_mut()
                .set_completion_highlighter(Rc::new(completion::highlight));
        });
        // An edit makes a file's tab differ from its file. (Moving the
        // cursor also notifies observers; only a change of text counts.)
        let edits = cx.subscribe(&editor, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && this.file.is_some() && !this.unsaved {
                this.unsaved = true;
                cx.notify();
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
        // The connection's commit mode: manual on production unless chosen.
        let manual_commit = target
            .as_ref()
            .and_then(|target| connections.read(cx).get(&target.connection))
            .is_some_and(|saved| saved.config.starts_in_manual_commit());
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
            session: None,
            databases: Vec::new(),
            schemas: Vec::new(),
            results: Vec::new(),
            active_result: 0,
            notices: Vec::new(),
            activity: Activity::Idle,
            run: None,
            cancel_requested: false,
            manual_commit,
            tx_log: Vec::new(),
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
            catalog: SharedCatalog::default(),
            catalog_load: None,
            source: None,
            editor_options,
            next_requery: None,
            requery: None,
            result_filter,
            result_filter_for: None,
            result_filter_shown: None,
            result_history: HashMap::new(),
            side: None,
            auto_refresh: None,
            auto_refresh_stopped: None,
            _subscriptions: vec![subscription, edits, settings, filtering],
        };
        tab.follow_connection(cx);
        tab
    }

    /// Soft wrap, line numbers and tab size, as the settings have them.
    fn editor_settings(cx: &App) -> (bool, bool, u8) {
        let settings = &AppState::get(cx).settings;
        (settings.soft_wrap, settings.line_numbers, settings.tab_size)
    }

    /// The editor takes a changed setting at once.
    fn follow_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            pending: 0,
        }
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

    /// The server's messages of the last run; for tests.
    #[cfg(test)]
    pub fn messages(&self) -> Vec<String> {
        self.notices
            .iter()
            .map(|notice| format!("{}: {}", notice.severity, notice.message))
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

    /// Runs on `target` from now on. What was running is stopped. On
    /// another connection without a transaction open, the tab takes that
    /// connection's commit mode.
    pub fn set_target(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        if self.target == target {
            return;
        }
        let connection = |target: &Option<Target>| target.as_ref().map(|t| t.connection.clone());
        if connection(&self.target) != connection(&target) && self.tx_state() == TxState::Idle {
            self.manual_commit = target
                .as_ref()
                .and_then(|target| self.connections.read(cx).get(&target.connection))
                .is_some_and(|saved| saved.config.starts_in_manual_commit());
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
        if self.connected(cx) || self.tx_state() == TxState::Idle {
            return;
        }
        let message = "The connection closed while this tab's transaction was open: the server \
                       rolled it back, and nothing of it was committed.";
        self.notices.push(Notice {
            severity: "Warning".into(),
            message: message.into(),
            code: None,
            detail: None,
        });
        // The messages, after the results.
        self.active_result = self.results.len();
        self.show_results();
        window.push_notification(
            Notification::warning(SharedString::from(format!("“{}”: {message}", self.title))),
            cx,
        );
        cx.notify();
    }

    /// Loads the lists of the selectors when the connection is open; drops
    /// the session when it closed.
    fn follow_connection(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        if !self.connected(cx) {
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
        let generation = self.generation;
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
                    if this.generation != generation {
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
                Rc::new(SqlCompletion { catalog, session }) as Rc<dyn CompletionProvider>
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
                    let _ = this.update(cx, |this, _| {
                        *this.catalog.borrow_mut() = Arc::new(catalog);
                    });
                }
                Err(error) => tracing::warn!(%error, "the completion catalog was not loaded"),
            }
        }));
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
            return Task::ready(Err(Error::Config(
                "choose a connection for this tab first".into(),
            )));
        };
        if !self.connected(cx) {
            let name = self
                .connections
                .read(cx)
                .get(&target.connection)
                .map(|saved| saved.config.name.clone())
                .unwrap_or_else(|| "this tab's connection".into());
            return Task::ready(Err(Error::Closed(format!(
                "“{name}” is not connected. Double-click it in the sidebar, then run again."
            ))));
        }
        let opening =
            self.connections
                .read(cx)
                .open_session(&target.connection, target.database.clone(), cx);
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let session = opening.await?;
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
                    session
                }
            });
            session.map_err(|_| Error::Closed("the tab was closed".into()))
        })
    }

    /// Closes the tab's session now: the tab is closing, moving to another
    /// target, or its connection closed. Results on screen stay.
    pub fn close_session(&mut self, cx: &mut Context<Self>) {
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
    fn lex_rules(&self, cx: &App) -> LexRules {
        self.target
            .as_ref()
            .and_then(|target| self.connections.read(cx).meta_session(&target.connection))
            .map_or(LexRules::POSTGRES, |meta| meta.dialect().lex_rules())
    }

    fn statements(&self, scope: Scope, cx: &App) -> Vec<Statement> {
        let rules = self.lex_rules(cx);
        let editor = self.editor.read(cx);
        let text = editor.text().to_string();
        let selection = editor.selected_range();
        let from = |source: &str, base: usize, span: &kerno_core::sql::StatementSpan| Statement {
            sql: span.text(source).to_string(),
            origin: Some(base + span.range.start),
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
            let mut formatted = format_sql(part, rules);
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
            cx.notify();
            return;
        }
        if !matches!(self.activity, Activity::Running { .. }) {
            // Nothing is being computed; a result waiting to be paged is
            // simply let go.
            self.stop(cx);
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
        let name = connections
            .get(&target.connection)
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default();
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
                format!("“{name}” could not connect, and nothing was run: {message}")
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
                detail: None,
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
        let mut statements = self.statements(scope, cx);
        if explain.is_some() {
            // One plan at a time: the first statement of a selection.
            statements.truncate(1);
        }
        self.explain_mode = explain;
        self.keep_results = keep;
        if statements.is_empty() {
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
            self.execute(statements, window, cx);
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
            self.execute(statements, window, cx);
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
                        // Positions the server reports are in the text it got,
                        // not in the editor's.
                        origin: if params.is_empty() {
                            statement.origin
                        } else {
                            None
                        },
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
                    this.execute(bound, window, cx);
                });
                // Back to the editor once the prompt is gone, so the
                // keyboard keeps working without a click.
                let editor = view.read(cx).focus_handle(cx);
                window.defer(cx, move |window, cx| editor.focus(window, cx));
            },
        );
    }

    /// Runs `statements` one after the other, stopping at the first that
    /// fails.
    fn execute(&mut self, statements: Vec<Statement>, window: &mut Window, cx: &mut Context<Self>) {
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
        if saved.config.read_only
            && let Some(blocked) = classes.iter().position(|class| class.writes)
        {
            self.refuse_read_only(&statements[blocked].sql, &name, cx);
            return;
        }
        // Statements that destroy something are shown and confirmed first;
        // on a production connection, so is anything that writes.
        let production = saved.config.environment == Environment::Production;
        let ask_destructive = AppState::get(cx).settings.confirm_destructive;
        let to_confirm: Vec<&Statement> = statements
            .iter()
            .zip(&classes)
            .filter(|(_, class)| {
                (class.destructive && ask_destructive) || (production && class.writes)
            })
            .map(|(statement, _)| statement)
            .collect();
        if to_confirm.is_empty() {
            self.start(statements, false, window, cx);
            return;
        }
        let destructive = classes.iter().any(|class| class.destructive);
        let confirmation = Confirmation {
            title: match to_confirm.len() {
                1 => "Run this statement?".to_string(),
                count => format!("Run these {count} statements?"),
            },
            explanation: if destructive {
                "It removes data or objects for good (a DROP, a TRUNCATE, an ALTER that drops, or a DELETE \
                 or UPDATE without a WHERE). This cannot be undone."
                    .to_string()
            } else {
                "It changes the database.".to_string()
            },
            statement: ConfirmStatement::Fixed(
                to_confirm
                    .iter()
                    .map(|statement| statement.sql.as_str())
                    .collect::<Vec<_>>()
                    .join(";\n\n"),
            ),
            action: "Run",
            destructive,
            connection: name,
            environment: saved.config.environment,
        };
        let view = cx.entity();
        sql_confirm::open(confirmation, window, cx, move |_, window, cx| {
            let statements = statements.clone();
            view.update(cx, |this, cx| this.start(statements, false, window, cx));
            let editor = view.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| editor.focus(window, cx));
        });
    }

    /// Shows why a statement was not run on a read-only connection.
    fn refuse_read_only(&mut self, statement: &str, connection: &str, cx: &mut Context<Self>) {
        self.refuse(
            statement.to_string(),
            Failure {
                message: format!(
                    "“{connection}” is a read-only connection: this statement would change the database and was not run."
                ),
                code: None,
                detail: None,
                hint: Some(
                    "Untick “Read-only” in the connection's settings to allow changes.".into(),
                ),
                position: None,
            },
            cx,
        );
    }

    /// Shows why a run did not happen, as its result.
    fn refuse(&mut self, statement: String, failure: Failure, cx: &mut Context<Self>) {
        self.stop(cx);
        self.requery = None;
        self.show_results();
        self.clear_results();
        self.notices.clear();
        self.push_pane(statement, Outcome::Failed(failure), None, None);
        self.active_result = self.results.len() - 1;
        cx.notify();
    }

    /// The results come back on screen when they were hidden.
    fn show_results(&mut self) {
        if self.panels == Panels::Editor {
            self.panels = Panels::Both;
        }
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
            requery: self.requery.clone(),
            total: None,
            _grid_events: None,
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
                origin: Some(span.range.start),
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
        let (name, environment) = (saved.config.name.clone(), saved.config.environment);
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
        self.generation += 1;
        self.requery = self.next_requery.take();
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
                        cx.notify();
                    });
                    return;
                }
            };
            // In manual-commit mode the first statement opens a transaction.
            if manual && session.tx_state() == TxState::Idle {
                let begin = if session.dialect().has_schemas() {
                    "BEGIN"
                } else {
                    "START TRANSACTION"
                };
                let Ok(beginning) =
                    cx.update(|_, cx| bridge::spawn(cx, exec::collect(session.clone(), begin)))
                else {
                    return;
                };
                if let Err(error) = beginning.await {
                    let _ = this.update(cx, |this, cx| {
                        this.push_pane(
                            begin.to_string(),
                            Outcome::Failed(Failure::from(&error)),
                            None,
                            None,
                        );
                        this.activity = Activity::Idle;
                        this.run = None;
                        cx.notify();
                    });
                    return;
                }
                // A new transaction: its log starts empty, whatever ended
                // the last one (a lost connection, a rollback on close).
                let _ = this.update(cx, |this, _| this.tx_log.clear());
            }
            let mut changed_schema = false;
            let mut any_failed = false;
            for (index, statement) in statements.into_iter().enumerate() {
                let started = Instant::now();
                let started_at_ms = kerno_core::storage::now_ms();
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
                                        cx.notify();
                                        grid.delegate().has_more()
                                    });
                                    if paused {
                                        // The rows asked for are here: the tab is not
                                        // waiting for the server any more.
                                        this.activity = Activity::Idle;
                                        if let Some(pane) = this.pane_mut(id) {
                                            pane.elapsed = Some(started.elapsed());
                                        }
                                        // In a script the next statement needs the
                                        // session: this one stops at its first page.
                                        if !last {
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
                                        pane.elapsed = Some(started.elapsed());
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
                                ended = Some(if cancelled {
                                    (HistoryStatus::Cancelled, None)
                                } else {
                                    (HistoryStatus::Error, Some(Failure::from(&error)))
                                });
                                this.push_pane(
                                    statement.sql.clone(),
                                    if cancelled {
                                        Outcome::Cancelled
                                    } else {
                                        Outcome::Failed(Failure::from(&error))
                                    },
                                    None,
                                    Some(started.elapsed()),
                                );
                                this.active_result = this.results.len() - 1;
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
                    let ended = Ended {
                        started_at_ms,
                        elapsed: started.elapsed(),
                        status,
                        failure,
                        // The lines of a plan are not rows of data.
                        row_count: row_count.filter(|_| explain.is_none()),
                    };
                    // The history holds what was sent: an EXPLAIN is one.
                    let recorded = Statement {
                        sql: sent.clone(),
                        origin: statement.origin.filter(|_| explain.is_none()),
                    };
                    this.statement_ended(&recorded, ended, cx)
                });
                if failed {
                    any_failed = true;
                    break;
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
        // In manual commit the statement joins the transaction's log; one
        // that ended the transaction (COMMIT typed, MySQL's DDL) empties it.
        if self.manual_commit && !auto {
            if self.tx_state() == TxState::Idle {
                self.tx_log.clear();
            } else {
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
        if let (Some(origin), Some(failure)) = (statement.origin, &failure)
            && let Some(position) = failure.position
        {
            self.mark_error(origin, &statement.sql, position, &failure.message, cx);
        }
    }

    /// The statements run in the open transaction; none when no
    /// transaction is open.
    pub fn transaction_log(&self) -> &[TxLogEntry] {
        if self.tx_state() == TxState::Idle {
            &[]
        } else {
            &self.tx_log
        }
    }

    /// Shows the statements of the open transaction (DBeaver's
    /// "Transaction log").
    pub fn show_transaction_log(&self, window: &mut Window, cx: &mut App) {
        let open = self.tx_state() != TxState::Idle;
        tx_log::open(self.transaction_log().to_vec(), open, window, cx);
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
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let (start, end) = (origin + at, (origin + at + word).min(text.len()));
            // The text changed since the run started: nothing sure to mark.
            if text.get(origin..origin + statement.len()) != Some(statement) {
                return;
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
        });
        self.error_marked = true;
    }

    fn clear_error_mark(&mut self, cx: &mut Context<Self>) {
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

    /// Whether a statement is running.
    pub fn running(&self) -> bool {
        matches!(self.activity, Activity::Running { .. })
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

    /// What the editor's menu offers to run on.
    fn choices(&self, cx: &App) -> Choices {
        let connections = self.connections.read(cx);
        let target = self.target.as_ref();
        Choices {
            connections: connections
                .saved()
                .iter()
                .map(|saved| {
                    let connected =
                        matches!(connections.status(&saved.id), Status::Connected { .. });
                    (saved.id.clone(), saved.config.name.clone(), connected)
                })
                .collect(),
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
            tabs::fit_window(menu, window)
        }
    }

    /// Statements run in a transaction the user ends (`true`), or each is
    /// committed as it runs.
    pub fn set_manual_commit(&mut self, manual: bool, cx: &mut Context<Self>) {
        self.manual_commit = manual;
        cx.notify();
    }

    /// Commits or rolls back the tab's open transaction.
    pub fn end_transaction(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ending = self.finish_transaction(commit, cx);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = ending.await {
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(SharedString::from(error)), cx)
                });
            }
        })
        .detach();
    }

    /// Commits or rolls back the tab's open transaction; done at once when
    /// none is open. The error says which statement failed.
    pub fn finish_transaction(
        &mut self,
        commit: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let Some(session) = self.session.clone() else {
            return Task::ready(Ok(()));
        };
        if session.tx_state() == TxState::Idle {
            return Task::ready(Ok(()));
        }
        // A result still being paged belongs to the transaction.
        self.stop(cx);
        let sql = if commit { "COMMIT" } else { "ROLLBACK" };
        let ending = bridge::spawn(cx, exec::collect(session, sql));
        cx.spawn(async move |this, cx| {
            let outcome = ending.await;
            let _ = this.update(cx, |this, cx| {
                if outcome.is_ok() {
                    this.tx_log.clear();
                }
                cx.notify()
            });
            outcome
                .map(drop)
                .map_err(|error| format!("{sql} failed: {error}"))
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
        // The output is one cell, or one line per row for text plans.
        let raw = {
            let grid = grid.read(cx).delegate();
            (0..grid.row_count())
                .filter_map(|row| grid.value_at(row, 1).and_then(|(_, value)| value))
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
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
        let mut grid = grid.with_owner(Owner {
            act: Rc::new(move |action, row, column, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    if action == GridAction::Export {
                        this.export_statement(sql.clone(), window, cx);
                    } else {
                        this.result_action(id, action, row, column, window, cx);
                    }
                });
            }),
            related: None,
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
        let table = cx.new(|cx| {
            TableState::new(grid, window, cx)
                .col_resizable(true)
                .col_movable(true)
                .sortable(true)
                .row_selectable(true)
                .col_selectable(true)
                .cell_selectable(true)
                // The grid numbers its rows itself.
                .row_header(false)
        });
        // A double click on a cell shows its whole value.
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
                    let grid = table.read(cx).delegate();
                    if let Some((column, value)) = grid.value_at(*row, *column) {
                        let (column, value) = (column.clone(), value.map(<[u8]>::to_vec));
                        viewer::open(&column, value.as_deref(), window, cx);
                    }
                }
            },
        );
        let id = self.push_pane(statement.to_string(), Outcome::Rows(table), None, None);
        if let Some(pane) = self.pane_mut(id) {
            pane._grid_events = Some(events);
            pane.requery = requery;
        }
        id
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
        self.execute(vec![Statement { sql, origin: None }], window, cx);
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
                    Err(error) => format!("The rows could not be counted: {error}"),
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
        self.next_requery = Some(Requery {
            filter,
            sort,
            order_dropped,
            ..requery
        });
        self.close_results(&[id], cx);
        self.explain_mode = None;
        self.keep_results = true;
        self.execute(vec![Statement { sql, origin: None }], window, cx);
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
            return Task::ready(Err(Error::Config(
                "choose a connection for this tab first".into(),
            )));
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
                    Notification::error(SharedString::from(format!(
                        "The rows could not be counted: {error}"
                    ))),
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
                        Notification::error(SharedString::from(format!(
                            "The values of {name} could not be read: {error}"
                        ))),
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

    /// Whether the results area has the keyboard; for tests.
    #[cfg(test)]
    pub fn results_focused(&self, window: &Window, cx: &App) -> bool {
        self.results_focus.contains_focused(window, cx)
    }

    // Rendering ---------------------------------------------------------------

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
            .unwrap_or_else(|| "database".into());
        let schema_label = self
            .target
            .as_ref()
            .and_then(|target| target.schema.clone())
            .unwrap_or_else(|| "default schema".into());

        // What the menus offer, captured when they open.
        let choices: Vec<(String, String, bool)> = connections
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
        let (databases, schemas) = (self.databases.clone(), self.schemas.clone());
        let running = matches!(self.activity, Activity::Running { .. });
        let waiting = self.run.is_some() || self.pending.is_some();
        let transaction = self
            .session
            .as_ref()
            .map_or(TxState::Idle, |session| session.tx_state());
        let view = cx.entity();

        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
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
                    .dropdown_menu({
                        let view = view.clone();
                        move |mut menu, _, _| {
                            for (id, name, connected) in &choices {
                                let (view, id) = (view.clone(), id.clone());
                                let label = if *connected {
                                    name.clone()
                                } else {
                                    format!("{name} (not connected)")
                                };
                                menu = menu.item(
                                    PopupMenuItem::new(SharedString::from(label))
                                        .disabled(!connected)
                                        .on_click(move |_, _, cx| {
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
                                        }),
                                );
                            }
                            menu
                        }
                    }),
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
                                    auto.update(cx, |this, cx| this.set_manual_commit(false, cx))
                                }),
                            )
                            .item(
                                PopupMenuItem::new(
                                    "Manual commit: statements run in a transaction you end",
                                )
                                .on_click(move |_, _, cx| {
                                    manual.update(cx, |this, cx| this.set_manual_commit(true, cx))
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
            Outcome::Failed(failure) => v_flex()
                .id("result-error")
                .test_support()
                .p_3()
                .gap_1()
                .text_sm()
                .child(div().text_color(theme.danger).child(SharedString::from(
                    match &failure.code {
                        Some(code) => format!("{} [{code}]", failure.message),
                        None => failure.message.clone(),
                    },
                )))
                .children(
                    failure
                        .detail
                        .clone()
                        .map(|detail| div().child(SharedString::from(format!("Detail: {detail}")))),
                )
                .children(
                    failure
                        .hint
                        .clone()
                        .map(|hint| div().child(SharedString::from(format!("Hint: {hint}")))),
                )
                .into_any_element(),
        }
    }

    /// The filter of a SELECT's result, read again by the server
    /// (DBeaver's `ResultSetFilterPanel` over an editor's result).
    fn render_result_filter(&self, requery: &Requery, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let refined = !requery.filter.trim().is_empty() || requery.sort.is_some();
        let statement = SharedString::from(requery.statement.clone());
        h_flex()
            .id("result-filter-bar")
            .test_support()
            .flex_none()
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
                div().flex_1().min_w_0().child(
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
                        .child(if requery.order_dropped {
                            "read again by the server, which ignored the statement's ORDER BY: \
                             order by a column header"
                        } else {
                            "read again by the server"
                        })
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

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted, warning, mono) = {
            let theme = cx.theme();
            (
                theme.border,
                theme.muted_foreground,
                theme.warning,
                theme.mono_font_family.clone(),
            )
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
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_results(&[id], cx)
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
                format!("Connecting to “{name}”… The statements run once it is open.")
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
        // What the footer says about the result on screen.
        let (summary, has_more, complete, order) = match &pane.outcome {
            Outcome::Rows(grid) => {
                let grid = grid.read(cx).delegate();
                let rows = grid.row_count();
                let more = grid.has_more();
                let text = match pane.total {
                    _ if grid.is_complete() => {
                        format!("{rows} row{}", if rows == 1 { "" } else { "s" })
                    }
                    Some(total) => format!(
                        "first {} of {} rows",
                        plan_view::number(rows as f64),
                        plan_view::number(total as f64)
                    ),
                    None if more => format!("first {rows} rows; more on the server"),
                    None => format!("{rows} rows so far"),
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
            .map(|requery| self.render_result_filter(requery, cx));
        v_flex()
            .size_full()
            .child(tabs)
            .children(filter_bar)
            .child(div().flex_1().min_h_0().child(self.render_pane(pane, cx)))
            .child(
                h_flex()
                    .id("result-footer")
                    .test_support()
                    .flex_none()
                    .h(px(26.))
                    .px_3()
                    .gap_3()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(summary))
                    .children(
                        order.map(|order| div().id("result-order").test_support().child(order)),
                    )
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
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.count_result(window, cx)
                                    })),
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
                    .children(elapsed.map(SharedString::from)),
            )
    }
}

/// Runs an entry of the editor's menu as its command does: through the
/// editor, which has the focus.
fn run_editor_action(
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
        TabAction::FetchAllRows => Box::new(actions::FetchAllRows),
        TabAction::ExportFromQuery => Box::new(actions::ExportFromQuery),
        TabAction::RevertFile => Box::new(actions::RevertFile),
        TabAction::OpenFile => Box::new(actions::OpenSqlFile),
        TabAction::Rename => Box::new(actions::RenameTab),
        TabAction::RunStatement => Box::new(RunStatement),
        TabAction::RunInNewTab => Box::new(RunStatementInNewTab),
        TabAction::RunFromCursor => Box::new(RunFromCursor),
        TabAction::RunScript => Box::new(RunAll),
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

impl Render for QueryTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        // The result on screen is scrolled into the tab bar's view.
        let shown = self.results.get(self.active_result).map(|pane| pane.id);
        if panels != Panels::Editor && shown.is_some() && self.revealed_result != shown {
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
            );
        let results = div()
            .id("results")
            .test_support()
            .track_focus(&self.results_focus)
            .size_full()
            // With the grid focused: copy what is selected, select everything.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let keystroke = &event.keystroke;
                if !keystroke.modifiers.secondary(cx) || keystroke.modifiers.shift {
                    return;
                }
                match keystroke.key.as_str() {
                    "c" => {
                        if this.copy_selection(Some(CopyFormat::Tsv), cx) {
                            cx.stop_propagation();
                        }
                    }
                    "a" => {
                        this.select_all_rows(cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .child(self.render_results(cx));
        v_flex()
            .id("query-tab")
            .key_context("QueryTab")
            .test_support()
            .size_full()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().map(|area| {
                match panels {
                    Panels::Editor => area.child(editor),
                    Panels::Results => area.child(results),
                    Panels::Both => area.child(
                        v_resizable("query-split")
                            .child(resizable_panel().child(editor))
                            .child(resizable_panel().child(results)),
                    ),
                }
            }))
    }
}
