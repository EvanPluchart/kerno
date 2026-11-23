//! A table's data: its rows, filtered and sorted by the server, read a load
//! at a time as the scroll reaches the end (K36).
//! Cells are edited in place; the changes wait, are reviewed as SQL, and
//! are applied in one transaction or discarded.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::base::input::IndentInline;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::table::{TableEvent, TableSelection, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _,
    WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div, px,
};
use kerno_core::browse::{
    Condition, Filter, Operator, Page, count_sql, distinct_sql, one_of_sql, page_sql, select_sql,
};
use kerno_core::exec::CollectedResult;
use kerno_core::pending::Pending;
use kerno_core::schema::{ForeignKeyInfo, RelationKind, TableRef, TableStructure};
use kerno_core::sql::{ApplyFailure, CellValue, RowChange, RowSql, apply_edits_until};
use kerno_core::storage::TableLayout;
use kerno_core::{Dialect, Engine, Error, ResultStore, Session, TxState, describe, exec};

use crate::actions::CloseTab;
use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::export_dialog::{self, ExportDialog, Source};
use crate::query::completion;
use crate::query::distinct;
use crate::query::edit;
use crate::query::grid::{CopyFormat, EditTarget, Grid, GridAction, LoadRows, Owner, SortOnServer};
use crate::query::plan_view::number;
use crate::query::tab::{TabStatus, duration_text};
use crate::query::value_panel::{self, ValuePanel};
use crate::settings::{PAGE_SIZES, page_size};
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::state::AppState;
use crate::tabs::{TabSnapshot, Target};

// Why rows take no change, as the menus say it.
const READ_ONLY_CONNECTION: &str = "read-only connection";
const VIEW: &str = "view";
const NO_KEY: &str = "no key";

/// What a data tab asks of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataTabEvent {
    /// Show the rows of another table that `filter` keeps: the other end of
    /// a foreign key.
    Open {
        table: TableRef,
        kind: RelationKind,
        filter: Filter,
    },
    /// Show the structure of this table.
    OpenStructure { table: TableRef, kind: RelationKind },
    /// Open a query tab holding `sql`, on this table's database and
    /// schema; nothing runs.
    OpenQuery { title: String, sql: String },
    /// The table is gone from the server (dropped or renamed elsewhere):
    /// the tree lists its schema's tables again.
    TableGone { table: TableRef },
}

/// One line of the filter bar.
struct FilterRow {
    column: String,
    operator: Operator,
    value: Entity<InputState>,
    _enter: Subscription,
}

/// Where the tab is with its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// The connection is not open.
    Waiting,
    Loading,
    Ready,
    /// What the server said, and whether the table itself is gone.
    Failed {
        message: String,
        gone: bool,
    },
    /// The user stopped the read (C3).
    Cancelled,
}

/// How many rows the filter keeps in the whole table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Total {
    Exact(u64),
    /// The planner's estimate of an unfiltered table too large to count
    /// after each read from the first row; "Count exactly" counts it.
    About(u64),
}

/// An unfiltered table whose estimate is above this is not counted after
/// each read: the estimate is shown instead (M6 Part 2).
const COUNT_ESTIMATE_ABOVE: u64 = 1_000_000;

/// The intervals auto-refresh offers, in seconds (DBeaver's
/// `AutoRefreshControl.AUTO_REFRESH_DEFAULTS`).
pub const AUTO_REFRESH_SECONDS: [u64; 6] = [1, 5, 10, 15, 30, 60];

/// What the toggle command starts auto-refresh with.
pub const AUTO_REFRESH_DEFAULT: Duration = Duration::from_secs(10);

/// Reads the rows again every `every` while on.
pub struct AutoRefresh {
    pub every: Duration,
    _timer: Task<()>,
}

impl AutoRefresh {
    /// Calls `tick` every `every`, on the entity, until it returns false or
    /// the entity is gone. Ticks are counted from the last one: a refresh
    /// still running when the next tick comes is not started again.
    pub fn start<T: 'static>(
        every: Duration,
        window: &mut Window,
        cx: &mut Context<T>,
        tick: fn(&mut T, &mut Window, &mut Context<T>) -> bool,
    ) -> Self {
        let timer = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                let going = this.update_in(cx, tick);
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        });
        Self {
            every,
            _timer: timer,
        }
    }

    /// "every 5 s".
    pub fn label(every: Duration) -> String {
        format!("every {} s", every.as_secs())
    }
}

