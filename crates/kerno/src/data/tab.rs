//! A table's data: one page of its rows, filtered and sorted by the server.
//! Cells are edited in place; the changes wait, are reviewed as SQL, and
//! are applied in one transaction or discarded.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, div, px,
};
use kerno_core::browse::{Condition, Filter, Operator, Page, count_sql, page_sql};
use kerno_core::config::Environment;
use kerno_core::exec::CollectedResult;
use kerno_core::pending::Pending;
use kerno_core::schema::{ColumnGeneration, RelationKind, TableRef, TableStructure};
use kerno_core::sql::{CellValue, RowChange, apply_edits, edit_statements};
use kerno_core::{Error, ResultStore, Session, TxState, exec};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::query::grid::{CopyFormat, Grid, RowMenu, SortOnServer};
use crate::query::plan_view::number;
use crate::query::tab::{TabStatus, duration_text};
use crate::query::viewer;
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::state::AppState;
use crate::tabs::{TabSnapshot, Target};

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
    Failed(String),
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
    /// Rows the filter keeps in the whole table, once counted.
    total: Option<u64>,
    counting: bool,
    elapsed: Option<Duration>,
    state: State,
    /// The load in flight; the next one waits for it.
    load: Option<Task<()>>,
    generation: u64,
    applying: bool,
    /// Why the last apply changed nothing.
    apply_error: Option<String>,
    grid_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

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
                        this.session = None;
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
            elapsed: None,
            state: State::Waiting,
            load: None,
            generation: 0,
            applying: false,
            apply_error: None,
            grid_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        for condition in filter.conditions {
            tab.push_filter_row(condition, window, cx);
        }
        tab.reload(window, cx);
        tab
    }

    pub fn title(&self) -> SharedString {
        SharedString::from(self.table.name.clone())
    }

    pub fn table(&self) -> &TableRef {
        &self.table
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
            rows: self
                .grid
                .as_ref()
                .map(|grid| grid.read(cx).delegate().total_rows()),
            elapsed: self.elapsed,
            notices: 0,
            pending: self.pending_count(cx),
        }
    }

    /// Why the rows cannot be edited, when they cannot.
    pub fn read_only_reason(&self, cx: &App) -> Option<String> {
        let read_only = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .is_some_and(|saved| saved.config.read_only);
        if read_only {
            return Some(format!(
                "“{}” is a read-only connection.",
                self.connection_name(cx)
            ));
        }
        if !self.kind.is_table() {
            return Some("The rows of a view are read-only here.".into());
        }
        let structure = self.structure.as_ref()?;
        structure.row_identity().is_none().then(|| {
            "Read-only: this table has no primary key and no unique index on NOT NULL columns, \
             so a row cannot be told from another."
                .into()
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

    /// Reads the page again, with the filter and sort as they are.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        if !self.connected(cx) {
            self.session = None;
            self.state = State::Waiting;
            cx.notify();
            return;
        }
        self.state = State::Loading;
        self.apply_error = None;
        let previous = self.load.take();
        let opening = self.session(cx);
        let table = self.table.clone();
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
                        let identity = structure.row_identity().unwrap_or_default();
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
                        Ok::<_, Error>((structure, result, counting))
                    })
                })
            };
            let Ok(reading) = reading else {
                return;
            };
            let (structure, result, counting) = match reading.await {
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
                    true
                }
            });
            if !matches!(shown, Ok(true)) {
                return;
            }
            // The page shows at once; how many rows there are in all follows.
            let Ok(counted) =
                cx.update(|_, cx| bridge::spawn(cx, exec::collect(session, counting)))
            else {
                return;
            };
            let total = counted.await.ok().and_then(|collected| {
                let result = collected.results.first()?;
                result.batches.first()?.text(0, 0)?.parse::<u64>().ok()
            });
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
        if error.is_disconnect() {
            self.session = None;
        }
        self.counting = false;
        self.state = State::Failed(error.to_string());
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
        // A computed column takes no value, nor does an identity the server
        // always generates.
        let writable = self.read_only_reason(cx).is_none().then(|| {
            store
                .columns()
                .iter()
                .map(|column| {
                    structure
                        .columns
                        .iter()
                        .find(|info| info.name == column.name)
                        .is_some_and(|info| {
                            !matches!(
                                info.generation,
                                Some(
                                    ColumnGeneration::Stored(_)
                                        | ColumnGeneration::Virtual(_)
                                        | ColumnGeneration::IdentityAlways
                                )
                            )
                        })
                })
                .collect()
        });
        let view = cx.weak_entity();
        let sort_on_server: Rc<SortOnServer> = Rc::new({
            let view = view.clone();
            move |sort, window, cx| {
                let _ = view.update(cx, |this, cx| this.sort_by(sort, window, cx));
            }
        });
        let row_menu: Rc<RowMenu> = Rc::new({
            let editable = writable.is_some();
            move |grid, row, menu, _, _| {
                related_rows(
                    &view,
                    &structure,
                    grid,
                    row,
                    row_actions(&view, grid, row, editable, menu),
                )
            }
        });
        let grid = Grid::page(
            store,
            session,
            self.table.clone(),
            sort,
            writable,
            sort_on_server,
            row_menu,
        );
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
                        this.edit_cell(*row, *column, window, cx);
                    }
                },
            ),
            // Pending changes show in the toolbar.
            cx.observe(&table, |_, _, cx| cx.notify()),
        ];
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
            Some(total) => shown < total,
            // Not counted yet: a full page may have a next one.
            None => self.fetched(cx) == self.page_size,
        }
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
        let Some(table) = self.grid.clone() else {
            return;
        };
        let editing = table.update(cx, |table, cx| {
            let grid = table.delegate_mut();
            grid.begin_edit(row, column, window, cx)
        });
        if !editing {
            let grid = table.read(cx).delegate();
            if let Some((column, value)) = grid.value_at(row, column) {
                let (column, value) = (column.clone(), value.map(<[u8]>::to_vec));
                viewer::open(&column, value.as_deref(), window, cx);
            }
        }
    }

    /// The rows and columns of the result that are selected.
    fn selection(&self, cx: &App) -> Option<(Vec<usize>, Vec<usize>)> {
        let table = self.grid.as_ref()?.read(cx);
        let selected = table.delegate().selected(table.selection())?;
        Some((selected.rows.collect(), selected.columns))
    }

    /// Changes the pending changes and redraws.
    fn change(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Pending, &ResultStore)) {
        if let Some(table) = &self.grid {
            table.update(cx, |table, cx| {
                table.delegate_mut().edit(change);
                table.refresh(cx);
                cx.notify();
            });
        }
        self.apply_error = None;
        cx.notify();
    }

    /// Whether the rows take changes; says why not when they do not.
    fn can_edit(&self, window: &mut Window, cx: &mut App) -> bool {
        if self.pending(cx).is_some() {
            return true;
        }
        if let Some(reason) = self.read_only_reason(cx) {
            window.push_notification(Notification::warning(SharedString::from(reason)), cx);
        }
        false
    }

    /// Adds an empty row at the end and shows it.
    pub fn add_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit(window, cx) {
            return;
        }
        let mut added = None;
        self.change(cx, |pending, store| added = Some(pending.add_row(store)));
        self.show_row(added, cx);
    }

    fn show_row(&mut self, row: Option<usize>, cx: &mut Context<Self>) {
        if let (Some(row), Some(table)) = (row, &self.grid) {
            table.update(cx, |table, cx| {
                table.set_selected_row(row, cx);
                table.scroll_to_row(row, cx);
            });
        }
    }

    /// Adds copies of the selected rows (or of `row`) at the end.
    pub fn duplicate_rows(
        &mut self,
        row: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_edit(window, cx) {
            return;
        }
        let Some(rows) = row
            .map(|row| vec![row])
            .or_else(|| Some(self.selection(cx)?.0))
        else {
            return;
        };
        // Keys the server generates are left to it: a copy would collide.
        let generated: Vec<usize> = match (&self.grid, &self.structure) {
            (Some(table), Some(structure)) => table
                .read(cx)
                .delegate()
                .store()
                .columns()
                .iter()
                .enumerate()
                .filter(|(_, column)| {
                    structure
                        .columns
                        .iter()
                        .any(|info| info.name == column.name && info.generation.is_some())
                })
                .map(|(index, _)| index)
                .collect(),
            _ => Vec::new(),
        };
        let mut last = None;
        self.change(cx, |pending, store| {
            for row in rows {
                last = pending.duplicate(store, row, &generated).or(last);
            }
        });
        self.show_row(last, cx);
    }

    /// Marks the selected rows (or `row`) for deletion.
    pub fn delete_rows(&mut self, row: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit(window, cx) {
            return;
        }
        let Some(rows) = row
            .map(|row| vec![row])
            .or_else(|| Some(self.selection(cx)?.0))
        else {
            return;
        };
        self.change(cx, |pending, store| pending.delete(store.row_count(), rows));
    }

    /// Takes a row back to what was fetched.
    pub fn revert_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.change(cx, |pending, _| pending.revert(row));
    }

    /// Gives every selected cell `value` (`NULL` or the column's default).
    pub fn set_selected(&mut self, value: CellValue, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_edit(window, cx) {
            return;
        }
        let Some((rows, columns)) = self.selection(cx) else {
            return;
        };
        let Some(table) = self.grid.clone() else {
            return;
        };
        // Cells that take no value are left alone.
        let cells: Vec<(usize, usize)> = {
            let table = table.read(cx);
            let grid = table.delegate();
            rows.iter()
                .flat_map(|&row| columns.iter().map(move |&column| (row, column)))
                .filter(|&(row, column)| grid.takes_value(row, column))
                .collect()
        };
        self.change(cx, |pending, store| {
            for (row, column) in cells {
                pending.set(store, row, column, value.clone());
            }
        });
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

    /// Forgets every pending change.
    pub fn discard(&mut self, cx: &mut Context<Self>) {
        self.change(cx, |pending, _| pending.clear());
    }

    /// The statements the pending changes become; for the review and tests.
    pub fn pending_sql(&self, cx: &App) -> kerno_core::Result<(Vec<RowChange>, Vec<String>)> {
        let (Some(table), Some(structure), Some(session)) =
            (&self.grid, &self.structure, &self.session)
        else {
            return Ok((Vec::new(), Vec::new()));
        };
        let grid = table.read(cx).delegate();
        let Some(pending) = grid.pending() else {
            return Ok((Vec::new(), Vec::new()));
        };
        let identity = structure.row_identity().unwrap_or_default();
        let changes = pending.changes(grid.store(), &identity)?;
        let statements =
            edit_statements(session.dialect(), &self.table, &structure.columns, &changes)?;
        Ok((changes, statements))
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
        let environment = saved.map_or(Environment::Local, |saved| saved.config.environment);
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

    /// The grid; for tests.
    #[cfg(test)]
    pub fn grid(&self) -> Option<Entity<TableState<Grid>>> {
        self.grid.clone()
    }

    /// Where the tab is with its rows, in words; for tests.
    #[cfg(test)]
    pub fn state(&self, cx: &App) -> String {
        match &self.state {
            State::Waiting => "waiting".into(),
            State::Loading => "loading".into(),
            State::Failed(error) => format!("failed: {error}"),
            State::Ready => self.range_text(cx),
        }
    }

    /// Copies the selection. Returns whether there was something to copy.
    fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        self.grid.as_ref().is_some_and(|table| {
            table.update(cx, |table, cx| {
                Grid::copy(table, Some(CopyFormat::Tsv), None, cx)
            })
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
            Some(total) if fetched > 0 => format!("{range} of {}", number(total as f64)),
            _ => range,
        }
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
        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
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
            .when(editable, |bar| {
                bar.child(
                    action("data-add-row", "Add row", "Add an empty row")
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
                        this.set_selected(CellValue::Default, window, cx)
                    })),
                )
            })
            .child(div().flex_1())
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
            (_, None) => String::new(),
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
                    .filter(|_| !loading)
                    .map(|elapsed| SharedString::from(duration_text(elapsed))),
            )
            .child(div().flex_1())
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
                    .on_click(cx.listener(|this, _, window, cx| this.turn_page(-1, window, cx))),
            )
            .child(
                Button::new("data-next")
                    .ghost()
                    .xsmall()
                    .label("Next")
                    .disabled(!self.has_next(cx) || loading)
                    .on_click(cx.listener(|this, _, window, cx| this.turn_page(1, window, cx))),
            )
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
        let theme = cx.theme();
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
            (State::Failed(error), _) => {
                let retry = Button::new("data-retry")
                    .outline()
                    .label("Try again")
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx)))
                    .into_any_element();
                self.render_notice(
                    "data-error",
                    "The rows could not be read",
                    format!("{error} Check the filter, then try again."),
                    Some(retry),
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
                div()
                    .size_full()
                    .relative()
                    .child(DataTable::new(table).xsmall())
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

/// The entries of a row's menu that change the row.
fn row_actions(
    view: &gpui_kit::WeakEntity<DataTab>,
    grid: &Grid,
    row: usize,
    editable: bool,
    menu: PopupMenu,
) -> PopupMenu {
    if !editable {
        return menu;
    }
    let item = |label: &'static str,
                act: fn(&mut DataTab, usize, &mut Window, &mut Context<DataTab>)| {
        let view = view.clone();
        PopupMenuItem::new(label).on_click(move |_, window, cx| {
            let _ = view.update(cx, |this, cx| act(this, row, window, cx));
        })
    };
    let fetched = grid.store().row_count();
    let pending = grid.pending();
    let deleted = pending.is_some_and(|pending| pending.is_deleted(row));
    let touched = row < fetched
        && (deleted
            || (0..grid.store().columns().len()).any(|column| {
                pending.is_some_and(|pending| pending.value(fetched, row, column).is_some())
            }));
    let mut menu = menu
        .item(item("Duplicate row", |this, row, window, cx| {
            this.duplicate_rows(Some(row), window, cx)
        }))
        .item(
            item("Delete row", |this, row, window, cx| {
                this.delete_rows(Some(row), window, cx)
            })
            .disabled(deleted),
        );
    if touched {
        menu = menu.item(item("Revert row", |this, row, _, cx| {
            this.revert_row(row, cx)
        }));
    }
    menu
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

/// The entries of a row's menu that follow its foreign keys.
fn related_rows(
    view: &gpui_kit::WeakEntity<DataTab>,
    structure: &TableStructure,
    grid: &Grid,
    row: usize,
    mut menu: PopupMenu,
) -> PopupMenu {
    let related = related(structure, grid, row);
    if !related.is_empty() {
        menu = menu.separator();
    }
    for related in related {
        let view = view.clone();
        menu = menu.item(
            PopupMenuItem::new(SharedString::from(related.label.clone())).on_click(
                move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.follow(&related, cx));
                },
            ),
        );
    }
    menu
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
            // With the grid focused: copy the selection, select everything,
            // type in the selected cell.
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
                        if this.copy_selection(cx) {
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
                    "enter" | "f2" => {
                        if let Some((row, column)) = table.read(cx).selected_cell() {
                            this.edit_cell(row, column, window, cx);
                            cx.stop_propagation();
                        }
                    }
                    _ => {}
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
            .child(div().flex_1().min_h_0().child(self.render_rows(cx)))
            .child(self.render_footer(cx))
    }
}
