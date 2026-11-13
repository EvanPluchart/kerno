//! The results grid: one fetched result, drawn by the UI kit's virtualized
//! table. Only the visible rows and columns are laid out; the rows live in
//! the result store as the server sent them.
//!
//! The kit's table selects one row, one column or one cell. A range (shift
//! and click) is kept here, as the far corner of a rectangle whose near
//! corner is the table's own selection.

use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableSelection, TableState};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, DismissEvent, Div, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    SharedString, Stateful, Styled as _, Subscription, Task, TestSupportExt as _, Window, div,
    font, px,
};
use kerno_core::browse::Operator;
use kerno_core::export::{DelimitedOptions, ExportFormat, export_to_string};
use kerno_core::format::{CellStyle, display};
use kerno_core::pending::Pending;
use kerno_core::schema::{ColumnGeneration, ColumnInfo, TableRef, TableStructure};
use kerno_core::sort::sorted_rows;
use kerno_core::sql::{CellValue, RowChange, RowSql, edit_statements, rows_sql};
use kerno_core::{Dialect, ResultStore, RowBatch, RowDemand, Session, TypeKind};

/// Cells longer than this are cut in the grid; the value viewer shows all.
const MAX_CELL_CHARS: usize = 300;

/// Results up to this many rows are sorted at once; larger ones off the UI
/// thread.
const SORT_INLINE_ROWS: usize = 20_000;

/// "Fit width" measures the cells of this many rows.
const FIT_ROWS: usize = 1_000;

/// The text size of the grids (Settings ▸ Grid, and the zoom keys).
pub fn text_size(cx: &App) -> Pixels {
    px(crate::state::AppState::get(cx).settings.grid_font_size)
}

/// The size a grid's table takes for its text: the kit's smallest rows
/// until the text needs taller ones.
pub fn table_size(cx: &App) -> gpui_kit::component::Size {
    let height = (crate::state::AppState::get(cx).settings.grid_font_size * 2.0).round();
    if height <= 26.0 {
        gpui_kit::component::Size::XSmall
    } else {
        gpui_kit::component::Size::Size(px(height))
    }
}

/// A timestamp's text without its offset (`+02`, `-05:30`, `Z`), when the
/// settings hide time zones.
fn without_zone(text: &str) -> &str {
    if let Some(utc) = text.strip_suffix('Z') {
        return utc;
    }
    match text.rfind(['+', '-']) {
        Some(at)
            if at > 10
                && text[..at].contains(':')
                && text[at + 1..]
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == ':') =>
        {
            &text[..at]
        }
        _ => text,
    }
}

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
    /// What "Copy as ▸" offers ("Copy" itself is TSV).
    pub const COPY_AS: [CopyFormat; 5] = [
        CopyFormat::TsvWithHeader,
        CopyFormat::Csv,
        CopyFormat::Json,
        CopyFormat::Markdown,
        CopyFormat::SqlInsert,
    ];
}

/// What is selected: rows in display order, columns of the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub rows: RangeInclusive<usize>,
    /// Indexes into the result's columns, left to right as shown.
    pub columns: Vec<usize>,
}

/// Asks whoever owns the grid to sort on the server: a column of the
/// result and whether descending, or `None` for the server's own order.
pub type SortOnServer = dyn Fn(Option<(usize, bool)>, &mut Window, &mut App);

/// Runs, for the grid's owner, an entry of a menu that is the owner's
/// (editing, filters, export…), on the cell at a row and a column of the
/// grid.
pub type OnCell = dyn Fn(GridAction, usize, usize, &mut Window, &mut App);

/// Labels of the entries the owner has for a row: where its foreign keys
/// lead, in the order [`GridAction::Follow`] numbers them.
pub type RelatedLabels = dyn Fn(&Grid, usize) -> Vec<String>;

/// What the owner of a grid adds to its menus.
pub struct Owner {
    pub act: Rc<OnCell>,
    pub related: Option<Rc<RelatedLabels>>,
    /// Why the rows cannot be exported, when they cannot.
    pub export: Option<&'static str>,
    /// The rows were read through a filter (a table's data).
    pub filtered: bool,
    /// The rows have no key to find them by (Generate SQL ▸ can only
    /// insert them).
    pub keyless: bool,
    /// The table the rows are from, when they are one table's: the
    /// columns panel and the enum editor read it.
    pub structure: Option<Arc<TableStructure>>,
}

/// What a column of a table's page takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnEdit {
    /// Its cells take a value (a computed column does not).
    pub writable: bool,
    pub nullable: bool,
}

impl ColumnEdit {
    /// What a column showing the table column `name` takes: nothing when
    /// it shows none, or one the server computes (a generated column, an
    /// identity it always makes).
    pub fn of(structure: &TableStructure, name: Option<&str>) -> Self {
        let info = name.and_then(|name| structure.columns.iter().find(|info| info.name == name));
        Self {
            writable: info.is_some_and(|info| {
                !matches!(
                    info.generation,
                    Some(
                        ColumnGeneration::Stored(_)
                            | ColumnGeneration::Virtual(_)
                            | ColumnGeneration::IdentityAlways
                    )
                )
            }),
            nullable: info.is_some_and(|info| info.nullable),
        }
    }
}

/// The table the rows of a grid are written to.
pub struct EditTarget {
    pub table: TableRef,
    pub structure: Arc<TableStructure>,
    /// The columns that address a row.
    pub identity: Vec<String>,
    /// The table column each column of the result shows: `None` for one
    /// written nowhere (an expression, a column shown a second time).
    pub names: Vec<Option<String>>,
}

/// An entry of the menus of a cell or a column header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GridAction {
    Copy,
    CopyAs(CopyFormat),
    CopyColumnName,
    Paste,
    /// Keeps the rows with this cell's value (`IS NULL` for NULL).
    FilterByValue,
    /// Keeps the rows without it (`IS NOT NULL` for NULL).
    ExcludeValue,
    /// Adds a condition with this operator and the cell's value to the
    /// filter bar, not applied.
    FilterWith(Operator),
    /// Adds an empty condition on the column to the filter bar.
    FilterColumn,
    /// Lists the column's values with their counts, to filter on some.
    DistinctValues,
    ClearFilters,
    /// Ascending (`Some(false)`), descending, or the server's own order.
    Order(Option<bool>),
    /// The n-th place a foreign key of the row leads to, and its label.
    Follow(usize, String),
    ViewValue,
    EditCell,
    SetNull,
    SetDefault,
    /// Copies the values of the row above (`true`) or below the selection.
    CopyFromRow(bool),
    DuplicateRow,
    DeleteRow,
    RevertRow,
    /// Pins the column, or unpins it when it is (`true`).
    Pin(bool),
    HideColumn,
    /// Shows every column; how many are hidden.
    ShowAll(usize),
    FitWidth,
    FitAllWidths,
    Export,
    /// Writes statements for the selected rows into a query tab.
    Generate(RowSql),
    /// The numbers of the selected rows, one per line.
    CopyRowNumbers,
    AddRow,
    /// Reads the rows again.
    Refresh,
    /// Edits the cell in the value panel.
    EditValue,
    /// One row as a list of its columns, or the grid again (`true` while
    /// the list shows).
    RecordView(bool),
    /// The figures of the selection in the footer, on or off (`true` while
    /// on).
    SelectionStats(bool),
    /// Larger text (`1`), smaller (`-1`), the usual size (`0`).
    Zoom(i8),
    Panel(Panel),
    /// Asks for a row number and selects that row.
    GoToRow,
    /// Lists the columns to select one.
    GoToColumn,
}

/// Where "First / Previous / Next / Last row" moves the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowMove {
    First,
    Previous,
    Next,
    Last,
}

/// The row and the column of the grid a selection starts at: what the
/// record view shows, where the row commands move from.
pub fn current(selection: TableSelection) -> (usize, usize) {
    match selection {
        TableSelection::Cell(row, column) => (row, column),
        TableSelection::Row(row) => (row, 1),
        TableSelection::Column(column) => (0, column),
        TableSelection::None => (0, 1),
    }
}

/// A panel about the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// Count, sum, average, minimum, maximum of the selection.
    Aggregate,
    /// The result's columns: type, table, key, nullability.
    Columns,
}

impl GridAction {
    pub fn label(&self) -> String {
        match self {
            Self::Copy => "Copy",
            Self::CopyAs(CopyFormat::Tsv) => "TSV",
            Self::CopyAs(CopyFormat::TsvWithHeader) => "With column names",
            Self::CopyAs(CopyFormat::Csv) => "CSV",
            Self::CopyAs(CopyFormat::Json) => "JSON",
            Self::CopyAs(CopyFormat::Markdown) => "Markdown",
            Self::CopyAs(CopyFormat::SqlInsert) => "SQL INSERT",
            Self::CopyColumnName => "Copy column name",
            Self::Paste => "Paste",
            Self::FilterByValue => "Filter by this value",
            Self::ExcludeValue => "Exclude this value",
            Self::FilterWith(operator) => return format!("{} this value…", operator.label()),
            Self::FilterColumn => "Filter on this column…",
            Self::DistinctValues => "Distinct values…",
            Self::ClearFilters => "Clear filters",
            Self::Order(Some(false)) => "Ascending",
            Self::Order(Some(true)) => "Descending",
            Self::Order(None) => "Clear order",
            Self::Follow(_, label) => return label.clone(),
            Self::ViewValue => "View value",
            Self::EditCell => "Edit cell",
            Self::SetNull => "Set NULL",
            Self::SetDefault => "Set default",
            Self::CopyFromRow(true) => "Copy from row above",
            Self::CopyFromRow(false) => "Copy from row below",
            Self::DuplicateRow => "Duplicate row",
            Self::DeleteRow => "Delete row",
            Self::RevertRow => "Revert row",
            Self::Pin(false) => "Pin column",
            Self::Pin(true) => "Unpin column",
            Self::HideColumn => "Hide column",
            Self::ShowAll(0) => "Show all columns",
            Self::ShowAll(hidden) => return format!("Show all columns ({hidden} hidden)"),
            Self::FitWidth => "Fit width",
            Self::FitAllWidths => "Fit all widths",
            Self::Export => "Export…",
            Self::Generate(kind) => kind.label(),
            Self::CopyRowNumbers => "Copy row numbers",
            Self::AddRow => "Add row",
            Self::Refresh => "Refresh",
            Self::EditValue => "Edit in value panel",
            Self::RecordView(_) => "Record view",
            Self::SelectionStats(_) => "Selection statistics",
            Self::Zoom(1..) => "Larger text",
            Self::Zoom(..0) => "Smaller text",
            Self::Zoom(0) => "Usual text size",
            Self::Panel(Panel::Aggregate) => "Aggregate",
            Self::Panel(Panel::Columns) => "Columns",
            Self::GoToRow => "Go to row…",
            Self::GoToColumn => "Go to column…",
        }
        .to_string()
    }
}

/// One line of a grid menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// An action, why it cannot be used now when it cannot, and whether it
    /// is ticked (the current order).
    Item(GridAction, Option<&'static str>, bool),
    Submenu(&'static str, Vec<Entry>),
    Separator,
}

fn enabled(action: GridAction) -> Entry {
    Entry::Item(action, None, false)
}

fn item(action: GridAction, why_not: Option<&'static str>) -> Entry {
    Entry::Item(action, why_not, false)
}

/// What the menu of a cell depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellFacts {
    /// A table's data, not a query result.
    pub data: bool,
    /// The rows of a query result that take changes, as a table's data.
    pub editable: bool,
    /// Why the rows take no change: read-only connection, view, no key.
    pub read_only: Option<&'static str>,
    pub kind: TypeKind,
    pub null: bool,
    /// The column takes no value.
    pub computed: bool,
    pub nullable: bool,
    /// How the row differs from what was fetched.
    pub row: Mark,
    /// The selection holds more than this cell.
    pub many: bool,
    pub clipboard: bool,
    pub column: HeaderFacts,
    /// Where the row's foreign keys lead.
    pub related: Vec<String>,
    /// Why the rows cannot be exported.
    pub export: Option<&'static str>,
    /// The rows have no key to find them by.
    pub keyless: bool,
    /// The record view shows.
    pub record: bool,
    /// The footer shows the figures of the selection.
    pub stats: bool,
}