/// The first cell of a one-value result, as a count: `None` for NULL, a
/// negative number (PostgreSQL's estimate of a table never analysed) or
/// a failure.
pub fn first_count(collected: kerno_core::Result<exec::Collected>) -> Option<u64> {
    let collected = collected.ok()?;
    let result = collected.results.first()?;
    result
        .batches
        .first()?
        .text(0, 0)?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Whether the server says the table itself does not exist: PostgreSQL's
/// SQLSTATE 42P01, MySQL's and MariaDB's error 1146 (`1146 (42S02)`).
fn table_gone(error: &Error) -> bool {
    match error {
        Error::Db(db) => db
            .code
            .as_deref()
            .is_some_and(|code| code == "42P01" || code.starts_with("1146 ")),
        _ => false,
    }
}

/// What a read stopped by Cancel says (C3).
const CANCELLED: &str = "Cancelled: the rows were not read. Reload reads them again.";

/// What the error state says: the server's message, then what to do, then
/// the server's detail and hint on lines of their own (C14).
fn failure_text(message: &str, gone: bool) -> String {
    let advice = if gone {
        "The table may have been dropped or renamed."
    } else {
        "Check the filter, then try again."
    };
    let (first, more) = message.split_once('\n').unwrap_or((message, ""));
    let mut text = format!("{} {advice}", sentence(first));
    if !more.is_empty() {
        text.push('\n');
        text.push_str(more);
    }
    text
}

/// What an apply that changed nothing says (C14): what happened in plain
/// words, then the server's message, detail and hint. Shared with the
/// edits of a query tab's result.
pub fn apply_failure_text(failure: &ApplyFailure) -> String {
    let what = match failure.statement {
        Some(index) => format!("Nothing was changed: statement {} failed.", index + 1),
        None => "Nothing was changed.".to_string(),
    };
    format!(
        "{what} The pending changes are kept.\n{}",
        describe(&failure.error)
    )
}

/// The server's message as a sentence, before Kerno's advice.
fn sentence(message: &str) -> String {
    let message = message.trim_end();
    if message.ends_with(['.', '!', '?']) {
        message.to_string()
    } else {
        format!("{message}.")
    }
}

/// What a read from the first row brought back.
struct Loaded {
    session: Arc<dyn Session>,
    structure: Arc<TableStructure>,
    result: CollectedResult,
    /// The rows it asked for: fewer came when the table has no more.
    asked: u64,
}

/// What a read from the first row keeps of the rows it replaces (K36):
/// the selection, the first row in view, the record view.
struct Place {
    selection: TableSelection,
    top: usize,
    record: bool,
}

/// The sizes of a load the footer offers (K36); another is typed.
pub const LOAD_SIZES: [u64; 5] = [100, 500, 1_000, 5_000, 10_000];

/// The most rows one read of Fetch all asks for (K36): the count grows
/// as they come, and few reads are needed.
const FETCH_ALL_AT_MOST: u64 = 100_000;

/// What a further load stopped by Cancel says (K36).
const LOAD_CANCELLED: &str = "Cancelled: no more rows were read.";

/// The statement that reads `limit` rows of `table` (every one, with
/// `None`) after the first `offset`, in the order the tab shows them. A
/// remembered order on a column gone since is dropped.
fn rows_sql(
    dialect: &dyn Dialect,
    structure: &TableStructure,
    table: &TableRef,
    filter: &Filter,
    sort: Option<&(String, bool)>,
    limit: Option<u64>,
    offset: u64,
) -> kerno_core::Result<String> {
    let identity = structure.row_identity().unwrap_or_default();
    let sort = sort
        .filter(|(name, _)| structure.columns.iter().any(|column| column.name == *name))
        .map(|(column, down)| (column.as_str(), *down));
    match limit {
        Some(limit) => page_sql(
            dialect,
            &structure.columns,
            Page {
                table,
                filter,
                sort,
                identity: &identity,
                limit,
                offset,
            },
        ),
        None => select_sql(dialect, &structure.columns, table, filter, sort, &identity),
    }
}

/// The rows of one table or view.
pub struct DataTab {
    focus_handle: FocusHandle,
    connections: Entity<Connections>,
    target: Target,
    table: TableRef,
    kind: RelationKind,
    session: Option<Arc<dyn Session>>,
    /// Restored and not shown yet: nothing is read, no session is open.
    hidden: bool,
    structure: Option<Arc<TableStructure>>,
    filter_rows: Vec<FilterRow>,
    /// The WHERE typed by hand, with the table's columns proposed (K40).
    raw_filter: Entity<EditorState>,
    /// The filter the rows on screen were read with.
    applied: Filter,
    /// The column sorted on and whether descending.
    sort: Option<(String, bool)>,
    /// Rows one load reads: Settings' size when the tab opens, changed in
    /// the footer for this tab (K36).
    load_size: u64,
    /// The next rows are being read (K36).
    appending: bool,
    /// Loads go on until every row is here (Fetch all).
    load_all: bool,
    /// Why the last load of more rows read none, until the next one.
    load_error: Option<String>,
    /// What the read from the first row on its way keeps of the rows on
    /// screen.
    place: Option<Place>,
    grid: Option<Entity<TableState<Grid>>>,
    /// Rows the filter keeps in the whole table, once counted or estimated.
    total: Option<Total>,
    counting: bool,
    /// An unfiltered table estimated above this is not counted after each
    /// page.
    estimate_above: u64,
    /// The filters applied here before, the latest first (RES-06).
    history: Vec<String>,
    /// The filter last put in the history.
    remembered: Option<Filter>,
    /// Reads the page again every so often (RES-25).
    auto_refresh: Option<AutoRefresh>,
    /// Why auto-refresh stopped by itself, until it is started again.
    auto_refresh_stopped: Option<&'static str>,
    elapsed: Option<Duration>,
    state: State,
    /// The load in flight; the next one waits for it.
    load: Option<Task<()>>,
    generation: u64,
    applying: bool,
    /// Cancel was asked for the read or the apply in flight: its failure
    /// is the cancel, not an error (C3).
    cancel_requested: bool,
    /// Cancel for the apply in flight, read between its statements: one that
    /// lands between two (the server, idle, would ignore it) rolls back (C3).
    apply_stop: Arc<AtomicBool>,
    /// The page read after an apply takes the keyboard, as the grid had it.
    refocus_grid: bool,
    /// Why the last apply changed nothing.
    apply_error: Option<String>,
    grid_subscriptions: Vec<Subscription>,
    /// How the columns were last laid out (J6, DBeaver #38133), once read:
    /// the first page waits for it, for its sort.
    layout: Option<TableLayout>,
    /// The grid's column changes already noted.
    layout_seen: u64,
    /// Counts the layouts given to be written: a delayed write that is
    /// not the last one gives up.
    layout_writes: Arc<AtomicU64>,
    /// The selected cell's value beside the grid, while shown (K17).
    value_panel: Option<Entity<ValuePanel>>,
    _subscriptions: Vec<Subscription>,
}

/// A layout is written this long after its last change.
const LAYOUT_SAVE_DELAY: Duration = Duration::from_millis(500);

impl EventEmitter<DataTabEvent> for DataTab {}

impl DataTab {
    pub fn new(
        target: Target,
        table: TableRef,
        kind: RelationKind,
        filter: Filter,
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let raw_filter = cx.new(|cx| {
            completion::condition_field(
                "SQL condition, for example: total > 100 AND status = 'paid'",
                filter.raw.clone(),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &raw_filter,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.apply_filter(window, cx);
                    }
                },
            ),
            // The rows load when the connection opens, and go stale when it
            // closes.
            cx.observe_in(&connections, window, |this, _, window, cx| {
                let connected = this.connected(cx);
                match (&this.state, connected) {
                    (State::Waiting, true) => this.reload(window, cx),
                    (State::Waiting, false) => {}
                    (_, false) => {
                        this.close_session(cx);
                        this.state = State::Waiting;
                        cx.notify();
                    }
                    _ => {}
                }
            }),
        ];
        let mut tab = Self {
            focus_handle: cx.focus_handle(),
            connections,
            target,
            table,
            kind,
            session: None,
            hidden: false,
            structure: None,
            filter_rows: Vec::new(),
            raw_filter,
            applied: filter.clone(),
            sort: None,
            load_size: u64::from(AppState::get(cx).settings.page_size.max(1)),
            appending: false,
            load_all: false,
            load_error: None,
            place: None,
            grid: None,
            total: None,
            counting: false,
            estimate_above: COUNT_ESTIMATE_ABOVE,
            history: Vec::new(),
            remembered: None,
            auto_refresh: None,
            auto_refresh_stopped: None,
            elapsed: None,
            state: State::Waiting,
            load: None,
            generation: 0,
            applying: false,
            cancel_requested: false,
            apply_stop: Arc::default(),
            refocus_grid: false,
            apply_error: None,
            grid_subscriptions: Vec::new(),
            layout: None,
            layout_seen: 0,
            value_panel: None,
            layout_writes: Arc::new(AtomicU64::new(0)),
            _subscriptions: subscriptions,
        };
        for condition in filter.conditions {
            tab.push_filter_row(condition, window, cx);
        }
        tab.load_history(cx);
        tab.load_layout(window, cx);
        tab
    }

    /// What the layout of this table's columns is stored under (J6).
    fn layout_key(&self, cx: &App) -> String {
        table_key(self.connections.read(cx), &self.target, &self.table)
    }

    /// Reads the layout last given to this table, then the first page, in
    /// its order.
    fn load_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (storage, connection, key) = (
            AppState::get(cx).storage.clone(),
            self.target.connection.clone(),
            self.layout_key(cx),
        );
        let reading = bridge::spawn_blocking(cx, move || storage.table_layout(&connection, &key));
        cx.spawn_in(window, async move |this, cx| {
            let layout = reading.await.ok().flatten().unwrap_or_default();
            let _ = this.update_in(cx, |this, window, cx| {
                if this.sort.is_none() {
                    this.sort = layout.sort.clone();
                }
                this.layout = Some(layout);
                this.reload(window, cx);
            });
        })
        .detach();
    }

    /// Notes how the columns are laid out now, and writes it a moment
    /// later when it changed.
    fn remember_layout(&mut self, cx: &mut Context<Self>) {
        let Some(table) = &self.grid else {
            return;
        };
        let state = table.read(cx);
        let grid = state.delegate();
        let columns = grid.store().columns();
        let shown: Vec<String> = grid
            .layout()
            .iter()
            .map(|&column| columns[column].name.clone())
            .collect();
        let pinned = shown
            .iter()
            .enumerate()
            .filter(|(index, _)| grid.is_pinned(index + 1))
            .map(|(_, name)| name.clone())
            .collect();
        let hidden: Vec<String> = columns
            .iter()
            .map(|column| column.name.clone())
            .filter(|name| !shown.contains(name))
            .collect();
        let mut widths = self
            .layout
            .as_ref()
            .map(|layout| layout.widths.clone())
            .unwrap_or_default();
        widths.extend(
            Grid::widths(state)
                .into_iter()
                .map(|(name, width)| (name, f32::from(width))),
        );
        let layout = TableLayout {
            order: shown.iter().chain(&hidden).cloned().collect(),
            hidden,
            pinned,
            widths,
            sort: self.sort.clone(),
        };
        if self.layout.as_ref() == Some(&layout) {
            return;
        }
        self.layout = Some(layout.clone());
        self.write_layout(layout, cx);
    }

    /// Writes a layout once no other came for a moment, off the UI
    /// thread; closing the tab does not stop it.
    fn write_layout(&mut self, layout: TableLayout, cx: &mut Context<Self>) {
        let version = self.layout_writes.fetch_add(1, Ordering::SeqCst) + 1;
        let writes = self.layout_writes.clone();
        let (storage, connection, key) = (
            AppState::get(cx).storage.clone(),
            self.target.connection.clone(),
            self.layout_key(cx),
        );
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(LAYOUT_SAVE_DELAY).await;
            if writes.load(Ordering::SeqCst) != version {
                return;
            }
            let writing = cx.update(|cx| {
                bridge::spawn_blocking(cx, move || {
                    storage.save_table_layout(&connection, &key, &layout)
                })
            });
            if let Err(error) = writing.await {
                tracing::warn!(%error, "the layout of a table could not be stored");
            }
        })
        .detach();
    }

    /// Forgets the layout of the columns: the table's own order, every
    /// column shown, widths from the rows, no order (J6).
    pub fn reset_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.free_to_move(window, cx) {
            return;
        }
        self.layout = Some(TableLayout::default());
        self.sort = None;
        self.write_layout(TableLayout::default(), cx);
        self.restart(window, cx);
    }

    /// What the filter history of this table is stored under.
    fn history_key(&self) -> String {
        format!(
            "data:{}/{}/{}.{}",
            self.target.connection,
            self.target.database.as_deref().unwrap_or_default(),
            self.table.schema,
            self.table.name
        )
    }

    /// Reads the filters applied to this table before.
    fn load_history(&mut self, cx: &mut Context<Self>) {
        let (storage, key) = (AppState::get(cx).storage.clone(), self.history_key());
        let reading = bridge::spawn_blocking(cx, move || storage.filters(&key));
        cx.spawn(async move |this, cx| {
            let Ok(history) = reading.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.history = history;
                cx.notify();
            });
        })
        .detach();
    }

    /// Puts the filter of the rows on screen first in the history, written
    /// as SQL with its values as literals.
    fn remember_filter(&mut self, session: &Arc<dyn Session>, cx: &mut Context<Self>) {
        let (Some(structure), false) = (&self.structure, self.applied.is_empty()) else {
            return;
        };
        if self.remembered.as_ref() == Some(&self.applied) {
            return;
        }
        // The conditions as SQL, and what was typed as it was typed.
        let conditions = Filter {
            conditions: self.applied.conditions.clone(),
            raw: String::new(),
        };
        let Ok(conditions) = conditions.where_sql(session.dialect(), &structure.columns) else {
            return;
        };
        let raw = self.applied.raw.trim();
        let text = match conditions {
            Some(conditions) if raw.is_empty() => conditions,
            Some(conditions) => kerno_core::browse::and_where(raw, &conditions),
            None => raw.to_string(),
        };
        self.remembered = Some(self.applied.clone());
        let (storage, key, rules) = (
            AppState::get(cx).storage.clone(),
            self.history_key(),
            session.dialect().lex_rules(),
        );
        let kept = text.clone();
        let saving = bridge::spawn_blocking(cx, move || storage.add_filter(&key, &kept, rules));
        cx.spawn(async move |this, cx| {
            if !matches!(saving.await, Ok(true)) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.history.retain(|known| *known != text);
                this.history.insert(0, text);
                this.history.truncate(kerno_core::storage::FILTERS_KEPT);
                cx.notify();
            });
        })
        .detach();
    }

    /// The filters offered by the history; for tests.
    #[cfg(test)]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn title(&self) -> SharedString {
        SharedString::from(self.table.name.clone())
    }

    pub fn table(&self) -> &TableRef {
        &self.table
    }

    pub fn kind(&self) -> RelationKind {
        self.kind
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The filter the rows were read with; for tests.
    #[cfg(test)]
    pub fn applied_filter(&self) -> &Filter {
        &self.applied
    }

    pub fn snapshot(&self) -> TabSnapshot {
        TabSnapshot::Data {
            target: self.target.clone(),
            schema: self.table.schema.clone(),
            table: self.table.name.clone(),
            view: !self.kind.is_table(),
        }
    }

    fn connected(&self, cx: &App) -> bool {
        matches!(
            self.connections.read(cx).status(&self.target.connection),
            Status::Connected { .. }
        )
    }

    fn connection_name(&self, cx: &App) -> String {
        self.connections
            .read(cx)
            .get(&self.target.connection)
            .map_or_else(
                || "this connection".into(),
                |saved| saved.config.name.clone(),
            )
    }

    /// The changes waiting to be applied.
    fn pending<'a>(&self, cx: &'a App) -> Option<&'a Pending> {
        self.grid.as_ref()?.read(cx).delegate().pending()
    }

    /// How many rows have changes waiting.
    pub fn pending_count(&self, cx: &App) -> usize {
        self.pending(cx).map_or(0, Pending::count)
    }

    /// What the status bar says about this tab.
    pub fn status(&self, cx: &App) -> TabStatus {
        let connections = self.connections.read(cx);
        let saved = connections.get(&self.target.connection);
        TabStatus {
            connection: saved.map(|saved| saved.config.name.clone()),
            read_only: saved.is_some_and(|saved| saved.config.read_only),
            database: self.target.database.clone().or_else(|| {
                connections
                    .meta_session(&self.target.connection)
                    .and_then(|meta| meta.database())
            }),
            transaction: TxState::Idle,
            running: self.state == State::Loading || self.appending || self.applying,
            // A failed read says nothing about rows: the error says it all.
            rows: self
                .grid
                .as_ref()
                .filter(|_| !self.read_failed())
                .map(|grid| grid.read(cx).delegate().total_rows()),
            elapsed: self.elapsed.filter(|_| !self.read_failed()),
            notices: 0,
            pending: self.pending_count(cx),
        }
    }

    /// The last read failed: its error is shown instead of rows.
    fn read_failed(&self) -> bool {
        matches!(self.state, State::Failed { .. } | State::Cancelled)
    }

    /// Why the rows cannot be edited, in a few words, when they cannot.
    fn read_only(&self, cx: &App) -> Option<&'static str> {
        let read_only = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .is_some_and(|saved| saved.config.read_only);
        if read_only {
            Some(READ_ONLY_CONNECTION)
        } else if !self.kind.is_table() {
            Some(VIEW)
        } else {
            let structure = self.structure.as_ref()?;
            structure.row_identity().is_none().then_some(NO_KEY)
        }
    }

    /// Why the rows cannot be edited, when they cannot.
    pub fn read_only_reason(&self, cx: &App) -> Option<String> {
        Some(match self.read_only(cx)? {
            READ_ONLY_CONNECTION => {
                format!("“{}” is a read-only connection.", self.connection_name(cx))
            }
            VIEW => "The rows of a view are read-only here.".into(),
            _ => "Read-only: this table has no primary key and no unique index on NOT NULL \
                  columns, so a row cannot be told from another."
                .into(),
        })
    }

    // Loading -----------------------------------------------------------------

    /// This tab's session, opened if needed.
    fn session(&self, cx: &App) -> Task<kerno_core::Result<Arc<dyn Session>>> {
        match &self.session {
            Some(session) => Task::ready(Ok(session.clone())),
            None => self.connections.read(cx).open_session(
                &self.target.connection,
                self.target.database.clone(),
                cx,
            ),
        }
    }

    /// Closes the tab's session now: the tab is closing, or its connection
    /// closed. What is on screen stays.
    pub fn close_session(&mut self, cx: &mut Context<Self>) {
        // A statement still waiting (behind a lock, say) is stopped first,
        // so nothing of a closed tab keeps running on the server (C3).
        let busy = self.state == State::Loading || self.appending || self.counting || self.applying;
        if let Some(session) = self.session.take() {
            bridge::spawn(cx, async move {
                if busy && let Err(error) = session.cancel().await {
                    tracing::debug!(%error, "the statement could not be cancelled");
                }
                session.close().await
            })
            .detach();
        }
    }

    /// Reads nothing until shown (a restored tab): see `reveal`.
    pub fn defer_until_shown(&mut self, cx: &mut Context<Self>) {
        self.hidden = true;
        self.generation += 1;
        (self.appending, self.load_all) = (false, false);
        self.load = None;
        self.close_session(cx);
        self.state = State::Waiting;
        cx.notify();
    }

    /// Another tab is on screen: auto-refresh stops.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.stop_auto_refresh(Some("the tab was hidden"), cx);
    }

    /// The tab is on screen: a deferred tab reads its rows now.
    pub fn reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.hidden) {
            self.reload(window, cx);
        }
    }

    /// Reads the rows again from the first, as many as are loaded, with
    /// the filter and sort as they are: the selection, the scroll and the
    /// record view stay where they were (K36, DBeaver's `refreshData`).
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let place = self
            .grid
            .as_ref()
            .filter(|_| self.state == State::Ready)
            .map(|table| {
                let table = table.read(cx);
                Place {
                    selection: table.selection(),
                    top: table.visible_range().rows().start,
                    record: table.delegate().record,
                }
            });
        let rows = self.fetched(cx).max(self.load_size);
        self.read(rows, place, window, cx);
    }

    /// Reads one load of rows from the first: another order or filter.
    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.read(self.load_size, None, window, cx);
    }

    /// Reads `rows` rows from the first in place of those on screen, then
    /// counts them all; `place` is what of the rows on screen is kept.
    fn read(
        &mut self,
        rows: u64,
        place: Option<Place>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.generation += 1;
        let generation = self.generation;
        // A further load on its way is of rows read again now.
        (self.appending, self.load_all, self.load_error) = (false, false, None);
        self.place = place;
        if self.hidden || !self.connected(cx) {
            self.close_session(cx);
            self.state = State::Waiting;
            cx.notify();
            return;
        }
        self.state = State::Loading;
        // The layout read first gives the order; it reads again when done.
        if self.layout.is_none() {
            cx.notify();
            return;
        }
        self.apply_error = None;
        self.cancel_requested = false;
        let previous = self.load.take();
        let opening = self.session(cx);
        let table = self.table.clone();
        let estimate_above = self.estimate_above;
        let known = self.structure.clone();
        let filter = self.applied.clone();
        let sort = self.sort.clone();
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
            // One statement at a time on the session, in the order asked.
            if let Some(previous) = previous {
                previous.await;
            }
            let started = Instant::now();
            let session = match opening.await {
                Ok(session) => session,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| this.failed(generation, &error, cx));
                    return;
                }
            };
            // The tab holds its session before the first statement, so that
            // Cancel reaches the first page too (C3); a Cancel that came
            // while it was opening stops the read here.
            let go_on = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return false;
                }
                this.session = Some(session.clone());
                if this.cancel_requested {
                    this.failed(generation, &Error::Cancelled, cx);
                    return false;
                }
                true
            });
            if !matches!(go_on, Ok(true)) {
                return;
            }
            let reading = {
                let (session, filter) = (session.clone(), filter.clone());
                cx.update(|_, cx| {
                    bridge::spawn(cx, async move {
                        let structure = match known {
                            Some(structure) => structure,
                            None => Arc::new(session.table_structure(&table).await?),
                        };
                        // The planner's estimate of an unfiltered table: above
                        // the threshold it stands for the count, read with
                        // the rows instead of a count after it.
                        let about = if filter.is_empty() {
                            let estimate = session.dialect().estimated_rows_sql(&table);
                            first_count(exec::collect(session.clone(), estimate).await)
                                .filter(|about| *about > estimate_above)
                        } else {
                            None
                        };
                        let sql = rows_sql(
                            session.dialect(),
                            &structure,
                            &table,
                            &filter,
                            sort.as_ref(),
                            Some(rows),
                            0,
                        )?;
                        let result = exec::collect(session.clone(), sql)
                            .await?
                            .results
                            .pop()
                            .unwrap_or_default();
                        let counting =
                            count_sql(session.dialect(), &structure.columns, &table, &filter)?;
                        Ok::<_, Error>((structure, result, counting, about))
                    })
                })
            };
            let Ok(reading) = reading else {
                return;
            };
            let (structure, result, counting, about) = match reading.await {
                Ok(read) => read,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| this.failed(generation, &error, cx));
                    return;
                }
            };
            let elapsed = started.elapsed();
            let loaded = Loaded {
                session: session.clone(),
                structure,
                result,
                asked: rows,
            };
            let shown = this.update_in(cx, |this, window, cx| {
                this.generation == generation && {
                    this.show(loaded, elapsed, window, cx);
                    if let Some(about) = about {
                        this.total = Some(Total::About(about));
                        this.counting = false;
                    }
                    true
                }
            });
            if !matches!(shown, Ok(true)) || about.is_some() {
                return;
            }
            // The rows show at once; how many there are in all follows.
            let Ok(counted) = cx.update(|_, cx| {
                bridge::spawn(cx, async move {
                    first_count(exec::collect(session, counting).await).map(Total::Exact)
                })
            }) else {
                return;
            };
            let total = counted.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.total = total;
                    this.counting = false;
                    this.settle_end(cx);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn failed(&mut self, generation: u64, error: &Error, cx: &mut Context<Self>) {
        if self.generation != generation {
            return;
        }
        self.refocus_grid = false;
        if error.is_disconnect() {
            self.close_session(cx);
        }
        self.counting = false;
        self.stop_auto_refresh(Some("the rows could not be read"), cx);
        // The server words a cancel its own way (an error on PostgreSQL);
        // the tab knows it asked.
        if std::mem::take(&mut self.cancel_requested) || matches!(error, Error::Cancelled) {
            self.state = State::Cancelled;
            cx.notify();
            return;
        }
        let gone = table_gone(error);
        self.state = State::Failed {
            message: describe(error).to_string(),
            gone,
        };
        if gone {
            cx.emit(DataTabEvent::TableGone {
                table: self.table.clone(),
            });
        }
        cx.notify();
    }

    /// Puts rows read from the first on screen.
    fn show(
        &mut self,
        loaded: Loaded,
        elapsed: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Loaded {
            session,
            structure,
            result,
            asked,
        } = loaded;
        let complete = (result.row_count() as u64) < asked;
        self.session = Some(session.clone());
        self.structure = Some(structure.clone());
        self.elapsed = Some(elapsed);
        self.total = None;
        self.counting = true;
        self.state = State::Ready;
        self.remember_filter(&session, cx);
        // The WHERE field proposes this table's columns (K40).
        let provider = completion::condition_completion(
            session.clone(),
            &self.table,
            structure
                .columns
                .iter()
                .map(|column| kerno_core::sql::CatalogColumn {
                    name: column.name.clone(),
                    data_type: column.data_type.clone(),
                })
                .collect(),
        );
        self.raw_filter.update(cx, |field, _| {
            field.lsp_mut().completion_provider = Some(provider);
        });

        let mut store = ResultStore::new(result.columns.clone());
        for batch in result.batches {
            store.push(batch);
        }
        let sort = self.sort.as_ref().and_then(|(name, descending)| {
            let column = store
                .columns()
                .iter()
                .position(|column| &column.name == name)?;
            Some((column, *descending))
        });
        if sort.is_none() {
            self.sort = None;
        }
        // A computed column takes no value, nor does an identity the server
        // always generates (`ColumnEdit::of`).
        let target = match self.read_only(cx) {
            Some(reason) => Err(reason),
            None => Ok(EditTarget {
                table: self.table.clone(),
                structure: structure.clone(),
                identity: structure.row_identity().unwrap_or_default(),
                names: store
                    .columns()
                    .iter()
                    .map(|column| Some(column.name.clone()))
                    .collect(),
            }),
        };
        let view = cx.weak_entity();
        let sort_on_server: Rc<SortOnServer> = Rc::new({
            let view = view.clone();
            move |sort, window, cx| {
                let _ = view.update(cx, |this, cx| this.sort_by(sort, window, cx));
            }
        });
        let load_rows: Rc<LoadRows> = Rc::new({
            let view = view.clone();
            move |window, cx| {
                let _ = view.update(cx, |this, cx| this.fetch_more(false, window, cx));
            }
        });
        let keyless = structure.row_identity().is_none();
        let described = structure.clone();
        let owner = Owner {
            act: Rc::new(move |action, row, column, window, cx| {
                let _ = view.update(cx, |this, cx| this.perform(action, row, column, window, cx));
            }),
            related: Some(Rc::new(move |grid, row| {
                related(&structure, grid, row)
                    .into_iter()
                    .map(|related| related.label)
                    .collect()
            })),
            export: None,
            filtered: !self.applied.is_empty(),
            keyless,
            structure: Some(described),
        };
        let mut grid = Grid::table_rows(
            store,
            session,
            self.table.clone(),
            sort,
            target,
            (sort_on_server, load_rows),
            complete,
        )
        .with_owner(owner);
        // The columns as they were last left (J6).
        if let Some(layout) = &self.layout {
            let (columns, pinned) = {
                let names: Vec<&str> = grid
                    .store()
                    .columns()
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect();
                layout.arrange(&names)
            };
            grid.arrange(columns, pinned);
            grid = grid.with_widths(
                layout
                    .widths
                    .iter()
                    .map(|(name, width)| (name.clone(), px(*width)))
                    .collect(),
            );
        }
        // The find bar stays open, with its text, over the rows read again.
        let find = self
            .grid
            .as_ref()
            .and_then(|old| crate::query::find::carried(old, cx));
        // The columns stay scrolled as they were: a sort, a filter, a
        // refresh or an Apply goes back to the first row only (K40).
        let scrolled = self
            .grid
            .as_ref()
            .map(|old| old.read(cx).horizontal_scroll_handle.offset().x);
        let table = cx.new(|cx| {
            let table = TableState::new(grid, window, cx)
                .col_resizable(true)
                .col_movable(true)
                .sortable(true)
                .row_selectable(true)
                .col_selectable(true)
                .cell_selectable(true)
                .row_header(false);
            if let Some(x) = scrolled {
                table
                    .horizontal_scroll_handle
                    .set_offset(gpui_kit::point(x, px(0.)));
            }
            table
        });
        self.grid_subscriptions = vec![
            cx.subscribe_in(
                &table,
                window,
                |this, table, event: &TableEvent, window, cx| {
                    // A column resized is remembered (J6).
                    if matches!(event, TableEvent::ColumnWidthsChanged(_)) {
                        this.remember_layout(cx);
                    }
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
                    // A double click types in the cell, or shows the whole
                    // value of one that cannot be typed in.
                    if let TableEvent::DoubleClickedCell(row, column) = event {
                        edit::double_click(table, *row, *column, window, cx);
                    }
                },
            ),
            // Pending changes show in the toolbar, and stop auto-refresh: a
            // refresh would drop them.
            cx.observe(&table, |this, table, cx| {
                if this.auto_refresh.is_some() && this.pending_count(cx) > 0 {
                    this.stop_auto_refresh(Some("changes are pending"), cx);
                }
                // Columns hidden, pinned or moved are remembered (J6).
                let version = table.read(cx).delegate().layout_version();
                if version != this.layout_seen {
                    this.layout_seen = version;
                    this.remember_layout(cx);
                }
                cx.notify()
            }),
        ];
        self.layout_seen = 0;
        if let Some(find) = find {
            crate::query::find::restore(&table, find, window, cx);
        }
        // Rows read again keep the user's place (K36): the selection, then
        // the first row that was in view, by position.
        if let Some(place) = self.place.take() {
            table.update(cx, |table, cx| {
                let Some(last) = table.delegate().total_rows().checked_sub(1) else {
                    return;
                };
                table.delegate_mut().record = place.record;
                match place.selection {
                    TableSelection::Cell(row, column) => {
                        table.set_selected_cell(row.min(last), column, cx)
                    }
                    TableSelection::Row(row) => table.set_selected_row(row.min(last), cx),
                    TableSelection::Column(column) => table.set_selected_col(column, cx),
                    TableSelection::None => {}
                }
                table.scroll_to_row(place.top.min(last), cx);
            });
        }
        if std::mem::take(&mut self.refocus_grid) {
            // Only while the keyboard is still on this tab, not after the
            // user moved on during the reload.
            let old = self.grid.as_ref().map(|grid| grid.focus_handle(cx));
            let focused = window.focused(cx);
            if focused.is_none() || focused == old || self.focus_handle.contains_focused(window, cx)
            {
                table.focus_handle(cx).focus(window, cx);
            }
        }
        self.grid = Some(table);
        cx.notify();
    }

    /// Whether the page can change. With changes waiting it cannot: they
    /// are about the rows on screen.
    fn free_to_move(&self, window: &mut Window, cx: &mut App) -> bool {
        if self.pending_count(cx) == 0 {
            return true;
        }
        window.push_notification(
            Notification::warning("Apply or discard the pending changes first."),
            cx,
        );
        false
    }

    fn sort_by(
        &mut self,
        sort: Option<(usize, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.free_to_move(window, cx) {
            return;
        }
        let Some(grid) = &self.grid else {
            return;
        };
        let columns = grid.read(cx).delegate().store().columns().clone();
        self.sort = sort
            .and_then(|(column, descending)| Some((columns.get(column)?.name.clone(), descending)));
        self.remember_layout(cx);
        self.restart(window, cx);
    }

    /// The WHERE field; for tests.
    #[cfg(test)]
    pub fn raw_filter(&self) -> &Entity<EditorState> {
        &self.raw_filter
    }

    /// What the layout of the table is stored under; for tests.
    #[cfg(test)]
    pub fn stored_layout_key(&self, cx: &App) -> String {
        self.layout_key(cx)
    }

    /// The column sorted on and whether descending; for tests.
    #[cfg(test)]
    pub fn sort(&self) -> Option<(&str, bool)> {
        self.sort
            .as_ref()
            .map(|(column, descending)| (column.as_str(), *descending))
    }

    /// Rows read from the server and held (rows added here aside).
    fn fetched(&self, cx: &App) -> u64 {
        self.grid.as_ref().map_or(0, |grid| {
            grid.read(cx).delegate().store().row_count() as u64
        })
    }

    /// Whether the server has, or may have, rows not read yet.
    fn has_more(&self, cx: &App) -> bool {
        self.grid
            .as_ref()
            .is_some_and(|grid| !grid.read(cx).delegate().is_complete())
    }

    /// A counted table whose rows are all here has none left to read.
    fn settle_end(&mut self, cx: &mut Context<Self>) {
        let fetched = self.fetched(cx);
        if let (Some(Total::Exact(total)), Some(table)) = (self.total, &self.grid)
            && fetched >= total
        {
            table.update(cx, |table, _| table.delegate_mut().finish(true));
        }
    }

    /// Changes how many rows the next loads of this tab read (K36): what is
    /// here stays.
    pub fn set_load_size(&mut self, size: u64, cx: &mut Context<Self>) {
        self.load_size = size.max(1);
        cx.notify();
    }

    /// How many rows one load reads; for tests.
    #[cfg(test)]
    pub fn load_size(&self) -> u64 {
        self.load_size
    }

    /// Reads the next rows and adds them under those on screen (K36),
    /// after what the session is doing; with `all`, every row left, a load
    /// after another, the count growing. The rows on screen, the selection,
    /// the scroll, the record view and the pending changes stay. A table
    /// without a key, or a view, has no order the server keeps from one
    /// statement to the next: its rows are read again from the first, one
    /// load more (all of them in one read for `all`), in place of those on
    /// screen (D-109).
    pub fn fetch_more(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(session), Some(structure), Some(table)) = (
            self.session.clone(),
            self.structure.clone(),
            self.grid.clone(),
        ) else {
            return;
        };
        if self.state != State::Ready || self.appending || !self.has_more(cx) {
            return;
        }
        table.update(cx, |table, _| table.delegate_mut().asked());
        let loaded = self.fetched(cx);
        let keyed = structure.row_identity().is_some();
        let size = if all {
            loaded.clamp(self.load_size, FETCH_ALL_AT_MOST.max(self.load_size))
        } else {
            self.load_size
        };
        // The rows the read asks for, and after how many.
        let (limit, offset) = match (keyed, all) {
            (true, _) => (Some(size), loaded),
            (false, true) => (None, 0),
            (false, false) => (Some(loaded + size), 0),
        };
        self.appending = true;
        self.load_all = all;
        self.load_error = None;
        self.cancel_requested = false;
        let generation = self.generation;
        let previous = self.load.take();
        let (name, filter, sort) = (self.table.clone(), self.applied.clone(), self.sort.clone());
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
            // One statement at a time on the session, in the order asked.
            if let Some(previous) = previous {
                previous.await;
            }
            let Ok(reading) = cx.update(|_, cx| {
                bridge::spawn(cx, async move {
                    let sql = rows_sql(
                        session.dialect(),
                        &structure,
                        &name,
                        &filter,
                        sort.as_ref(),
                        limit,
                        offset,
                    )?;
                    exec::collect(session, sql).await
                })
            }) else {
                return;
            };
            let read = reading.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.landed(generation, read, limit, !keyed, window, cx)
            });
        }));
        cx.notify();
    }

    /// Puts the rows of a further load on screen, or says why none came.
    fn landed(
        &mut self,
        generation: u64,
        read: kerno_core::Result<exec::Collected>,
        asked: Option<u64>,
        from_start: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.generation != generation {
            return;
        }
        self.appending = false;
        let cancelled = std::mem::take(&mut self.cancel_requested);
        let Some(table) = self.grid.clone() else {
            return;
        };
        let result = match read {
            Ok(mut collected) => collected.results.pop().unwrap_or_default(),
            Err(error) => {
                self.load_all = false;
                if error.is_disconnect() {
                    self.close_session(cx);
                }
                self.load_error = Some(if cancelled || matches!(error, Error::Cancelled) {
                    LOAD_CANCELLED.into()
                } else {
                    format!("The next rows could not be read. {}", describe(&error))
                });
                if table_gone(&error) {
                    cx.emit(DataTabEvent::TableGone {
                        table: self.table.clone(),
                    });
                }
                cx.notify();
                return;
            }
        };
        // Rows of another shape (the table was altered since) cannot go
        // under these.
        let same = {
            let held = table.read(cx).delegate().store().columns().clone();
            held.len() == result.columns.len()
                && held
                    .iter()
                    .zip(result.columns.iter())
                    .all(|(held, read)| held.name == read.name)
        };
        if !same {
            self.load_all = false;
            self.load_error = Some(
                "The table's columns changed since its rows were read: Refresh reads them again."
                    .into(),
            );
            cx.notify();
            return;
        }
        let complete = asked.is_none_or(|asked| (result.row_count() as u64) < asked);
        table.update(cx, |table, cx| {
            // A row added here is drawn after the rows read: a selection on
            // it follows it down.
            let first_added = |table: &TableState<Grid>| {
                let grid = table.delegate();
                grid.total_rows() - grid.pending().map_or(0, Pending::inserted)
            };
            let (before, selection) = (first_added(table), table.selection());
            table
                .delegate_mut()
                .took_rows(result.batches, from_start, complete, cx);
            let shift = first_added(table).saturating_sub(before);
            let adding = table
                .delegate()
                .pending()
                .is_some_and(|pending| pending.inserted() > 0);
            if shift > 0 && adding {
                match selection {
                    TableSelection::Cell(row, column) if row >= before => {
                        table.set_selected_cell(row + shift, column, cx)
                    }
                    TableSelection::Row(row) if row >= before => {
                        table.set_selected_row(row + shift, cx)
                    }
                    _ => {}
                }
            }
            cx.notify();
        });
        self.settle_end(cx);
        if self.load_all && self.has_more(cx) {
            self.fetch_more(true, window, cx);
        } else {
            self.load_all = false;
        }
        cx.notify();
    }

    /// How many rows the filter keeps, as counted or estimated; for tests.
    #[cfg(test)]
    pub fn total(&self) -> Option<Total> {
        self.total
    }

    /// Estimates above `rows` are shown as such; for tests.
    #[cfg(test)]
    pub fn set_estimate_above(&mut self, rows: u64) {
        self.estimate_above = rows;
    }

    /// Counts the rows the filter keeps (DBeaver's "Calculate total row
    /// count", `core.resultset.count`), after what the session is doing;
    /// Cancel stops it.
    pub fn count_exactly(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(session), Some(structure)) = (self.session.clone(), self.structure.clone())
        else {
            return;
        };
        if self.counting || self.state != State::Ready {
            return;
        }
        let Ok(counting) = count_sql(
            session.dialect(),
            &structure.columns,
            &self.table,
            &self.applied,
        ) else {
            return;
        };
        self.counting = true;
        let generation = self.generation;
        let previous = self.load.take();
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let Ok(counted) =
                cx.update(|_, cx| bridge::spawn(cx, exec::collect(session, counting)))
            else {
                return;
            };
            let total = first_count(counted.await);
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.counting = false;
                    // A cancelled count leaves the estimate.
                    if let Some(total) = total {
                        this.total = Some(Total::Exact(total));
                        this.settle_end(cx);
                    }
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// Lists the distinct values of the grid's `column` among the rows the
    /// filter keeps, but for the column's own conditions, with their counts
    /// (DBeaver's "Filter by distinct values"); the values picked are added
    /// to the filter.
    pub fn distinct_values(&mut self, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(session), Some(structure), Some(table)) = (
            self.session.clone(),
            self.structure.clone(),
            self.grid.clone(),
        ) else {
            return;
        };
        let Some(name) = ({
            let grid = table.read(cx).delegate();
            grid.source_column(column)
                .map(|source| grid.store().columns()[source].name.clone())
        }) else {
            return;
        };
        let Some(info) = structure
            .columns
            .iter()
            .find(|info| info.name == name)
            .cloned()
        else {
            return;
        };
        let mut filter = self.applied.clone();
        filter
            .conditions
            .retain(|condition| condition.column != name);
        let listing = distinct_sql(
            session.dialect(),
            kerno_core::browse::Source::Table(&self.table),
            &structure.columns,
            &filter,
            &name,
            distinct::LIMIT,
        );
        let listing = match listing {
            Ok(listing) => listing,
            Err(error) => {
                window.push_notification(
                    Notification::warning(SharedString::from(error.to_string())),
                    cx,
                );
                return;
            }
        };
        let previous = self.load.take();
        let generation = self.generation;
        self.counting = true;
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let dialect_of = session.clone();
            let Ok(listed) = cx.update(|_, cx| bridge::spawn(cx, exec::collect(session, listing)))
            else {
                return;
            };
            let listed = listed.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.counting = false;
                cx.notify();
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
                    let Some(condition) = one_of_sql(dialect_of.dialect(), &info, &picked) else {
                        return;
                    };
                    let _ = view.update(cx, |this, cx| {
                        this.add_raw_condition(&condition, window, cx)
                    });
                });
            });
        }));
        cx.notify();
    }

    /// Adds `condition` (SQL) to the raw `WHERE` of the filter bar and reads
    /// the rows the filter keeps.
    fn add_raw_condition(&mut self, condition: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.free_to_move(window, cx) {
            return;
        }
        let raw = kerno_core::browse::and_where(&self.raw_filter.read(cx).value(), condition);
        self.raw_filter
            .update(cx, |input, cx| input.set_value(raw, window, cx));
        self.apply_filter(window, cx);
    }

    // Auto-refresh ----------------------------------------------------------

    /// Reads the rows again every `every`, or stops (`None`).
    pub fn set_auto_refresh(
        &mut self,
        every: Option<Duration>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.auto_refresh_stopped = None;
        self.auto_refresh =
            every.map(|every| AutoRefresh::start(every, window, cx, Self::auto_tick));
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

    /// One tick of auto-refresh: whether it goes on.
    fn auto_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.pending_count(cx) > 0 {
            self.stop_auto_refresh(Some("changes are pending"), cx);
            return false;
        }
        if self.hidden || self.read_failed() || !self.connected(cx) {
            self.stop_auto_refresh(Some("the rows are not shown"), cx);
            return false;
        }
        // Never two reads at once: the next tick tries again.
        if !(self.state == State::Loading || self.appending || self.counting || self.applying) {
            self.reload(window, cx);
        }
        true
    }

    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.free_to_move(window, cx) {
            self.reload(window, cx);
        }
    }

    /// Stops the statement the tab is waiting for.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if !(self.state == State::Loading || self.appending || self.counting || self.applying) {
            return;
        }
        // Fetch all stops at the load in flight.
        self.load_all = false;
        // Seen by a read whose session is still opening (C3).
        self.cancel_requested = true;
        if self.applying {
            self.apply_stop.store(true, Ordering::Release);
        }
        if let Some(session) = self.session.clone() {
            bridge::spawn(cx, async move {
                if let Err(error) = session.cancel().await {
                    tracing::debug!(%error, "the statement could not be cancelled");
                }
            })
            .detach();
        }
    }

    // Filter ------------------------------------------------------------------

    fn push_filter_row(
        &mut self,
        condition: Condition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("value")
                .default_value(condition.value)
        });
        let enter = cx.subscribe_in(&value, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.apply_filter(window, cx);
            }
        });
        self.filter_rows.push(FilterRow {
            column: condition.column,
            operator: condition.operator,
            value,
            _enter: enter,
        });
    }

    /// Adds a line to the filter bar, on the first column.
    pub fn add_filter_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(column) = self
            .structure
            .as_ref()
            .and_then(|structure| structure.columns.first())
            .map(|column| column.name.clone())
        else {
            return;
        };
        self.push_filter_row(Condition::equals(column, ""), window, cx);
        if let Some(row) = self.filter_rows.last() {
            row.value.update(cx, |value, cx| value.focus(window, cx));
        }
        cx.notify();
    }

    /// The filter as the bar has it now.
    fn typed_filter(&self, cx: &App) -> Filter {
        Filter {
            conditions: self
                .filter_rows
                .iter()
                .map(|row| Condition {
                    column: row.column.clone(),
                    operator: row.operator,
                    value: row.value.read(cx).value().to_string(),
                })
                .collect(),
            raw: self.raw_filter.read(cx).value().trim().to_string(),
        }
    }

    /// Reads the rows the filter bar asks for, one load from the first.
    pub fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.free_to_move(window, cx) {
            return;
        }
        self.applied = self.typed_filter(cx);
        self.restart(window, cx);
    }

    /// Replaces the filter and reads the rows it keeps.
    pub fn set_filter(&mut self, filter: Filter, window: &mut Window, cx: &mut Context<Self>) {
        if !self.free_to_move(window, cx) {
            return;
        }
        self.filter_rows.clear();
        for condition in filter.conditions {
            self.push_filter_row(condition, window, cx);
        }
        self.raw_filter
            .update(cx, |raw, cx| raw.set_value(filter.raw, window, cx));
        self.apply_filter(window, cx);
    }

    // Editing -----------------------------------------------------------------

    /// Types in a cell of the grid; one that cannot be typed in shows its
    /// whole value instead.
    pub fn edit_cell(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(table) = self.grid.clone() {
            edit::edit_cell(&table, row, column, window, cx);
        }
    }

    /// The rows and columns of the result that are selected.
    fn selection(&self, cx: &App) -> Option<(Vec<usize>, Vec<usize>)> {
        edit::selection(self.grid.as_ref()?, cx)
    }

    /// Runs a change of the shared edit layer on the grid, when there is
    /// one, and redraws: the last apply's error goes.
    fn on_rows(
        &mut self,
        cx: &mut Context<Self>,
        act: impl FnOnce(&Entity<TableState<Grid>>, &mut App),
    ) {
        if let Some(table) = self.grid.clone() {
            act(&table, cx);
        }
        self.apply_error = None;
        cx.notify();
    }

    /// Takes the last change back (Cmd+Z). Returns whether there was one.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        let mut undone = false;
        self.on_rows(cx, |table, cx| undone = edit::undo(table, cx));
        undone
    }

    /// Drops the pending values of the selected cells (Esc). Returns
    /// whether there was one, so that Esc otherwise clears the selection.
    pub fn revert_cells(&mut self, cx: &mut Context<Self>) -> bool {
        let mut reverted = false;
        self.on_rows(cx, |table, cx| reverted = edit::revert_cells(table, cx));
        reverted
    }

    /// Gives the selected cells the values of the row above (`above`) or
    /// below (see [`edit::copy_from_row`]).
    pub fn copy_from_row(&mut self, above: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit(window, cx) {
            self.on_rows(cx, |table, cx| edit::copy_from_row(table, above, cx));
        }
    }

    /// Whether the rows take changes, and why not when they do not.
    fn edits(&self, cx: &App) -> edit::Allowed {
        if self.pending(cx).is_some() {
            return Ok(());
        }
        Err(self.read_only_reason(cx).map(SharedString::from))
    }

    /// Whether the rows take changes; says why not when they do not.
    fn can_edit(&self, window: &mut Window, cx: &mut App) -> bool {
        let edits = self.edits(cx);
        if let Err(Some(why)) = &edits {
            window.push_notification(Notification::warning(why.clone()), cx);
        }
        edits.is_ok()
    }

    /// Adds an empty row at the end and shows it.
    pub fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit(window, cx) {
            self.on_rows(cx, edit::add_row);
        }
    }

    /// Adds copies of the selected rows (or of `row`) at the end.
    pub fn duplicate_rows(
        &mut self,
        row: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.can_edit(window, cx) {
            self.on_rows(cx, |table, cx| edit::duplicate_rows(table, row, cx));
        }
    }

    /// Marks the selected rows (or `row`) for deletion.
    pub fn delete_rows(&mut self, row: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit(window, cx) {
            self.on_rows(cx, |table, cx| edit::delete_rows(table, row, cx));
        }
    }

    /// Takes a row back to what was fetched.
    pub fn revert_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.on_rows(cx, |table, cx| edit::revert_row(table, row, cx));
    }

    /// Gives every selected cell `value` (`NULL` or the column's default).
    pub fn set_selected(&mut self, value: CellValue, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit(window, cx) {
            self.on_rows(cx, |table, cx| edit::set_cells(table, value, false, cx));
        }
    }

    /// Sets the selected cells that take NULL to NULL (Delete, Backspace).
    pub fn clear_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_edit(window, cx) {
            self.on_rows(cx, |table, cx| {
                edit::set_cells(table, CellValue::Null, true, cx)
            });
        }
    }

    /// Puts the text of the clipboard in the selected cells: one value in
    /// each, or a block of tab-separated cells from the first one.
    pub fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if !text.is_empty() && self.can_edit(window, cx) {
            self.on_rows(cx, |table, cx| edit::paste(table, &text, cx));
        }
    }

    /// A condition on the column of a cell of the grid, with the cell's
    /// value: `operator`, or for a NULL cell `IS NULL` (`=`) and
    /// `IS NOT NULL` (`<>`).
    fn condition_at(
        &self,
        row: usize,
        column: usize,
        operator: Operator,
        cx: &App,
    ) -> Option<Condition> {
        condition_at(
            self.grid.as_ref()?.read(cx).delegate(),
            row,
            column,
            operator,
        )
    }

    /// Adds a condition to the filter bar and reads the rows it keeps
    /// (refused, the bar unchanged, while changes are pending).
    fn filter_by(&mut self, condition: Condition, window: &mut Window, cx: &mut Context<Self>) {
        if !self.free_to_move(window, cx) {
            return;
        }
        self.push_filter_row(condition, window, cx);
        self.apply_filter(window, cx);
    }

    /// Adds a condition to the filter bar, to be finished and applied
    /// there: its value is focused.
    fn prefill_filter(
        &mut self,
        condition: Condition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.push_filter_row(condition, window, cx);
        if let Some(row) = self.filter_rows.last() {
            row.value.update(cx, |value, cx| value.focus(window, cx));
        }
        cx.notify();
    }

    /// Runs an entry of a menu of the grid that is the tab's, on the cell
    /// at `row` and `column` of the grid.
    pub fn perform(
        &mut self,
        action: GridAction,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let condition =
            |this: &Self, operator, cx: &App| this.condition_at(row, column, operator, cx);
        match action {
            GridAction::Paste => self.paste(window, cx),
            GridAction::FilterByValue | GridAction::ExcludeValue => {
                let operator = if action == GridAction::FilterByValue {
                    Operator::Equals
                } else {
                    Operator::NotEquals
                };
                if let Some(condition) = condition(self, operator, cx) {
                    self.filter_by(condition, window, cx);
                }
            }
            GridAction::FilterWith(operator) => {
                if let Some(condition) = condition(self, operator, cx) {
                    self.prefill_filter(condition, window, cx);
                }
            }
            GridAction::FilterColumn => {
                let name = self.grid.as_ref().and_then(|table| {
                    let grid = table.read(cx).delegate();
                    let source = grid.source_column(column)?;
                    Some(grid.store().columns()[source].name.clone())
                });
                if let Some(name) = name {
                    self.prefill_filter(Condition::equals(name, ""), window, cx);
                }
            }
            GridAction::ClearFilters => self.set_filter(Filter::default(), window, cx),
            GridAction::DistinctValues => self.distinct_values(column, window, cx),
            GridAction::Follow(index, _) => {
                let related = match (&self.grid, &self.structure) {
                    (Some(table), Some(structure)) => {
                        related(structure, table.read(cx).delegate(), row)
                    }
                    _ => Vec::new(),
                };
                if let Some(related) = related.get(index) {
                    self.follow(related, cx);
                }
            }
            GridAction::EditCell => self.edit_cell(row, column, window, cx),
            // Cells of NOT NULL columns are left alone, as with Delete.
            GridAction::SetNull => self.clear_selected(window, cx),
            GridAction::SetDefault => {
                self.set_selected(CellValue::Expression("DEFAULT".into()), window, cx)
            }
            GridAction::CopyFromRow(above) => self.copy_from_row(above, window, cx),
            GridAction::DuplicateRow => self.duplicate_rows(Some(row), window, cx),
            GridAction::DeleteRow => self.delete_rows(Some(row), window, cx),
            GridAction::RevertRow => self.revert_row(row, cx),
            GridAction::Export => {
                self.export(window, cx);
            }
            GridAction::Generate(kind) => self.generate_sql(kind, row, window, cx),
            GridAction::AddRow => self.add_row(window, cx),
            GridAction::Refresh => self.refresh(window, cx),
            GridAction::ResetLayout => self.reset_layout(window, cx),
            // The grid runs the others itself.
            _ => {}
        }
    }

    /// Writes `kind` statements for the selected rows (or `row`) into a new
    /// query tab, with their values as literals: nothing runs. Rows added
    /// and not applied yet are left out.
    pub fn generate_sql(
        &mut self,
        kind: RowSql,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(table) = &self.grid else {
            return;
        };
        let shown = self
            .selection(cx)
            .map_or_else(|| vec![row], |(rows, _)| rows);
        let grid = table.read(cx).delegate();
        let rows: Vec<usize> = shown
            .into_iter()
            .map(|row| grid.source_row(row))
            .filter(|&row| row < grid.row_count())
            .collect();
        if rows.is_empty() {
            return;
        }
        let written = grid.rows_sql(kind, &rows);
        match written {
            Ok(sql) => cx.emit(DataTabEvent::OpenQuery {
                title: self.table.name.clone(),
                sql,
            }),
            Err(error) => window.push_notification(
                Notification::error(SharedString::from(format!(
                    "The {} could not be written: {error}",
                    kind.label()
                ))),
                cx,
            ),
        }
    }

    /// Opens the other end of a foreign key in a tab of its own.
    pub fn follow(&mut self, related: &Related, cx: &mut Context<Self>) {
        cx.emit(DataTabEvent::Open {
            table: related.table.clone(),
            kind: RelationKind::Table,
            filter: related.filter.clone(),
        });
    }

    /// Where the foreign keys of a row lead; for tests.
    #[cfg(test)]
    pub fn related(&self, row: usize, cx: &App) -> Vec<Related> {
        match (&self.grid, &self.structure) {
            (Some(table), Some(structure)) => related(structure, table.read(cx).delegate(), row),
            _ => Vec::new(),
        }
    }

    /// Opens the export of every row the applied filter keeps, in the
    /// order on screen.
    pub fn export(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ExportDialog>> {
        export_dialog::open(
            self.connections.clone(),
            self.target.clone(),
            Source::Table {
                table: self.table.clone(),
                filter: self.applied.clone(),
                sort: self.sort.clone(),
            },
            window,
            cx,
        )
    }

    /// Forgets every pending change.
    pub fn discard(&mut self, cx: &mut Context<Self>) {
        self.on_rows(cx, edit::discard);
    }

    /// The statements the pending changes become; for the review and tests.
    pub fn pending_sql(&self, cx: &App) -> kerno_core::Result<(Vec<RowChange>, Vec<String>)> {
        match &self.grid {
            Some(table) => table.read(cx).delegate().pending_sql(),
            None => Ok((Vec::new(), Vec::new())),
        }
    }

    /// Shows the statements of the pending changes and, once confirmed,
    /// applies them in one transaction.
    pub fn review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // What is being typed counts.
        if let Some(table) = &self.grid {
            table.update(cx, |table, cx| {
                table.delegate_mut().commit_edit(cx);
                cx.notify();
            });
        }
        let (changes, statements) = match self.pending_sql(cx) {
            Ok(written) => written,
            Err(error) => {
                self.apply_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        if statements.is_empty() {
            return;
        }
        let saved = self.connections.read(cx).get(&self.target.connection);
        let environment = saved
            .map(|saved| saved.config.environment.clone())
            .unwrap_or_default();
        let count = self.pending_count(cx);
        let view = cx.entity();
        let run = statements.clone();
        sql_confirm::open(
            Confirmation {
                title: format!(
                    "Apply {count} {} to {}",
                    if count == 1 { "change" } else { "changes" },
                    self.table.name
                ),
                explanation: "These statements run in one transaction: all of them take effect, \
                              or none does."
                    .into(),
                statement: Statement::Fixed(
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
                connection: self.connection_name(cx),
                environment,
                writes: true,
            },
            window,
            cx,
            move |_, window, cx| {
                view.update(cx, |this, cx| this.apply(run.clone(), window, cx));
            },
        );
    }

    /// Runs the reviewed statements: all of them, or none.
    fn apply(&mut self, statements: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        self.applying = true;
        self.apply_error = None;
        self.cancel_requested = false;
        let stop = Arc::new(AtomicBool::new(false));
        self.apply_stop = stop.clone();
        let previous = self.load.take();
        let count = self.pending_count(cx);
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let Ok(applying) = cx.update(|_, cx| {
                bridge::spawn(cx, async move {
                    apply_edits_until(&session, &statements, &stop).await
                })
            }) else {
                return;
            };
            let outcome = applying.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.applying = false;
                let cancelled = std::mem::take(&mut this.cancel_requested);
                match outcome {
                    Ok(()) => {
                        window.push_notification(
                            Notification::success(SharedString::from(format!(
                                "{count} {} applied.",
                                if count == 1 { "change" } else { "changes" }
                            ))),
                            cx,
                        );
                        // The grid is read again, without the changes.
                        this.refocus_grid = true;
                        this.load = None;
                        this.reload(window, cx);
                    }
                    // Nothing was written; the changes are still there.
                    // One transaction, rolled back: the cancel undid it all (C3).
                    Err(_) if cancelled => {
                        this.apply_error = Some(
                            "Cancelled: nothing was changed. The pending changes are kept.".into(),
                        );
                        cx.notify();
                    }
                    Err(failure) => {
                        this.apply_error = Some(apply_failure_text(&failure));
                        cx.notify();
                    }
                }
            });
        }));
        cx.notify();
    }

    /// Why the last apply changed nothing; for tests.
    #[cfg(test)]
    pub fn apply_error(&self) -> Option<&str> {
        self.apply_error.as_deref()
    }

    /// The grid, once rows are read.
    /// The value panel, while shown; for tests.
    #[cfg(test)]
    pub fn value_panel(&self) -> Option<Entity<ValuePanel>> {
        self.value_panel.clone()
    }

    pub fn grid(&self) -> Option<Entity<TableState<Grid>>> {
        self.grid.clone()
    }

    /// What the error state says, when the rows could not be read; for
    /// tests.
    #[cfg(test)]
    pub fn advice(&self) -> String {
        match &self.state {
            State::Failed { message, gone } => failure_text(message, *gone),
            State::Cancelled => CANCELLED.into(),
            _ => String::new(),
        }
    }

    /// Where the tab is with its rows, in words; for tests.
    #[cfg(test)]
    pub fn state(&self, cx: &App) -> String {
        match &self.state {
            State::Waiting => "waiting".into(),
            State::Loading => "loading".into(),
            State::Failed { message, .. } => format!("failed: {message}"),
            State::Cancelled => "cancelled".into(),
            State::Ready => self.range_text(cx),
        }
    }

    /// Copies the selection in `format`. Returns whether there was
    /// something to copy.
    pub fn copy_selection(&mut self, format: CopyFormat, cx: &mut Context<Self>) -> bool {
        self.grid.as_ref().is_some_and(|table| {
            table.update(cx, |table, cx| Grid::copy(table, Some(format), None, cx))
        })
    }

    // Rendering ---------------------------------------------------------------

    /// "Rows 1–3,000 of 12,345", "of about 12,000", or without a total
    /// before it is counted.
    fn range_text(&self, cx: &App) -> String {
        let fetched = self.fetched(cx);
        let range = if fetched == 0 {
            "No rows".to_string()
        } else {
            format!("Rows 1–{}", number(fetched as f64))
        };
        match self.total {
            Some(Total::Exact(total)) if fetched > 0 => {
                format!("{range} of {}", number(total as f64))
            }
            Some(Total::About(total)) if fetched > 0 => {
                format!("{range} of about {}", number(total as f64))
            }
            _ => range,
        }
    }

    /// How many rows the next load reads: its size, or what is left of a
    /// counted table.
    fn next_load(&self, cx: &App) -> u64 {
        match self.total {
            Some(Total::Exact(total)) => total
                .saturating_sub(self.fetched(cx))
                .clamp(1, self.load_size),
            _ => self.load_size,
        }
    }

    /// What the connection's environment paints on the tab.
    fn chrome(&self, cx: &App) -> Option<crate::theme::EnvironmentChrome> {
        let saved = self.connections.read(cx).get(&self.target.connection)?;
        crate::theme::environment_chrome(&saved.config, cx)
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let engine = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .map(|saved| saved.config.engine);
        let editable = self.pending(cx).is_some();
        let pending = self.pending_count(cx);
        let has_selection = self.selection(cx).is_some();
        let busy = self.state == State::Loading || self.applying;
        let action = |id: &'static str, label: &'static str, tip: &'static str| {
            Button::new(id).ghost().small().label(label).tooltip(tip)
        };
        let chrome = self.chrome(cx);
        h_flex()
            .flex_none()
            // A group of a split is often narrower than the toolbar: its
            // buttons go to a second line instead of over the next group.
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .when_some(chrome, |this, chrome| this.bg(chrome.wash))
            .children(
                engine.map(|engine| div().flex_none().child(engine_logo(engine, px(14.), cx))),
            )
            .child(
                div()
                    .id("data-table-name")
                    .test_support()
                    .px_1()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .child(SharedString::from(format!(
                        "{}.{}",
                        self.table.schema, self.table.name
                    ))),
            )
            .child(
                action("data-refresh", "Refresh", "Read the rows again")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
            // Pending changes stop auto-refresh: no use for it then, and
            // the toolbar keeps its room for them.
            .when(pending == 0, |bar| {
                bar.child(auto_refresh_button(
                    "data-auto-refresh",
                    self.auto_refresh(),
                    cx.entity(),
                    |this, every, window, cx| this.set_auto_refresh(every, window, cx),
                ))
            })
            .when(editable, |bar| {
                bar.child(
                    action("data-add-row", "Add row", "Add an empty row")
                        .disabled(self.read_failed())
                        .on_click(cx.listener(|this, _, window, cx| this.add_row(window, cx))),
                )
                .child(
                    action(
                        "data-duplicate",
                        "Duplicate",
                        "Copy the selected rows as new rows",
                    )
                    .disabled(!has_selection)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.duplicate_rows(None, window, cx)),
                    ),
                )
                .child(
                    action(
                        "data-delete",
                        "Delete",
                        "Mark the selected rows for deletion",
                    )
                    .disabled(!has_selection)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.delete_rows(None, window, cx)),
                    ),
                )
                .child(
                    action(
                        "data-set-null",
                        "Set NULL",
                        "Set the selected cells to NULL",
                    )
                    .disabled(!has_selection)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_selected(CellValue::Null, window, cx)
                    })),
                )
                .child(
                    action(
                        "data-set-default",
                        "Set default",
                        "Give the selected cells their column's default",
                    )
                    .disabled(!has_selection)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_selected(CellValue::Expression("DEFAULT".into()), window, cx)
                    })),
                )
            })
            .child(div().flex_1())
            // The right end holds the pending changes when there are some.
            .when(pending == 0, |bar| {
                bar.child(
                    action(
                        "data-open-structure",
                        "Structure",
                        "Show the columns, keys, indexes and DDL",
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.emit(DataTabEvent::OpenStructure {
                            table: this.table.clone(),
                            kind: this.kind,
                        })
                    })),
                )
                .child(
                    action(
                        "data-export",
                        "Export…",
                        "Write every row matching the filter to a file",
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.export(window, cx);
                    })),
                )
            })
            .when(pending > 0, |bar| {
                bar.child(
                    div()
                        .id("data-pending")
                        .test_support()
                        .px_1()
                        .text_sm()
                        .text_color(theme.warning)
                        .child(SharedString::from(format!(
                            "{pending} pending {}",
                            if pending == 1 { "change" } else { "changes" }
                        ))),
                )
                .child(
                    action("data-discard", "Discard", "Forget the pending changes")
                        .disabled(self.applying)
                        .on_click(cx.listener(|this, _, _, cx| this.discard(cx))),
                )
                .child(
                    action(
                        "data-review",
                        "Review SQL",
                        "Show the statements of the pending changes",
                    )
                    .disabled(self.applying)
                    .on_click(cx.listener(|this, _, window, cx| this.review(window, cx))),
                )
                .child(
                    Button::new("data-apply")
                        .primary()
                        .small()
                        .label("Apply")
                        .tooltip("Review the SQL, then apply it in one transaction")
                        .disabled(self.applying)
                        .on_click(cx.listener(|this, _, window, cx| this.review(window, cx))),
                )
            })
    }

    fn render_filter_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let columns: Vec<String> = self.structure.as_ref().map_or_else(Vec::new, |structure| {
            structure
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect()
        });
        let view = cx.entity();
        let rows = self.filter_rows.iter().enumerate().map(|(index, row)| {
            let column_menu = {
                let (view, columns) = (view.clone(), columns.clone());
                move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                    for column in &columns {
                        let (view, column) = (view.clone(), column.clone());
                        menu = menu.item(
                            PopupMenuItem::new(SharedString::from(column.clone())).on_click(
                                move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        if let Some(row) = this.filter_rows.get_mut(index) {
                                            row.column = column.clone();
                                        }
                                        cx.notify();
                                    })
                                },
                            ),
                        );
                    }
                    menu
                }
            };
            let operator_menu = {
                let view = view.clone();
                move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                    for operator in Operator::ALL {
                        let view = view.clone();
                        menu = menu.item(PopupMenuItem::new(operator.label()).on_click(
                            move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    if let Some(row) = this.filter_rows.get_mut(index) {
                                        row.operator = operator;
                                    }
                                    cx.notify();
                                })
                            },
                        ));
                    }
                    menu
                }
            };
            h_flex()
                .gap_1()
                .child(
                    Button::new(("filter-column", index))
                        .outline()
                        .small()
                        .label(SharedString::from(row.column.clone()))
                        .dropdown_caret(true)
                        .dropdown_menu(column_menu),
                )
                .child(
                    Button::new(("filter-operator", index))
                        .outline()
                        .small()
                        .label(row.operator.label())
                        .dropdown_caret(true)
                        .dropdown_menu(operator_menu),
                )
                .when(row.operator.takes_value(), |line| {
                    line.child(
                        div()
                            .w(px(220.))
                            .child(Input::new(&row.value).small().id(("filter-value", index))),
                    )
                })
                .child(
                    Button::new(("filter-remove", index))
                        .ghost()
                        .small()
                        .label("Remove")
                        .tooltip("Remove this condition")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if index < this.filter_rows.len() {
                                this.filter_rows.remove(index);
                            }
                            cx.notify();
                        })),
                )
        });
        let filtered = !self.applied.is_empty();
        v_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .children(rows)
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .child(
                        Button::new("filter-add")
                            .ghost()
                            .small()
                            .label("Add filter")
                            .tooltip("Add a condition on a column")
                            .disabled(columns.is_empty())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_filter_row(window, cx)),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("WHERE"),
                    )
                    .child(
                        div()
                            .id("filter-raw")
                            .test_support()
                            .flex_1()
                            .min_w(FILTER_MIN_WIDTH)
                            // Tab takes a proposal (J1) or goes to the next
                            // control: the field is one line, nothing to indent.
                            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                                if !completion::tab_accepts(&this.raw_filter, window, cx) {
                                    window.focus_next(cx);
                                }
                                cx.stop_propagation();
                            }))
                            .child(Editor::new(&self.raw_filter).small().h(px(24.))),
                    )
                    .child(history_button(
                        "filter-history",
                        &self.history,
                        view.clone(),
                        |this, filter, window, cx| {
                            this.set_filter(
                                Filter {
                                    conditions: Vec::new(),
                                    raw: filter,
                                },
                                window,
                                cx,
                            )
                        },
                    ))
                    .child(
                        Button::new("filter-apply")
                            .outline()
                            .small()
                            .label("Filter")
                            .tooltip("Read the rows the conditions keep (Enter)")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.apply_filter(window, cx)),
                            ),
                    )
                    .when(filtered || !self.filter_rows.is_empty(), |line| {
                        line.child(
                            Button::new("filter-clear")
                                .ghost()
                                .small()
                                .label("Clear")
                                .tooltip("Show every row again")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_filter(Filter::default(), window, cx)
                                })),
                        )
                    }),
            )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let loading = self.state == State::Loading;
        let range = match (&self.state, &self.grid) {
            (State::Loading, None) => "Loading rows…".to_string(),
            (State::Failed { .. } | State::Cancelled, _) | (_, None) => String::new(),
            _ => self.range_text(cx),
        };
        h_flex()
            .id("data-footer")
            .test_support()
            .flex_none()
            // A narrow group of a split (K19) keeps the pager in reach: it
            // goes to a second line instead of off the edge.
            .flex_wrap()
            .px_3()
            .py_1()
            .gap_3()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(SharedString::from(range))
            .when(
                matches!(self.total, Some(Total::About(_))) && !self.counting && !loading,
                |bar| {
                    bar.child(
                        Button::new("data-count")
                            .ghost()
                            .xsmall()
                            .label("Count exactly")
                            .tooltip("Count the rows of the table (the number shown is the server's estimate)")
                            .on_click(cx.listener(|this, _, window, cx| this.count_exactly(window, cx))),
                    )
                },
            )
            .children(self.auto_refresh().map(|every| {
                div()
                    .id("data-auto-refresh-on")
                    .test_support()
                    .child(SharedString::from(format!(
                        "Auto-refresh {}",
                        AutoRefresh::label(every)
                    )))
            }))
            .children(self.auto_refresh_stopped.map(|why| {
                div()
                    .id("data-auto-refresh-stopped")
                    .test_support()
                    .text_color(cx.theme().warning)
                    .child(SharedString::from(format!("Auto-refresh stopped: {why}.")))
            }))
            .when(self.counting && !loading, |bar| bar.child("counting…"))
            .when(loading && self.grid.is_some(), |bar| bar.child("loading…"))
            .when(self.appending, |bar| bar.child("fetching more…"))
            .when(self.applying, |bar| bar.child("applying…"))
            .when(loading || self.appending || self.counting || self.applying, |bar| {
                bar.child(
                    Button::new("data-cancel")
                        .ghost()
                        .xsmall()
                        .label("Cancel")
                        .tooltip("Stop the statement")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .children(
                self.elapsed
                    .filter(|_| !loading && !self.read_failed())
                    .map(|elapsed| SharedString::from(duration_text(elapsed))),
            )
            .children(self.grid.clone().map(|table| {
                Button::new("data-record")
                    .ghost()
                    .xsmall()
                    .label("Record")
                    .tooltip("One row as a list of its columns, or the grid again (Tab)")
                    .on_click(move |_, window, cx| Grid::toggle_record(&table, window, cx))
            }))
            .when(self.grid.is_some(), |footer| {
                footer.child(value_panel::button("data-value-panel", self.value_panel.as_ref(), cx))
            })
            .children(
                self.grid
                    .as_ref()
                    .map(|table| crate::query::grid_view::SelectionStats::of(table, cx)),
            )
            .child(div().flex_1())
            // No way on before the rows are read, or when they could not be.
            // While there is one it stands out and says what it reads (K21:
            // a first user missed it at the end of a page); the size of a
            // load is changed here, for this tab (K36, DBeaver's fetch size
            // box).
            .when(!self.read_failed() && self.grid.is_some(), |footer| {
                let more = self.has_more(cx);
                let idle = !(loading || self.appending);
                let next = self.next_load(cx);
                let label = match &self.load_error {
                    Some(_) if more => "Try again".to_string(),
                    _ if more => format!(
                        "Fetch {} more",
                        number(next as f64)
                    ),
                    _ => "All rows fetched".to_string(),
                };
                footer
                    .children(self.load_error.clone().map(|error| {
                        let error = SharedString::from(error);
                        div()
                            .id("data-fetch-error")
                            .test_support()
                            .aria_label(error.clone())
                            .text_color(cx.theme().danger)
                            .child(error)
                    }))
                    .child(load_size_button(self.load_size, cx.entity()))
                    .child(
                        Button::new("data-fetch-more")
                            .map(|button| if more { button.outline() } else { button.ghost() })
                            .xsmall()
                            .label(SharedString::from(label))
                            .tooltip_with_action(
                                "Read the next rows and add them under these; scrolling to \
                                 the last rows does it too",
                                &crate::actions::FetchMoreRows,
                                None,
                            )
                            .disabled(!(more && idle))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.fetch_more(false, window, cx)
                            })),
                    )
                    .when(more, |footer| {
                        footer.child(
                            Button::new("data-fetch-all")
                                .ghost()
                                .xsmall()
                                .label("Fetch all")
                                .tooltip_with_action(
                                    "Read every row left; Cancel stops it",
                                    &crate::actions::FetchAllRows,
                                    None,
                                )
                                .disabled(!idle)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.fetch_more(true, window, cx)
                                })),
                        )
                    })
            })
    }

    /// A message in place of the rows, with what to do next.
    fn render_notice(
        &self,
        id: &'static str,
        title: &str,
        detail: String,
        action: Option<AnyElement>,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .id(id)
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .child(
                div()
                    .font_semibold()
                    .child(SharedString::from(title.to_string())),
            )
            .child(
                div()
                    .max_w(px(640.))
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(detail)),
            )
            .children(action)
            .into_any_element()
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> AnyElement {
        match (&self.state, &self.grid) {
            (State::Waiting, _) => {
                let connect = Button::new("data-connect")
                    .primary()
                    .label("Connect")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let id = this.target.connection.clone();
                        this.connections
                            .update(cx, |connections, cx| connections.connect(&id, cx));
                    }))
                    .into_any_element();
                self.render_notice(
                    "data-waiting",
                    "Not connected",
                    format!(
                        "“{}” is not connected. Connect it to read the rows of {}.",
                        self.connection_name(cx),
                        self.table.name
                    ),
                    Some(connect),
                    cx,
                )
            }
            (State::Cancelled, _) => {
                let reload = Button::new("data-retry")
                    .outline()
                    .label("Reload")
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx)));
                self.render_notice(
                    "data-cancelled",
                    "Cancelled",
                    CANCELLED.to_string(),
                    Some(reload.into_any_element()),
                    cx,
                )
            }
            (State::Failed { message, gone }, _) => {
                let retry = Button::new("data-retry")
                    .outline()
                    .label("Try again")
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx)));
                let actions = if *gone {
                    let close = Button::new("data-close-tab")
                        .outline()
                        .label("Close tab")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(CloseTab), cx));
                    h_flex().gap_2().child(close).child(retry)
                } else {
                    h_flex().child(retry)
                };
                self.render_notice(
                    "data-error",
                    "The rows could not be read",
                    failure_text(message, *gone),
                    Some(actions.into_any_element()),
                    cx,
                )
            }
            (_, None) => self.render_notice(
                "data-loading",
                "Loading rows…",
                format!("Reading {}.{}.", self.table.schema, self.table.name),
                None,
                cx,
            ),
            (_, Some(table)) => {
                let empty = table.read(cx).delegate().total_rows() == 0;
                let rows = crate::query::grid_view::render(table, cx);
                let theme = cx.theme();
                let area = div()
                    .size_full()
                    .relative()
                    .child(rows)
                    .when(empty, |area| {
                        area.child(
                            div()
                                .id("data-empty")
                                .test_support()
                                .absolute()
                                .top_10()
                                .left_0()
                                .right_0()
                                .flex()
                                .justify_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(if self.applied.is_empty() {
                                    "This table has no rows."
                                } else {
                                    "No row matches the filter."
                                }),
                        )
                    })
                    .into_any_element();
                value_panel::beside(area, self.value_panel.as_ref(), cx)
            }
        }
    }
}

