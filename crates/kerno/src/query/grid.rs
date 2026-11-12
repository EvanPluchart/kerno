//! The results grid: one fetched result, drawn by the UI kit's virtualized
//! table. Only the visible rows and columns are laid out; the rows live in
//! the result store as the server sent them.
//!
//! The kit's table selects one row, one column or one cell. A range (shift
//! and click) is kept here, as the far corner of a rectangle whose near
//! corner is the table's own selection.

use std::ops::RangeInclusive;
use std::sync::Arc;

use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableSelection, TableState};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClipboardItem, Context, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, SharedString, Styled as _, Task, Window, div, px,
};
use kerno_core::export::{DelimitedOptions, ExportFormat, export_to_string};
use kerno_core::format::{CellStyle, display};
use kerno_core::schema::TableRef;
use kerno_core::sort::sorted_rows;
use kerno_core::{Dialect, ResultStore, RowBatch, RowDemand, Session, TypeKind};

/// Cells longer than this are cut in the grid; the value viewer shows all.
const MAX_CELL_CHARS: usize = 300;

/// Results up to this many rows are sorted at once; larger ones off the UI
/// thread.
const SORT_INLINE_ROWS: usize = 20_000;

/// How a selection is written to the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyFormat {
    /// Tab-separated, without a header: what a spreadsheet pastes.
    Tsv,
    /// Tab-separated with the column names first.
    TsvWithHeader,
    Csv,
    Json,
    Markdown,
    SqlInsert,
}

impl CopyFormat {
    pub const MENU: [(CopyFormat, &'static str); 6] = [
        (CopyFormat::Tsv, "Copy"),
        (CopyFormat::TsvWithHeader, "Copy with column names"),
        (CopyFormat::Csv, "Copy as CSV"),
        (CopyFormat::Json, "Copy as JSON"),
        (CopyFormat::Markdown, "Copy as Markdown"),
        (CopyFormat::SqlInsert, "Copy as SQL INSERT"),
    ];
}

/// What is selected: rows in display order, columns of the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub rows: RangeInclusive<usize>,
    /// Indexes into the result's columns, left to right as shown.
    pub columns: Vec<usize>,
}

/// The rows of one result set and how they are shown.
pub struct Grid {
    store: ResultStore,
    /// Rows asked of the server so far.
    requested: u64,
    /// Rows asked for at a time.
    page: u64,
    /// The server has no more rows for this result.
    complete: bool,
    /// Asks the running statement for more rows; gone once it ended.
    demand: Option<RowDemand>,
    /// The result's columns as shown, left to right. A hidden column is
    /// not in it.
    layout: Vec<usize>,
    /// The first columns of the layout that stay in view when scrolling.
    pinned: usize,
    /// The sorted column of the result and whether descending.
    sort: Option<(usize, bool)>,
    /// Rows in sorted order, once computed.
    order: Option<Arc<[u32]>>,
    sorting: Option<Task<()>>,
    /// The far corner of a range selection: display row and shown column.
    extent: Option<(usize, usize)>,
    /// Everything is selected.
    all: bool,
    /// The table's own selection, as last reported: the near corner of a
    /// range. (The table cannot be asked while it draws its cells.)
    anchor: TableSelection,
    /// The session the rows came from: its dialect writes the literals of
    /// "Copy as SQL INSERT".
    session: Arc<dyn Session>,
}

impl Grid {
    /// An empty result with these columns, of which `page` rows were asked.
    pub fn new(
        store: ResultStore,
        demand: RowDemand,
        page: u64,
        session: Arc<dyn Session>,
    ) -> Self {
        let layout = (0..store.columns().len()).collect();
        Self {
            store,
            requested: page,
            page,
            complete: false,
            demand: Some(demand),
            layout,
            pinned: 0,
            sort: None,
            order: None,
            sorting: None,
            extent: None,
            all: false,
            anchor: TableSelection::None,
            session,
        }
    }

    pub fn push(&mut self, batch: RowBatch) {
        self.store.push(batch);
    }

    /// The statement ended: what is here is all there is, or all there
    /// will be (`complete` tells which).
    pub fn finish(&mut self, complete: bool) {
        self.complete = complete;
        self.demand = None;
    }

    #[cfg(test)]
    pub fn store(&self) -> &ResultStore {
        &self.store
    }

