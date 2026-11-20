//! What surrounds a grid, in a query result and in a table's data: the
//! record view (one row as a list of its columns), the figures of the
//! selection, the aggregate and columns panels, "Go to row" and "Go to
//! column".

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::base::FocusableExt as _;
use gpui_kit::base::actions::{SelectDown, SelectUp};
use gpui_kit::component::button::Button;
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, WeakEntity, Window, div, px,
};
use kerno_core::aggregate::{Accumulator, Stats, figure};
use kerno_core::format::{CellStyle, display};
use kerno_core::schema::{TableRef, TableStructure};
use kerno_core::{Column, ResultStore};

use super::grid::{
    Grid, GridAction, Mark, Panel, RowMove, Selected, current, editor_element, table_size,
    text_size,
};
use super::viewer::give_back;

/// Selections up to this many cells are counted at once; larger ones off
/// the UI thread.
const INLINE_CELLS: usize = 100_000;

/// The grid, or the record view when it is on; the find bar above it
/// while open.
pub fn render(table: &Entity<TableState<Grid>>, cx: &mut App) -> AnyElement {
    let rows = rows(table, cx);
    match super::find::bar(table, cx) {
        Some(bar) => v_flex()
            .size_full()
            .child(bar)
            .child(div().flex_1().min_h_0().w_full().child(rows))
            .into_any_element(),
        None => rows,
    }
}

/// The grid, or the record view when it is on.
fn rows(table: &Entity<TableState<Grid>>, cx: &mut App) -> AnyElement {
    if !table.read(cx).delegate().record {
        return DataTable::new(table)
            .with_size(table_size(cx))
            .focus_ring(false)
            .into_any_element();
    }
    if let Some(view) = table.read(cx).delegate().record_view.clone() {
        return view.into_any_element();
    }
    let view = cx.new(|cx| RecordView {
        table: table.downgrade(),
        _observe: cx.observe(table, |_, _, cx| cx.notify()),
    });
    table.update(cx, |table, _| {
        table.delegate_mut().record_view = Some(view.clone())
    });
    view.into_any_element()
}

/// The record view of a grid: drawn again whenever the grid changes, in
/// whichever tab holds it.
pub struct RecordView {
    table: WeakEntity<TableState<Grid>>,
    _observe: Subscription,
}

impl Render for RecordView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.table.upgrade() {
            Some(table) => record_view(&table, cx),
            None => div().into_any_element(),
        }
    }
}

// Record view ---------------------------------------------------------------

