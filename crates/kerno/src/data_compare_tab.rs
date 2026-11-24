//! The data compare (K60, D-126): a tab that compares the rows of a source
//! table (the model) with those of a target table (what the script
//! changes), by a key, and shows the counts and the rows that differ, one
//! per key, each changed cell with its source and target values; the row's
//! full values beside them; and opens the script that makes the target like
//! the source in a query tab on the target. Nothing runs from here: the
//! script is read and run there, under the connection's rules. The merge is
//! `kerno_core::data_compare`; this tab is the schema compare's sibling
//! (`compare_tab.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::base::{FocusableExt as _, TestTextExt as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, WeakEntity, Window, div, px,
};
use kerno_core::compare::commented;
use kerno_core::data_compare::{self, Counts, Difference, Limits, Outcome, Plan, Request};
use kerno_core::export::{DelimitedOptions, ExportFormat, export_to_string};
use kerno_core::{Error, Session, TxState, TypeKind, describe};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::query::grid::CopyFormat;
use crate::query::plan_view::number;
use crate::query::tab::TabStatus;
use crate::tabs::Target;

/// What a data compare compares: the places of both tables and the request
/// (the tables, their key, the columns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// The model.
    pub source: Target,
    /// What the script changes.
    pub target: Target,
    pub request: Request,
}

/// What the tab asks of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataCompareTabEvent {
    /// Open `sql` in a new query tab on `target`; nothing runs.
    OpenScript {
        target: Target,
        title: String,
        sql: String,
    },
}

/// Which differences the grid shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Differences,
    OnlyInSource,
    OnlyInTarget,
    Different,
}

impl Filter {
    pub const ALL: [Filter; 4] = [
        Filter::Differences,
        Filter::OnlyInSource,
        Filter::OnlyInTarget,
        Filter::Different,
    ];

    fn label(self) -> &'static str {
        match self {
            Filter::Differences => "All differences",
            Filter::OnlyInSource => "Only in source",
            Filter::OnlyInTarget => "Only in target",
            Filter::Different => "Different",
        }
    }

    fn lets(self, difference: Difference) -> bool {
        match self {
            Filter::Differences => true,
            Filter::OnlyInSource => difference == Difference::OnlyInSource,
            Filter::OnlyInTarget => difference == Difference::OnlyInTarget,
            Filter::Different => difference == Difference::Different,
        }
    }

    /// How many differences of this kind were found, kept or not.
    fn count(self, counts: &Counts) -> u64 {
        match self {
            Filter::Differences => counts.differences(),
            Filter::OnlyInSource => counts.only_in_source,
            Filter::OnlyInTarget => counts.only_in_target,
            Filter::Different => counts.different,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// A connection is not open, or the tab is not shown yet.
    Waiting,
    Loading,
    Ready,
    Failed(String),
    Cancelled(String),
}

/// What a finished compare found.
#[derive(Clone)]
struct Found {
    plan: Arc<Plan>,
    outcome: Arc<Outcome>,
    /// The sessions the sides were read on, closed: their dialects write
    /// the script and "Copy as SQL INSERT".
    sessions: [Arc<dyn Session>; 2],
}

/// A cell as one line of text: NULL said, bytes in hex, long text cut.
fn cell_text(cell: Option<&[u8]>, kind: TypeKind) -> String {
    const SHOWN: usize = 200;
    match cell {
        None => "NULL".into(),
        Some(bytes) if kind == TypeKind::Binary => {
            let hex = kerno_core::dialect::hex(&bytes[..bytes.len().min(SHOWN / 2)]);
            let more = if bytes.len() > SHOWN / 2 { "…" } else { "" };
            format!("\\x{hex}{more}")
        }
        Some(bytes) => {
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(SHOWN * 4)]);
            let mut line: String = text
                .chars()
                .take(SHOWN)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            if text.chars().count() > SHOWN || bytes.len() > SHOWN * 4 {
                line.push('…');
            }
            line
        }
    }
}