    pub fn row_count(&self) -> usize {
        self.store.row_count()
    }

    /// Whether the server has, or may have, rows that were not asked for.
    pub fn has_more(&self) -> bool {
        !self.complete && self.demand.is_some() && self.store.row_count() as u64 >= self.requested
    }

    /// Whether every row of the result is here.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Whether rows are still arriving or can still be asked for.
    fn loading(&self) -> bool {
        self.demand.is_some()
    }

    /// Asks for one more page.
    pub fn fetch_more(&mut self) {
        if let Some(demand) = &self.demand
            && self.has_more()
        {
            self.requested += self.page;
            demand.request_rows(self.page);
        }
    }

    /// Asks for every remaining row.
    pub fn fetch_all(&mut self) {
        if let Some(demand) = &self.demand {
            self.requested = u64::MAX;
            demand.request_all();
        }
    }

    /// The row of the result shown on a row of the grid.
    pub fn source_row(&self, row: usize) -> usize {
        match &self.order {
            Some(order) => order.get(row).map_or(row, |&source| source as usize),
            None => row,
        }
    }

    /// The column of the result shown in a column of the grid (column 0
    /// numbers the rows and shows none).
    pub fn source_column(&self, column: usize) -> Option<usize> {
        self.layout.get(column.checked_sub(1)?).copied()
    }

    // Layout ------------------------------------------------------------------

    /// Stops showing a column of the grid.
    pub fn hide(&mut self, column: usize) {
        let Some(index) = column
            .checked_sub(1)
            .filter(|index| *index < self.layout.len())
        else {
            return;
        };
        self.layout.remove(index);
        if index < self.pinned {
            self.pinned -= 1;
        }
        self.clear_selection();
    }

    /// Shows every column again, in the result's order.
    pub fn show_all(&mut self) {
        self.layout = (0..self.store.columns().len()).collect();
        self.pinned = 0;
        self.clear_selection();
    }

    pub fn hidden_count(&self) -> usize {
        self.store.columns().len() - self.layout.len()
    }

    pub fn is_pinned(&self, column: usize) -> bool {
        column
            .checked_sub(1)
            .is_some_and(|index| index < self.pinned)
    }

    /// Keeps a column of the grid in view (or lets it scroll again). Pinned
    /// columns sit together at the left.
    pub fn toggle_pin(&mut self, column: usize) {
        let Some(index) = column
            .checked_sub(1)
            .filter(|index| *index < self.layout.len())
        else {
            return;
        };
        let source = self.layout.remove(index);
        if index < self.pinned {
            self.pinned -= 1;
            self.layout.insert(self.pinned, source);
        } else {
            self.layout.insert(self.pinned, source);
            self.pinned += 1;
        }
        self.clear_selection();
    }

    // Selection ---------------------------------------------------------------

    /// What the grid draws in its cells, row by row (column 0, the row
    /// numbers, left out); for tests.
    #[cfg(test)]
    pub fn shown(&self) -> Vec<Vec<String>> {
        (0..self.store.row_count())
            .map(|row| {
                self.layout
                    .iter()
                    .map(|&column| {
                        display(
                            self.store.cell(self.source_row(row), column),
                            &self.store.columns()[column],
                            MAX_CELL_CHARS,
                        )
                        .text
                        .into_owned()
                    })
                    .collect()
            })
            .collect()
    }

    /// The names of the columns shown, left to right; for tests.
    #[cfg(test)]
    pub fn shown_columns(&self) -> Vec<String> {
        self.layout
            .iter()
            .map(|&column| self.store.columns()[column].name.clone())
            .collect()
    }

    /// Extends the selection to this cell of the grid.
    pub fn extend_to(&mut self, row: usize, column: usize) {
        self.all = false;
        self.extent = Some((row, column));
    }

    pub fn select_all(&mut self) {
        self.all = true;
        self.extent = None;
    }

    /// Notes what the table itself has selected.
    pub fn set_anchor(&mut self, selection: TableSelection) {
        self.anchor = selection;
    }

    /// Forgets the range: the table's own selection is all there is.
    pub fn clear_selection(&mut self) {
        self.all = false;
        self.extent = None;
    }

