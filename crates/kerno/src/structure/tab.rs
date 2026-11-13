//! The structure of one table or view, read from the server: its columns,
//! indexes, constraints, foreign keys both ways, triggers and DDL, each in
//! a sub-tab of its own. Nothing here changes the database.

use gpui_kit::base::FocusableExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels,
    Render, SharedString, Styled as _, Subscription, Task, TestSupportExt as _, Window, div, px,
};
use kerno_core::schema::{
    ColumnGeneration, ColumnInfo, ConstraintKind, ObjectRef, RelationKind, TableRef, TableStructure,
};
use kerno_core::{Error, TxState};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::query::tab::TabStatus;
use crate::tabs::{TabSnapshot, Target};

/// What a structure tab asks of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureTabEvent {
    /// Show the structure of another table: the other end of a foreign key.
    OpenStructure { table: TableRef, kind: RelationKind },
    /// Show the rows of this table.
    OpenData { table: TableRef, kind: RelationKind },
    /// Open this table in the table form.
    EditTable { table: TableRef },
}

/// A list of the structure, shown as a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    Columns,
    Indexes,
    Constraints,
    /// Foreign keys from this table to others.
    ForeignKeys,
    /// Foreign keys from other tables to this one.
    ReferencedBy,
    Triggers,
}

impl List {
    const ALL: [List; 6] = [
        List::Columns,
        List::Indexes,
        List::Constraints,
        List::ForeignKeys,
        List::ReferencedBy,
        List::Triggers,
    ];
}

/// The sub-tabs, in order, with the lists each shows; the last is the DDL.
const SECTIONS: [(&str, &[List]); 6] = [
    ("Columns", &[List::Columns]),
    ("Indexes", &[List::Indexes]),
    ("Constraints", &[List::Constraints]),
    ("Foreign keys", &[List::ForeignKeys, List::ReferencedBy]),
    ("Triggers", &[List::Triggers]),
    ("DDL", &[]),
];

/// Index of the DDL sub-tab.
pub const DDL_SECTION: usize = 5;

/// Where the tab is with the structure.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// The connection is not open.
    Waiting,
    Loading,
    Ready,
    Failed(String),
}

/// The structure of one table or view.
pub struct StructureTab {
    focus_handle: FocusHandle,
    connections: Entity<Connections>,
    target: Target,
    table: TableRef,
    kind: RelationKind,
    /// Restored and not shown yet: nothing is read, no session is open.
    hidden: bool,
    structure: Option<TableStructure>,
    /// One per list, in `List::ALL` order, once read.
    grids: Vec<Entity<TableState<ListGrid>>>,
    ddl: Option<Entity<EditorState>>,
    /// The DDL, or why it could not be read, as a SQL comment.
    ddl_text: String,
    /// The sub-tab on screen.
    section: usize,
    state: State,
    /// The load in flight; the next one waits for it.
    load: Option<Task<()>>,
    generation: u64,
    grid_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<StructureTabEvent> for StructureTab {}

impl StructureTab {
    pub fn new(
        target: Target,
        table: TableRef,
        kind: RelationKind,
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The structure loads when the connection opens.
        let follow = cx.observe_in(&connections, window, |this, _, window, cx| {
            match (&this.state, this.connected(cx)) {
                (State::Waiting, true) => this.reload(window, cx),
                (State::Waiting, false) | (_, true) => {}
                (_, false) => {
                    this.state = State::Waiting;
                    cx.notify();
                }
            }
        });
        let mut tab = Self {
            focus_handle: cx.focus_handle(),
            connections,
            target,
            table,
            kind,
            hidden: false,
            structure: None,
            grids: Vec::new(),
            ddl: None,
            ddl_text: String::new(),
            section: 0,
            state: State::Waiting,
            load: None,
            generation: 0,
            grid_subscriptions: Vec::new(),
            _subscriptions: vec![follow],
        };
        tab.reload(window, cx);
        tab
    }