/// The current row as one line per shown column: name, type, value. It
/// takes the grid's focus and keys (Tab goes back to the grid), and edits
/// through the grid's own editor.
fn record_view(table: &Entity<TableState<Grid>>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let state = table.read(cx);
    let grid = state.delegate();
    let (row, selected) = current(state.selection());
    let total = grid.total_rows();
    let row = row.min(total.saturating_sub(1));
    let size = text_size(cx);
    let null_text = crate::state::AppState::get(cx).settings.null_text.clone();
    let editable = grid.pending().is_some();
    let lines = (total > 0).then(|| {
        grid.layout().iter().enumerate().map(|(index, &source)| {
            let column = index + 1;
            let described = &grid.store().columns()[source];
            let value = match grid.editor_at(row, source) {
                Some(editor) => {
                    let shown = grid
                        .cell(row, source)
                        .map_or(SharedString::from("NULL"), |bytes| {
                            SharedString::from(String::from_utf8_lossy(bytes).into_owned())
                        });
                    editor_element(table, editor, shown, described.kind.is_numeric(), cx)
                }
                None => {
                    let shown = display(grid.cell(row, source), described, 300);
                    let text = match shown.style {
                        CellStyle::Null => null_text.clone(),
                        _ if shown.truncated => format!("{}…", shown.text),
                        _ => shown.text.into_owned(),
                    };
                    div()
                        .truncate()
                        .when(shown.style == CellStyle::Null, |cell| {
                            cell.italic().text_color(theme.muted_foreground)
                        })
                        .when(grid.mark(row, source) == Mark::Edited, |cell| {
                            cell.bg(theme.warning.opacity(0.22))
                        })
                        .child(SharedString::from(text))
                        .into_any_element()
                }
            };
            let (pick, open) = (table.clone(), table.clone());
            h_flex()
                .id(("record-field", index))
                .test_support()
                .px_3()
                .py_1()
                .gap_3()
                .border_b_1()
                .border_color(theme.border)
                .when(column == selected, |line| line.bg(theme.selection))
                .on_click(move |event, window, cx| {
                    pick.update(cx, |table, cx| {
                        Grid::select(table, row, column, cx);
                        table.focus_handle(cx).focus(window, cx);
                    });
                    // A double click edits the value where the rows take
                    // changes, and shows it whole elsewhere.
                    if event.click_count() == 2 {
                        let action = if editable {
                            GridAction::EditCell
                        } else {
                            GridAction::ViewValue
                        };
                        let pick = Grid::pick(&open, Some(row), column, cx);
                        pick(action, window, cx);
                    }
                })
                .child(
                    div()
                        .w(px(200.))
                        .flex_none()
                        .truncate()
                        .child(SharedString::from(described.name.clone())),
                )
                .child(
                    div()
                        .w(px(140.))
                        .flex_none()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(described.type_name.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(theme.mono_font_family.clone())
                        .child(value),
                )
        })
    });
    let step = |id: &'static str, label: &'static str, to: RowMove| {
        let table = table.clone();
        // Nowhere to go before the first row or after the last.
        let at_end = match to {
            RowMove::First | RowMove::Previous => row == 0,
            RowMove::Next | RowMove::Last => row + 1 >= total,
        };
        Button::new(id)
            .outline()
            .xsmall()
            .label(label)
            .disabled(total == 0 || at_end)
            .on_click(move |_, _, cx| Grid::move_row(&table, to, cx))
    };
    let (up, down) = (table.clone(), table.clone());
    let fields = grid.layout().len();
    v_flex()
        .id("record-view")
        .test_support()
        .key_context("DataTable")
        .track_focus(&state.focus_handle(cx))
        .size_full()
        .text_size(size)
        // The arrows move between the fields of the row.
        .on_action(move |_: &SelectUp, _, cx| {
            up.update(cx, |table, cx| {
                let (row, column) = current(table.selection());
                Grid::select(table, row, column.saturating_sub(1).max(1), cx);
            })
        })
        .on_action(move |_: &SelectDown, _, cx| {
            down.update(cx, |table, cx| {
                let (row, column) = current(table.selection());
                Grid::select(table, row, (column + 1).min(fields), cx);
            })
        })
        .child(
            h_flex()
                .flex_none()
                .px_2()
                .py_1()
                .gap_1()
                .border_b_1()
                .border_color(theme.border)
                .child(step("record-first", "First", RowMove::First))
                .child(step("record-previous", "Previous", RowMove::Previous))
                .child(step("record-next", "Next", RowMove::Next))
                .child(step("record-last", "Last", RowMove::Last))
                .child(
                    div()
                        .id("record-row")
                        .test_support()
                        .px_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(if total == 0 {
                            "No rows".to_string()
                        } else {
                            format!("Row {} of {total}", row + 1)
                        })),
                ),
        )
        .child(
            div()
                .id("record-fields")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(lines.into_iter().flatten()),
        )
        .into_any_element()
}

// Selection statistics --------------------------------------------------------