/// A condition on the column of a cell of `grid`, with the cell's value:
/// `operator`, or for a NULL cell `IS NULL` (`=`) and `IS NOT NULL` (`<>`).
pub fn condition_at(
    grid: &Grid,
    row: usize,
    column: usize,
    operator: Operator,
) -> Option<Condition> {
    let (described, value) = grid.value_at(row, column)?;
    let operator = match (value, operator) {
        (None, Operator::Equals) => Operator::IsNull,
        (None, Operator::NotEquals) => Operator::IsNotNull,
        (None, _) => return None,
        (Some(_), operator) => operator,
    };
    Some(Condition {
        column: described.name.clone(),
        operator,
        value: value.map_or(String::new(), |bytes| {
            String::from_utf8_lossy(bytes).into_owned()
        }),
    })
}

/// The other end of a foreign key of a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Related {
    /// What the menu calls it.
    pub label: String,
    pub table: TableRef,
    /// The rows of `table` at that end.
    pub filter: Filter,
}

/// Where the foreign keys of a row lead: to the row it references, and to
/// the rows that reference it. A key with a NULL column leads nowhere.
pub fn related(structure: &TableStructure, grid: &Grid, row: usize) -> Vec<Related> {
    let references = structure
        .foreign_keys
        .iter()
        .filter_map(|key| reference(key, grid, row));
    let referenced_by = structure.referenced_by.iter().filter_map(|key| {
        let values = values_of(grid, row, &key.referenced_columns)?;
        Some(Related {
            label: format!(
                "Rows of {} referencing this one ({})",
                key.table.name,
                key.columns.join(", ")
            ),
            table: key.table.clone(),
            filter: equal_to(&key.columns, values),
        })
    });
    references.chain(referenced_by).collect()
}

