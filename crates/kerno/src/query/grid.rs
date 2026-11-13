//! The results grid: one fetched result, drawn by the UI kit's virtualized
//! table. Only the visible rows and columns are laid out; the rows live in
//! the result store as the server sent them.
//!
//! The kit's table selects one row, one column or one cell. A range (shift
//! and click) is kept here, as the far corner of a rectangle whose near
//! corner is the table's own selection.

use std::ops::RangeInclusive;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableSelection, TableState};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Div, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    SharedString, Stateful, Styled as _, Subscription, Task, TestSupportExt as _, TextRun, Window,
    div, font, px,
};
use kerno_core::browse::Operator;
use kerno_core::export::{DelimitedOptions, ExportFormat, export_to_string};
use kerno_core::format::{CellStyle, display};
use kerno_core::pending::Pending;
use kerno_core::schema::TableRef;
use kerno_core::sort::sorted_rows;
use kerno_core::sql::CellValue;
use kerno_core::{Dialect, ResultStore, RowBatch, RowDemand, Session, TypeKind};

/// Cells longer than this are cut in the grid; the value viewer shows all.
const MAX_CELL_CHARS: usize = 300;

/// Results up to this many rows are sorted at once; larger ones off the UI
/// thread.
const SORT_INLINE_ROWS: usize = 20_000;

/// "Fit width" measures the cells of this many rows.
const FIT_ROWS: usize = 1_000;

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
}

/// What a column of a table's page takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnEdit {
    /// Its cells take a value (a computed column does not).
    pub writable: bool,
    pub nullable: bool,
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
    ClearFilters,
    /// Ascending (`Some(false)`), descending, or the server's own order.
    Order(Option<bool>),
    /// The n-th place a foreign key of the row leads to, and its label.
    Follow(usize, String),
    ViewValue,
    EditCell,
    SetNull,
    SetDefault,
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
            Self::ClearFilters => "Clear filters",
            Self::Order(Some(false)) => "Ascending",
            Self::Order(Some(true)) => "Descending",
            Self::Order(None) => "Clear order",
            Self::Follow(_, label) => return label.clone(),
            Self::ViewValue => "View value",
            Self::EditCell => "Edit cell",
            Self::SetNull => "Set NULL",
            Self::SetDefault => "Set default",
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
/// foreign keys, the value and its edits, the row, the columns, export.
/// What a query result never has is left out; what this cell does not
/// allow is there, with the reason.
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
    let mut entries = vec![
        enabled(A::Copy),
        Entry::Submenu("Copy as", copy_as),
        enabled(A::CopyColumnName),
    ];
    if facts.data {
        let paste = read_only
            .or((!facts.clipboard).then_some("clipboard empty"))
            .or(this_cell(deleted.or(computed).or(binary)));
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
        entries.extend([
            item(A::Paste, paste),
            Entry::Separator,
            item(A::FilterByValue, no_filter),
            item(A::ExcludeValue, no_filter),
            Entry::Submenu(
                "Filter",
                operators
                    .into_iter()
                    .map(|operator| item(A::FilterWith(operator), with_value))
                    .collect(),
            ),
            item(
                A::ClearFilters,
                (!facts.column.filtered).then_some("no filter"),
            ),
        ]);
    }
    entries.extend([Entry::Separator, order_menu(&facts.column)]);
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
    if facts.data {
        let revert = match facts.row {
            Mark::Fetched => Some("no change"),
            Mark::New => Some("new row: delete it"),
            Mark::Edited | Mark::Deleted => None,
        };
        entries.extend([
            item(A::EditCell, read_only.or(deleted).or(computed).or(binary)),
            item(
                A::SetNull,
                takes_value.or(this_cell((!facts.nullable).then_some("NOT NULL"))),
            ),
            item(A::SetDefault, takes_value),
            Entry::Separator,
            item(A::DuplicateRow, read_only),
            item(A::DeleteRow, read_only.or(deleted)),
            item(A::RevertRow, read_only.or(revert)),
        ]);
    }
    entries.extend([
        Entry::Separator,
        enabled(A::Pin(facts.column.pinned)),
        enabled(A::HideColumn),
        item(
            A::ShowAll(facts.column.hidden),
            (facts.column.hidden == 0).then_some("none hidden"),
        ),
        Entry::Separator,
        item(A::Export, facts.export),
    ]);
    entries
}