/// Stops a count running off the UI thread when dropped.
pub(super) struct StopOnDrop(pub(super) Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// The figures of what is selected in a grid: in the footer, one line
/// whenever more than one cell is selected; in the aggregate panel, a
/// list. A large selection is counted off the UI thread, and the count
/// stops when the selection changes.
pub struct SelectionStats {
    table: WeakEntity<TableState<Grid>>,
    /// The aggregate panel: a list, and a single cell counts.
    panel: bool,
    /// What the figures are of, and the grid's generation then.
    of: Option<Selected>,
    generation: u64,
    /// `None` while counting.
    stats: Option<Stats>,
    counting: Option<(Task<()>, StopOnDrop)>,
    _observe: Subscription,
}

impl SelectionStats {
    fn new(table: &Entity<TableState<Grid>>, panel: bool, cx: &mut Context<Self>) -> Self {
        Self {
            table: table.downgrade(),
            panel,
            of: None,
            generation: 0,
            stats: None,
            counting: None,
            _observe: cx.observe(table, |_, _, cx| cx.notify()),
        }
    }

    /// The footer's figures of `table`, made once and kept by its grid.
    pub fn of(table: &Entity<TableState<Grid>>, cx: &mut App) -> Entity<Self> {
        if let Some(stats) = table.read(cx).delegate().stats_entity() {
            return stats;
        }
        let stats = cx.new(|cx| Self::new(table, false, cx));
        table.update(cx, |table, _| {
            *table.delegate_mut().stats_view() = Some(stats.clone());
        });
        stats
    }

    /// Counts again when the selection changed.
    fn follow(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.table.upgrade() else {
            return;
        };
        let state = table.read(cx);
        let grid = state.delegate();
        let least = if self.panel { 1 } else { 2 };
        let selected = grid
            .selected(state.selection())
            .filter(|selected| cells(selected) >= least);
        if selected == self.of && grid.generation() == self.generation {
            return;
        }
        // ponytail: a selection that grows while rows arrive (a whole
        // column) keeps the figures of its first rows; counting again on
        // each batch would never end on a large result.
        if let (Some(of), Some(now)) = (&self.of, &selected)
            && grid.has_more_coming()
            && of.columns == now.columns
            && of.rows.start() == now.rows.start()
        {
            return;
        }
        self.of = selected.clone();
        self.generation = grid.generation();
        self.stats = None;
        self.counting = None;
        let Some(selected) = selected else {
            return;
        };
        // Pending values exist only on a table's page (at most 10,000
        // rows), counted here.
        if cells(&selected) <= INLINE_CELLS || grid.has_pending() {
            self.stats = Some(count(grid, &selected));
            return;
        }
        let (store, order) = (grid.store().clone(), grid.order());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let counting = cx
            .background_executor()
            .spawn(async move { count_snapshot(&store, order.as_deref(), &selected, &stopped) });
        let task = cx.spawn(async move |this, cx| {
            if let Some(stats) = counting.await {
                let _ = this.update(cx, |this, cx| {
                    this.stats = Some(stats);
                    this.counting = None;
                    cx.notify();
                });
            }
        });
        self.counting = Some((task, StopOnDrop(stop)));
    }

    /// The footer's line, or nothing for a single cell.
    fn line(&self) -> Option<String> {
        let selected = self.of.as_ref()?;
        let rows = selected.rows.end() - selected.rows.start() + 1;
        let shape = format!(
            "{} cells ({rows} {}, {} {})",
            cells(selected),
            if rows == 1 { "row" } else { "rows" },
            selected.columns.len(),
            if selected.columns.len() == 1 {
                "column"
            } else {
                "columns"
            }
        );
        let Some(stats) = &self.stats else {
            return Some(format!("{shape} · computing…"));
        };
        let mut line = format!("{shape} · {} distinct", distinct(stats));
        if let Some(numbers) = stats.numbers {
            line += &format!(
                " · sum {} · avg {} · min {} · max {}",
                figure(numbers.sum),
                figure(numbers.average()),
                figure(numbers.min),
                figure(numbers.max)
            );
        }
        Some(line)
    }

    /// The aggregate panel's lines: a function and its figure.
    fn lines(&self) -> Vec<(&'static str, String)> {
        let Some(stats) = &self.stats else {
            return Vec::new();
        };
        let mut lines = vec![
            ("Count", stats.count.to_string()),
            ("Count distinct", distinct(stats)),
        ];
        if let Some(numbers) = stats.numbers {
            lines.extend([
                ("Sum", figure(numbers.sum)),
                ("Average", figure(numbers.average())),
                ("Minimum", figure(numbers.min)),
                ("Maximum", figure(numbers.max)),
            ]);
        }
        lines
    }

    /// What the footer says, for tests.
    #[cfg(test)]
    pub fn text(&self) -> Option<String> {
        self.line()
    }
}

/// How many different values, or that there are more than are counted.
fn distinct(stats: &Stats) -> String {
    stats.distinct.map_or_else(
        || format!("over {}", kerno_core::aggregate::DISTINCT_CAP),
        |distinct| distinct.to_string(),
    )
}

/// How many cells a selection holds.
fn cells(selected: &Selected) -> usize {
    (selected.rows.end() - selected.rows.start() + 1) * selected.columns.len()
}

/// The figures of the selected cells, as the grid shows them.
fn count(grid: &Grid, selected: &Selected) -> Stats {
    let kinds: Vec<_> = selected
        .columns
        .iter()
        .map(|&column| grid.store().columns()[column].kind)
        .collect();
    let mut acc = Accumulator::default();
    for row in selected.rows.clone() {
        for (&column, &kind) in selected.columns.iter().zip(&kinds) {
            acc.push(kind, grid.cell(row, column));
        }
    }
    acc.finish()
}

/// The figures of a selection of fetched rows, off the UI thread, rows in
/// `order` when sorted. `None` once stopped.
fn count_snapshot(
    store: &ResultStore,
    order: Option<&[u32]>,
    selected: &Selected,
    stop: &AtomicBool,
) -> Option<Stats> {
    let mut acc = Accumulator::default();
    for row in selected.rows.clone() {
        if row % 4096 == 0 && stop.load(Ordering::Relaxed) {
            return None;
        }
        let source = order
            .and_then(|order| order.get(row))
            .map_or(row, |&row| row as usize);
        if source >= store.row_count() {
            continue;
        }
        for &column in &selected.columns {
            acc.push(store.columns()[column].kind, store.cell(source, column));
        }
    }
    Some(acc.finish())
}

impl Render for SelectionStats {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.panel || crate::state::AppState::get(cx).settings.selection_stats;
        if !shown {
            return div().into_any_element();
        }
        self.follow(cx);
        let theme = cx.theme();
        if self.panel {
            let lines = self.lines();
            return v_flex()
                .id("aggregate-panel")
                .test_support()
                .gap_1()
                .text_sm()
                .when(self.of.is_none(), |panel| {
                    panel.child("Select cells to see their figures.")
                })
                .when(self.of.is_some() && self.stats.is_none(), |panel| {
                    panel.child("Computing…")
                })
                .children(lines.into_iter().map(|(name, value)| {
                    h_flex().gap_3().child(div().w(px(140.)).child(name)).child(
                        div()
                            .flex_1()
                            .text_right()
                            .font_family(theme.mono_font_family.clone())
                            .child(SharedString::from(value)),
                    )
                }))
                .into_any_element();
        }
        div()
            .id("selection-stats")
            .test_support()
            // A long line wraps inside a narrow footer, never past its edge.
            .min_w_0()
            .when_some(self.line(), |footer, line| {
                footer.child(SharedString::from(line))
            })
            .into_any_element()
    }
}