/// The row a foreign key of a row points to, when none of its columns is
/// NULL: the jump of the cell menu, of a cell's arrow (K49), and the value
/// panel's preview (K18). Labelled as the arrow's tooltip says it: "Open
/// shop.customers where id = 42".
pub fn reference(key: &ForeignKeyInfo, grid: &Grid, row: usize) -> Option<Related> {
    let values = values_of(grid, row, &key.columns)?;
    let filter = equal_to(&key.referenced_columns, values);
    let conditions: Vec<String> = filter
        .conditions
        .iter()
        .map(|condition| format!("{} = {}", condition.column, condition.value))
        .collect();
    Some(Related {
        label: format!(
            "Open {}.{} where {}",
            key.referenced_table.schema,
            key.referenced_table.name,
            conditions.join(" and ")
        ),
        table: key.referenced_table.clone(),
        filter,
    })
}

/// The jump of the foreign key that holds the table column `name`, from
/// `row` (K49): its number among the row's jumps ([`related`], as
/// [`GridAction::Follow`] numbers them) and where it leads. None when the
/// column is in no foreign key or a column of the key is NULL. A column in
/// several keys follows the first, as the value panel's preview does.
pub fn follow_at(
    structure: &TableStructure,
    grid: &Grid,
    row: usize,
    name: &str,
) -> Option<(usize, Related)> {
    let keys = &structure.foreign_keys;
    let index = keys
        .iter()
        .position(|key| key.columns.iter().any(|column| column == name))?;
    let related = reference(&keys[index], grid, row)?;
    let before = keys[..index]
        .iter()
        .filter(|earlier| reference(earlier, grid, row).is_some())
        .count();
    Some((before, related))
}