    pub fn title(&self) -> SharedString {
        SharedString::from(format!("{} · structure", self.table.name))
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

    /// Shows another table: the same one under its new name.
    pub fn set_table(&mut self, table: TableRef, window: &mut Window, cx: &mut Context<Self>) {
        self.table = table;
        self.reload(window, cx);
    }

    /// Why the table cannot be edited here, when it cannot.
    fn read_only_reason(&self, cx: &App) -> Option<String> {
        let read_only = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .is_some_and(|saved| saved.config.read_only);
        read_only.then(|| "Read-only connection: the table cannot be edited.".to_string())
    }

    pub fn snapshot(&self) -> TabSnapshot {
        TabSnapshot::Structure {
            target: self.target.clone(),
            schema: self.table.schema.clone(),
            table: self.table.name.clone(),
            view: !self.kind.is_table(),
            materialized: self.kind == RelationKind::MaterializedView,
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

    /// "table" or "view", for the messages.
    fn noun(&self) -> &'static str {
        if self.kind.is_table() {
            "table"
        } else {
            "view"
        }
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
            running: self.state == State::Loading,
            rows: None,
            elapsed: None,
            notices: 0,
            pending: 0,
        }
    }

    // Loading -----------------------------------------------------------------

    /// Reads nothing until shown (a restored tab): see `reveal`.
    pub fn defer_until_shown(&mut self, cx: &mut Context<Self>) {
        self.hidden = true;
        self.generation += 1;
        self.load = None;
        self.state = State::Waiting;
        cx.notify();
    }

    /// The tab is on screen: a deferred tab reads the structure now.
    pub fn reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.hidden) {
            self.reload(window, cx);
        }
    }