/// A cell whole, for the values panel: bytes in hex, a very long value cut
/// with what is left said.
fn full_text(cell: Option<&[u8]>, kind: TypeKind) -> String {
    const SHOWN: usize = 64 * 1024;
    match cell {
        None => "NULL".into(),
        Some(bytes) if bytes.len() > SHOWN => format!(
            "{}… ({} more bytes)",
            full_text(Some(&bytes[..SHOWN]), kind).trim_end_matches('…'),
            number((bytes.len() - SHOWN) as f64)
        ),
        Some(bytes) if kind == TypeKind::Binary => {
            format!("\\x{}", kerno_core::dialect::hex(bytes))
        }
        Some(bytes) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The differences on screen, as the kit's table draws them: a mark, then
/// the key's cells and the compared columns'.
pub struct DiffGrid {
    tab: WeakEntity<DataCompareTab>,
    found: Found,
    /// Indexes into the outcome's rows.
    shown: Arc<[usize]>,
}

impl DiffGrid {
    fn row(&self, row: usize) -> Option<&data_compare::Row> {
        self.found.outcome.rows.get(*self.shown.get(row)?)
    }

    /// The cell of side `side` (0: source) at `column` of the plan.
    fn cell(&self, row: &data_compare::Row, side: usize, column: usize) -> Option<Option<&[u8]>> {
        let at = if side == 0 { row.source } else { row.target }?;
        Some(self.found.outcome.kept[side].cell(at, column))
    }

    fn kind(&self, side: usize, column: usize) -> TypeKind {
        self.found.outcome.kept[side].columns()[column].kind
    }

    /// The text of a cell as the grid shows it: a changed one as
    /// `source → target`.
    fn text(&self, row: usize, column: usize) -> String {
        let Some(row) = self.row(row) else {
            return String::new();
        };
        if column == 0 {
            return row.difference.mark().1.to_string();
        }
        let at = column - 1;
        let side = usize::from(row.source.is_none());
        let shown = |side| cell_text(self.cell(row, side, at).flatten(), self.kind(side, at));
        if self.changed(row, at) {
            format!("{} → {}", shown(0), shown(1))
        } else {
            shown(side)
        }
    }

    fn changed(&self, row: &data_compare::Row, column: usize) -> bool {
        let key = self.found.plan.key.len();
        column >= key && row.changed.contains(&((column - key) as u32))
    }
}

impl TableDelegate for DiffGrid {
    fn columns_count(&self, _: &App) -> usize {
        self.found.plan.width() + 1
    }

    fn rows_count(&self, _: &App) -> usize {
        self.shown.len()
    }

    fn column(&self, index: usize, _: &App) -> Column {
        if index == 0 {
            return Column::new("mark", "")
                .width(px(110.))
                .resizable(true)
                .movable(false);
        }
        let plan = &self.found.plan;
        let col = plan.cols().nth(index - 1).expect("a column");
        let name = if index <= plan.key.len() {
            format!("{} (key)", col.name)
        } else {
            col.name.clone()
        };
        Column::new(format!("column-{index}"), name)
            .width(px(150.))
            .min_width(px(40.))
            .resizable(true)
            .movable(false)
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let cell = h_flex()
            .id(("data-compare-cell", (row << 12) | column))
            .test_support()
            .test_text(self.text(row, column))
            .w_full()
            .gap_1()
            .overflow_hidden();
        let Some(diff) = self.row(row) else {
            return cell;
        };
        let tone = match diff.difference {
            Difference::OnlyInSource => theme.success,
            Difference::OnlyInTarget => theme.danger,
            Difference::Different => theme.warning,
        };
        if column == 0 {
            let (mark, words) = diff.difference.mark();
            return cell
                .child(
                    div()
                        .flex_none()
                        .w(px(12.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(tone)
                        .child(mark),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(words),
                );
        }
        let at = column - 1;
        let value = |side: usize| {
            let raw = self.cell(diff, side, at).flatten();
            let text = cell_text(raw, self.kind(side, at));
            div()
                .min_w_0()
                .truncate()
                .when(raw.is_none(), |text| text.italic())
                .child(SharedString::from(text))
        };
        if self.changed(diff, at) {
            return cell
                .child(value(0))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.muted_foreground)
                        .child("→"),
                )
                .child(value(1).text_color(tone));
        }
        let side = usize::from(diff.source.is_none());
        let plain = diff.difference == Difference::Different && at >= self.found.plan.key.len();
        cell.child(value(side).map(|text| {
            if plain {
                text.text_color(theme.muted_foreground)
            } else if diff.difference == Difference::Different {
                text
            } else {
                text.text_color(tone)
            }
        }))
    }

    fn cell_label(&self, row: usize, column: usize, _: &App) -> Option<SharedString> {
        Some(SharedString::from(self.text(row, column)))
    }

    fn cell_text(&self, row: usize, column: usize, _: &App) -> String {
        self.text(row, column)
    }

    fn context_menu(
        &mut self,
        row: usize,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(index) = self.shown.get(row).copied() else {
            return menu;
        };
        let tab = self.tab.clone();
        let diff = &self.found.outcome.rows[index];
        let item = |label: &'static str, side: usize, format: Option<CopyFormat>, there: bool| {
            let tab = tab.clone();
            PopupMenuItem::new(label)
                .disabled(!there)
                .on_click(move |_, _, cx| {
                    let _ = tab.update(cx, |tab, cx| tab.copy_row(index, side, format, cx));
                })
        };
        let side = usize::from(diff.source.is_none());
        let mut menu = menu
            .item(item("Copy key", side, None, true))
            .item(item(
                "Copy source row",
                0,
                Some(CopyFormat::Tsv),
                diff.source.is_some(),
            ))
            .item(item(
                "Copy target row",
                1,
                Some(CopyFormat::Tsv),
                diff.target.is_some(),
            ))
            .separator();
        for (label, format) in [
            ("Copy as TSV with column names", CopyFormat::TsvWithHeader),
            ("Copy as CSV", CopyFormat::Csv),
            ("Copy as JSON", CopyFormat::Json),
            ("Copy as Markdown", CopyFormat::Markdown),
            ("Copy as SQL INSERT", CopyFormat::SqlInsert),
        ] {
            menu = menu.item(item(label, side, Some(format), true));
        }
        menu
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .id("data-compare-no-match")
            .test_support()
            .size_full()
            .flex()
            .justify_center()
            .pt_6()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .test_text("No row differs this way.")
            .child("No row differs this way.")
    }
}

/// The compare of two tables' rows.
pub struct DataCompareTab {
    focus_handle: FocusHandle,
    connections: Entity<Connections>,
    spec: Spec,
    hidden: bool,
    state: State,
    counts: Counts,
    started: Option<Instant>,
    elapsed: Duration,
    found: Option<Found>,
    filter: Filter,
    /// Only the rows where this compared column differs.
    column: Option<usize>,
    grid: Option<Entity<TableState<DiffGrid>>>,
    /// The selected difference, an index into the outcome's rows.
    selected: Option<usize>,
    /// Write the deletes of what only the target has.
    deletes: bool,
    stop: bridge::Stop,
    cancel: Arc<AtomicBool>,
    generation: u64,
    load: Vec<Task<()>>,
    grid_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DataCompareTabEvent> for DataCompareTab {}

/// `schema.table`, as a side's table is named in the tab.
fn table_name(table: &kerno_core::schema::TableRef) -> String {
    format!("{}.{}", table.schema, table.name)
}

impl DataCompareTab {
    pub fn new(
        spec: Spec,
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let follow = cx.observe_in(&connections, window, |this, _, window, cx| {
            match (&this.state, this.connected(cx)) {
                (State::Waiting, true) => this.reload(window, cx),
                (State::Waiting, false) | (_, true) => {}
                (_, false) => {
                    this.stop_loading(cx);
                    this.state = State::Waiting;
                    cx.notify();
                }
            }
        });
        let mut tab = Self {
            focus_handle: cx.focus_handle(),
            connections,
            spec,
            hidden: false,
            state: State::Waiting,
            counts: Counts::default(),
            started: None,
            elapsed: Duration::ZERO,
            found: None,
            filter: Filter::Differences,
            column: None,
            grid: None,
            selected: None,
            deletes: false,
            stop: bridge::Stop::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            generation: 0,
            load: Vec::new(),
            grid_subscription: None,
            _subscriptions: vec![follow],
        };
        tab.reload(window, cx);
        tab
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    /// The target: what the tab acts on (its environment, its status).
    pub fn target(&self) -> &Target {
        &self.spec.target
    }

    pub fn title(&self) -> SharedString {
        let request = &self.spec.request;
        SharedString::from(format!(
            "{} ⇄ {} · data",
            request.source.name, request.target.name
        ))
    }

    fn side(&self, index: usize) -> &Target {
        if index == 0 {
            &self.spec.source
        } else {
            &self.spec.target
        }
    }

    fn connected(&self, cx: &App) -> bool {
        [&self.spec.source, &self.spec.target].iter().all(|side| {
            matches!(
                self.connections.read(cx).status(&side.connection),
                Status::Connected { .. }
            )
        })
    }

    fn connection_name(&self, side: &Target, cx: &App) -> String {
        self.connections
            .read(cx)
            .get(&side.connection)
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default()
    }

    /// `connection · database.schema.table` (or `connection · database.table`).
    pub fn side_label(&self, index: usize, cx: &App) -> String {
        let side = self.side(index);
        let table = if index == 0 {
            &self.spec.request.source
        } else {
            &self.spec.request.target
        };
        let place = match (&side.database, &side.schema) {
            (Some(database), Some(_)) => format!("{database}.{}", table_name(table)),
            _ => table_name(table),
        };
        format!("{} · {place}", self.connection_name(side, cx))
    }

    fn read_only(&self, cx: &App) -> bool {
        self.connections
            .read(cx)
            .get(&self.spec.target.connection)
            .is_some_and(|saved| saved.config.read_only)
    }

    pub fn status(&self, cx: &App) -> TabStatus {
        TabStatus {
            connection: Some(self.connection_name(&self.spec.target, cx))
                .filter(|name| !name.is_empty()),
            read_only: self.read_only(cx),
            database: self.spec.target.database.clone(),
            transaction: TxState::Idle,
            running: self.state == State::Loading,
            rows: None,
            elapsed: None,
            notices: 0,
            pending: 0,
        }
    }

    /// `waiting`, `loading`, `ready`, `cancelled` or the error; for tests.
    #[cfg(test)]
    pub fn state_text(&self) -> String {
        match &self.state {
            State::Waiting => "waiting".into(),
            State::Loading => "loading".into(),
            State::Ready => "ready".into(),
            State::Cancelled(_) => "cancelled".into(),
            State::Failed(message) => message.clone(),
        }
    }

    #[cfg(test)]
    pub fn counts(&self) -> Counts {
        self.counts
    }

    #[cfg(test)]
    pub fn outcome(&self) -> Option<Arc<Outcome>> {
        self.found.as_ref().map(|found| found.outcome.clone())
    }

    #[cfg(test)]
    pub fn grid(&self) -> Option<Entity<TableState<DiffGrid>>> {
        self.grid.clone()
    }

    // Reading -----------------------------------------------------------------

    pub fn defer_until_shown(&mut self, cx: &mut Context<Self>) {
        self.hidden = true;
        self.stop_loading(cx);
        self.state = State::Waiting;
        cx.notify();
    }

    pub fn reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.hidden) {
            self.reload(window, cx);
        }
    }

    fn stop_loading(&mut self, cx: &App) {
        self.generation += 1;
        self.cancel.store(true, Ordering::Release);
        self.stop.stop(cx);
        self.stop = bridge::Stop::default();
        self.load.clear();
    }

    /// Stops both reads (Cmd+., Cancel, the tab closed), on the servers too.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.state == State::Loading {
            self.stop_loading(cx);
            self.tick();
            let [source, target] = self.counts.read;
            self.state = State::Cancelled(format!(
                "Cancelled after {:.1} s: {} rows of the source and {} of the target were read, \
                 and nothing is shown. Refresh to compare again.",
                self.elapsed.as_secs_f64(),
                number(source as f64),
                number(target as f64)
            ));
            cx.notify();
        }
    }

    fn tick(&mut self) {
        if let Some(started) = self.started {
            self.elapsed = started.elapsed();
        }
    }

    /// Reads both tables again, at the same time, each on a session of its
    /// own, and merges them as they come.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_loading(cx);
        let generation = self.generation;
        if self.hidden || !self.connected(cx) {
            self.state = State::Waiting;
            cx.notify();
            return;
        }
        self.state = State::Loading;
        self.counts = Counts::default();
        self.started = Some(Instant::now());
        self.elapsed = Duration::ZERO;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let openings = [0, 1].map(|index| {
            let side = self.side(index).clone();
            self.connections
                .read(cx)
                .open_session(&side.connection, side.database.clone(), cx)
        });
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        self.load.push(cx.spawn(async move |this, cx| {
            while let Some(counts) = receiver.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.counts = counts;
                        this.tick();
                        cx.notify();
                    }
                });
            }
        }));
        // The time goes on while a server sorts before its first row.
        self.load.push(cx.spawn(async move |this, cx| {
            loop {
                let tick = async { tokio::time::sleep(Duration::from_millis(250)).await };
                cx.update(|cx| bridge::spawn(cx, tick)).await;
                let going = this.update(cx, |this, cx| {
                    let going = this.generation == generation && this.state == State::Loading;
                    if going {
                        this.tick();
                        cx.notify();
                    }
                    going
                });
                if !matches!(going, Ok(true)) {
                    return;
                }
            }
        }));
        let request = self.spec.request.clone();
        let stop = self.stop.clone();
        let [source, target] = openings;
        self.load.push(cx.spawn_in(window, async move |this, cx| {
            let opened = async { Ok::<_, Error>([source.await?, target.await?]) };
            let sessions = match opened.await {
                Ok(sessions) => sessions,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.failed(generation, "A connection could not be opened", error, cx)
                    });
                    return;
                }
            };
            let reading = cx.update(|_, cx| {
                // Both sessions are the stop's: Cancel closes them, which
                // stops both statements on their servers.
                stop.run(cx, sessions[1].clone(), |_| async {}).detach();
                let other = sessions[1].clone();
                stop.run(cx, sessions[0].clone(), move |source| async move {
                    let sessions = [source, other];
                    let structures = (
                        sessions[0].table_structure(&request.source).await,
                        sessions[1].table_structure(&request.target).await,
                    );
                    let (a, b) = match structures {
                        (Ok(a), Ok(b)) => (a, b),
                        (Err(error), _) | (_, Err(error)) => return Err(error),
                    };
                    let plan = data_compare::Plan::new(&request, &a, &b)
                        .map_err(|why| Error::Db(Box::new(kerno_core::DbError::message(why))))?;
                    let tell = move |counts| {
                        let _ = sender.send(counts);
                    };
                    let outcome = data_compare::run(
                        sessions.clone(),
                        &plan,
                        Limits::default(),
                        tell,
                        &cancel,
                    )
                    .await;
                    for session in &sessions {
                        session.close().await;
                    }
                    Ok((plan, outcome?, sessions))
                })
            });
            let Ok(reading) = reading else {
                return;
            };
            let read = reading.await.unwrap_or(Err(Error::Cancelled));
            let _ = this.update_in(cx, |this, window, cx| match read {
                Ok((plan, outcome, sessions)) if this.generation == generation => {
                    this.show(plan, outcome, sessions, window, cx)
                }
                Ok(_) => {}
                Err(error) => this.failed(generation, "The rows could not be compared", error, cx),
            });
        }));
        cx.notify();
    }

    fn failed(&mut self, generation: u64, what: &str, error: Error, cx: &mut Context<Self>) {
        if self.generation != generation || matches!(error, Error::Cancelled) {
            return;
        }
        self.stop_loading(cx);
        self.state = State::Failed(format!(
            "{what}. Refresh to try again.\n{}",
            describe(&error)
        ));
        cx.notify();
    }

    fn show(
        &mut self,
        plan: Plan,
        outcome: Outcome,
        sessions: [Arc<dyn Session>; 2],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tick();
        self.counts = outcome.counts;
        self.load.clear();
        self.state = State::Ready;
        self.found = Some(Found {
            plan: Arc::new(plan),
            outcome: Arc::new(outcome),
            sessions,
        });
        self.column = None;
        self.selected = None;
        self.refresh_grid(window, cx);
        cx.notify();
    }

    /// Exchanges the source and the target and compares again.
    pub fn swap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let spec = &mut self.spec;
        std::mem::swap(&mut spec.source, &mut spec.target);
        std::mem::swap(&mut spec.request.source, &mut spec.request.target);
        self.found = None;
        self.grid = None;
        self.reload(window, cx);
    }

    // The grid ----------------------------------------------------------------

    /// The differences the filters let through, as indexes into the
    /// outcome's rows.
    pub fn shown(&self) -> Vec<usize> {
        let Some(found) = &self.found else {
            return Vec::new();
        };
        found
            .outcome
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| self.filter.lets(row.difference))
            .filter(|(_, row)| {
                self.column
                    .is_none_or(|column| row.changed.contains(&(column as u32)))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn refresh_grid(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(found) = self.found.clone() else {
            self.grid = None;
            return;
        };
        let shown: Arc<[usize]> = self.shown().into();
        if let Some(grid) = &self.grid {
            grid.update(cx, |grid, cx| {
                grid.delegate_mut().shown = shown;
                grid.clear_selection(cx);
                grid.refresh(cx);
            });
            return;
        }
        let delegate = DiffGrid {
            tab: cx.entity().downgrade(),
            found,
            shown,
        };
        let grid = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .col_resizable(true)
                .row_selectable(true)
                .cell_selectable(false)
                .row_header(false)
        });
        self.grid_subscription = Some(cx.subscribe(&grid, |this, grid, event: &TableEvent, cx| {
            if let TableEvent::SelectRow(row) | TableEvent::SelectCell(row, _) = event {
                let index = grid.read(cx).delegate().shown.get(*row).copied();
                this.selected = index;
                cx.notify();
            }
        }));
        self.grid = Some(grid);
    }

    pub fn set_filter(&mut self, filter: Filter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = filter;
        self.selected = None;
        self.refresh_grid(window, cx);
        cx.notify();
    }

    /// Only the rows where the compared column `column` differs, or all.
    pub fn set_column(
        &mut self,
        column: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.column = column;
        if column.is_some() && self.filter != Filter::Different {
            self.filter = Filter::Different;
        }
        self.selected = None;
        self.refresh_grid(window, cx);
        cx.notify();
    }

    /// Selects the `row`-th difference shown.
    #[cfg(test)]
    pub fn select(&mut self, row: usize, cx: &mut Context<Self>) {
        self.selected = self.shown().get(row).copied();
        if let Some(grid) = &self.grid {
            grid.update(cx, |grid, cx| grid.set_selected_row(row, cx));
        }
        cx.notify();
    }

    pub fn set_deletes(&mut self, deletes: bool, cx: &mut Context<Self>) {
        self.deletes = deletes;
        cx.notify();
    }

    /// Copies the row of side `side` of the difference `index`: its key
    /// alone (`format` none), or the whole row as the grid copies.
    pub fn copy_row(
        &mut self,
        index: usize,
        side: usize,
        format: Option<CopyFormat>,
        cx: &mut Context<Self>,
    ) {
        if let Some(text) = self.row_text(index, side, format) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// What [`Self::copy_row`] copies.
    pub fn row_text(
        &self,
        index: usize,
        side: usize,
        format: Option<CopyFormat>,
    ) -> Option<String> {
        let found = self.found.as_ref()?;
        let row = found.outcome.rows.get(index)?;
        let at = if side == 0 { row.source } else { row.target }?;
        let store = &found.outcome.kept[side];
        let width = match format {
            None => found.plan.key.len(),
            Some(_) => found.plan.width(),
        };
        let columns = &store.columns()[..width];
        let cells: Vec<Option<&[u8]>> = (0..width).map(|column| store.cell(at, column)).collect();
        let table = if side == 0 {
            found.plan.source.clone()
        } else {
            found.plan.target.clone()
        };
        let format = match format.unwrap_or(CopyFormat::Tsv) {
            CopyFormat::Tsv => ExportFormat::Delimited(DelimitedOptions {
                header: false,
                ..DelimitedOptions::tsv()
            }),
            CopyFormat::TsvWithHeader => ExportFormat::Delimited(DelimitedOptions::tsv()),
            CopyFormat::Csv => ExportFormat::Delimited(DelimitedOptions::csv()),
            CopyFormat::Json => ExportFormat::JsonArray,
            CopyFormat::Markdown => ExportFormat::Markdown,
            CopyFormat::SqlInsert => ExportFormat::SqlInsert { table },
        };
        let dialect = found.sessions[side].dialect();
        export_to_string(format, columns, dialect, [cells])
            .ok()
            .map(|text| text.trim_end_matches(['\n', '\r']).to_string())
    }

    /// Whether anything differs: an empty comparison has no script.
    fn differs(&self) -> bool {
        self.found
            .as_ref()
            .is_some_and(|found| !found.outcome.rows.is_empty())
    }

    /// The script of the differences kept, with what it is about.
    pub fn script(&self, cx: &App) -> Option<String> {
        let found = self.found.as_ref()?;
        let dialect = found.sessions[1].dialect();
        let body = match data_compare::script(&found.plan, &found.outcome, dialect, self.deletes) {
            Ok(body) => body,
            Err(error) => {
                commented(&format!(
                    "The script could not be written: {}",
                    describe(&error)
                )) + "\n"
            }
        };
        // The connections' names are the user's: commented line by line.
        let heading = commented(&format!(
            "Data compare: makes {} like {}.\nNothing has run: read the script, then run it here, \
             on the target. Switch this tab to manual commit to apply it all or nothing.",
            self.side_label(1, cx),
            self.side_label(0, cx)
        ));
        Some(format!("{heading}\n\n{body}"))
    }

    /// Opens the script in a new query tab on the target; nothing runs.
    pub fn open_script(&mut self, cx: &mut Context<Self>) {
        if self.state != State::Ready || !self.differs() {
            return;
        }
        let Some(sql) = self.script(cx) else {
            return;
        };
        let request = &self.spec.request;
        cx.emit(DataCompareTabEvent::OpenScript {
            title: format!("Make {} like {}", request.target.name, request.source.name),
            target: self.spec.target.clone(),
            sql,
        });
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key == "c"
            && keystroke.modifiers.secondary(cx)
            && let Some(index) = self.selected
            && let Some(row) = self
                .found
                .as_ref()
                .and_then(|found| found.outcome.rows.get(index))
        {
            let side = usize::from(row.source.is_none());
            self.copy_row(index, side, Some(CopyFormat::Tsv), cx);
            cx.stop_propagation();
        }
    }

    // View --------------------------------------------------------------------

    fn render_side(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let side = self.side(index);
        let engine = self
            .connections
            .read(cx)
            .get(&side.connection)
            .map(|saved| saved.config.engine);
        let label = self.side_label(index, cx);
        let (role, id) = if index == 0 {
            ("Source", "data-compare-source")
        } else {
            ("Target", "data-compare-target")
        };
        h_flex()
            .gap_1()
            .min_w_0()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(role),
            )
            .children(
                engine.map(|engine| div().flex_none().child(engine_logo(engine, px(14.), cx))),
            )
            .child(
                div()
                    .id(id)
                    .test_support()
                    .text_sm()
                    .font_semibold()
                    .truncate()
                    .test_text(label.clone())
                    .child(SharedString::from(label)),
            )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ready = self.state == State::Ready;
        h_flex()
            .flex_none()
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(self.render_side(0, cx))
            .child(
                Button::new("data-compare-swap")
                    .ghost()
                    .small()
                    .label("⇄ Swap")
                    .tooltip("Make the target the model, and compare again")
                    .on_click(cx.listener(|this, _, window, cx| this.swap(window, cx))),
            )
            .child(self.render_side(1, cx))
            .child(div().flex_1())
            .child(
                Button::new("data-compare-refresh")
                    .ghost()
                    .small()
                    .label("Refresh")
                    .tooltip("Read both tables again")
                    .disabled(self.state == State::Loading)
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
            )
            .child(
                Button::new("data-compare-open-script")
                    .primary()
                    .small()
                    .label("Open script")
                    .tooltip("Open the script in a query tab on the target: nothing runs")
                    .disabled(!ready || !self.differs())
                    .on_click(cx.listener(|this, _, _, cx| this.open_script(cx))),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ready = self.state == State::Ready;
        let view = cx.entity();
        let columns: Vec<(String, u64)> = self
            .found
            .as_ref()
            .map(|found| {
                found
                    .plan
                    .columns
                    .iter()
                    .zip(&found.outcome.changed)
                    .map(|(col, &count)| (col.name.clone(), count))
                    .collect()
            })
            .unwrap_or_default();
        let picked = self.column;
        let column_label = match picked.and_then(|index| columns.get(index)) {
            Some((name, _)) => format!("Column: {name}"),
            None => "Any column".to_string(),
        };
        let no_columns = columns.is_empty();
        let column_menu =
            move |mut menu: PopupMenu, window: &mut Window, _: &mut Context<PopupMenu>| {
                let pick = |view: Entity<DataCompareTab>, column: Option<usize>| {
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        view.update(cx, |this, cx| this.set_column(column, window, cx))
                    }
                };
                menu = menu.item(
                    PopupMenuItem::new("Any column")
                        .checked(picked.is_none())
                        .on_click(pick(view.clone(), None)),
                );
                for (index, (name, count)) in columns.iter().enumerate() {
                    menu = menu.item(
                        PopupMenuItem::new(SharedString::from(format!(
                            "{name} · {} rows",
                            number(*count as f64)
                        )))
                        .checked(picked == Some(index))
                        .disabled(*count == 0)
                        .on_click(pick(view.clone(), Some(index))),
                    );
                }
                crate::tabs::fit_window(menu, window)
            };
        h_flex()
            .flex_none()
            .flex_wrap()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .children(Filter::ALL.into_iter().enumerate().map(|(index, filter)| {
                let label = format!(
                    "{} {}",
                    filter.label(),
                    number(filter.count(&self.counts) as f64)
                );
                Button::new(("data-compare-filter", index))
                    .small()
                    .map(|button| {
                        if self.filter == filter {
                            button.primary()
                        } else {
                            button.ghost()
                        }
                    })
                    .label(SharedString::from(label))
                    .disabled(!ready)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.set_filter(filter, window, cx)),
                    )
            }))
            .child(
                div()
                    .id("data-compare-same")
                    .test_support()
                    .px_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .test_text(format!("Same {}", number(self.counts.same as f64)))
                    .child(SharedString::from(format!(
                        "Same {}",
                        number(self.counts.same as f64)
                    ))),
            )
            .child(div().flex_1())
            .child(
                Button::new("data-compare-column")
                    .ghost()
                    .small()
                    .label(SharedString::from(column_label))
                    .dropdown_caret(true)
                    .tooltip("Only the rows where this column differs")
                    .disabled(!ready || no_columns)
                    .dropdown_menu(column_menu),
            )
            .child(
                Checkbox::new("data-compare-deletes")
                    .label("Also delete what only the target has")
                    .checked(self.deletes)
                    .on_click(
                        cx.listener(|this, checked: &bool, _, cx| this.set_deletes(*checked, cx)),
                    ),
            )
    }

    /// Progress, the kept differences, the read-only target, under the
    /// toolbar.
    fn render_notes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let loading = self.state == State::Loading;
        let reason = "The target connection is read-only: the script opens, and its statements \
                      will be refused there.";
        let partial = self.found.as_ref().filter(|found| !found.outcome.complete).map(|found| {
            format!(
                "Showing the first {} of the {} differences: the counts are complete. The script \
                 holds the rows shown; compare again once it has run.",
                number(found.outcome.rows.len() as f64),
                number(self.counts.differences() as f64)
            )
        });
        v_flex()
            .flex_none()
            .px_3()
            .gap_1()
            .text_xs()
            .when(loading, |this| {
                this.child(
                    h_flex()
                        .py_1()
                        .gap_3()
                        .children((0..2).map(|index| {
                            let text = format!(
                                "{}: {} rows read",
                                if index == 0 { "Source" } else { "Target" },
                                number(self.counts.read[index] as f64)
                            );
                            div()
                                .id(("data-compare-progress", index))
                                .test_support()
                                .test_text(text.clone())
                                .child(SharedString::from(text))
                        }))
                        .child(
                            div()
                                .id("data-compare-elapsed")
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(format!(
                                    "{:.1} s",
                                    self.elapsed.as_secs_f64()
                                ))),
                        )
                        .child(
                            Button::new("data-compare-cancel")
                                .ghost()
                                .xsmall()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        ),
                )
            })
            .when_some(partial, |this, text| {
                this.child(
                    div()
                        .id("data-compare-partial")
                        .test_support()
                        .py_1()
                        .text_color(theme.warning)
                        .whitespace_normal()
                        .test_text(text.clone())
                        .child(SharedString::from(text)),
                )
            })
            .when(self.read_only(cx), |this| {
                this.child(
                    div()
                        .id("data-compare-read-only")
                        .test_support()
                        .py_1()
                        .text_color(theme.warning)
                        .test_text(reason)
                        .child(reason),
                )
            })
    }

    fn notice(id: &'static str, text: String, cx: &App) -> AnyElement {
        div()
            .id(id)
            .test_support()
            .p_4()
            .text_sm()
            .whitespace_normal()
            .text_color(cx.theme().muted_foreground)
            .test_text(text.clone())
            .child(SharedString::from(text))
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.state {
            State::Waiting => {
                return Self::notice(
                    "data-compare-waiting",
                    "Connect both connections to compare their rows.".into(),
                    cx,
                );
            }
            State::Loading => {
                return Self::notice(
                    "data-compare-loading",
                    "Reading both tables in the order of their key, and comparing their rows as \
                     they come…"
                        .into(),
                    cx,
                );
            }
            State::Failed(message) => {
                return Self::notice("data-compare-error", message.clone(), cx);
            }
            State::Cancelled(message) => {
                return Self::notice("data-compare-cancelled", message.clone(), cx);
            }
            State::Ready => {}
        }
        if !self.differs() {
            return Self::notice(
                "data-compare-identical",
                format!(
                    "The two tables hold the same rows: {} rows compared.",
                    number(self.counts.same as f64)
                ),
                cx,
            );
        }
        let Some(grid) = &self.grid else {
            return div().into_any_element();
        };
        let border = cx.theme().border;
        h_flex()
            .size_full()
            .child(
                div()
                    .id("data-compare-grid")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(DataTable::new(grid).xsmall().focus_ring(false)),
            )
            .child(
                div()
                    .w(px(320.))
                    .flex_none()
                    .h_full()
                    .border_l_1()
                    .border_color(border)
                    .child(self.render_values(cx)),
            )
            .into_any_element()
    }

    /// The selected difference's full values, side by side per column.
    fn render_values(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let found = self.found.as_ref();
        let Some((found, row)) = found.and_then(|found| {
            let row = found.outcome.rows.get(self.selected?)?;
            Some((found, row))
        }) else {
            return Self::notice(
                "data-compare-values-empty",
                "Select a row to see its values on both sides.".into(),
                cx,
            );
        };
        let plan = &found.plan;
        let (mark, words) = row.difference.mark();
        let key = plan.key.len();
        let value = |side: usize, column: usize| -> Option<String> {
            let at = if side == 0 { row.source } else { row.target }?;
            let store = &found.outcome.kept[side];
            Some(full_text(
                store.cell(at, column),
                store.columns()[column].kind,
            ))
        };
        let mut lines = Vec::new();
        for (column, col) in plan.cols().enumerate() {
            let changed = column >= key && row.changed.contains(&((column - key) as u32));
            let (source, target) = (value(0, column), value(1, column));
            let same = row.difference == Difference::Different && !changed;
            let line = v_flex()
                .px_3()
                .py_1()
                .gap_0p5()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .font_semibold()
                        .text_color(if changed {
                            theme.warning
                        } else {
                            theme.muted_foreground
                        })
                        .child(SharedString::from(col.name.clone()))
                        .when(column < key, |this| this.child("· key"))
                        .when(changed, |this| this.child("· differs")),
                )
                .map(|this| {
                    let text = |label: &'static str, value: String| {
                        h_flex()
                            .gap_2()
                            .items_start()
                            .text_sm()
                            .child(
                                div()
                                    .w(px(48.))
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .child(SharedString::from(value)),
                            )
                    };
                    match (source, target) {
                        (Some(source), Some(_)) if same || column < key => {
                            this.child(text("Both", source))
                        }
                        (source, target) => this
                            .children(source.map(|value| text("Source", value)))
                            .children(target.map(|value| text("Target", value))),
                    }
                });
            lines.push(line);
        }
        let text = (0..plan.width())
            .map(|column| {
                format!(
                    "{}: {} | {}",
                    plan.cols().nth(column).map_or("", |col| col.name.as_str()),
                    value(0, column).unwrap_or_default(),
                    value(1, column).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        v_flex()
            .id("data-compare-values")
            .test_support()
            .test_text(text)
            .size_full()
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .font_semibold()
                    .child(SharedString::from(format!("{mark} {words}"))),
            )
            .child(
                v_flex()
                    .id("data-compare-values-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(lines),
            )
            .into_any_element()
    }
}

impl Focusable for DataCompareTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DataCompareTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let snapshot = "Each table is read in one statement, a snapshot of that table alone: a row \
                        changed during the compare is compared as its server had it when its read \
                        began.";
        let muted = theme.muted_foreground;
        let border = theme.border;
        v_flex()
            .id("data-compare-tab")
            .track_focus(&self.focus_handle)
            .size_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.key_down(event, window, cx)
            }))
            .child(self.render_header(cx))
            .child(self.render_toolbar(cx))
            .child(self.render_notes(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .whitespace_normal()
                    .child(snapshot),
            )
    }
}