/// The values of the table columns `columns` in a row, when none is NULL.
fn values_of(grid: &Grid, row: usize, columns: &[String]) -> Option<Vec<String>> {
    columns
        .iter()
        .map(|column| grid.table_text(row, column))
        .collect()
}

/// The rows whose `on` columns hold `values`.
fn equal_to(on: &[String], values: Vec<String>) -> Filter {
    Filter {
        conditions: on
            .iter()
            .zip(values)
            .map(|(column, value)| Condition::equals(column.clone(), value))
            .collect(),
        raw: String::new(),
    }
}

/// What the layout of a table's columns is stored under (J6), and the
/// columns its preview shows (K18): the database, the schema and the
/// table (a MySQL table's schema is its database), with the connection
/// apart. The database is the one the session is in, known before it
/// opens: a PostgreSQL connection that names none opens in the driver's
/// default one, which is also what the tree names.
pub fn table_key(connections: &Connections, target: &Target, table: &TableRef) -> String {
    let saved = connections.get(&target.connection);
    let database = match saved.map(|saved| saved.config.engine) {
        Some(Engine::MySql | Engine::MariaDb) => String::new(),
        _ => target
            .database
            .clone()
            .or_else(|| saved.and_then(|saved| saved.config.database.clone()))
            .unwrap_or_else(|| kerno_postgres::DEFAULT_DATABASE.to_string()),
    };
    serde_json::to_string(&[database.as_str(), &table.schema, &table.name]).unwrap_or_default()
}