    /// Reads the structure and the DDL again, on a session of its own that
    /// closes once they are read: a tab that only shows what it read holds
    /// no connection to the server.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        if self.hidden || !self.connected(cx) {
            self.state = State::Waiting;
            cx.notify();
            return;
        }
        self.state = State::Loading;
        let previous = self.load.take();
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        let table = self.table.clone();
        let object = ObjectRef::Relation(table.clone(), self.kind);
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
            // One statement at a time on the session.
            if let Some(previous) = previous {
                previous.await;
            }
            let session = match opening.await {
                Ok(session) => session,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| this.failed(generation, &error, cx));
                    return;
                }
            };
            // The session goes with the reading: it closes once read.
            let reading = cx.update(|_, cx| {
                bridge::spawn(cx, async move {
                    let structure = session.table_structure(&table).await;
                    // The columns are worth showing when the DDL is not.
                    let ddl = match structure {
                        Ok(_) => session.ddl(&object).await.unwrap_or_else(|error| {
                            format!("-- The DDL could not be read: {error}\n")
                        }),
                        Err(_) => String::new(),
                    };
                    session.close().await;
                    Ok::<_, Error>((structure?, ddl))
                })
            });
            let Ok(reading) = reading else {
                return;
            };
            let outcome = reading.await;
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok((structure, ddl)) if this.generation == generation => {
                    this.show(structure, ddl, window, cx)
                }
                Ok(_) => {}
                Err(error) => this.failed(generation, &error, cx),
            });
        }));
        cx.notify();
    }

    fn failed(&mut self, generation: u64, error: &Error, cx: &mut Context<Self>) {
        if self.generation != generation {
            return;
        }
        self.state = State::Failed(error.to_string());
        cx.notify();
    }

    /// Puts what was read on screen.
    fn show(
        &mut self,
        structure: TableStructure,
        ddl: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state = State::Ready;
        let noun = self.noun();
        self.grid_subscriptions.clear();
        self.grids = List::ALL
            .iter()
            .map(|&list| {
                let grid = ListGrid::new(list, &structure, noun);
                let table = cx.new(|cx| {
                    TableState::new(grid, window, cx)
                        .col_resizable(true)
                        .row_selectable(true)
                        .cell_selectable(true)
                        .row_header(false)
                });
                // A double click on a foreign key opens the table at its
                // other end.
                if matches!(list, List::ForeignKeys | List::ReferencedBy) {
                    self.grid_subscriptions.push(cx.subscribe(
                        &table,
                        move |this, _, event: &TableEvent, cx| {
                            if let TableEvent::DoubleClickedCell(row, _)
                            | TableEvent::DoubleClickedRow(row) = event
                            {
                                this.open_related(list, *row, cx);
                            }
                        },
                    ));
                }
                table
            })
            .collect();
        self.ddl = Some(cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .soft_wrap(false)
                .default_value(ddl.clone())
        }));
        self.ddl_text = ddl;
        self.structure = Some(structure);
        cx.notify();
    }

    // Actions -----------------------------------------------------------------

    /// Shows a sub-tab.
    pub fn select(&mut self, section: usize, cx: &mut Context<Self>) {
        self.section = section.min(SECTIONS.len() - 1);
        cx.notify();
    }

    /// Opens the structure of the table at the other end of a foreign key.
    pub fn open_related(&mut self, list: List, row: usize, cx: &mut Context<Self>) {
        let Some(structure) = &self.structure else {
            return;
        };
        let table = match list {
            List::ForeignKeys => structure
                .foreign_keys
                .get(row)
                .map(|key| key.referenced_table.clone()),
            List::ReferencedBy => structure
                .referenced_by
                .get(row)
                .map(|key| key.table.clone()),
            _ => None,
        };
        if let Some(table) = table {
            cx.emit(StructureTabEvent::OpenStructure {
                table,
                kind: RelationKind::Table,
            });
        }
    }

    /// Copies the selected cell, or the selected row, of the list that has
    /// the focus. Returns whether there was something to copy.
    fn copy_selection(&self, window: &Window, cx: &mut App) -> bool {
        let text = self.grids.iter().find_map(|grid| {
            let table = grid.read(cx);
            if !table.focus_handle(cx).is_focused(window) {
                return None;
            }
            let rows = &table.delegate().rows;
            match (table.selected_cell(), table.selected_row()) {
                (Some((row, column)), _) => rows.get(row)?.get(column).cloned(),
                (None, Some(row)) => Some(rows.get(row)?.join("\t")),
                _ => None,
            }
        });
        text.is_some_and(|text| {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            true
        })
    }

    /// The table showing a list, once read.
    fn grid_of(&self, list: List) -> Option<&Entity<TableState<ListGrid>>> {
        let index = List::ALL.iter().position(|each| *each == list)?;
        self.grids.get(index)
    }

    /// The table showing a list; for tests.
    #[cfg(test)]
    pub fn grid(&self, list: List) -> Option<Entity<TableState<ListGrid>>> {
        self.grid_of(list).cloned()
    }

    /// The rows of a list as the tab shows them; for tests.
    #[cfg(test)]
    pub fn rows_of(&self, list: List, cx: &App) -> Vec<Vec<String>> {
        self.grid(list)
            .map(|grid| grid.read(cx).delegate().rows.clone())
            .unwrap_or_default()
    }

    /// The DDL shown; for tests.
    #[cfg(test)]
    pub fn ddl(&self) -> &str {
        &self.ddl_text
    }

    /// Where the tab is, in words; for tests.
    #[cfg(test)]
    pub fn state(&self) -> String {
        match &self.state {
            State::Waiting => "waiting".into(),
            State::Loading => "loading".into(),
            State::Ready => "ready".into(),
            State::Failed(error) => format!("failed: {error}"),
        }
    }

    // Rendering ---------------------------------------------------------------

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let engine = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .map(|saved| saved.config.engine);
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
                    .id("structure-table-name")
                    .test_support()
                    .px_1()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .child(SharedString::from(format!(
                        "{}.{}",
                        self.table.schema, self.table.name
                    ))),
            )
            .children(self.structure.as_ref().and_then(|structure| {
                structure.comment.clone().map(|comment| {
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(comment))
                })
            }))
            .child(div().flex_1())
            .child(
                Button::new("structure-refresh")
                    .ghost()
                    .small()
                    .label("Refresh")
                    .tooltip("Read the structure again")
                    .disabled(self.state == State::Loading)
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
            )
            .child(
                Button::new("structure-open-data")
                    .ghost()
                    .small()
                    .label("Open data")
                    .tooltip(if self.kind.is_table() {
                        "Show the rows of this table"
                    } else {
                        "Show the rows of this view"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.emit(StructureTabEvent::OpenData {
                            table: this.table.clone(),
                            kind: this.kind,
                        })
                    })),
            )
            // Views are edited through their source, not this form.
            .when(self.kind.is_table(), |bar| {
                match self.read_only_reason(cx) {
                    Some(reason) => bar.child(
                        div()
                            .id("structure-edit-disabled")
                            .test_support()
                            .px_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(reason)),
                    ),
                    None => bar.child(
                        Button::new("structure-edit")
                            .ghost()
                            .small()
                            .label("Edit table…")
                            .tooltip("Change columns, keys and indexes in a form")
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.emit(StructureTabEvent::EditTable {
                                    table: this.table.clone(),
                                })
                            })),
                    ),
                }
            })
    }

    fn render_sections(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let counts: Vec<Option<usize>> = SECTIONS
            .iter()
            .map(|(_, lists)| {
                let grids: Vec<_> = lists
                    .iter()
                    .filter_map(|list| self.grid_of(*list))
                    .collect();
                (!grids.is_empty()).then(|| {
                    grids
                        .iter()
                        .map(|grid| grid.read(cx).delegate().rows.len())
                        .sum()
                })
            })
            .collect();
        TabBar::new("structure-sections")
            .selected_index(self.section)
            .on_click(cx.listener(|this, index: &usize, _, cx| this.select(*index, cx)))
            .children(SECTIONS.iter().zip(counts).map(|((label, _), count)| {
                Tab::new().label(SharedString::from(match count {
                    Some(count) => format!("{label} ({count})"),
                    None => label.to_string(),
                }))
            }))
    }

    /// A message in place of the structure, with what to do next.
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

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match (&self.state, self.grids.is_empty()) {
            (State::Waiting, _) => {
                let connect = Button::new("structure-connect")
                    .primary()
                    .label("Connect")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let id = this.target.connection.clone();
                        this.connections
                            .update(cx, |connections, cx| connections.connect(&id, cx));
                    }))
                    .into_any_element();
                self.render_notice(
                    "structure-waiting",
                    "Not connected",
                    format!(
                        "“{}” is not connected. Connect it to read the structure of {}.",
                        self.connection_name(cx),
                        self.table.name
                    ),
                    Some(connect),
                    cx,
                )
            }
            (State::Failed(error), _) => {
                let retry = Button::new("structure-retry")
                    .outline()
                    .label("Try again")
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx)))
                    .into_any_element();
                self.render_notice(
                    "structure-error",
                    "The structure could not be read",
                    format!(
                        "{error} Check that the {} still exists, then try again.",
                        self.noun()
                    ),
                    Some(retry),
                    cx,
                )
            }
            (_, true) => self.render_notice(
                "structure-loading",
                "Loading the structure…",
                format!("Reading {}.{}.", self.table.schema, self.table.name),
                None,
                cx,
            ),
            _ if self.section == DDL_SECTION => self.render_ddl(cx),
            _ => self.render_lists(cx),
        }
    }

    fn render_lists(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let lists = SECTIONS[self.section].1;
        let titled = lists.len() > 1;
        v_flex()
            .size_full()
            .children(lists.iter().filter_map(|list| {
                let grid = self.grid_of(*list)?;
                let title = match list {
                    List::ReferencedBy => "Referenced by",
                    _ => "References",
                };
                Some(
                    v_flex()
                        // Each table keeps its own element state.
                        .id(("structure-list", *list as usize))
                        .flex_1()
                        .min_h_0()
                        .when(titled, |area| {
                            area.child(
                                div()
                                    .flex_none()
                                    .px_3()
                                    .py_1()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(theme.muted_foreground)
                                    .child(title),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .child(DataTable::new(grid).xsmall().focus_ring(false)),
                        ),
                )
            }))
            .into_any_element()
    }

    fn render_ddl(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(ddl) = &self.ddl else {
            return div().into_any_element();
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1())
                    .child(
                        Button::new("structure-copy-ddl")
                            .ghost()
                            .small()
                            .label("Copy")
                            .tooltip("Copy the DDL")
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    this.ddl_text.clone(),
                                ))
                            })),
                    ),
            )
            .child(
                div()
                    .id("structure-ddl")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Editor::new(ddl)
                            .readonly(true)
                            .h_full()
                            .bordered(false)
                            .appearance(false),
                    ),
            )
            .into_any_element()
    }
}