/// What the menu of a column header depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderFacts {
    pub data: bool,
    /// The column's order: descending or not.
    pub sort: Option<bool>,
    /// Rows are still arriving: they cannot be put in order yet.
    pub loading: bool,
    pub pinned: bool,
    pub hidden: usize,
    pub filtered: bool,
    /// The server filters the rows: a table's data, or the result of a
    /// SELECT that it reads again with the filter.
    pub filters: bool,
}

/// "Order by this column ▸", the current order ticked.
fn order_menu(column: &HeaderFacts) -> Entry {
    let arriving = column.loading.then_some("rows still arriving");
    Entry::Submenu(
        "Order by this column",
        vec![
            Entry::Item(
                GridAction::Order(Some(false)),
                arriving,
                column.sort == Some(false),
            ),
            Entry::Item(
                GridAction::Order(Some(true)),
                arriving,
                column.sort == Some(true),
            ),
            item(
                GridAction::Order(None),
                arriving.or(column.sort.is_none().then_some("not ordered")),
            ),
        ],
    )
}

/// The menu of a cell, in DBeaver's order: copy and paste, filters, order,
/// foreign keys, the value and its edits, the row, the column, export. The
/// row's and the column's entries sit in submenus so the menu fits a
/// window. What a query result never has is left out; what this cell does
/// not allow is there, with the reason.
pub fn cell_menu(facts: &CellFacts) -> Vec<Entry> {
    use GridAction as A;
    let read_only = facts.read_only;
    let deleted = (facts.row == Mark::Deleted).then_some("row deleted");
    let computed = facts.computed.then_some("computed column");
    let binary = (facts.kind == TypeKind::Binary).then_some("binary value");
    // With several cells selected, the cells that take no value are skipped.
    let this_cell = |reason: Option<&'static str>| if facts.many { None } else { reason };
    let takes_value = read_only.or(this_cell(deleted.or(computed)));
    let no_filter = binary.or((facts.kind == TypeKind::Json).then_some("JSON value"));
    let copy_as = CopyFormat::COPY_AS
        .into_iter()
        .map(|format| enabled(A::CopyAs(format)))
        .collect();
    let mut entries = vec![enabled(A::Copy), Entry::Submenu("Copy as", copy_as)];
    let edits = facts.data || facts.editable;
    if edits {
        let paste = read_only
            .or((!facts.clipboard).then_some("clipboard empty"))
            .or(this_cell(deleted.or(computed).or(binary)));
        entries.push(item(A::Paste, paste));
    }
    if facts.data || facts.column.filters {
        let operators = [
            Operator::Greater,
            Operator::GreaterOrEqual,
            Operator::Less,
            Operator::LessOrEqual,
            Operator::Contains,
            Operator::Like,
            Operator::NotLike,
            Operator::In,
        ];
        let with_value = no_filter.or(facts.null.then_some("NULL value"));
        // DBeaver's Filter ▸ (`fillFiltersMenu`): the operators on this
        // value, the column's distinct values, and clearing.
        let mut filter: Vec<Entry> = operators
            .into_iter()
            .map(|operator| item(A::FilterWith(operator), with_value))
            .collect();
        filter.extend([
            Entry::Separator,
            item(A::DistinctValues, no_filter),
            item(
                A::ClearFilters,
                (!facts.column.filtered).then_some("no filter"),
            ),
        ]);
        entries.extend([
            Entry::Separator,
            item(A::FilterByValue, no_filter),
            item(A::ExcludeValue, no_filter),
            Entry::Submenu("Filter", filter),
        ]);
    }
    entries.extend([Entry::Separator, order_menu(&facts.column), navigate_menu()]);
    if !facts.related.is_empty() {
        entries.push(Entry::Separator);
        entries.extend(
            facts
                .related
                .iter()
                .enumerate()
                .map(|(index, label)| enabled(A::Follow(index, label.clone()))),
        );
    }
    entries.extend([Entry::Separator, enabled(A::ViewValue)]);
    if edits {
        let revert = match facts.row {
            Mark::Fetched => Some("no change"),
            Mark::New => Some("new row: delete it"),
            Mark::Edited | Mark::Deleted => None,
        };
        let typed = read_only.or(deleted).or(computed).or(binary);
        entries.extend([
            item(A::EditCell, typed),
            item(A::EditValue, typed),
            item(
                A::SetNull,
                takes_value.or(this_cell((!facts.nullable).then_some("NOT NULL"))),
            ),
            item(A::SetDefault, takes_value),
            Entry::Separator,
            Entry::Submenu(
                "Row",
                vec![
                    item(A::DuplicateRow, read_only),
                    item(A::DeleteRow, read_only.or(deleted)),
                    item(A::RevertRow, read_only.or(revert)),
                    item(A::CopyFromRow(true), takes_value),
                    item(A::CopyFromRow(false), takes_value),
                ],
            ),
        ]);
    }
    entries.extend([
        Entry::Separator,
        Entry::Submenu(
            "Column",
            vec![
                enabled(A::CopyColumnName),
                enabled(A::Pin(facts.column.pinned)),
                enabled(A::HideColumn),
                item(
                    A::ShowAll(facts.column.hidden),
                    (facts.column.hidden == 0).then_some("none hidden"),
                ),
            ],
        ),
        layout_menu(facts),
        panels_menu(),
        Entry::Separator,
        generate_menu(facts),
        item(A::Export, facts.export),
    ]);
    // DBeaver ends the menu with `file.refresh`.
    if facts.data {
        entries.extend([Entry::Separator, enabled(A::Refresh)]);
    }
    entries
}

/// "Layout ▸" (DBeaver's `fillLayoutMenu`): the record view, the figures of
/// the selection, the text size.
fn layout_menu(facts: &CellFacts) -> Entry {
    use GridAction as A;
    Entry::Submenu(
        "Layout",
        vec![
            Entry::Item(A::RecordView(facts.record), None, facts.record),
            Entry::Item(A::SelectionStats(facts.stats), None, facts.stats),
            Entry::Separator,
            enabled(A::Zoom(1)),
            enabled(A::Zoom(-1)),
            enabled(A::Zoom(0)),
        ],
    )
}

/// "Navigate ▸" (DBeaver's `fillNavigateMenu`, after the order): go to a
/// row or a column; the foreign keys have their own entries.
fn navigate_menu() -> Entry {
    Entry::Submenu(
        "Navigate",
        vec![
            enabled(GridAction::GoToRow),
            enabled(GridAction::GoToColumn),
        ],
    )
}

/// "Panels ▸" (DBeaver's `PanelsMenuContributor`); the value has its own
/// entry.
fn panels_menu() -> Entry {
    Entry::Submenu(
        "Panels",
        vec![
            enabled(GridAction::Panel(Panel::Aggregate)),
            enabled(GridAction::Panel(Panel::Columns)),
        ],
    )
}

/// The menu of a row number (DBeaver's `ROW_HEADER` entries of
/// `ResultSetViewer.fillContextMenu`): its number, the row's copies and
/// edits, export.
pub fn row_menu(facts: &CellFacts) -> Vec<Entry> {
    use GridAction as A;
    let copy_as = CopyFormat::COPY_AS
        .into_iter()
        .map(|format| enabled(A::CopyAs(format)))
        .collect();
    let mut entries = vec![
        enabled(A::CopyRowNumbers),
        Entry::Separator,
        enabled(A::Copy),
        Entry::Submenu("Copy as", copy_as),
    ];
    if facts.data || facts.editable {
        let read_only = facts.read_only;
        let deleted = (facts.row == Mark::Deleted).then_some("row deleted");
        let revert = match facts.row {
            Mark::Fetched => Some("no change"),
            Mark::New => Some("new row: delete it"),
            Mark::Edited | Mark::Deleted => None,
        };
        entries.extend([
            Entry::Separator,
            item(A::AddRow, read_only),
            item(A::DuplicateRow, read_only),
            item(A::DeleteRow, read_only.or(deleted)),
            item(A::RevertRow, read_only.or(revert)),
        ]);
    }
    if facts.data {
        entries.extend([
            Entry::Separator,
            item(
                A::ClearFilters,
                (!facts.column.filtered).then_some("no filter"),
            ),
        ]);
    }
    entries.extend([
        Entry::Separator,
        layout_menu(facts),
        Entry::Separator,
        generate_menu(facts),
        item(A::Export, facts.export),
    ]);
    if facts.data {
        entries.extend([Entry::Separator, enabled(A::Refresh)]);
    }
    entries
}

/// "Generate SQL ▸" for the selected rows of a table (DBeaver's
/// `SQLGeneratorContributor` on result rows), or of a result that is one
/// table's rows; a read-only result says why. Rows added and not applied
/// yet are skipped; on their own, nothing is written.
fn generate_menu(facts: &CellFacts) -> Entry {
    let result =
        (!facts.data && !facts.editable).then(|| facts.read_only.unwrap_or("query result"));
    let new = (!facts.many && facts.row == Mark::New).then_some("new row");
    let no_key = facts.keyless.then_some("no key");
    Entry::Submenu(
        "Generate SQL",
        RowSql::ALL
            .into_iter()
            .map(|kind| {
                let key = if kind.needs_key() { no_key } else { None };
                item(GridAction::Generate(kind), result.or(new).or(key))
            })
            .collect(),
    )
}

/// The menu of a column header.
pub fn header_menu(facts: &HeaderFacts) -> Vec<Entry> {
    use GridAction as A;
    let mut entries = vec![
        enabled(A::CopyColumnName),
        Entry::Separator,
        order_menu(facts),
    ];
    if facts.data || facts.filters {
        entries.extend([enabled(A::FilterColumn), enabled(A::DistinctValues)]);
    }
    entries.extend([
        Entry::Separator,
        enabled(A::Pin(facts.pinned)),
        enabled(A::HideColumn),
        item(
            A::ShowAll(facts.hidden),
            (facts.hidden == 0).then_some("none hidden"),
        ),
        Entry::Separator,
        enabled(A::FitWidth),
        enabled(A::FitAllWidths),
    ]);
    entries
}

/// Runs the entry chosen in a grid menu.
type Pick = Rc<dyn Fn(GridAction, &mut Window, &mut App)>;

/// Fills `menu` with `entries`; `pick` runs the chosen one.
fn fill_menu(
    mut menu: PopupMenu,
    entries: &[Entry],
    pick: &Pick,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    for entry in entries {
        menu = match entry {
            Entry::Separator => menu.separator(),
            Entry::Submenu(label, items) => {
                let (items, pick) = (items.clone(), pick.clone());
                menu.submenu(*label, window, cx, move |menu, window, cx| {
                    fill_menu(menu, &items, &pick, window, cx)
                })
            }
            Entry::Item(action, why_not, checked) => {
                // An entry that cannot be used says why.
                let label = match why_not {
                    Some(reason) => format!("{} ({reason})", action.label()),
                    None => action.label(),
                };
                let (pick, action) = (pick.clone(), action.clone());
                menu.item(
                    PopupMenuItem::new(SharedString::from(label))
                        .checked(*checked)
                        .disabled(why_not.is_some())
                        .on_click(move |_, window, cx| pick(action.clone(), window, cx)),
                )
            }
        };
    }
    menu
}