    /// What is selected, given the table's own selection.
    pub fn selected(&self, selection: TableSelection) -> Option<Selected> {
        let rows = self.store.row_count();
        if rows == 0 || self.layout.is_empty() {
            return None;
        }
        let all_rows = 0..=rows - 1;
        if self.all {
            return Some(Selected {
                rows: all_rows,
                columns: self.layout.clone(),
            });
        }
        let span = |a: usize, b: usize| a.min(b)..=a.max(b);
        // Columns of the grid from `a` to `b`, as columns of the result.
        let columns = |a: usize, b: usize| -> Vec<usize> {
            span(a.max(1), b.max(1))
                .filter_map(|column| self.source_column(column))
                .collect()
        };
        let selected = match (selection, self.extent) {
            (TableSelection::None, _) => return None,
            (TableSelection::Cell(row, column), None) => Selected {
                rows: row..=row,
                columns: columns(column, column),
            },
            (TableSelection::Cell(row, column), Some((to_row, to_column))) => Selected {
                rows: span(row, to_row),
                columns: columns(column, to_column),
            },
            (TableSelection::Row(row), extent) => Selected {
                rows: span(row, extent.map_or(row, |(to_row, _)| to_row)),
                columns: self.layout.clone(),
            },
            (TableSelection::Column(column), extent) => Selected {
                rows: all_rows,
                columns: columns(column, extent.map_or(column, |(_, to_column)| to_column)),
            },
        };
        (!selected.columns.is_empty() && *selected.rows.end() < rows).then_some(selected)
    }

    /// Whether a cell of the grid is inside a range selection (the table
    /// draws its own single selection).
    fn in_range(&self, row: usize, column: usize) -> bool {
        let selection = self.anchor;
        if self.all {
            return true;
        }
        let Some((to_row, to_column)) = self.extent else {
            return false;
        };
        let within = |value: usize, a: usize, b: usize| (a.min(b)..=a.max(b)).contains(&value);
        match selection {
            TableSelection::Cell(from_row, from_column) => {
                within(row, from_row, to_row) && within(column, from_column, to_column)
            }
            TableSelection::Row(from_row) => within(row, from_row, to_row),
            TableSelection::Column(from_column) => within(column, from_column, to_column),
            TableSelection::None => false,
        }
    }

    /// The selection as text. A single cell is its value as it is; anything
    /// larger is written in `format`. `table` names the target of `INSERT`s.
    pub fn copy_text(
        &self,
        selected: &Selected,
        format: CopyFormat,
        dialect: &dyn Dialect,
        table: TableRef,
    ) -> kerno_core::Result<String> {
        let single = selected.rows.start() == selected.rows.end() && selected.columns.len() == 1;
        if single && format == CopyFormat::Tsv {
            let cell = self
                .store
                .cell(self.source_row(*selected.rows.start()), selected.columns[0]);
            return Ok(cell.map_or(String::new(), |bytes| {
                String::from_utf8_lossy(bytes).into_owned()
            }));
        }
        let all = self.store.columns();
        let columns: Vec<_> = selected
            .columns
            .iter()
            .map(|&column| all[column].clone())
            .collect();
        let rows = selected.rows.clone().map(|row| {
            let source = self.source_row(row);
            selected
                .columns
                .iter()
                .map(|&column| self.store.cell(source, column))
                .collect::<Vec<_>>()
        });
        let format = match format {
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
        export_to_string(format, &columns, dialect, rows)
    }

    /// Writes the selection of `table` to the clipboard in `format`.
    /// Without a selection, `fallback` (a row of the grid) is taken.
    /// Returns whether anything was copied.
    pub fn copy(
        table: &TableState<Self>,
        format: Option<CopyFormat>,
        fallback: Option<usize>,
        cx: &mut App,
    ) -> bool {
        let grid = table.delegate();
        let selection = match (table.selection(), fallback) {
            (TableSelection::None, Some(row)) => TableSelection::Row(row),
            (selection, _) => selection,
        };
        let Some(selected) = grid.selected(selection) else {
            return false;
        };
        let text = match format {
            None => Ok(grid.column_names(&selected)),
            // The statement does not say which table an INSERT would go
            // to: a placeholder the user replaces.
            Some(format) => grid.copy_text(
                &selected,
                format,
                grid.session.dialect(),
                TableRef::new("schema_name", "table_name"),
            ),
        };
        match text {
            Ok(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            Err(error) => {
                tracing::warn!(%error, "the selection could not be copied");
                false
            }
        }
    }

    /// The names of the selected columns, one per line.
    pub fn column_names(&self, selected: &Selected) -> String {
        let all = self.store.columns();
        selected
            .columns
            .iter()
            .map(|&column| all[column].name.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A cell of the grid: its column and raw value (`None` for NULL).
    pub fn value_at(
        &self,
        row: usize,
        column: usize,
    ) -> Option<(&kerno_core::Column, Option<&[u8]>)> {
        let source = self.source_column(column)?;
        (row < self.store.row_count()).then(|| {
            (
                &self.store.columns()[source],
                self.store.cell(self.source_row(row), source),
            )
        })
    }

    /// Entries of the menu of a column header.
    fn header_menu(
        &self,
        column: usize,
        menu: PopupMenu,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(source) = self.source_column(column) else {
            return menu;
        };
        let name = self.store.columns()[source].name.clone();
        let table = cx.entity();
        let act = move |act: fn(&mut Grid, usize)| {
            let table = table.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                table.update(cx, |table, cx| {
                    act(table.delegate_mut(), column);
                    table.refresh(cx);
                    cx.notify();
                });
            }
        };
        menu.item(PopupMenuItem::new("Copy name").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(name.clone()));
        }))
        .separator()
        .item(
            PopupMenuItem::new(if self.is_pinned(column) {
                "Unpin column"
            } else {
                "Pin column"
            })
            .on_click(act(Grid::toggle_pin)),
        )
        .item(PopupMenuItem::new("Hide column").on_click(act(Grid::hide)))
        .item(
            PopupMenuItem::new(match self.hidden_count() {
                0 => "Show all columns".to_string(),
                hidden => format!("Show all columns ({hidden} hidden)"),
            })
            .disabled(self.hidden_count() == 0)
            .on_click(act(|grid, _| grid.show_all())),
        )
    }
}