// Panels --------------------------------------------------------------------

/// One line of the columns panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnFacts {
    /// From 1, in the result.
    pub position: usize,
    pub name: String,
    pub type_name: String,
    /// The table it comes from, when the rows are one table's.
    pub table: Option<String>,
    /// Part of the primary key.
    pub key: bool,
    /// Known only for a table's columns.
    pub nullable: Option<bool>,
}

/// The columns of a result, with what the table says of them when the rows
/// are one table's (DBeaver's `panel/metadata`).
pub fn column_facts(
    columns: &[Column],
    table: Option<&TableRef>,
    structure: Option<&TableStructure>,
) -> Vec<ColumnFacts> {
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let info = structure.and_then(|structure| {
                structure
                    .columns
                    .iter()
                    .find(|info| info.name == column.name)
            });
            ColumnFacts {
                position: index + 1,
                name: column.name.clone(),
                type_name: info
                    .map_or_else(|| column.type_name.clone(), |info| info.data_type.clone()),
                table: info
                    .and(table)
                    .map(|table| format!("{}.{}", table.schema, table.name)),
                key: info.is_some_and(|info| info.primary_key_position.is_some()),
                nullable: info.map(|info| info.nullable),
            }
        })
        .collect()
}

/// Opens a panel about the result of `table`.
pub fn open_panel(
    table: &Entity<TableState<Grid>>,
    panel: Panel,
    window: &mut Window,
    cx: &mut App,
) {
    // Back to the grid, though a menu may have opened the panel.
    let back = Some(table.read(cx).focus_handle(cx));
    match panel {
        Panel::Aggregate => {
            let stats = cx.new(|cx| SelectionStats::new(table, true, cx));
            window.open_dialog(cx, move |dialog, _, _| {
                let (stats, back) = (stats.clone(), back.clone());
                dialog
                    .title("Aggregate")
                    .w(px(420.))
                    .on_close(move |_, window, cx| give_back(back.clone(), window, cx))
                    .content(move |content, _, _| content.child(stats.clone()))
            });
        }
        Panel::Columns => {
            let grid = table.read(cx).delegate();
            let facts = column_facts(
                grid.store().columns(),
                grid.table(),
                grid.structure().map(Arc::as_ref),
            );
            window.open_dialog(cx, move |dialog, _, _| {
                let (facts, back) = (facts.clone(), back.clone());
                dialog
                    .title("Columns")
                    .w(px(640.))
                    .on_close(move |_, window, cx| give_back(back.clone(), window, cx))
                    .content(move |content, _, cx| content.child(columns_panel(facts.clone(), cx)))
            });
        }
    }
}