/// What a view does with an interval of auto-refresh (`None`: stop).
type SetAutoRefresh<T> = fn(&mut T, Option<Duration>, &mut Window, &mut Context<T>);

/// The auto-refresh dropdown (DBeaver's `AutoRefreshControl`): the
/// intervals, the current one ticked, and Stop; it says when it is on.
pub fn auto_refresh_button<T: 'static>(
    id: &'static str,
    on: Option<Duration>,
    view: Entity<T>,
    set: SetAutoRefresh<T>,
) -> impl IntoElement {
    let label = match on {
        Some(every) => format!("Every {} s", every.as_secs()),
        None => "Auto-refresh".into(),
    };
    Button::new(id)
        .ghost()
        .small()
        .label(SharedString::from(label))
        .selected(on.is_some())
        .tooltip("Read the rows again every few seconds")
        .dropdown_caret(true)
        .dropdown_menu(move |mut menu, _, _| {
            for seconds in AUTO_REFRESH_SECONDS {
                let (every, view) = (Duration::from_secs(seconds), view.clone());
                menu = menu.item(
                    PopupMenuItem::new(SharedString::from(format!("Every {seconds} s")))
                        .checked(on == Some(every))
                        .on_click(move |_, window, cx| {
                            view.update(cx, |this, cx| set(this, Some(every), window, cx))
                        }),
                );
            }
            let view = view.clone();
            menu.separator()
                .item(PopupMenuItem::new("Stop").disabled(on.is_none()).on_click(
                    move |_, window, cx| view.update(cx, |this, cx| set(this, None, window, cx)),
                ))
        })
}