impl Focusable for StructureTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for StructureTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("structure-tab")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                let secondary = keystroke.modifiers.secondary(cx) && !keystroke.modifiers.shift;
                if secondary && keystroke.key == "c" && this.copy_selection(window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(self.render_toolbar(cx))
            .child(self.render_sections(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
    }
}

// Lists -----------------------------------------------------------------------

/// One list of the structure: headers and rows of text.
pub struct ListGrid {
    list: List,
    headers: &'static [&'static str],
    rows: Vec<Vec<String>>,
    widths: Vec<Pixels>,
    /// What the list says when it is empty.
    empty: SharedString,
}

impl ListGrid {
    fn new(list: List, structure: &TableStructure, noun: &str) -> Self {
        let (headers, rows) = list_rows(structure, list);
        // ponytail: width from the longest text, about 7 px a character;
        // measure with the text system if columns look off.
        let widths = headers
            .iter()
            .enumerate()
            .map(|(column, header)| {
                let longest = rows
                    .iter()
                    .filter_map(|row| row.get(column))
                    .map(|text| one_line(text).chars().count())
                    .chain([header.chars().count()])
                    .max()
                    .unwrap_or(0);
                px(longest.clamp(2, 60) as f32 * 7. + 24.)
            })
            .collect();
        let empty = match list {
            List::Columns => format!("No column in this {noun}."),
            List::Indexes => format!("No index on this {noun}."),
            List::Constraints => format!("No constraint on this {noun}."),
            List::ForeignKeys => format!("This {noun} references no other table."),
            List::ReferencedBy => format!("No table references this {noun}."),
            List::Triggers => format!("No trigger on this {noun}."),
        };
        Self {
            list,
            headers,
            rows,
            widths,
            empty: SharedString::from(empty),
        }
    }
}