fn columns_panel(facts: Vec<ColumnFacts>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let cell = |text: String, width: f32| {
        div()
            .w(px(width))
            .flex_none()
            .truncate()
            .child(SharedString::from(text))
    };
    let header = h_flex()
        .gap_2()
        .text_color(theme.muted_foreground)
        .child(cell("#".into(), 28.))
        .child(cell("Column".into(), 150.))
        .child(cell("Type".into(), 130.))
        .child(cell("Table".into(), 130.))
        .child(cell("Key".into(), 40.))
        .child(cell("Nullable".into(), 70.));
    v_flex()
        .id("columns-panel")
        .test_support()
        .max_h(px(420.))
        .overflow_y_scroll()
        .gap_1()
        .text_sm()
        .child(header)
        .children(facts.into_iter().map(|facts| {
            h_flex()
                .gap_2()
                .child(cell(facts.position.to_string(), 28.))
                .child(cell(facts.name, 150.))
                .child(cell(facts.type_name, 130.))
                .child(cell(facts.table.unwrap_or_default(), 130.))
                .child(cell(if facts.key { "PK" } else { "" }.into(), 40.))
                .child(cell(
                    match facts.nullable {
                        Some(true) => "yes",
                        Some(false) => "no",
                        None => "",
                    }
                    .into(),
                    70.,
                ))
        }))
        .into_any_element()
}

// Go to row, go to column -------------------------------------------------------

/// Asks for a row number and selects that row (DBeaver's `grid.gotoRow`).
pub fn go_to_row(table: &Entity<TableState<Grid>>, window: &mut Window, cx: &mut App) {
    let total = table.read(cx).delegate().total_rows();
    let input =
        cx.new(|cx| InputState::new(window, cx).placeholder(format!("Row number, 1 to {total}")));
    let go = {
        let (table, input) = (table.clone(), input.clone());
        move |window: &mut Window, cx: &mut App| {
            let typed = input.read(cx).value().trim().to_string();
            let Ok(number) = typed.parse::<usize>() else {
                return;
            };
            window.close_dialog(cx);
            if let Some(reason) = Grid::go_to_row(&table, number, cx) {
                window.push_notification(Notification::warning(reason), cx);
            }
            let table = table.clone();
            window.defer(cx, move |window, cx| {
                table.read(cx).focus_handle(cx).focus(window, cx)
            });
        }
    };
    let enter = window.subscribe(&input, cx, move |_, event: &InputEvent, window, cx| {
        if matches!(event, InputEvent::PressEnter { .. }) {
            go(window, cx);
        }
    });
    let field = input.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let _ = &enter;
        let field = field.clone();
        dialog
            .title("Go to Row")
            .w(px(360.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(Input::new(&field).id("go-to-row").small()))
    });
    window.defer(cx, move |window, cx| {
        input.update(cx, |input, cx| input.focus(window, cx))
    });
}