/// How many rows a load of a table's data reads, as a dropdown (K36,
/// DBeaver's fetch size box in the result's status bar): a few sizes, the
/// current one ticked, and another typed.
fn load_size_button(size: u64, view: Entity<DataTab>) -> impl IntoElement {
    Button::new("data-fetch-size")
        .ghost()
        .xsmall()
        .label(SharedString::from(format!(
            "Fetch size: {}",
            number(size as f64)
        )))
        .tooltip("How many rows each fetch reads in this tab")
        .dropdown_caret(true)
        .dropdown_menu(move |mut menu, _, _| {
            for each in LOAD_SIZES {
                let view = view.clone();
                menu = menu.item(
                    PopupMenuItem::new(SharedString::from(number(each as f64)))
                        .checked(each == size)
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.set_load_size(each, cx))
                        }),
                );
            }
            let view = view.clone();
            menu.separator().item(
                PopupMenuItem::new("Other size…")
                    .checked(!LOAD_SIZES.contains(&size))
                    .on_click(move |_, window, cx| ask_load_size(size, view.clone(), window, cx)),
            )
        })
}

/// Asks for the size of a load, within the bounds of Settings' page size.
fn ask_load_size(size: u64, view: Entity<DataTab>, window: &mut Window, cx: &mut App) {
    let (low, high) = (
        number(f64::from(*PAGE_SIZES.start())),
        number(f64::from(*PAGE_SIZES.end())),
    );
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(format!("{low} to {high}"))
            .default_value(size.to_string())
    });
    // Takes a size the settings would take; another keeps the dialog open.
    let take = Rc::new({
        let input = input.clone();
        move |_: &mut Window, cx: &mut App| -> bool {
            let Some(size) = page_size(&input.read(cx).value()) else {
                return false;
            };
            view.update(cx, |this, cx| this.set_load_size(u64::from(size), cx));
            true
        }
    });
    let field = input.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let (field, take) = (field.clone(), take.clone());
        let on_enter = take.clone();
        let (low, high) = (low.clone(), high.clone());
        dialog
            .title("Fetch size")
            .w(px(360.))
            .on_ok(move |_, window, cx| on_enter(window, cx))
            .content(move |content, _, _| {
                let take = take.clone();
                content.child(
                    v_flex()
                        .gap_3()
                        .pt_1()
                        .child(div().text_sm().child(SharedString::from(format!(
                            "Rows each fetch reads in this tab, {low} to {high}."
                        ))))
                        .child(Input::new(&field).id("data-fetch-size-input").small())
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("data-fetch-size-cancel")
                                        .ghost()
                                        .label("Cancel")
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("data-fetch-size-set")
                                        .primary()
                                        .label("Set")
                                        .on_click(move |_, window, cx| {
                                            if take(window, cx) {
                                                window.close_dialog(cx);
                                            }
                                        }),
                                ),
                        ),
                )
            })
    });
    window.defer(cx, move |window, cx| {
        input.update(cx, |input, cx| input.focus(window, cx))
    });
}