/// A first width for a column, from its type and name; the user resizes.
fn initial_width(name: &str, kind: TypeKind) -> Pixels {
    let by_kind: f32 = match kind {
        TypeKind::Boolean => 70.,
        TypeKind::Integer | TypeKind::Float | TypeKind::Decimal | TypeKind::Bit => 100.,
        TypeKind::Date | TypeKind::Time => 110.,
        TypeKind::DateTime | TypeKind::DateTimeTz | TypeKind::Interval => 210.,
        TypeKind::Uuid => 300.,
        TypeKind::Json | TypeKind::Array | TypeKind::Text | TypeKind::Binary => 220.,
        TypeKind::Enum | TypeKind::Other => 140.,
    };
    // Room for the name and the sort arrow.
    let by_name = name.chars().count() as f32 * 8. + 44.;
    px(by_kind.max(by_name).min(360.))
}

/// Width of the row-number column for a result of `rows` rows.
fn number_width(rows: usize) -> Pixels {
    let digits = rows.max(1).ilog10() + 1;
    px(digits.max(3) as f32 * 8. + 20.)
}

impl TableDelegate for Grid {
    // Column 0 numbers the rows; the shown columns of the result follow.
    fn columns_count(&self, _: &App) -> usize {
        self.layout.len() + 1
    }

    fn rows_count(&self, _: &App) -> usize {
        self.store.row_count()
    }

    fn column(&self, index: usize, _: &App) -> Column {
        let Some(source) = self.source_column(index) else {
            return Column::new("row-number", "")
                .width(number_width(self.store.row_count()))
                .fixed_left()
                .resizable(false)
                .movable(false)
                .selectable(false)
                .text_right();
        };
        let column = &self.store.columns()[source];
        let mut shown = Column::new(
            format!("column-{source}"),
            SharedString::from(column.name.clone()),
        )
        .width(initial_width(&column.name, column.kind))
        .min_width(px(48.))
        .resizable(true)
        .movable(!self.is_pinned(index));
        if column.kind.is_numeric() {
            shown = shown.text_right();
        }
        if self.is_pinned(index) {
            shown = shown.fixed_left();
        }
        // What is still arriving cannot be put in order yet.
        if !self.loading() {
            shown = match self.sort {
                Some((sorted, descending)) if sorted == source => {
                    if descending {
                        shown.descending()
                    } else {
                        shown.ascending()
                    }
                }
                _ => shown.sortable(),
            };
        }
        shown
    }