/// The cells of a block of text as a spreadsheet copies it: rows on lines,
/// cells split by tabs; the line break that ends the block is not a row.
// ponytail: no quoting, so a value holding a tab or a line break splits;
// read quoted TSV if spreadsheets that quote turn up.
fn text_block(text: &str) -> Vec<Vec<&str>> {
    let text = text
        .strip_suffix('\n')
        .map_or(text, |text| text.strip_suffix('\r').unwrap_or(text));
    text.split('\n')
        .map(|line| {
            line.strip_suffix('\r')
                .unwrap_or(line)
                .split('\t')
                .collect()
        })
        .collect()
}

/// What putting `typed` in a cell of `kind` that showed `initial` gives:
/// nothing when the text is unchanged; emptied text is NULL, except in a
/// text column, where it is the empty string.
pub(super) fn typed_value(typed: &str, initial: &str, kind: TypeKind) -> Option<CellValue> {
    if typed == initial {
        None
    } else if typed.is_empty() && kind != TypeKind::Text {
        Some(CellValue::Null)
    } else {
        Some(CellValue::Text(typed.to_string()))
    }
}

/// The numbers of rows of the grid as the row-number column shows them, one
/// per line (DBeaver's `grid.copyRowNames`, `ResultSetHandlerMain`).
pub fn row_numbers(rows: &RangeInclusive<usize>) -> String {
    rows.clone()
        .map(|row| (row + 1).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a click on a boolean cell makes of it: true becomes false, false
/// becomes NULL when the column takes it (true when it does not), NULL and
/// anything else become true. Ported from DBeaver's
/// `SpreadsheetPresentation.toggleBooleanValue` (plugin ui.editors.data).
pub fn next_bool(current: Option<bool>, nullable: bool) -> Option<bool> {
    match current {
        Some(true) => Some(false),
        Some(false) if nullable => None,
        _ => Some(true),
    }
}

/// A boolean as `column` holds it: `1`/`0` in MySQL's `TINYINT(1)` and
/// `BIT(1)`, `t`/`f` elsewhere, so that toggling twice is no change.
pub fn bool_text(column: &kerno_core::Column, value: bool) -> &'static str {
    let numeric = ["tinyint", "bit"]
        .iter()
        .any(|prefix| column.type_name.to_ascii_lowercase().starts_with(prefix));
    match (numeric, value) {
        (true, true) => "1",
        (true, false) => "0",
        (false, true) => "t",
        (false, false) => "f",
    }
}

/// What is open in a cell being edited.
pub(super) enum Editor {
    /// Typed text.
    Text(Entity<InputState>),
    /// A list of the column's values.
    Choice(Entity<PopupMenu>),
}

/// The cell being typed in.
struct Editing {
    row: usize,
    /// Column of the result.
    column: usize,
    /// What the cell held when the edit began.
    initial: String,
    editor: Editor,
    _subscription: Subscription,
}

/// What a table's data view, or an editable result, adds to the rows:
/// where changes go, changes waiting to be applied, and the cell being
/// typed in.
pub struct Edits {
    pub pending: Pending,
    /// What each column of the result takes.
    columns: Vec<ColumnEdit>,
    structure: Arc<TableStructure>,
    identity: Vec<String>,
    /// The result's columns under the names of the table columns they
    /// show (empty: none), when some differ from the result's labels.
    renamed: Option<Arc<[kerno_core::Column]>>,
    editing: Option<Editing>,
}

impl Edits {
    fn new(columns: &[kerno_core::Column], target: EditTarget) -> Self {
        let EditTarget {
            structure,
            identity,
            names,
            ..
        } = target;
        let differs = columns
            .iter()
            .zip(&names)
            .any(|(column, name)| name.as_deref() != Some(column.name.as_str()));
        let renamed = differs.then(|| {
            columns
                .iter()
                .zip(&names)
                .map(|(column, name)| kerno_core::Column {
                    name: name.clone().unwrap_or_default(),
                    ..column.clone()
                })
                .collect()
        });
        Self {
            pending: Pending::default(),
            columns: names
                .iter()
                .map(|name| ColumnEdit::of(&structure, name.as_deref()))
                .collect(),
            structure,
            identity,
            renamed,
            editing: None,
        }
    }
}

/// How a cell differs from what was fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Fetched,
    Edited,
    /// Its row is to be deleted.
    Deleted,
    /// Its row is new.
    New,
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
    /// The table the rows are from, when they are one table's: `INSERT`s
    /// name it.
    table: Option<TableRef>,
    /// Pending changes, in a table's data view.
    edits: Option<Edits>,
    /// Why a table's rows take no change, when they do not.
    read_only: Option<&'static str>,
    /// Sorting is the server's work (a page of a table), not the grid's.
    sort_on_server: Option<Rc<SortOnServer>>,
    /// Reads a result again in another order (a SELECT's): asked when not
    /// every row is here, as DBeaver's "smart" ordering
    /// (`ResultSetViewer.toggleSortOrder`); the rows here are sorted here.
    reorder: Option<Rc<SortOnServer>>,
    /// The rows came from the server in the order `sort` says.
    server_sorted: bool,
    owner: Option<Owner>,
    /// One row as a list of its columns instead of the grid.
    pub record: bool,
    /// The figures of the selection, made when first shown.
    stats: Option<Entity<super::grid_view::SelectionStats>>,
    /// The record view, made when first shown.
    pub(super) record_view: Option<Entity<super::grid_view::RecordView>>,
    /// Waits for the rows "Go to row" asked the server for.
    going_to: Option<Subscription>,
    /// Counts the changes to what the cells show (edits, order): the
    /// figures of a selection that did not move follow it.
    generation: u64,
    /// Widths kept from before the result was read again, by column name.
    widths: HashMap<String, Pixels>,
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
            table: None,
            edits: None,
            read_only: None,
            sort_on_server: None,
            reorder: None,
            server_sorted: false,
            owner: None,
            record: false,
            stats: None,
            record_view: None,
            going_to: None,
            generation: 0,
            widths: HashMap::new(),
        }
    }

    /// Lets the grid's owner run the entries of its menus that are its own.
    pub fn with_owner(mut self, owner: Owner) -> Self {
        self.owner = Some(owner);
        self
    }

    /// The rows of a SELECT that the server can read again filtered and in
    /// another order: `reorder` asks for an order while not every row is
    /// here; `sort` is the order the server gave these rows.
    pub fn with_server_order(
        mut self,
        reorder: Rc<SortOnServer>,
        sort: Option<(usize, bool)>,
    ) -> Self {
        self.reorder = Some(reorder);
        self.server_sorted = sort.is_some();
        self.sort = sort;
        self
    }

    /// Who put the rows in their order: the server (`true`) or the grid;
    /// `None` when they are in no chosen order.
    pub fn ordered_by_server(&self) -> Option<bool> {
        self.sort.map(|_| self.server_sorted)
    }

    /// Whether the rows can be put in another order now: once they stopped
    /// arriving, or by the server while it holds more.
    fn sortable(&self) -> bool {
        !self.loading() || (self.reorder.is_some() && self.has_more())
    }

    /// One page of `table`, fetched in full and sorted by the server as
    /// `sort` says. With a target, cells can be edited and the changes wait
    /// in [`Edits::pending`]; otherwise the rows are read-only for the
    /// reason given.
    pub fn page(
        store: ResultStore,
        session: Arc<dyn Session>,
        table: TableRef,
        sort: Option<(usize, bool)>,
        target: Result<EditTarget, &'static str>,
        sort_on_server: Rc<SortOnServer>,
    ) -> Self {
        let layout = (0..store.columns().len()).collect();
        Self {
            requested: store.row_count() as u64,
            page: 0,
            complete: true,
            demand: None,
            layout,
            pinned: 0,
            sort,
            order: None,
            sorting: None,
            extent: None,
            all: false,
            anchor: TableSelection::None,
            session,
            table: Some(table),
            read_only: target.as_ref().err().copied(),
            edits: target
                .ok()
                .map(|target| Edits::new(store.columns(), target)),
            sort_on_server: Some(sort_on_server),
            reorder: None,
            server_sorted: false,
            owner: None,
            record: false,
            stats: None,
            record_view: None,
            going_to: None,
            generation: 0,
            widths: HashMap::new(),
            store,
        }
    }

    pub fn store(&self) -> &ResultStore {
        &self.store
    }

    /// Makes the rows of a result editable, as rows of `target`'s table,
    /// or says in a few words why they are not. A result put in order here
    /// goes back to the server's order: pending changes are kept by row as
    /// fetched, and an editable result is ordered by the server.
    pub fn set_edit_target(&mut self, target: Result<EditTarget, &'static str>) {
        match target {
            Ok(target) => {
                if self.order.is_some() || self.sorting.is_some() {
                    self.order = None;
                    self.sorting = None;
                    self.sort = self.sort.filter(|_| self.server_sorted);
                }
                self.table = Some(target.table.clone());
                self.read_only = None;
                self.edits = Some(Edits::new(self.store.columns(), target));
            }
            Err(why) => {
                self.read_only = Some(why);
                self.edits = None;
            }
        }
        self.generation += 1;
    }

    /// The rows as rows of the table they are written to: the result's
    /// columns under the names of the table's columns.
    fn edit_store(&self) -> ResultStore {
        match self.edits.as_ref().and_then(|edits| edits.renamed.clone()) {
            Some(columns) => self.store.renamed(columns),
            None => self.store.clone(),
        }
    }

    /// The table column a column of the result shows, when it is written
    /// to one.
    pub fn table_column(&self, source: usize) -> Option<&ColumnInfo> {
        let edits = self.edits.as_ref()?;
        let name = match &edits.renamed {
            Some(columns) => &columns.get(source)?.name,
            None => &self.store.columns().get(source)?.name,
        };
        edits
            .structure
            .columns
            .iter()
            .find(|info| &info.name == name)
    }

    /// Whether a column of the result shows a column of the key.
    pub fn is_key(&self, source: usize) -> bool {
        self.table_column(source).is_some_and(|info| {
            self.edits
                .as_ref()
                .is_some_and(|edits| edits.identity.contains(&info.name))
        })
    }

    /// The columns of the result whose values the server makes, or that
    /// show no column of the table: a copied row leaves them out.
    pub fn generated_columns(&self) -> Vec<usize> {
        (0..self.store.columns().len())
            .filter(|&source| {
                self.table_column(source)
                    .is_none_or(|info| info.generation.is_some())
            })
            .collect()
    }

    /// The statements the pending changes become, and the changes.
    pub fn pending_sql(&self) -> kerno_core::Result<(Vec<RowChange>, Vec<String>)> {
        let (Some(edits), Some(table)) = (&self.edits, &self.table) else {
            return Ok((Vec::new(), Vec::new()));
        };
        let changes = edits.pending.changes(&self.edit_store(), &edits.identity)?;
        let statements = edit_statements(
            self.session.dialect(),
            table,
            &edits.structure.columns,
            &changes,
        )?;
        Ok((changes, statements))
    }

    /// `kind` statements for the fetched rows `rows` of the result (rows of
    /// the store), with their values as literals.
    pub fn rows_sql(&self, kind: RowSql, rows: &[usize]) -> kerno_core::Result<String> {
        let (Some(table), Some(structure)) = (&self.table, self.structure()) else {
            return Err(kerno_core::Error::Config(
                "these rows are no table's".into(),
            ));
        };
        let identity = match &self.edits {
            Some(edits) => edits.identity.clone(),
            None => structure.row_identity().unwrap_or_default(),
        };
        rows_sql(
            kind,
            self.session.dialect(),
            table,
            &structure.columns,
            &identity,
            &self.edit_store(),
            rows,
        )
    }

    /// The pending changes of a data view.
    pub fn pending(&self) -> Option<&Pending> {
        self.edits.as_ref().map(|edits| &edits.pending)
    }

    /// Changes the pending changes, when the grid takes some.
    pub fn edit(&mut self, change: impl FnOnce(&mut Pending, &ResultStore)) {
        if let Some(edits) = &mut self.edits {
            edits.editing = None;
            change(&mut edits.pending, &self.store);
            self.generation += 1;
        }
    }

    /// Changes each time what the cells show changes, rows aside.
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    /// Rows shown: the fetched ones, then the new ones.
    pub fn total_rows(&self) -> usize {
        self.store.row_count() + self.pending().map_or(0, Pending::inserted)
    }

    /// What a cell holds as it shows: the pending value when there is one,
    /// else what was fetched. `None` for NULL (and for a default not known
    /// yet).
    pub(super) fn cell(&self, row: usize, source: usize) -> Option<&[u8]> {
        let fetched = self.store.row_count();
        match self
            .pending()
            .and_then(|pending| pending.value(fetched, row, source))
        {
            Some(CellValue::Text(text)) => Some(text.as_bytes()),
            Some(CellValue::Bytes(bytes)) => Some(bytes),
            Some(CellValue::Null | CellValue::Default) => None,
            None if row < fetched => self.store.cell(self.source_row(row), source),
            None => None,
        }
    }

    /// How a cell of the result differs from what was fetched.
    pub fn mark(&self, row: usize, source: usize) -> Mark {
        let fetched = self.store.row_count();
        match self.pending() {
            Some(pending) if pending.is_new(fetched, row) => Mark::New,
            Some(pending) if pending.is_deleted(row) => Mark::Deleted,
            Some(pending) if pending.value(fetched, row, source).is_some() => Mark::Edited,
            _ => Mark::Fetched,
        }
    }

    /// The text of a row's cell in the column called `name`; `None` for
    /// NULL, bytes, and a column the result does not have.
    pub fn text_of(&self, row: usize, name: &str) -> Option<String> {
        let columns = self.store.columns();
        let source = columns.iter().position(|column| column.name == name)?;
        if columns[source].kind == TypeKind::Binary || row >= self.total_rows() {
            return None;
        }
        Some(String::from_utf8_lossy(self.cell(row, source)?).into_owned())
    }

    /// Whether a cell of the result can be given a value: its column is
    /// not computed and its row is not on its way out.
    pub fn takes_value(&self, row: usize, source: usize) -> bool {
        self.edits.as_ref().is_some_and(|edits| {
            edits
                .columns
                .get(source)
                .is_some_and(|column| column.writable)
                && !edits.pending.is_deleted(row)
        })
    }

    /// Whether a column of the result takes NULL.
    pub fn nullable(&self, source: usize) -> bool {
        self.edits.as_ref().is_some_and(|edits| {
            edits
                .columns
                .get(source)
                .is_some_and(|column| column.nullable)
        })
    }

    /// Whether a cell of the grid can be typed in, and if not, why.
    pub fn editable(&self, row: usize, column: usize) -> Result<(), &'static str> {
        let Some(edits) = &self.edits else {
            return Err("These rows cannot be edited.");
        };
        let Some(source) = self.source_column(column) else {
            return Err("Row numbers are not data.");
        };
        if row >= self.total_rows() {
            return Err("There is no such row.");
        }
        if edits.pending.is_deleted(row) {
            return Err("This row is marked for deletion.");
        }
        if !self.takes_value(row, source) {
            return Err("This column is computed by the server.");
        }
        if self.store.columns()[source].kind == TypeKind::Binary {
            return Err("Binary values are not typed in; they can be set to NULL.");
        }
        Ok(())
    }

    /// Starts typing in a cell of the grid. Returns whether it could.
    pub fn begin_edit(
        &mut self,
        row: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> bool {
        self.commit_edit(cx);
        if self.editable(row, column).is_err() {
            return false;
        }
        let Some(source) = self.source_column(column) else {
            return false;
        };
        // A boolean has no editor: editing it toggles it, as DBeaver's
        // checkbox does.
        if self.store.columns()[source].kind == TypeKind::Boolean {
            self.toggle(row, source);
            cx.notify();
            return true;
        }
        let initial = self.cell(row, source).map_or(String::new(), |bytes| {
            String::from_utf8_lossy(bytes).into_owned()
        });
        if let Some(choices) = self.choices(source) {
            let table = cx.entity();
            let current = self.cell(row, source).is_some().then(|| initial.clone());
            let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
                for choice in choices {
                    let (table, label) = (table.clone(), choice.clone());
                    menu = menu.item(
                        PopupMenuItem::new(SharedString::from(
                            label.clone().unwrap_or_else(|| "NULL".into()),
                        ))
                        .checked(label == current)
                        .on_click(move |_, window, cx| {
                            table.update(cx, |table, cx| {
                                let value = label.clone().map_or(CellValue::Null, CellValue::Text);
                                table.delegate_mut().choose(row, source, value);
                                table.focus_handle(cx).focus(window, cx);
                                table.refresh(cx);
                                cx.notify();
                            })
                        }),
                    );
                }
                menu
            });
            let subscription =
                cx.subscribe_in(&menu, window, |table, _, _: &DismissEvent, window, cx| {
                    table.delegate_mut().cancel_edit();
                    table.focus_handle(cx).focus(window, cx);
                    cx.notify();
                });
            menu.update(cx, |menu, cx| menu.focus_handle(cx).focus(window, cx));
            if let Some(edits) = &mut self.edits {
                edits.editing = Some(Editing {
                    row,
                    column: source,
                    initial,
                    editor: Editor::Choice(menu),
                    _subscription: subscription,
                });
            }
            cx.notify();
            return true;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial.clone()));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |table, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    table.delegate_mut().commit_edit(cx);
                    table.focus_handle(cx).focus(window, cx);
                    cx.notify();
                }
                // Clicking elsewhere keeps what was typed.
                InputEvent::Blur => {
                    table.delegate_mut().commit_edit(cx);
                    cx.notify();
                }
                _ => {}
            },
        );
        // The value is selected: typing replaces it, an arrow key edits it.
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        if let Some(edits) = &mut self.edits {
            edits.editing = Some(Editing {
                row,
                column: source,
                initial,
                editor: Editor::Text(input),
                _subscription: subscription,
            });
        }
        cx.notify();
        true
    }

    /// The values a cell of an enum column is chosen from, NULL (`None`)
    /// last when the column takes it; `None` for other columns.
    pub fn choices(&self, source: usize) -> Option<Vec<Option<String>>> {
        let info = self.table_column(source)?;
        if info.enum_values.is_empty() {
            return None;
        }
        let mut choices: Vec<_> = info.enum_values.iter().cloned().map(Some).collect();
        if self.nullable(source) {
            choices.push(None);
        }
        Some(choices)
    }

    /// Gives a cell a value chosen from its list, as one change.
    pub fn choose(&mut self, row: usize, source: usize, value: CellValue) {
        self.edit(|pending, store| pending.step(|pending| pending.set(store, row, source, value)));
    }

    /// Toggles a boolean cell (see [`next_bool`]); a selection of several
    /// rows that holds it takes the same value in its column, as in
    /// DBeaver.
    pub fn toggle(&mut self, row: usize, source: usize) {
        let column = &self.store.columns()[source];
        let current = self
            .cell(row, source)
            .and_then(kerno_core::format::parse_bool);
        let value = match next_bool(current, self.nullable(source)) {
            Some(value) => CellValue::Text(bool_text(column, value).into()),
            None => CellValue::Null,
        };
        let rows = self
            .selected(self.anchor)
            .filter(|selected| selected.rows.contains(&row) && selected.columns.contains(&source))
            .map_or(row..=row, |selected| selected.rows);
        let rows: Vec<usize> = rows.filter(|&row| self.takes_value(row, source)).collect();
        self.edit(|pending, store| {
            pending.step(|pending| {
                for row in rows {
                    pending.set(store, row, source, value.clone());
                }
            })
        });
    }

    /// Keeps what was typed as a pending change. Text left as it was is no
    /// change; emptied text is NULL, except in a text column, where it is
    /// the empty string.
    pub fn commit_edit(&mut self, cx: &App) {
        let Some(edits) = &mut self.edits else {
            return;
        };
        let Some(editing) = edits.editing.take() else {
            return;
        };
        // A list sets its value when a choice is clicked.
        let Editor::Text(input) = &editing.editor else {
            return;
        };
        let typed = input.read(cx).value().to_string();
        let kind = self.store.columns()[editing.column].kind;
        if let Some(value) = typed_value(&typed, &editing.initial, kind) {
            let store = &self.store;
            edits
                .pending
                .step(|pending| pending.set(store, editing.row, editing.column, value));
            self.generation += 1;
        }
    }

    /// Drops what is being typed.
    pub fn cancel_edit(&mut self) {
        if let Some(edits) = &mut self.edits {
            edits.editing = None;
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

    pub fn row_count(&self) -> usize {
        self.store.row_count()
    }

    /// Shows the whole value of the selected cell in the value panel,
    /// editable when the cell takes typed values (Shift+Enter, DBeaver's
    /// `row.edit`).
    pub fn view_selected_value(
        table: &Entity<TableState<Self>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some((row, column)) = table.read(cx).selected_cell() {
            Self::open_value(table, row, column, true, window, cx);
        }
    }

    /// Shows one row as a list of its columns, or the grid again, on the
    /// same row (Tab, DBeaver's `toggleMode`).
    pub fn toggle_record(table: &Entity<TableState<Self>>, window: &mut Window, cx: &mut App) {
        table.update(cx, |table, cx| {
            let (row, column) = current(table.selection());
            let grid = table.delegate_mut();
            grid.commit_edit(cx);
            grid.record = !grid.record;
            if table.delegate().total_rows() > 0 {
                Self::select(table, row, column, cx);
            }
            table.focus_handle(cx).focus(window, cx);
            cx.notify();
        });
    }

    /// Moves the selection to another row, in the same column (DBeaver's
    /// `row.first` … `row.last`).
    pub fn move_row(table: &Entity<TableState<Self>>, to: RowMove, cx: &mut App) {
        table.update(cx, |table, cx| {
            let Some(last) = table.delegate().total_rows().checked_sub(1) else {
                return;
            };
            let (row, column) = current(table.selection());
            let row = match to {
                RowMove::First => 0,
                RowMove::Previous => row.saturating_sub(1),
                RowMove::Next => row + 1,
                RowMove::Last => last,
            };
            Self::select(table, row.min(last), column, cx);
        });
    }

    /// Selects row `number` (counted from 1) in the current column. A row
    /// the server has not sent yet is asked for and selected when it
    /// arrives. Returns why it cannot be reached, when it cannot.
    pub fn go_to_row(
        table: &Entity<TableState<Self>>,
        number: usize,
        cx: &mut App,
    ) -> Option<&'static str> {
        table.update(cx, |table, cx| {
            let row = number.checked_sub(1)?;
            let (_, column) = current(table.selection());
            let grid = table.delegate_mut();
            if row < grid.total_rows() {
                Self::select(table, row, column, cx);
                return None;
            }
            if !grid.loading() {
                return Some(if grid.sort_on_server.is_some() {
                    "There is no such row on this page."
                } else {
                    "The result has fewer rows."
                });
            }
            // As many rows as reach it, and the rest of their page, unless
            // they were asked for already.
            if let Some(demand) = &grid.demand
                && row as u64 >= grid.requested
            {
                let wanted = row as u64 + 1 - grid.requested + grid.page;
                grid.requested += wanted;
                demand.request_rows(wanted);
            }
            grid.going_to = Some(cx.observe_self(move |table, cx| {
                let grid = table.delegate();
                if row < grid.total_rows() || !grid.loading() {
                    let last = grid.total_rows().saturating_sub(1);
                    table.delegate_mut().going_to = None;
                    Self::select(table, row.min(last), column, cx);
                }
            }));
            None
        })
    }

    /// Selects a column of the grid on the current row and shows it.
    pub fn go_to_column(table: &Entity<TableState<Self>>, column: usize, cx: &mut App) {
        table.update(cx, |table, cx| {
            let (row, _) = current(table.selection());
            if table.delegate().total_rows() > 0 {
                Self::select(table, row, column, cx);
            }
            table.scroll_to_col(column, cx);
        });
    }

    /// Selects one cell (the whole row when no column shows) and scrolls
    /// to it.
    pub(super) fn select(
        table: &mut TableState<Self>,
        row: usize,
        column: usize,
        cx: &mut Context<TableState<Self>>,
    ) {
        let grid = table.delegate_mut();
        grid.clear_selection();
        let row = row.min(grid.total_rows().saturating_sub(1));
        let columns = grid.layout.len();
        if columns == 0 {
            table.set_selected_row(row, cx);
        } else {
            table.set_selected_cell(row, column.clamp(1, columns), cx);
        }
        let selection = table.selection();
        table.delegate_mut().set_anchor(selection);
        table.scroll_to_row(row, cx);
        cx.notify();
    }

    /// The columns shown, left to right, as columns of the result.
    pub fn layout(&self) -> &[usize] {
        &self.layout
    }

    /// What is open in a cell being edited, when it is this one.
    pub(super) fn editor_at(&self, row: usize, source: usize) -> Option<&Editor> {
        let editing = self.edits.as_ref()?.editing.as_ref()?;
        (editing.row == row && editing.column == source).then_some(&editing.editor)
    }

    /// The rows in the order shown, when sorted here.
    pub(super) fn order(&self) -> Option<Arc<[u32]>> {
        self.order.clone()
    }

    /// Whether changes wait to be applied.
    pub(super) fn has_pending(&self) -> bool {
        self.pending().is_some_and(|pending| !pending.is_empty())
    }

    /// The table the rows are from, when they are one table's.
    pub(super) fn structure(&self) -> Option<&Arc<TableStructure>> {
        if let Some(edits) = &self.edits {
            return Some(&edits.structure);
        }
        self.owner.as_ref()?.structure.as_ref()
    }

    /// The figures of the selection, made the first time they are asked.
    pub(super) fn stats_view(&mut self) -> &mut Option<Entity<super::grid_view::SelectionStats>> {
        &mut self.stats
    }

    pub(super) fn stats_entity(&self) -> Option<Entity<super::grid_view::SelectionStats>> {
        self.stats.clone()
    }

    /// Rows are still arriving, or can still be asked for.
    pub(super) fn has_more_coming(&self) -> bool {
        self.loading()
    }

    /// The table the rows are from.
    pub(super) fn table(&self) -> Option<&TableRef> {
        self.table.as_ref()
    }

    /// The session the rows came from.
    pub(super) fn session(&self) -> &Arc<dyn Session> {
        &self.session
    }

    /// Shows a cell's whole value. With `edit`, a cell that takes typed
    /// values is edited there: saving makes one pending change, as typing
    /// in the cell does.
    pub fn open_value(
        table: &Entity<TableState<Self>>,
        row: usize,
        column: usize,
        edit: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        let grid = table.read(cx).delegate();
        let Some((described, value)) = grid.value_at(row, column) else {
            return;
        };
        let (described, value) = (described.clone(), value.map(<[u8]>::to_vec));
        let save = grid
            .source_column(column)
            .filter(|_| edit && grid.editable(row, column).is_ok())
            .map(|source| {
                let table = table.clone();
                super::viewer::Save {
                    nullable: grid.nullable(source),
                    apply: Rc::new(move |value, _: &mut Window, cx: &mut App| {
                        table.update(cx, |table, cx| {
                            table.delegate_mut().choose(row, source, value);
                            table.refresh(cx);
                            cx.notify();
                        })
                    }),
                }
            });
        // The keyboard goes back to the grid, whether the panel was opened
        // from a key or from a menu.
        let back = table.read(cx).focus_handle(cx);
        super::viewer::edit(&described, value.as_deref(), save, back, window, cx);
    }

    /// Whether the server has, or may have, rows that were not asked for.
    pub fn has_more(&self) -> bool {
        !self.complete && self.demand.is_some() && self.store.row_count() as u64 >= self.requested
    }

    /// Whether a click on a header has the server read the result again
    /// in that order, rather than sorting the rows held here.
    pub fn sorts_on_server(&self) -> bool {
        self.sort_on_server.is_some()
            || (self.reorder.is_some() && (!self.complete || self.edits.is_some()))
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

    /// The cell being typed in (row, column of the result); for tests.
    #[cfg(test)]
    pub fn editing(&self) -> Option<(usize, usize)> {
        let editing = self.edits.as_ref()?.editing.as_ref()?;
        Some((editing.row, editing.column))
    }

    /// What the grid draws in its cells, row by row (column 0, the row
    /// numbers, left out); for tests.
    #[cfg(test)]
    pub fn shown(&self) -> Vec<Vec<String>> {
        (0..self.total_rows())
            .map(|row| {
                self.layout
                    .iter()
                    .map(|&column| {
                        display(
                            self.cell(row, column),
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
        let rows = self.total_rows();
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
            let cell = self.cell(*selected.rows.start(), selected.columns[0]);
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
            selected
                .columns
                .iter()
                .map(|&column| self.cell(row, column))
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
            // A statement does not say which table an INSERT would go to:
            // a placeholder the user replaces. A table's own rows do.
            Some(format) => grid.copy_text(
                &selected,
                format,
                grid.session.dialect(),
                grid.table
                    .clone()
                    .unwrap_or_else(|| TableRef::new("schema_name", "table_name")),
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
        (row < self.total_rows()).then(|| (&self.store.columns()[source], self.cell(row, source)))
    }

    /// What the menu of a column header depends on.
    fn header_facts(&self, column: usize) -> HeaderFacts {
        let source = self.source_column(column);
        HeaderFacts {
            data: self.sort_on_server.is_some(),
            sort: self
                .sort
                .filter(|(sorted, _)| Some(*sorted) == source)
                .map(|(_, descending)| descending),
            loading: !self.sortable(),
            pinned: self.is_pinned(column),
            hidden: self.hidden_count(),
            filtered: self.owner.as_ref().is_some_and(|owner| owner.filtered),
            filters: self.sort_on_server.is_some() || self.reorder.is_some(),
        }
    }

    /// What the menu of a cell of the grid depends on.
    fn cell_facts(
        &self,
        row: usize,
        column: usize,
        many: bool,
        clipboard: bool,
        cx: &App,
    ) -> CellFacts {
        let source = self.source_column(column).unwrap_or_default();
        let fetched = self.store.row_count();
        let row_mark = match self.pending() {
            Some(pending) if pending.is_new(fetched, row) => Mark::New,
            Some(pending) if pending.is_deleted(row) => Mark::Deleted,
            Some(pending)
                if (0..self.store.columns().len())
                    .any(|column| pending.value(fetched, row, column).is_some()) =>
            {
                Mark::Edited
            }
            _ => Mark::Fetched,
        };
        let owner = self.owner.as_ref();
        CellFacts {
            data: self.sort_on_server.is_some(),
            editable: self.sort_on_server.is_none() && self.edits.is_some(),
            read_only: self.read_only,
            kind: self.store.columns()[source].kind,
            null: self.cell(row, source).is_none(),
            computed: self.edits.as_ref().is_some_and(|edits| {
                !edits
                    .columns
                    .get(source)
                    .is_some_and(|column| column.writable)
            }),
            nullable: self.nullable(source),
            row: row_mark,
            many,
            clipboard,
            column: self.header_facts(column),
            related: owner
                .and_then(|owner| owner.related.clone())
                .map_or_else(Vec::new, |related| related(self, row)),
            export: owner.map_or(Some("not available"), |owner| owner.export),
            keyless: owner.is_some_and(|owner| owner.keyless),
            record: self.record,
            stats: crate::state::AppState::get(cx).settings.selection_stats,
        }
    }

    /// The pending values pasting `text` gives. One value goes in every
    /// selected cell; a block of rows and tab-separated cells goes from the
    /// top-left selected cell, down and to the right as the columns show,
    /// stopping at the last row and column. Cells that take no typed value
    /// are skipped; a value left as it was is no change.
    pub fn pasted(&self, selected: &Selected, text: &str) -> Vec<(usize, usize, CellValue)> {
        let block = text_block(text);
        let cells: Vec<(usize, usize, &str)> = match block.as_slice() {
            [line] if line.len() == 1 => selected
                .rows
                .clone()
                .flat_map(|row| {
                    selected
                        .columns
                        .iter()
                        .map(move |&source| (row, source, line[0]))
                })
                .collect(),
            _ => {
                let top = *selected.rows.start();
                let left = self
                    .layout
                    .iter()
                    .position(|&source| Some(&source) == selected.columns.first())
                    .unwrap_or_default();
                block
                    .iter()
                    .zip(top..self.total_rows())
                    .flat_map(|(line, row)| {
                        line.iter()
                            .zip(&self.layout[left..])
                            .map(move |(value, &source)| (row, source, *value))
                    })
                    .collect()
            }
        };
        cells
            .into_iter()
            .filter(|&(row, source, _)| {
                self.takes_value(row, source)
                    && self.store.columns()[source].kind != TypeKind::Binary
            })
            .filter_map(|(row, source, typed)| {
                let initial = self.cell(row, source).map_or(String::new(), |bytes| {
                    String::from_utf8_lossy(bytes).into_owned()
                });
                let kind = self.store.columns()[source].kind;
                typed_value(typed, &initial, kind).map(|value| (row, source, value))
            })
            .collect()
    }

    /// Opens the menu of a cell of the grid. A right-click outside the
    /// selection selects the cell first: the entries act on the selection.
    fn cell_menu(
        table: &Entity<TableState<Self>>,
        row: usize,
        column: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let clipboard = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .is_some_and(|text| !text.is_empty());
        let entries = table.update(cx, |table, cx| {
            let source = table.delegate().source_column(column);
            let inside = |table: &TableState<Self>| {
                table
                    .delegate()
                    .selected(table.selection())
                    .is_some_and(|selected| {
                        selected.rows.contains(&row)
                            && source.is_some_and(|source| selected.columns.contains(&source))
                    })
            };
            if !inside(table) {
                table.delegate_mut().clear_selection();
                table.set_selected_cell(row, column, cx);
                let selection = table.selection();
                table.delegate_mut().set_anchor(selection);
            }
            let many = table
                .delegate()
                .selected(table.selection())
                .is_some_and(|selected| {
                    selected.rows.start() != selected.rows.end() || selected.columns.len() > 1
                });
            cell_menu(
                &table
                    .delegate()
                    .cell_facts(row, column, many, clipboard, cx),
            )
        });
        let pick = Self::pick(table, Some(row), column, cx);
        let menu = fill_menu(menu, &entries, &pick, window, cx);
        crate::tabs::fit_window(menu, window)
    }

    /// Opens the menu of a row number. A right-click outside the selected
    /// rows selects the row first.
    fn number_menu(
        table: &Entity<TableState<Self>>,
        row: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let entries = table.update(cx, |table, cx| {
            let selected = table.delegate().selected(table.selection());
            if !selected.is_some_and(|selected| selected.rows.contains(&row)) {
                table.delegate_mut().clear_selection();
                table.set_selected_row(row, cx);
                let selection = table.selection();
                table.delegate_mut().set_anchor(selection);
            }
            let many = table
                .delegate()
                .selected(table.selection())
                .is_some_and(|selected| selected.rows.start() != selected.rows.end());
            row_menu(&table.delegate().cell_facts(row, 1, many, false, cx))
        });
        let pick = Self::pick(table, Some(row), 1, cx);
        let menu = fill_menu(menu, &entries, &pick, window, cx);
        crate::tabs::fit_window(menu, window)
    }

    /// Opens the menu of a column header.
    fn column_menu(
        table: &Entity<TableState<Self>>,
        column: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let entries = header_menu(&table.read(cx).delegate().header_facts(column));
        let pick = Self::pick(table, None, column, cx);
        let menu = fill_menu(menu, &entries, &pick, window, cx);
        crate::tabs::fit_window(menu, window)
    }

    /// Runs an entry of the grid menus on the current cell, as its menu
    /// does: a command of the menu bar or the palette.
    pub fn perform(
        table: &Entity<TableState<Self>>,
        action: GridAction,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (row, column) = current(table.read(cx).selection());
        let pick = Self::pick(table, Some(row), column, cx);
        pick(action, window, cx);
    }

    /// What runs an entry of the menu of a cell (`row`) or of a header.
    pub(super) fn pick(
        table: &Entity<TableState<Self>>,
        row: Option<usize>,
        column: usize,
        cx: &App,
    ) -> Pick {
        let owner = table
            .read(cx)
            .delegate()
            .owner
            .as_ref()
            .map(|owner| owner.act.clone());
        let table = table.clone();
        Rc::new(move |action, window, cx| {
            Self::run(&table, owner.as_deref(), action, row, column, window, cx)
        })
    }

    /// Runs an entry: the grid's own here, the others by the owner.
    fn run(
        table: &Entity<TableState<Self>>,
        owner: Option<&OnCell>,
        action: GridAction,
        row: Option<usize>,
        column: usize,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Rearranging the columns: the table reads them again.
        let rearrange = |cx: &mut App, change: fn(&mut Grid, usize)| {
            table.update(cx, |table, cx| {
                change(table.delegate_mut(), column);
                table.refresh(cx);
                cx.notify();
            })
        };
        match action {
            GridAction::Copy => {
                table.update(cx, |table, cx| {
                    Grid::copy(table, Some(CopyFormat::Tsv), None, cx)
                });
            }
            GridAction::CopyAs(format) => {
                table.update(cx, |table, cx| Grid::copy(table, Some(format), None, cx));
            }
            // From a cell: the names of the selected columns, which hold
            // that cell's; from a header: its own.
            GridAction::CopyColumnName if row.is_some() => {
                table.update(cx, |table, cx| Grid::copy(table, None, None, cx));
            }
            GridAction::CopyColumnName => {
                let grid = table.read(cx).delegate();
                if let Some(source) = grid.source_column(column) {
                    let name = grid.store.columns()[source].name.clone();
                    cx.write_to_clipboard(ClipboardItem::new_string(name));
                }
            }
            GridAction::ViewValue | GridAction::EditValue => {
                if let Some(row) = row {
                    let edit = action == GridAction::EditValue;
                    Self::open_value(table, row, column, edit, window, cx);
                }
            }
            GridAction::CopyRowNumbers => {
                let selection = table.read(cx).selection();
                let grid = table.read(cx).delegate();
                let rows = grid
                    .selected(selection)
                    .map_or_else(|| row.map(|row| row..=row), |selected| Some(selected.rows));
                if let Some(rows) = rows {
                    cx.write_to_clipboard(ClipboardItem::new_string(row_numbers(&rows)));
                }
            }
            GridAction::RecordView(_) => Self::toggle_record(table, window, cx),
            GridAction::SelectionStats(shown) => {
                crate::state::AppState::update_settings(cx, |settings| {
                    settings.selection_stats = !shown;
                });
            }
            GridAction::Zoom(step) => {
                crate::settings_view::zoom_grid((step != 0).then_some(f32::from(step)), window, cx)
            }
            GridAction::Panel(panel) => super::grid_view::open_panel(table, panel, window, cx),
            GridAction::GoToRow => super::grid_view::go_to_row(table, window, cx),
            GridAction::GoToColumn => super::grid_view::go_to_column(table, window, cx),
            GridAction::Order(order) => table.update(cx, |table, cx| {
                let sort = match order {
                    Some(false) => ColumnSort::Ascending,
                    Some(true) => ColumnSort::Descending,
                    None => ColumnSort::Default,
                };
                table.delegate_mut().perform_sort(column, sort, window, cx);
                // The arrow of the header follows (a table's page is read
                // again in a new grid anyway).
                table.refresh(cx);
            }),
            GridAction::Pin(_) => rearrange(cx, Grid::toggle_pin),
            GridAction::HideColumn => rearrange(cx, Grid::hide),
            GridAction::ShowAll(_) => rearrange(cx, |grid, _| grid.show_all()),
            GridAction::FitWidth => table.update(cx, |table, cx| {
                table.fit_col(column, window, cx);
            }),
            GridAction::FitAllWidths => table.update(cx, |table, cx| {
                for column in 1..table.delegate().layout.len() + 1 {
                    table.fit_col(column, window, cx);
                }
            }),
            action => {
                if let Some(owner) = owner {
                    owner(action, row.unwrap_or_default(), column, window, cx);
                }
            }
        }
    }
}

/// What a cell being edited draws, in the grid and in the record view: the
/// text input, right-aligned for numbers, or the list of the column's
/// values open under the value (`shown`).
pub(super) fn editor_element(
    table: &Entity<TableState<Grid>>,
    editor: &Editor,
    shown: SharedString,
    numeric: bool,
    cx: &App,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let size = text_size(cx);
    let cancel = {
        let table = table.clone();
        move |window: &mut Window, cx: &mut App| {
            table.update(cx, |table, cx| {
                table.delegate_mut().cancel_edit();
                table.focus_handle(cx).focus(window, cx);
                cx.notify();
            })
        }
    };
    match editor {
        Editor::Text(input) => div()
            .id("cell-editor")
            .w_full()
            // Escape, once the input has no use for it, drops the edit.
            .on_action(move |_: &Escape, window, cx| cancel(window, cx))
            // Tab while typing stays in the cell: it does not switch to the
            // record view.
            .on_action(|_: &crate::actions::ToggleRecordView, _, cx| cx.stop_propagation())
            .child(
                Input::new(input)
                    .xsmall()
                    .text_size(size)
                    .appearance(false)
                    .font_family(theme.mono_font_family.clone())
                    .when(numeric, |input| input.text_right()),
            )
            .into_any_element(),
        Editor::Choice(menu) => {
            let menu = menu.clone();
            Popover::new("cell-choices")
                .open(true)
                .appearance(false)
                .on_open_change(move |open, window, cx| {
                    if !open {
                        cancel(window, cx);
                    }
                })
                .trigger(
                    Button::new("cell-choice")
                        .ghost()
                        .xsmall()
                        .label(shown)
                        .dropdown_caret(true),
                )
                .content(move |_, _, _| menu.clone())
                .into_any_element()
        }
    }
}

/// The widest a column is first drawn; the user resizes, "Fit width" goes
/// further.
const DEFAULT_WIDTH_CAP: f32 = 360.;

impl Grid {
    /// How wide column `source` draws its header (the name, the gap, the
    /// type and the sort arrow) and the longest of its first rows.
    pub(crate) fn content_width(&self, source: usize, cx: &App) -> Pixels {
        let described = &self.store.columns()[source];
        let theme = cx.theme();
        let text = cx.text_system();
        let size = text_size(cx);
        let measure = |shown: &str, family: &SharedString, size: Pixels| -> Pixels {
            let id = text.resolve_font(&font(family.clone()));
            shown
                .chars()
                .map(|c| {
                    text.advance(id, size, c)
                        .map_or(px(0.), |advance| advance.width)
                })
                .sum()
        };
        let header = measure(&described.name, &theme.font_family, size)
            + measure(&described.type_name, &theme.font_family, size * 0.85)
            + px(8.)
            + px(16.);
        // ponytail: the cells of the first rows only, and the longest by
        // characters (the font is monospaced); a longer value further down
        // stays cut. Sample the rows on screen if that matters.
        let longest = (0..self.total_rows().min(FIT_ROWS))
            .map(|row| {
                let shown = display(self.cell(row, source), described, MAX_CELL_CHARS);
                if shown.truncated {
                    format!("{}…", shown.text)
                } else {
                    shown.text.into_owned()
                }
            })
            .max_by_key(|text| text.chars().count())
            .unwrap_or_default();
        header.max(measure(&longest, &theme.mono_font_family, size))
    }

    /// The width column `source` is first drawn at: what it shows, up to a
    /// cap, or the width it had before the result was read again.
    fn default_width(&self, source: usize, cx: &App) -> Pixels {
        if let Some(width) = self.widths.get(&self.store.columns()[source].name) {
            return *width;
        }
        let padding = table_size(cx).table_cell_padding();
        (self.content_width(source, cx) + padding.left + padding.right)
            .ceil()
            .min(px(DEFAULT_WIDTH_CAP))
    }

    /// Draws the columns of these names at these widths (those of the same
    /// result before it was read again).
    pub fn with_widths(mut self, widths: HashMap<String, Pixels>) -> Self {
        self.widths = widths;
        self
    }

    /// The width each shown column is drawn at, by name.
    pub fn widths(table: &TableState<Self>) -> HashMap<String, Pixels> {
        let grid = table.delegate();
        (1..=grid.layout.len())
            .filter_map(|column| {
                let source = grid.source_column(column)?;
                Some((
                    grid.store.columns()[source].name.clone(),
                    table.col_width(column)?,
                ))
            })
            .collect()
    }
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
        self.total_rows()
    }

    fn column(&self, index: usize, cx: &App) -> Column {
        let Some(source) = self.source_column(index) else {
            return Column::new("row-number", "")
                .width(number_width(self.total_rows()))
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
        .width(self.default_width(source, cx))
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
        if self.sortable() {
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
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(source) = self.source_column(column) else {
            return;
        };
        self.clear_selection();
        self.sorting = None;
        if let Some(ask) = self.sort_on_server.clone() {
            let sort = match sort {
                ColumnSort::Ascending => Some((source, false)),
                ColumnSort::Descending => Some((source, true)),
                ColumnSort::Default => None,
            };
            // Once the table is done with this click: the owner replaces it.
            window.defer(cx, move |window, cx| ask(sort, window, cx));
            return;
        }
        // Not every row is here, or the server's order is taken back: the
        // server reads the result again.
        // An editable result too: pending changes are kept by row as
        // fetched, which an order made here would mix up.
        if let Some(ask) = self.reorder.clone()
            && (!self.complete
                || self.edits.is_some()
                || (self.server_sorted && matches!(sort, ColumnSort::Default)))
        {
            let sort = match sort {
                ColumnSort::Ascending => Some((source, false)),
                ColumnSort::Descending => Some((source, true)),
                ColumnSort::Default => None,
            };
            window.defer(cx, move |window, cx| ask(sort, window, cx));
            return;
        }
        self.server_sorted = false;
        let descending = match sort {
            ColumnSort::Ascending => false,
            ColumnSort::Descending => true,
            ColumnSort::Default => {
                self.sort = None;
                self.order = None;
                self.generation += 1;
                cx.notify();
                return;
            }
        };
        self.sort = Some((source, descending));
        if self.store.row_count() <= SORT_INLINE_ROWS {
            self.order = Some(sorted_rows(&self.store, source, descending).into());
            self.generation += 1;
            crate::bench::sorted(cx);
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
                    grid.generation += 1;
                    crate::bench::sorted(cx);
                    grid.sorting = None;
                    cx.notify();
                }
            });
        }));
    }

    /// The widest of the header and the first rows of the column, as drawn.
    fn fit_column(
        &mut self,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<Pixels> {
        let source = self.source_column(column)?;
        Some(self.content_width(source, cx).min(px(600.)))
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
        let size = text_size(cx);
        // The sort mark keeps its room; the type gives way first, and the
        // name is cut only when there is no room left for it whole.
        h_flex()
            .id(("column-header", column))
            .test_support()
            .flex_1()
            .min_w_0()
            .gap_2()
            .text_size(size)
            .child(
                div()
                    .id(("column-name", column))
                    .test_support()
                    .flex_none()
                    .max_w_full()
                    .truncate()
                    .child(SharedString::from(described.name.clone())),
            )
            .child(
                div()
                    .id(("column-type", column))
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(size * 0.85)
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(described.type_name.clone())),
            )
            .context_menu(move |menu, window, cx| {
                Grid::column_menu(&table, column, menu, window, cx)
            })
            .into_any_element()
    }

    fn render_tr(
        &mut self,
        row: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let tr = div().id(("row", row));
        let fetched = self.store.row_count();
        match self.pending() {
            Some(pending) if pending.is_new(fetched, row) => tr.bg(theme.success.opacity(0.22)),
            Some(pending) if pending.is_deleted(row) => tr.bg(theme.danger.opacity(0.18)),
            // A result is striped here, not by the table: the table's own
            // stripes fill the pane below the last row with empty rows.
            _ if self.table.is_none() && row % 2 == 1 => tr.bg(theme.table_even),
            _ => tr,
        }
    }

    /// Nothing to show: the result has columns and no rows.
    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .justify_center()
            .pt_8()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            // A table's data view says why itself.
            .when(self.table.is_none(), |empty| empty.child("No rows"))
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let size = text_size(cx);
        let cell = div()
            .w_full()
            .truncate()
            .text_size(size)
            .font_family(theme.mono_font_family.clone());
        let fetched = self.store.row_count();
        let Some(source) = self.source_column(column) else {
            // A new row has no number yet.
            let (label, color) = match self.mark(row, 0) {
                Mark::New => ("+".to_string(), theme.success),
                Mark::Deleted => ((row + 1).to_string(), theme.danger),
                _ => ((row + 1).to_string(), theme.muted_foreground),
            };
            // The row number answers a right-click with the row's menu.
            let table = cx.entity();
            return div()
                .id(("row-number", row))
                .test_support()
                .w_full()
                .child(
                    cell.text_size(size * 0.85)
                        .text_color(color)
                        .child(SharedString::from(label)),
                )
                // A click selects the whole row (the table alone would
                // select the number's cell); shift and click stretches the
                // selection of rows to it.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |table, event: &MouseDownEvent, window, cx| {
                        // The table takes the keyboard, as a click on a cell
                        // gives it.
                        table.focus_handle(cx).focus(window, cx);
                        if event.modifiers.shift {
                            table.delegate_mut().extend_to(row, 1);
                        } else {
                            table.delegate_mut().clear_selection();
                            table.set_selected_row(row, cx);
                            let selection = table.selection();
                            table.delegate_mut().set_anchor(selection);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .context_menu(move |menu, window, cx| {
                    Grid::number_menu(&table, row, menu, window, cx)
                })
                .into_any_element();
        };
        if let Some(editor) = self.editor_at(row, source) {
            let numeric = self.store.columns()[source].kind.is_numeric();
            let shown = self
                .cell(row, source)
                .map_or(SharedString::from("NULL"), |bytes| {
                    SharedString::from(String::from_utf8_lossy(bytes).into_owned())
                });
            return editor_element(&cx.entity(), editor, shown, numeric, cx);
        }
        let mark = self.mark(row, source);
        let pending = self
            .pending()
            .and_then(|pending| pending.value(fetched, row, source));
        let shown = display(
            self.cell(row, source),
            &self.store.columns()[source],
            MAX_CELL_CHARS,
        );
        let table = cx.entity();
        let settings = &crate::state::AppState::get(cx).settings;
        let text = if pending == Some(&CellValue::Default) {
            // The server gives the value when the row is written.
            SharedString::from("default")
        } else if shown.style == CellStyle::Null {
            SharedString::from(settings.null_text.clone())
        } else if !settings.show_time_zone
            && self.store.columns()[source].kind == TypeKind::DateTimeTz
        {
            SharedString::from(without_zone(&shown.text).to_string())
        } else if shown.truncated {
            SharedString::from(format!("{}…", shown.text))
        } else {
            SharedString::from(shown.text.into_owned())
        };
        let text = cell
            // NULL is not the empty string: it is drawn apart.
            .when(
                shown.style == CellStyle::Null || pending == Some(&CellValue::Default),
                |cell| cell.italic().text_color(theme.muted_foreground),
            )
            .when(
                matches!(shown.style, CellStyle::Boolean | CellStyle::Binary),
                |cell| cell.text_color(theme.muted_foreground),
            )
            .when(mark == Mark::Deleted, |cell| {
                cell.line_through().text_color(theme.muted_foreground)
            })
            .child(text);
        // The text's style stays on the text: the menu, drawn inside the
        // element that opens it, would inherit it.
        div()
            .id(("cell", (row << 12) | column))
            .w_full()
            // What is not on the server yet shows (a new or deleted row is
            // tinted as a whole, by `render_tr`).
            .when(mark == Mark::Edited, |cell| {
                cell.bg(theme.warning.opacity(0.22))
            })
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
                        let grid = table.delegate_mut();
                        // A click on a selected boolean cell toggles it, as
                        // DBeaver's checkbox does; the click that selects it
                        // does not.
                        let toggle = event.click_count == 1
                            && grid.anchor == TableSelection::Cell(row, column)
                            && grid.store.columns()[source].kind == TypeKind::Boolean
                            && grid.editable(row, column).is_ok();
                        grid.clear_selection();
                        if toggle {
                            grid.commit_edit(cx);
                            grid.toggle(row, source);
                            table.refresh(cx);
                        }
                    }
                    cx.notify();
                }),
            )
            .child(text)
            // The table keeps a right click on a cell to itself: each cell
            // opens its own menu, built when it opens.
            .context_menu(move |menu, window, cx| {
                Grid::cell_menu(&table, row, column, menu, window, cx)
            })
            .into_any_element()
    }

    fn has_more(&self, _: &App) -> bool {
        Grid::has_more(self)
    }

    fn load_more(&mut self, _: &mut Window, cx: &mut Context<TableState<Self>>) {
        self.fetch_more();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_loses_only_its_offset() {
        assert_eq!(
            without_zone("2024-05-01 10:00:00+02"),
            "2024-05-01 10:00:00"
        );
        assert_eq!(
            without_zone("2024-05-01 10:00:00.123-05:30"),
            "2024-05-01 10:00:00.123"
        );
        assert_eq!(without_zone("2024-05-01T10:00:00Z"), "2024-05-01T10:00:00");
        assert_eq!(without_zone("2024-05-01 10:00:00"), "2024-05-01 10:00:00");
        assert_eq!(without_zone("2024-05-01"), "2024-05-01");
    }

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Item(action, why_not, checked) => {
                    let tick = if *checked { "✓ " } else { "" };
                    match why_not {
                        Some(reason) => format!("{tick}{} ({reason})", action.label()),
                        None => format!("{tick}{}", action.label()),
                    }
                }
                Entry::Submenu(label, items) => format!("{label} ▸ {}", labels(items).join(" | ")),
                Entry::Separator => "—".into(),
            })
            .collect()
    }

    /// The labels of a menu with those of its submenus, for `contains`.
    fn flat(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .flat_map(|entry| match entry {
                Entry::Submenu(_, items) => flat(items),
                entry => labels(std::slice::from_ref(entry)),
            })
            .collect()
    }

    fn column() -> HeaderFacts {
        HeaderFacts {
            data: false,
            sort: None,
            loading: false,
            pinned: false,
            hidden: 0,
            filtered: false,
            filters: false,
        }
    }

    /// A fetched text cell of a query result.
    fn result_cell() -> CellFacts {
        CellFacts {
            data: false,
            editable: false,
            read_only: None,
            kind: TypeKind::Text,
            null: false,
            computed: false,
            nullable: true,
            row: Mark::Fetched,
            many: false,
            clipboard: true,
            column: column(),
            related: Vec::new(),
            export: None,
            keyless: false,
            record: false,
            stats: true,
        }
    }

    /// A fetched text cell of a table that takes changes.
    fn data_cell() -> CellFacts {
        CellFacts {
            data: true,
            column: HeaderFacts {
                data: true,
                filters: true,
                ..column()
            },
            ..result_cell()
        }
    }

    const COPY_AS: &str = "Copy as ▸ With column names | CSV | JSON | Markdown | SQL INSERT";
    const ORDER: &str = "Order by this column ▸ Ascending | Descending | Clear order (not ordered)";
    const NAVIGATE: &str = "Navigate ▸ Go to row… | Go to column…";
    const COLUMN: &str = "Column ▸ Copy column name | Pin column | Hide column | \
                          Show all columns (none hidden)";
    const GENERATE: &str = "Generate SQL ▸ SELECT by key | SELECT … IN | INSERT | UPDATE | DELETE";
    const LAYOUT: &str = "Layout ▸ Record view | ✓ Selection statistics | — | Larger text | \
                          Smaller text | Usual text size";
    const PANELS: &str = "Panels ▸ Aggregate | Columns";

    #[test]
    fn a_result_cell_copies_orders_shows_and_exports_but_never_edits_or_filters() {
        assert_eq!(
            labels(&cell_menu(&result_cell())),
            [
                "Copy",
                COPY_AS,
                "—",
                ORDER,
                NAVIGATE,
                "—",
                "View value",
                "—",
                COLUMN,
                LAYOUT,
                PANELS,
                "—",
                "Generate SQL ▸ SELECT by key (query result) | SELECT … IN (query result) | \
                 INSERT (query result) | UPDATE (query result) | DELETE (query result)",
                "Export…"
            ]
        );
        // Ordered descending, with a column hidden, a statement that writes.
        let facts = CellFacts {
            column: HeaderFacts {
                sort: Some(true),
                pinned: true,
                hidden: 2,
                ..column()
            },
            export: Some("the statement writes"),
            ..result_cell()
        };
        let menu = labels(&cell_menu(&facts));
        assert!(menu.contains(
            &"Order by this column ▸ Ascending | ✓ Descending | Clear order".to_string()
        ));
        let menu = flat(&cell_menu(&facts));
        assert!(menu.contains(&"Unpin column".to_string()));
        assert!(menu.contains(&"Show all columns (2 hidden)".to_string()));
        assert!(menu.contains(&"Export… (the statement writes)".to_string()));
        // Rows still arriving cannot be ordered.
        let arriving = CellFacts {
            column: HeaderFacts {
                loading: true,
                ..column()
            },
            ..result_cell()
        };
        assert!(
            labels(&cell_menu(&arriving)).contains(
                &"Order by this column ▸ Ascending (rows still arriving) | \
              Descending (rows still arriving) | Clear order (rows still arriving)"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_data_cell_adds_paste_filters_edits_and_the_row_in_dbeavers_order() {
        // The plan's grouping (docs/plans/M10.md, Part 4): the row's and the
        // column's entries in submenus, so the menu fits an 800-point window.
        let facts = CellFacts {
            related: vec!["Go to customers (customer_id = 7)".into()],
            ..data_cell()
        };
        let filter = "Filter ▸ > this value… | >= this value… | < this value… | \
                      <= this value… | contains this value… | LIKE this value… | \
                      NOT LIKE this value… | IN this value… | — | Distinct values… | \
                      Clear filters (no filter)";
        assert_eq!(
            labels(&cell_menu(&facts)),
            [
                "Copy",
                COPY_AS,
                "Paste",
                "—",
                "Filter by this value",
                "Exclude this value",
                filter,
                "—",
                ORDER,
                NAVIGATE,
                "—",
                "Go to customers (customer_id = 7)",
                "—",
                "View value",
                "Edit cell",
                "Edit in value panel",
                "Set NULL",
                "Set default",
                "—",
                "Row ▸ Duplicate row | Delete row | Revert row (no change) | \
                 Copy from row above | Copy from row below",
                "—",
                COLUMN,
                LAYOUT,
                PANELS,
                "—",
                GENERATE,
                "Export…",
                "—",
                "Refresh",
            ]
        );
        // A table without a key can only be inserted; a new row is not written.
        let keyless = labels(&cell_menu(&CellFacts {
            keyless: true,
            ..data_cell()
        }));
        assert!(
            keyless.contains(
                &"Generate SQL ▸ SELECT by key (no key) | SELECT … IN (no key) | INSERT | \
              UPDATE (no key) | DELETE (no key)"
                    .to_string()
            )
        );
        let new = flat(&cell_menu(&CellFacts {
            row: Mark::New,
            ..data_cell()
        }));
        assert!(new.contains(&"INSERT (new row)".to_string()), "{new:?}");
    }

    #[test]
    fn what_a_cell_does_not_allow_is_disabled_with_the_reason() {
        let menu = |facts: CellFacts| flat(&cell_menu(&facts));
        // A read-only table (or a view): every change says why.
        let view = menu(CellFacts {
            read_only: Some("view"),
            ..data_cell()
        });
        for entry in [
            "Paste (view)",
            "Edit cell (view)",
            "Set NULL (view)",
            "Set default (view)",
            "Copy from row above (view)",
            "Duplicate row (view)",
            "Delete row (view)",
            "Revert row (view)",
        ] {
            assert!(view.contains(&entry.to_string()), "{entry}: {view:?}");
        }
        // Filters still work on a view.
        assert!(view.contains(&"Filter by this value".to_string()));

        // A NULL cell filters with IS NULL, but has no value for the others.
        let null = menu(CellFacts {
            null: true,
            ..data_cell()
        });
        assert!(null.contains(&"Exclude this value".to_string()));
        assert!(null.contains(&"> this value… (NULL value)".to_string()));

        // A binary cell: no value filter, no typing, but NULL is fine.
        let binary = menu(CellFacts {
            kind: TypeKind::Binary,
            ..data_cell()
        });
        for entry in [
            "Filter by this value (binary value)",
            "Exclude this value (binary value)",
            "Edit cell (binary value)",
            "Paste (binary value)",
            "Set NULL",
        ] {
            assert!(binary.contains(&entry.to_string()), "{entry}: {binary:?}");
        }
        let json = menu(CellFacts {
            kind: TypeKind::Json,
            ..data_cell()
        });
        assert!(json.contains(&"Filter by this value (JSON value)".to_string()));

        // A computed NOT NULL column, an empty clipboard, a filter on.
        let computed = menu(CellFacts {
            computed: true,
            nullable: false,
            clipboard: false,
            column: HeaderFacts {
                filtered: true,
                ..data_cell().column
            },
            ..data_cell()
        });
        for entry in [
            "Paste (clipboard empty)",
            "Edit cell (computed column)",
            "Set NULL (computed column)",
            "Set default (computed column)",
            "Clear filters",
        ] {
            assert!(
                computed.contains(&entry.to_string()),
                "{entry}: {computed:?}"
            );
        }
        let not_null = menu(CellFacts {
            nullable: false,
            ..data_cell()
        });
        assert!(not_null.contains(&"Set NULL (NOT NULL)".to_string()));
        // With several cells selected, the ones that take no value are
        // skipped: the entries stay.
        let many = menu(CellFacts {
            nullable: false,
            computed: true,
            many: true,
            ..data_cell()
        });
        assert!(many.contains(&"Set NULL".to_string()));
        assert!(many.contains(&"Paste".to_string()));

        // A new row has nothing to revert; a deleted one is not edited.
        let new = menu(CellFacts {
            row: Mark::New,
            ..data_cell()
        });
        assert!(new.contains(&"Revert row (new row: delete it)".to_string()));
        let deleted = menu(CellFacts {
            row: Mark::Deleted,
            ..data_cell()
        });
        for entry in [
            "Edit cell (row deleted)",
            "Set NULL (row deleted)",
            "Paste (row deleted)",
            "Delete row (row deleted)",
            "Copy from row above (row deleted)",
            "Revert row",
        ] {
            assert!(deleted.contains(&entry.to_string()), "{entry}: {deleted:?}");
        }
    }

    #[test]
    fn a_header_copies_orders_filters_and_fits_its_column() {
        assert_eq!(
            labels(&header_menu(&HeaderFacts {
                data: true,
                sort: Some(false),
                ..column()
            })),
            [
                "Copy column name",
                "—",
                "Order by this column ▸ ✓ Ascending | Descending | Clear order",
                "Filter on this column…",
                "Distinct values…",
                "—",
                "Pin column",
                "Hide column",
                "Show all columns (none hidden)",
                "—",
                "Fit width",
                "Fit all widths",
            ]
        );
        // A result the server cannot read again has no filter; a SELECT's has.
        assert!(!labels(&header_menu(&column())).contains(&"Filter on this column…".to_string()));
        let select = labels(&header_menu(&HeaderFacts {
            filters: true,
            ..column()
        }));
        assert!(select.contains(&"Filter on this column…".to_string()));
        assert!(select.contains(&"Distinct values…".to_string()));
    }

    #[test]
    fn a_result_of_one_table_edits_as_table_data_and_a_read_only_one_says_why() {
        let select = HeaderFacts {
            filters: true,
            ..column()
        };
        let editable = CellFacts {
            editable: true,
            column: select,
            ..result_cell()
        };
        let all = flat(&cell_menu(&editable));
        for entry in [
            "Paste",
            "Edit cell",
            "Set NULL",
            "Set default",
            "Duplicate row",
            "Delete row",
            "Revert row (no change)",
            "SELECT by key",
            "UPDATE",
        ] {
            assert!(all.contains(&entry.to_string()), "{entry}: {all:?}");
        }
        // A table's page only: refreshed and its filter cleared from here.
        assert!(!all.contains(&"Refresh".to_string()), "{all:?}");
        let rows = flat(&row_menu(&editable));
        for entry in ["Add row", "Duplicate row", "Delete row"] {
            assert!(rows.contains(&entry.to_string()), "{entry}: {rows:?}");
        }
        assert!(!rows.contains(&"Clear filters (no filter)".to_string()));
        // A read-only result keeps no edit entry; Generate SQL says why.
        let read_only = CellFacts {
            read_only: Some("joins tables"),
            column: select,
            ..result_cell()
        };
        let all = flat(&cell_menu(&read_only));
        assert!(!all.contains(&"Edit cell".to_string()), "{all:?}");
        assert!(
            all.contains(&"UPDATE (joins tables)".to_string()),
            "{all:?}"
        );
    }

    #[test]
    fn a_select_result_filters_on_the_server_but_never_edits() {
        let facts = CellFacts {
            column: HeaderFacts {
                filters: true,
                ..column()
            },
            ..result_cell()
        };
        let menu = labels(&cell_menu(&facts));
        assert!(
            menu.contains(&"Filter by this value".to_string()),
            "{menu:?}"
        );
        assert!(menu.contains(&"Exclude this value".to_string()));
        let all = flat(&cell_menu(&facts));
        for entry in [
            "Distinct values…",
            "Clear filters (no filter)",
            "> this value…",
        ] {
            assert!(all.contains(&entry.to_string()), "{entry}: {all:?}");
        }
        for entry in ["Paste", "Edit cell", "Set NULL", "Refresh"] {
            assert!(!all.contains(&entry.to_string()), "{entry}: {all:?}");
        }
        // A JSON column has no distinct values to list.
        let json = flat(&cell_menu(&CellFacts {
            kind: TypeKind::Json,
            ..facts
        }));
        assert!(json.contains(&"Distinct values… (JSON value)".to_string()));
    }

    #[test]
    fn the_record_view_and_the_value_panel_are_in_the_cell_menu() {
        let menu = labels(&cell_menu(&CellFacts {
            record: true,
            stats: false,
            kind: TypeKind::Binary,
            ..data_cell()
        }));
        assert!(
            menu.contains(
                &"Layout ▸ ✓ Record view | Selection statistics | — | Larger text | \
              Smaller text | Usual text size"
                    .to_string()
            )
        );
        // Binary values are shown, not edited, in the value panel.
        assert!(menu.contains(&"Edit in value panel (binary value)".to_string()));
        let view = flat(&cell_menu(&CellFacts {
            read_only: Some("view"),
            ..data_cell()
        }));
        assert!(view.contains(&"Edit in value panel (view)".to_string()));
        // Refresh ends the menu of a table's rows only.
        assert_eq!(
            cell_menu(&data_cell()).last(),
            Some(&enabled(GridAction::Refresh))
        );
        assert!(!flat(&cell_menu(&result_cell())).contains(&"Refresh".to_string()));
    }

    #[test]
    fn a_row_number_opens_the_menu_of_its_row() {
        assert_eq!(
            labels(&row_menu(&data_cell())),
            [
                "Copy row numbers",
                "—",
                "Copy",
                COPY_AS,
                "—",
                "Add row",
                "Duplicate row",
                "Delete row",
                "Revert row (no change)",
                "—",
                "Clear filters (no filter)",
                "—",
                LAYOUT,
                "—",
                GENERATE,
                "Export…",
                "—",
                "Refresh",
            ]
        );
        // A result's row copies, changes the layout and exports.
        assert_eq!(
            labels(&row_menu(&result_cell())),
            [
                "Copy row numbers",
                "—",
                "Copy",
                COPY_AS,
                "—",
                LAYOUT,
                "—",
                "Generate SQL ▸ SELECT by key (query result) | SELECT … IN (query result) | \
                 INSERT (query result) | UPDATE (query result) | DELETE (query result)",
                "Export…",
            ]
        );
        let read_only = flat(&row_menu(&CellFacts {
            read_only: Some("view"),
            ..data_cell()
        }));
        assert!(read_only.contains(&"Add row (view)".to_string()));
    }

    #[test]
    fn row_numbers_are_copied_one_per_line_from_one() {
        assert_eq!(row_numbers(&(0..=0)), "1");
        assert_eq!(row_numbers(&(4..=6)), "5\n6\n7");
    }

    #[test]
    fn a_boolean_toggles_through_null_when_its_column_takes_it() {
        assert_eq!(next_bool(Some(true), true), Some(false));
        assert_eq!(next_bool(Some(false), true), None);
        assert_eq!(next_bool(None, true), Some(true));
        assert_eq!(next_bool(Some(false), false), Some(true));
        assert_eq!(next_bool(None, false), Some(true));
        // Written as the server prints it, so that two toggles are no change.
        let pg = kerno_core::Column::new("b", "bool", TypeKind::Boolean);
        let mysql = kerno_core::Column::new("b", "TINYINT(1)", TypeKind::Boolean);
        assert_eq!((bool_text(&pg, true), bool_text(&pg, false)), ("t", "f"));
        assert_eq!(
            (bool_text(&mysql, true), bool_text(&mysql, false)),
            ("1", "0")
        );
    }

    #[test]
    fn a_pasted_block_is_split_in_rows_and_cells() {
        assert_eq!(text_block("one"), [["one"]]);
        assert_eq!(text_block("a\tb\nc\td\n"), [["a", "b"], ["c", "d"]]);
        assert_eq!(text_block("a\tb\r\nc\t\r\n"), [["a", "b"], ["c", ""]]);
        // An empty cell is still a cell; an empty line in the middle a row.
        assert_eq!(text_block("a\n\nb"), [vec!["a"], vec![""], vec!["b"]]);
    }

    #[test]
    fn text_put_in_a_cell_follows_the_rule_of_typing() {
        assert_eq!(typed_value("7", "7", TypeKind::Integer), None);
        assert_eq!(
            typed_value("", "7", TypeKind::Integer),
            Some(CellValue::Null)
        );
        assert_eq!(
            typed_value("", "x", TypeKind::Text),
            Some(CellValue::Text(String::new()))
        );
        assert_eq!(
            typed_value("8", "7", TypeKind::Integer),
            Some(CellValue::Text("8".into()))
        );
    }
}