/// The narrowest a WHERE field gets: below it, the buttons after it go to
/// another line (a narrow group of a split) instead of squeezing it to a few
/// letters.
pub const FILTER_MIN_WIDTH: Pixels = px(180.);

/// The filters applied before, to apply one again (DBeaver's history of
/// the filter panel).
pub fn history_button<T: 'static>(
    id: &'static str,
    history: &[String],
    view: Entity<T>,
    pick: fn(&mut T, String, &mut Window, &mut Context<T>),
) -> impl IntoElement {
    let history = history.to_vec();
    Button::new(id)
        .ghost()
        .small()
        .label("History")
        .tooltip("The filters applied here before")
        .disabled(history.is_empty())
        .dropdown_caret(true)
        .dropdown_menu(move |mut menu, _, _| {
            for filter in &history {
                let (view, filter) = (view.clone(), filter.clone());
                let label: String = one_line_filter(&filter);
                menu = menu.item(PopupMenuItem::new(SharedString::from(label)).on_click(
                    move |_, window, cx| {
                        view.update(cx, |this, cx| pick(this, filter.clone(), window, cx))
                    },
                ));
            }
            menu
        })
}

/// A filter on one line, cut short for a menu.
fn one_line_filter(filter: &str) -> String {
    let line = filter.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 80 {
        line.chars().take(79).collect::<String>() + "…"
    } else {
        line
    }
}

impl Focusable for DataTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DataTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let read_only = self.grid.as_ref().and_then(|_| self.read_only_reason(cx));
        let grid = self.grid.clone();
        let why = read_only.clone().map(SharedString::from);
        let lookup = value_panel::Lookup {
            connections: self.connections.clone(),
            target: self.target.clone(),
        };
        value_panel::sync(
            &mut self.value_panel,
            grid.as_ref(),
            why,
            Some(lookup),
            window,
            cx,
        );
        let theme = cx.theme();
        let (muted, danger, border) = (theme.muted_foreground, theme.danger, theme.border);
        v_flex()
            .id("data-tab")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            // With the grid focused: copy the selection, paste, select
            // everything, type in the selected cell, set it to NULL.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                let Some(table) = this.grid.clone() else {
                    return;
                };
                if !table.read(cx).focus_handle(cx).is_focused(window) {
                    return;
                }
                let secondary = keystroke.modifiers.secondary(cx) && !keystroke.modifiers.shift;
                match keystroke.key.as_str() {
                    "c" if secondary => {
                        if this.copy_selection(CopyFormat::Tsv, cx) {
                            cx.stop_propagation();
                        }
                    }
                    "a" if secondary => {
                        table.update(cx, |table, cx| {
                            table.delegate_mut().select_all();
                            cx.notify();
                        });
                        cx.stop_propagation();
                    }
                    _ => {
                        let edits = this.edits(cx);
                        if edit::on_key(&table, keystroke, edits, window, cx) {
                            this.apply_error = None;
                            cx.stop_propagation();
                        }
                    }
                }
            }))
            .child(self.render_toolbar(cx))
            .child(self.render_filter_bar(cx))
            .children(read_only.map(|reason| {
                div()
                    .id("data-read-only")
                    .test_support()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(muted)
                    .border_b_1()
                    .border_color(border)
                    .child(SharedString::from(reason))
            }))
            .children(self.apply_error.clone().map(|error| {
                div()
                    .id("data-apply-error")
                    .test_support()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_sm()
                    .text_color(danger)
                    .border_b_1()
                    .border_color(border)
                    .child(SharedString::from(error))
            }))
            .child(match self.chrome(cx) {
                // The environment's colour frames the grid.
                Some(chrome) => div()
                    .id("env-frame")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .border_2()
                    .border_color(chrome.frame)
                    .child(self.render_rows(cx))
                    .into_any_element(),
                None => div()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_rows(cx))
                    .into_any_element(),
            })
            .child(self.render_footer(cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::DbError;

    fn server(code: &str, message: &str) -> Error {
        Error::Db(Box::new(DbError {
            code: Some(code.into()),
            ..DbError::message(message)
        }))
    }

    #[test]
    fn a_missing_table_is_told_from_other_failures() {
        assert!(table_gone(&server(
            "42P01",
            "relation \"x\" does not exist"
        )));
        assert!(table_gone(&server(
            "1146 (42S02)",
            "Table 'kerno.x' doesn't exist"
        )));
        // A missing column (a bad filter) is not a missing table.
        assert!(!table_gone(&server("42703", "column \"y\" does not exist")));
        assert!(!table_gone(&server("1054 (42S22)", "Unknown column 'y'")));
        assert!(!table_gone(&Error::Closed("gone".into())));
    }

    #[test]
    fn the_servers_message_ends_with_a_full_stop() {
        assert_eq!(
            sentence("relation \"x\" does not exist [42P01]"),
            "relation \"x\" does not exist [42P01]."
        );
        assert_eq!(sentence("Already a sentence. "), "Already a sentence.");
    }
}