/// The text on one line: a trigger's body or a definition can span many.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl TableDelegate for ListGrid {
    fn columns_count(&self, _: &App) -> usize {
        self.headers.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, index: usize, _: &App) -> Column {
        let column = Column::new(format!("column-{index}"), self.headers[index])
            .width(self.widths[index])
            .min_width(px(32.))
            .resizable(true)
            .movable(false);
        if self.headers[index] == "#" {
            column.text_right()
        } else {
            column
        }
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let text = self
            .rows
            .get(row)
            .and_then(|row| row.get(column))
            .map_or_else(String::new, |text| one_line(text));
        div().truncate().child(SharedString::from(text))
    }

    fn cell_label(&self, row: usize, column: usize, _: &App) -> Option<SharedString> {
        Some(SharedString::from(self.rows.get(row)?.get(column)?.clone()))
    }

    fn cell_text(&self, row: usize, column: usize, _: &App) -> String {
        self.rows
            .get(row)
            .and_then(|row| row.get(column))
            .cloned()
            .unwrap_or_default()
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .id(("structure-empty", self.list as usize))
            .test_support()
            .size_full()
            .flex()
            .justify_center()
            .pt_6()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(self.empty.clone())
    }
}

type Headers = &'static [&'static str];

/// The headers and rows of a list.
fn list_rows(structure: &TableStructure, list: List) -> (Headers, Vec<Vec<String>>) {
    let yes = |value: bool| if value { "yes" } else { "no" }.to_string();
    let table_name = |table: &TableRef| format!("{}.{}", table.schema, table.name);
    match list {
        List::Columns => {
            static HEADERS: [&str; 9] = [
                "#",
                "Name",
                "Type",
                "Nullable",
                "Default",
                "Key",
                "Generated",
                "Comment",
                "On update",
            ];
            // "On update" only where a column has one (MySQL, MariaDB).
            let on_update = structure.columns.iter().any(|c| c.on_update.is_some());
            let headers: Headers = if on_update { &HEADERS } else { &HEADERS[..8] };
            (
                headers,
                structure
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let mut row = vec![
                            (index + 1).to_string(),
                            column.name.clone(),
                            column.data_type.clone(),
                            yes(column.nullable),
                            column.default.clone().unwrap_or_default(),
                            key_of(structure, column),
                            generation_of(column.generation.as_ref()),
                            column.comment.clone().unwrap_or_default(),
                        ];
                        if on_update {
                            row.push(column.on_update.clone().unwrap_or_default());
                        }
                        row
                    })
                    .collect(),
            )
        }
        List::Indexes => (
            &["Name", "Columns", "Kind", "Method", "Definition"],
            structure
                .indexes
                .iter()
                .map(|index| {
                    let kind = if index.primary {
                        "primary key"
                    } else if index.unique {
                        "unique"
                    } else {
                        ""
                    };
                    let definition = index.definition.clone().or_else(|| {
                        index
                            .predicate
                            .as_ref()
                            .map(|predicate| format!("WHERE {predicate}"))
                    });
                    vec![
                        index.name.clone(),
                        index.columns.join(", "),
                        kind.to_string(),
                        index.method.clone(),
                        definition.unwrap_or_default(),
                    ]
                })
                .collect(),
        ),
        List::Constraints => (
            &["Name", "Kind", "Columns", "Definition"],
            structure
                .constraints
                .iter()
                .map(|constraint| {
                    let kind = match constraint.kind {
                        ConstraintKind::PrimaryKey => "PRIMARY KEY",
                        ConstraintKind::Unique => "UNIQUE",
                        ConstraintKind::Check => "CHECK",
                        ConstraintKind::Exclusion => "EXCLUDE",
                    };
                    vec![
                        constraint.name.clone(),
                        kind.to_string(),
                        constraint.columns.join(", "),
                        constraint.definition.clone(),
                    ]
                })
                .collect(),
        ),
        List::ForeignKeys => (
            &[
                "Name",
                "Columns",
                "References",
                "Referenced columns",
                "On update",
                "On delete",
            ],
            structure
                .foreign_keys
                .iter()
                .map(|key| {
                    vec![
                        key.name.clone(),
                        key.columns.join(", "),
                        table_name(&key.referenced_table),
                        key.referenced_columns.join(", "),
                        key.on_update.clone(),
                        key.on_delete.clone(),
                    ]
                })
                .collect(),
        ),
        List::ReferencedBy => (
            &[
                "Name",
                "Table",
                "Columns",
                "Referenced columns",
                "On update",
                "On delete",
            ],
            structure
                .referenced_by
                .iter()
                .map(|key| {
                    vec![
                        key.name.clone(),
                        table_name(&key.table),
                        key.columns.join(", "),
                        key.referenced_columns.join(", "),
                        key.on_update.clone(),
                        key.on_delete.clone(),
                    ]
                })
                .collect(),
        ),
        List::Triggers => (
            &["Name", "Timing", "Events", "Level", "Enabled", "Action"],
            structure
                .triggers
                .iter()
                .map(|trigger| {
                    vec![
                        trigger.name.clone(),
                        trigger.timing.clone(),
                        trigger.events.clone(),
                        trigger.level.clone(),
                        yes(trigger.enabled),
                        trigger.action.clone(),
                    ]
                })
                .collect(),
        ),
    }
}