    fn perform_sort(
        &mut self,
        column: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(source) = self.source_column(column) else {
            return;
        };
        self.clear_selection();
        self.sorting = None;
        let descending = match sort {
            ColumnSort::Ascending => false,
            ColumnSort::Descending => true,
            ColumnSort::Default => {
                self.sort = None;
                self.order = None;
                cx.notify();
                return;
            }
        };
        self.sort = Some((source, descending));
        if self.store.row_count() <= SORT_INLINE_ROWS {
            self.order = Some(sorted_rows(&self.store, source, descending).into());
            cx.notify();
            return;
        }
        // A large result is sorted off the UI thread, on a snapshot of
        // the rows; the grid keeps its order until the new one is ready.
        let snapshot = self.store.clone();
        let sorting = cx
            .background_executor()
            .spawn(async move { sorted_rows(&snapshot, source, descending) });
        self.sorting = Some(cx.spawn(async move |table, cx| {
            let order = sorting.await;
            let _ = table.update(cx, |table, cx| {
                let grid = table.delegate_mut();
                if grid.sort == Some((source, descending)) {
                    grid.order = Some(order.into());
                    grid.sorting = None;
                    cx.notify();
                }
            });
        }));
    }

    fn move_column(
        &mut self,
        column: usize,
        to: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        // The row numbers stay first and pinned columns stay together.
        let (Some(from), Some(to)) = (column.checked_sub(1), to.checked_sub(1)) else {
            return;
        };
        if from < self.pinned
            || to < self.pinned
            || from >= self.layout.len()
            || to >= self.layout.len()
        {
            return;
        }
        let source = self.layout.remove(from);
        self.layout.insert(to, source);
        self.clear_selection();
        cx.notify();
    }

    fn render_th(
        &mut self,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let Some(source) = self.source_column(column) else {
            return div().into_any_element();
        };
        let described = &self.store.columns()[source];
        let table = cx.entity();
        h_flex()
            .id(("column-header", column))
            .w_full()
            .gap_2()
            .child(
                div()
                    .truncate()
                    .child(SharedString::from(described.name.clone())),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(described.type_name.clone())),
            )
            .context_menu(move |menu, _, cx| {
                table.update(cx, |table, cx| {
                    table.delegate().header_menu(column, menu, cx)
                })
            })
            .into_any_element()
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let cell = div()
            .w_full()
            .truncate()
            .text_sm()
            .font_family(theme.mono_font_family.clone());
        let Some(source) = self.source_column(column) else {
            return cell
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(SharedString::from((row + 1).to_string()))
                .into_any_element();
        };
        let shown = display(
            self.store.cell(self.source_row(row), source),
            &self.store.columns()[source],
            MAX_CELL_CHARS,
        );
        let text = if shown.truncated {
            SharedString::from(format!("{}…", shown.text))
        } else {
            SharedString::from(shown.text.into_owned())
        };
        cell
            // NULL is not the empty string: it is drawn apart.
            .when(shown.style == CellStyle::Null, |cell| {
                cell.italic().text_color(theme.muted_foreground)
            })
            .when(
                matches!(shown.style, CellStyle::Boolean | CellStyle::Binary),
                |cell| cell.text_color(theme.muted_foreground),
            )
            .when(self.in_range(row, column), |cell| cell.bg(theme.selection))
            // Shift and click stretches the selection to this cell; a plain
            // click is the table's.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |table, event: &MouseDownEvent, _, cx| {
                    if event.modifiers.shift {
                        table.delegate_mut().extend_to(row, column);
                        cx.stop_propagation();
                    } else {
                        table.delegate_mut().clear_selection();
                    }
                    cx.notify();
                }),
            )
            .child(text)
            .into_any_element()
    }

    fn context_menu(
        &mut self,
        row: usize,
        mut menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let table = cx.entity();
        for (format, label) in CopyFormat::MENU {
            let table = table.clone();
            menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                table.update(cx, |table, cx| {
                    Grid::copy(table, Some(format), Some(row), cx)
                });
            }));
            if format == CopyFormat::TsvWithHeader {
                menu = menu.separator();
            }
        }
        menu.separator().item(
            PopupMenuItem::new("Copy column names").on_click(move |_, _, cx| {
                table.update(cx, |table, cx| Grid::copy(table, None, Some(row), cx));
            }),
        )
    }

    fn has_more(&self, _: &App) -> bool {
        Grid::has_more(self)
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        self.fetch_more();
        cx.notify();
    }
}
