//! A table's data: one page of its rows, filtered and sorted by the server.
//! Cells are edited in place; the changes wait, are reviewed as SQL, and
//! are applied in one transaction or discarded.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _,
    WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, div, px,
};
use kerno_core::browse::{
    Condition, Filter, Operator, Page, count_sql, distinct_sql, one_of_sql, page_sql,
};
use kerno_core::exec::CollectedResult;
use kerno_core::pending::Pending;
use kerno_core::schema::{RelationKind, TableRef, TableStructure};
use kerno_core::sql::{CellValue, RowChange, RowSql, apply_edits};
use kerno_core::storage::TableLayout;
use kerno_core::{Engine, Error, ResultStore, Session, TxState, exec};

use crate::actions::CloseTab;
use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::export_dialog::{self, ExportDialog, Source};
use crate::query::distinct;
use crate::query::edit;
use crate::query::grid::{CopyFormat, EditTarget, Grid, GridAction, Owner, SortOnServer};
use crate::query::plan_view::number;
use crate::query::tab::{TabStatus, duration_text};
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
}

/// How many rows the filter keeps in the whole table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Total {
    Exact(u64),
    /// The planner's estimate of an unfiltered table too large to count
    /// after each page; "Count exactly" counts it.
    About(u64),
}

/// An unfiltered table whose estimate is above this is not counted after
/// each page: the estimate is shown instead (M6 Part 2).
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