/// The menu of a column header.
pub fn header_menu(facts: &HeaderFacts) -> Vec<Entry> {
    use GridAction as A;
    let mut entries = vec![
        enabled(A::CopyColumnName),
        Entry::Separator,
        order_menu(facts),
    ];
    if facts.data {
        entries.push(enabled(A::FilterColumn));
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
fn typed_value(typed: &str, initial: &str, kind: TypeKind) -> Option<CellValue> {
    if typed == initial {
        None
    } else if typed.is_empty() && kind != TypeKind::Text {
        Some(CellValue::Null)
    } else {
        Some(CellValue::Text(typed.to_string()))
    }
}

/// The cell being typed in.
struct Editing {
    row: usize,
    /// Column of the result.
    column: usize,
    /// What the cell held when the edit began.
    initial: String,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// What a table's data view adds to a result: changes waiting to be
/// applied, and the cell being typed in.
pub struct Edits {
    pub pending: Pending,
    /// What each column of the result takes.
    columns: Vec<ColumnEdit>,
    editing: Option<Editing>,
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
    owner: Option<Owner>,
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
            owner: None,
        }
    }

    /// Lets the grid's owner run the entries of its menus that are its own.
    pub fn with_owner(mut self, owner: Owner) -> Self {
        self.owner = Some(owner);
        self
    }

    /// One page of `table`, fetched in full and sorted by the server as
    /// `sort` says. With `columns` (what each column takes), cells can be
    /// edited and the changes wait in [`Edits::pending`]; otherwise the
    /// rows are read-only for the reason given.
    pub fn page(
        store: ResultStore,
        session: Arc<dyn Session>,
        table: TableRef,
        sort: Option<(usize, bool)>,
        columns: Result<Vec<ColumnEdit>, &'static str>,
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
            read_only: columns.as_ref().err().copied(),
            edits: columns.ok().map(|columns| Edits {
                pending: Pending::default(),
                columns,
                editing: None,
            }),
            sort_on_server: Some(sort_on_server),
            owner: None,
            store,
        }
    }

    pub fn store(&self) -> &ResultStore {
        &self.store
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
        }
    }

    /// Rows shown: the fetched ones, then the new ones.
    pub fn total_rows(&self) -> usize {
        self.store.row_count() + self.pending().map_or(0, Pending::inserted)
    }

    /// What a cell holds as it shows: the pending value when there is one,
    /// else what was fetched. `None` for NULL (and for a default not known
    /// yet).
    fn cell(&self, row: usize, source: usize) -> Option<&[u8]> {
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
        let initial = self.cell(row, source).map_or(String::new(), |bytes| {
            String::from_utf8_lossy(bytes).into_owned()
        });
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
                input,
                _subscription: subscription,
            });
        }
        cx.notify();
        true
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
        let typed = editing.input.read(cx).value().to_string();
        let kind = self.store.columns()[editing.column].kind;
        if let Some(value) = typed_value(&typed, &editing.initial, kind) {
            edits
                .pending
                .set(&self.store, editing.row, editing.column, value);
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
            loading: self.loading(),
            pinned: self.is_pinned(column),
            hidden: self.hidden_count(),
            filtered: self.owner.as_ref().is_some_and(|owner| owner.filtered),
        }
    }

    /// What the menu of a cell of the grid depends on.
    fn cell_facts(&self, row: usize, column: usize, many: bool, clipboard: bool) -> CellFacts {
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
            cell_menu(&table.delegate().cell_facts(row, column, many, clipboard))
        });
        let pick = Self::pick(table, Some(row), column, cx);
        fill_menu(menu, &entries, &pick, window, cx)
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
        fill_menu(menu, &entries, &pick, window, cx)
    }

    /// What runs an entry of the menu of a cell (`row`) or of a header.
    fn pick(table: &Entity<TableState<Self>>, row: Option<usize>, column: usize, cx: &App) -> Pick {
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
            GridAction::ViewValue => {
                let grid = table.read(cx).delegate();
                if let Some((described, value)) = row.and_then(|row| grid.value_at(row, column)) {
                    let (described, value) = (described.clone(), value.map(<[u8]>::to_vec));
                    super::viewer::open(&described, value.as_deref(), window, cx);
                }
            }
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
        self.total_rows()
    }

    fn column(&self, index: usize, _: &App) -> Column {
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

    /// The widest of the header and the first rows of the column, as drawn.
    fn fit_column(
        &mut self,
        column: usize,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Option<Pixels> {
        let source = self.source_column(column)?;
        let described = &self.store.columns()[source];
        let theme = cx.theme();
        let (sans, mono, color) = (
            theme.font_family.clone(),
            theme.mono_font_family.clone(),
            theme.foreground,
        );
        let rem = window.rem_size();
        let width = |text: String, family: SharedString, size: f32| {
            let run = TextRun {
                len: text.len(),
                font: font(family),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            window
                .text_system()
                .shape_line(text.into(), rem * size, &[run], None)
                .width
        };
        // The name, the gap, the type and the sort arrow with its padding.
        let header = width(described.name.clone(), sans, 0.875)
            + width(described.type_name.clone(), mono.clone(), 0.75)
            + px(8.)
            + rem * 0.75
            + px(4.);
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
        let cells = width(longest, mono, 0.875);
        Some(header.max(cells).min(px(600.)))
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
            .test_support()
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
        let cell = div()
            .w_full()
            .truncate()
            .text_sm()
            .font_family(theme.mono_font_family.clone());
        let fetched = self.store.row_count();
        let Some(source) = self.source_column(column) else {
            // A new row has no number yet.
            let (label, color) = match self.mark(row, 0) {
                Mark::New => ("+".to_string(), theme.success),
                Mark::Deleted => ((row + 1).to_string(), theme.danger),
                _ => ((row + 1).to_string(), theme.muted_foreground),
            };
            return cell
                .text_xs()
                .text_color(color)
                .child(SharedString::from(label))
                .into_any_element();
        };
        if let Some(editing) = self
            .edits
            .as_ref()
            .and_then(|edits| edits.editing.as_ref())
            .filter(|editing| editing.row == row && editing.column == source)
        {
            return div()
                .id("cell-editor")
                .w_full()
                // Escape, once the input has no use for it, drops the edit.
                .on_action(cx.listener(|table, _: &Escape, window, cx| {
                    table.delegate_mut().cancel_edit();
                    table.focus_handle(cx).focus(window, cx);
                    cx.notify();
                }))
                .child(
                    Input::new(&editing.input)
                        .xsmall()
                        .text_sm()
                        .appearance(false)
                        .font_family(theme.mono_font_family.clone()),
                )
                .into_any_element();
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
        let cell = cell.id(("cell", (row << 12) | column));
        let text = if pending == Some(&CellValue::Default) {
            // The server gives the value when the row is written.
            SharedString::from("default")
        } else if shown.truncated {
            SharedString::from(format!("{}…", shown.text))
        } else {
            SharedString::from(shown.text.into_owned())
        };
        cell
            // NULL is not the empty string: it is drawn apart.
            .when(
                shown.style == CellStyle::Null || pending == Some(&CellValue::Default),
                |cell| cell.italic().text_color(theme.muted_foreground),
            )
            .when(
                matches!(shown.style, CellStyle::Boolean | CellStyle::Binary),
                |cell| cell.text_color(theme.muted_foreground),
            )
            // What is not on the server yet shows (a new or deleted row is
            // tinted as a whole, by `render_tr`).
            .map(|cell| match mark {
                Mark::Fetched | Mark::New => cell,
                Mark::Edited => cell.bg(theme.warning.opacity(0.22)),
                Mark::Deleted => cell.line_through().text_color(theme.muted_foreground),
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
                        table.delegate_mut().clear_selection();
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

    fn column() -> HeaderFacts {
        HeaderFacts {
            data: false,
            sort: None,
            loading: false,
            pinned: false,
            hidden: 0,
            filtered: false,
        }
    }

    /// A fetched text cell of a query result.
    fn result_cell() -> CellFacts {
        CellFacts {
            data: false,
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
        }
    }

    /// A fetched text cell of a table that takes changes.
    fn data_cell() -> CellFacts {
        CellFacts {
            data: true,
            column: HeaderFacts {
                data: true,
                ..column()
            },
            ..result_cell()
        }
    }

    const COPY_AS: &str = "Copy as ▸ With column names | CSV | JSON | Markdown | SQL INSERT";
    const ORDER: &str = "Order by this column ▸ Ascending | Descending | Clear order (not ordered)";

    #[test]
    fn a_result_cell_copies_orders_shows_and_exports_but_never_edits_or_filters() {
        assert_eq!(
            labels(&cell_menu(&result_cell())),
            [
                "Copy",
                COPY_AS,
                "Copy column name",
                "—",
                ORDER,
                "—",
                "View value",
                "—",
                "Pin column",
                "Hide column",
                "Show all columns (none hidden)",
                "—",
                "Export…",
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
        let facts = CellFacts {
            related: vec!["Go to customers (customer_id = 7)".into()],
            ..data_cell()
        };
        let filter = "Filter ▸ > this value… | >= this value… | < this value… | \
                      <= this value… | contains this value… | LIKE this value… | \
                      NOT LIKE this value… | IN this value…";
        assert_eq!(
            labels(&cell_menu(&facts)),
            [
                "Copy",
                COPY_AS,
                "Copy column name",
                "Paste",
                "—",
                "Filter by this value",
                "Exclude this value",
                filter,
                "Clear filters (no filter)",
                "—",
                "Order by this column ▸ Ascending | Descending | Clear order (not ordered)",
                "—",
                "Go to customers (customer_id = 7)",
                "—",
                "View value",
                "Edit cell",
                "Set NULL",
                "Set default",
                "—",
                "Duplicate row",
                "Delete row",
                "Revert row (no change)",
                "—",
                "Pin column",
                "Hide column",
                "Show all columns (none hidden)",
                "—",
                "Export…",
            ]
        );
    }

    #[test]
    fn what_a_cell_does_not_allow_is_disabled_with_the_reason() {
        let menu = |facts: CellFacts| labels(&cell_menu(&facts));
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
        assert!(
            null.iter()
                .any(|entry| entry.starts_with("Filter ▸ > this value… (NULL value)"))
        );

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
                "—",
                "Pin column",
                "Hide column",
                "Show all columns (none hidden)",
                "—",
                "Fit width",
                "Fit all widths",
            ]
        );
        // A result has no filter.
        assert!(!labels(&header_menu(&column())).contains(&"Filter on this column…".to_string()));
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