/// "PK", "PK 2" in a key of several columns, or "unique" when a unique
/// constraint or index holds this column alone.
fn key_of(structure: &TableStructure, column: &ColumnInfo) -> String {
    if let Some(position) = column.primary_key_position {
        let key_size = structure
            .columns
            .iter()
            .filter(|each| each.primary_key_position.is_some())
            .count();
        return if key_size > 1 {
            format!("PK {position}")
        } else {
            "PK".into()
        };
    }
    let alone = std::slice::from_ref(&column.name);
    let unique =
        structure.constraints.iter().any(|constraint| {
            constraint.kind == ConstraintKind::Unique && constraint.columns == alone
        }) || structure.indexes.iter().any(|index| {
            index.unique && !index.primary && index.predicate.is_none() && index.columns == alone
        });
    if unique { "unique" } else { "" }.into()
}

fn generation_of(generation: Option<&ColumnGeneration>) -> String {
    match generation {
        None => String::new(),
        Some(ColumnGeneration::IdentityAlways) => "identity, always".into(),
        Some(ColumnGeneration::IdentityByDefault) => "identity, by default".into(),
        Some(ColumnGeneration::AutoIncrement) => "auto increment".into(),
        Some(ColumnGeneration::Stored(expression)) => format!("stored: {expression}"),
        Some(ColumnGeneration::Virtual(expression)) => format!("virtual: {expression}"),
    }
}