/// What the error state says: the server's message, then what to do.
fn failure_text(message: &str, gone: bool) -> String {
    let advice = if gone {
        "The table may have been dropped or renamed."
    } else {
        "Check the filter, then try again."
    };
    format!("{} {advice}", sentence(message))
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

/// What a load brought back.
struct Loaded {
    session: Arc<dyn Session>,
    structure: Arc<TableStructure>,
    result: CollectedResult,
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
    raw_filter: Entity<InputState>,
    /// The filter the rows on screen were read with.
    applied: Filter,
    /// The column sorted on and whether descending.
    sort: Option<(String, bool)>,
    /// Index of the page on screen.
    page: u64,
    page_size: u64,
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
            InputState::new(window, cx)
                .placeholder("SQL condition, for example: total > 100 AND status = 'paid'")
                .default_value(filter.raw.clone())
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
            page: 0,
            page_size: u64::from(AppState::get(cx).settings.page_size.max(1)),
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
            refocus_grid: false,
            apply_error: None,
            grid_subscriptions: Vec::new(),
            layout: None,
            layout_seen: 0,
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

    /// What the layout of this table's columns is stored under: the
    /// database, the schema and the table (a MySQL table's schema is its
    /// database), with the connection apart.
    fn layout_key(&self, cx: &App) -> String {
        let saved = self.connections.read(cx).get(&self.target.connection);
        let database = match saved.map(|saved| saved.config.engine) {
            Some(Engine::MySql | Engine::MariaDb) => String::new(),
            _ => self
                .target
                .database
                .clone()
                .or_else(|| saved.and_then(|saved| saved.config.database.clone()))
                .unwrap_or_default(),
        };
        serde_json::to_string(&[database.as_str(), &self.table.schema, &self.table.name])
            .unwrap_or_default()
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
        self.page = 0;
        self.reload(window, cx);
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
            running: self.state == State::Loading || self.applying,
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
        matches!(self.state, State::Failed { .. })
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
        if let Some(session) = self.session.take() {
            bridge::spawn(cx, async move { session.close().await }).detach();
        }
    }

    /// Reads nothing until shown (a restored tab): see `reveal`.
    pub fn defer_until_shown(&mut self, cx: &mut Context<Self>) {
        self.hidden = true;
        self.generation += 1;
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

    /// Reads the page again, with the filter and sort as they are.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
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
        let previous = self.load.take();
        let opening = self.session(cx);
        let table = self.table.clone();
        let estimate_above = self.estimate_above;
        let known = self.structure.clone();
        let filter = self.applied.clone();
        let sort = self.sort.clone();
        let (limit, offset) = (self.page_size, self.page * self.page_size);
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
                        // the page instead of a count after it.
                        let about = if filter.is_empty() {
                            let estimate = session.dialect().estimated_rows_sql(&table);
                            first_count(exec::collect(session.clone(), estimate).await)
                                .filter(|about| *about > estimate_above)
                        } else {
                            None
                        };
                        let identity = structure.row_identity().unwrap_or_default();
                        // A remembered order on a column gone since is dropped.
                        let sort = sort.filter(|(name, _)| {
                            structure.columns.iter().any(|column| column.name == *name)
                        });
                        let sql = page_sql(
                            session.dialect(),
                            &structure.columns,
                            Page {
                                table: &table,
                                filter: &filter,
                                sort: sort.as_ref().map(|(column, down)| (column.as_str(), *down)),
                                identity: &identity,
                                limit,
                                offset,
                            },
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
            // The page shows at once; how many rows there are in all follows.
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
        let gone = table_gone(error);
        self.state = State::Failed {
            message: error.to_string(),
            gone,
        };
        if gone {
            cx.emit(DataTabEvent::TableGone {
                table: self.table.clone(),
            });
        }
        cx.notify();
    }

    /// Puts a fetched page on screen.
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
        } = loaded;
        self.session = Some(session.clone());
        self.structure = Some(structure.clone());
        self.elapsed = Some(elapsed);
        self.total = None;
        self.counting = true;
        self.state = State::Ready;
        self.remember_filter(&session, cx);

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
        let mut grid = Grid::page(
            store,
            session,
            self.table.clone(),
            sort,
            target,
            sort_on_server,
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
        // The find bar stays open, with its text, over the new page.
        let find = self
            .grid
            .as_ref()
            .and_then(|old| crate::query::find::carried(old, cx));
        let table = cx.new(|cx| {
            TableState::new(grid, window, cx)
                .col_resizable(true)
                .col_movable(true)
                .sortable(true)
                .row_selectable(true)
                .col_selectable(true)
                .cell_selectable(true)
                .row_header(false)
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
        self.page = 0;
        self.reload(window, cx);
    }

    /// The column sorted on and whether descending; for tests.
    #[cfg(test)]
    pub fn sort(&self) -> Option<(&str, bool)> {
        self.sort
            .as_ref()
            .map(|(column, descending)| (column.as_str(), *descending))
    }

    /// Shows another page: the next (`1`) or the previous (`-1`) one.
    pub fn turn_page(&mut self, by: i64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.free_to_move(window, cx) {
            return;
        }
        let Some(page) = self.page.checked_add_signed(by) else {
            return;
        };
        if by > 0 && !self.has_next(cx) {
            return;
        }
        self.page = page;
        self.reload(window, cx);
    }

    fn fetched(&self, cx: &App) -> u64 {
        self.grid.as_ref().map_or(0, |grid| {
            grid.read(cx).delegate().store().row_count() as u64
        })
    }

    fn has_next(&self, cx: &App) -> bool {
        let shown = self.page * self.page_size + self.fetched(cx);
        match self.total {
            Some(Total::Exact(total)) => shown < total,
            // Not counted: a full page may have a next one.
            Some(Total::About(_)) | None => self.fetched(cx) == self.page_size,
        }
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

    /// Reads the page again every `every`, or stops (`None`).
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
        if !(self.state == State::Loading || self.counting || self.applying) {
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
        if let Some(session) = self.session.clone()
            && (self.state == State::Loading || self.counting)
        {
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

    /// Reads the rows the filter bar asks for, from the first page.
    pub fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.free_to_move(window, cx) {
            return;
        }
        self.applied = self.typed_filter(cx);
        self.page = 0;
        self.reload(window, cx);
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
        let previous = self.load.take();
        let count = self.pending_count(cx);
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let Ok(applying) = cx.update(|_, cx| {
                bridge::spawn(cx, async move { apply_edits(&session, &statements).await })
            }) else {
                return;
            };
            let outcome = applying.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.applying = false;
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
                    Err(failure) => {
                        this.apply_error = Some(format!(
                            "Nothing was changed: {failure}. The pending changes are kept."
                        ));
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
    pub fn grid(&self) -> Option<Entity<TableState<Grid>>> {
        self.grid.clone()
    }

    /// What the error state says, when the rows could not be read; for
    /// tests.
    #[cfg(test)]
    pub fn advice(&self) -> String {
        match &self.state {
            State::Failed { message, gone } => failure_text(message, *gone),
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

    /// "Rows 1–1,000 of 12,345".
    fn range_text(&self, cx: &App) -> String {
        let fetched = self.fetched(cx);
        let offset = self.page * self.page_size;
        let range = if fetched == 0 {
            "No rows".to_string()
        } else {
            format!(
                "Rows {}–{}",
                number((offset + 1) as f64),
                number((offset + fetched) as f64)
            )
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
                        div().flex_1().min_w_0().child(
                            Input::new(&self.raw_filter)
                                .small()
                                .id("filter-raw")
                                .font_family(theme.mono_font_family.clone()),
                        ),
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
            (State::Failed { .. }, _) | (_, None) => String::new(),
            _ => self.range_text(cx),
        };
        h_flex()
            .id("data-footer")
            .test_support()
            .flex_none()
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
            .when(self.applying, |bar| bar.child("applying…"))
            .when(loading || self.counting, |bar| {
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
            .children(
                self.grid
                    .as_ref()
                    .map(|table| crate::query::grid_view::SelectionStats::of(table, cx)),
            )
            .child(div().flex_1())
            // No pages to turn when the rows could not be read.
            .when(!self.read_failed(), |footer| {
                footer
                    .child(SharedString::from(format!(
                        "Page {}",
                        number((self.page + 1) as f64)
                    )))
                    .child(
                        Button::new("data-previous")
                            .ghost()
                            .xsmall()
                            .label("Previous")
                            .disabled(self.page == 0 || loading)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.turn_page(-1, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("data-next")
                            .ghost()
                            .xsmall()
                            .label("Next")
                            .disabled(!self.has_next(cx) || loading)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.turn_page(1, window, cx)),
                            ),
                    )
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
                div()
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
                    .into_any_element()
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
    // The values of `columns` in this row, when none is NULL.
    let values = |columns: &[String]| -> Option<Vec<String>> {
        columns
            .iter()
            .map(|column| grid.text_of(row, column))
            .collect()
    };
    let filter = |on: &[String], values: Vec<String>| Filter {
        conditions: on
            .iter()
            .zip(values)
            .map(|(column, value)| Condition::equals(column.clone(), value))
            .collect(),
        raw: String::new(),
    };
    let references = structure.foreign_keys.iter().filter_map(|key| {
        let values = values(&key.columns)?;
        Some(Related {
            label: format!(
                "Go to {} ({} = {})",
                key.referenced_table.name,
                key.columns.join(", "),
                values.join(", ")
            ),
            table: key.referenced_table.clone(),
            filter: filter(&key.referenced_columns, values),
        })
    });
    let referenced_by = structure.referenced_by.iter().filter_map(|key| {
        let values = values(&key.referenced_columns)?;
        Some(Related {
            label: format!(
                "Rows of {} referencing this one ({})",
                key.table.name,
                key.columns.join(", ")
            ),
            table: key.table.clone(),
            filter: filter(&key.columns, values),
        })
    });
    references.chain(referenced_by).collect()
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger, border) = (theme.muted_foreground, theme.danger, theme.border);
        let read_only = self.grid.as_ref().and_then(|_| self.read_only_reason(cx));
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