/// Lists the shown columns, found by a few letters, and selects the one
/// chosen on the current row (DBeaver's `grid.gotoColumn`).
pub fn go_to_column(table: &Entity<TableState<Grid>>, window: &mut Window, cx: &mut App) {
    let names: Vec<String> = {
        let grid = table.read(cx).delegate();
        grid.layout()
            .iter()
            .map(|&source| grid.store().columns()[source].name.clone())
            .collect()
    };
    let state = cx.new(|cx| CommandState::new(window, cx));
    // The columns of the grid (from 1) whose names hold what is typed.
    let found = move |query: &str| -> Vec<(usize, String)> {
        let query = query.trim().to_lowercase();
        names
            .iter()
            .enumerate()
            .filter(|(_, name)| name.to_lowercase().contains(&query))
            .map(|(index, name)| (index + 1, name.clone()))
            .collect()
    };
    let (table, focus) = (table.clone(), state.clone());
    window.open_dialog(cx, move |dialog, _, _| {
        let (table, state, found) = (table.clone(), state.clone(), found.clone());
        dialog
            .close_button(false)
            .p_0()
            .content(move |content, _, cx| {
                let matching = found(&state.read(cx).query(cx));
                let chosen = matching.clone();
                let table = table.clone();
                content.child(
                    div().id("go-to-column").test_support().child(
                        Command::new(&state)
                            .bordered(false)
                            .filterable(false)
                            .placeholder("Type a column name")
                            .max_h(px(360.))
                            .items(
                                matching
                                    .into_iter()
                                    .map(|(_, name)| CommandItem::new().label(name)),
                            )
                            .on_confirm(move |index, window, cx| {
                                let Some(&(column, _)) = chosen.get(index.row) else {
                                    return;
                                };
                                window.close_dialog(cx);
                                Grid::go_to_column(&table, column, cx);
                                let table = table.clone();
                                window.defer(cx, move |window, cx| {
                                    table.read(cx).focus_handle(cx).focus(window, cx)
                                });
                            }),
                    ),
                )
            })
    });
    window.defer(cx, move |window, cx| {
        focus.read(cx).focus_handle(cx).focus(window, cx)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::TypeKind;
    use kerno_core::schema::ColumnInfo;

    #[test]
    fn the_columns_panel_adds_what_the_table_says() {
        let columns = [
            Column::new("id", "int4", TypeKind::Integer),
            Column::new("name", "text", TypeKind::Text),
            Column::new("total", "numeric", TypeKind::Decimal),
        ];
        // A query result: name, type, position.
        let facts = column_facts(&columns, None, None);
        assert_eq!(facts[2].position, 3);
        assert_eq!(facts[2].type_name, "numeric");
        assert_eq!(
            (facts[0].key, facts[0].nullable, facts[0].table.clone()),
            (false, None, None)
        );
        // A table's rows: its key, nullability and full type.
        let info = |name: &str, data_type: &str, nullable, key| ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            kind: TypeKind::Other,
            nullable,
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: key,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        };
        let structure = TableStructure {
            columns: vec![
                info("id", "integer", false, Some(1)),
                info("name", "character varying(40)", true, None),
            ],
            ..TableStructure::default()
        };
        let table = TableRef::new("shop", "customers");
        let facts = column_facts(&columns, Some(&table), Some(&structure));
        assert!(facts[0].key);
        assert_eq!(facts[0].nullable, Some(false));
        assert_eq!(facts[0].table.as_deref(), Some("shop.customers"));
        assert_eq!(facts[1].type_name, "character varying(40)");
        assert_eq!(facts[1].nullable, Some(true));
        // A computed column of the query is no table's.
        assert_eq!(facts[2].table, None);
    }
}
