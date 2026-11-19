//! The structure of one table or view, read from the server: its
//! properties (owner, sizes, engine, comment…), columns, indexes,
//! constraints, foreign keys both ways, triggers and DDL, each in a sub-tab
//! of its own. The comment is the one thing changed from here, through the
//! confirmation every write takes.

use gpui_kit::base::FocusableExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::table::{Column, DataTable, TableDelegate, TableEvent, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div, px,
};
use kerno_core::format::byte_size;
use kerno_core::schema::{
    ColumnGeneration, ColumnInfo, ConstraintKind, ObjectRef, RelationKind, TableRef, TableStructure,
};
use kerno_core::table_draft::TableChange;
use kerno_core::{Error, TxState, describe, exec};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::query::tab::TabStatus;
use crate::sql_confirm::{self, Confirmation, Statement};
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
    /// Draw this table and its neighbours in an ER diagram (J18).
    ViewDiagram { table: TableRef },
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

/// The sub-tabs, in order, with the lists each shows; the first is the
/// properties (DBeaver's object editor opens there), the last the DDL.
const SECTIONS: [(&str, &[List]); 7] = [
    ("Properties", &[]),
    ("Columns", &[List::Columns]),
    ("Indexes", &[List::Indexes]),
    ("Constraints", &[List::Constraints]),
    ("Foreign keys", &[List::ForeignKeys, List::ReferencedBy]),
    ("Triggers", &[List::Triggers]),
    ("DDL", &[]),
];

/// Index of the properties sub-tab.
pub const PROPERTIES_SECTION: usize = 0;
/// Index of the columns sub-tab, where "Open structure" shows a table.
pub const COLUMNS_SECTION: usize = 1;
/// Index of the DDL sub-tab.
pub const DDL_SECTION: usize = 6;

/// A property and its value (`None`: the server does not know).
pub(crate) type Properties = Vec<(String, Option<String>)>;

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
    /// The properties, or why they could not be read; `None` when the
    /// engine has no such query.
    properties: Option<Result<Properties, String>>,
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
            properties: None,
            section: COLUMNS_SECTION,
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
        let (table, kind) = (self.table.clone(), self.kind);
        let object = ObjectRef::Relation(table.clone(), kind);
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
                            // Each line a comment, the server's detail and hint too (C14).
                            let lines = describe(&error).lines().join("\n-- ");
                            format!("-- The DDL could not be read: {lines}\n")
                        }),
                        Err(_) => String::new(),
                    };
                    let properties = match session.dialect().properties_sql(&table, kind) {
                        Some(sql) if structure.is_ok() => Some(
                            exec::collect(session.clone(), sql)
                                .await
                                .map(properties_of)
                                .map_err(|error| describe(&error).to_string()),
                        ),
                        _ => None,
                    };
                    session.close().await;
                    Ok::<_, Error>((structure?, ddl, properties))
                })
            });
            let Ok(reading) = reading else {
                return;
            };
            let outcome = reading.await;
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok((structure, ddl, properties)) if this.generation == generation => {
                    this.properties = properties;
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
        self.state = State::Failed(describe(error).to_string());
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

    /// Asks for the table's new comment (empty removes it) with the
    /// statement that sets it, then runs it.
    pub fn edit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(reason) = self.read_only_reason(cx) {
            window.push_notification(Notification::warning(SharedString::from(reason)), cx);
            return;
        }
        let manager = self.connections.read(cx);
        let (Some(meta), Some(saved)) = (
            manager.meta_session(&self.target.connection),
            manager.get(&self.target.connection),
        ) else {
            return;
        };
        let environment = saved.config.environment.clone();
        let connection = saved.config.name.clone();
        let current = self
            .structure
            .as_ref()
            .and_then(|structure| structure.comment.clone())
            .unwrap_or_default();
        let table = self.table.clone();
        let build = move |text: &str| {
            let comment = (!text.is_empty()).then(|| text.to_string());
            meta.dialect()
                .alter_table_sql(&table, &[TableChange::SetTableComment(comment)])
                .map(|statements| statements.join(";\n"))
                .map_err(|error| error.to_string())
        };
        let view = cx.entity();
        sql_confirm::open(
            Confirmation {
                title: format!("Comment of {}", self.table.name),
                explanation: "The comment describes the table to whoever reads its structure. \
                              Leave the field empty to remove it."
                    .into(),
                statement: Statement::Text {
                    label: "Comment",
                    initial: current,
                    build: std::rc::Rc::new(build),
                },
                action: "Set comment",
                destructive: false,
                connection,
                environment,
                writes: true,
            },
            window,
            cx,
            move |sql, window, cx| view.update(cx, |this, cx| this.run_write(sql, window, cx)),
        );
    }

    /// Runs a confirmed statement on a session of its own, then reads the
    /// structure again.
    fn run_write(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let outcome = match opening.await {
                Ok(session) => {
                    let Ok(running) = cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            let outcome = exec::collect(session.clone(), sql).await;
                            session.close().await;
                            outcome
                        })
                    }) else {
                        return;
                    };
                    running.await
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let notification = match outcome {
                    Ok(_) => Notification::success("Changed the comment."),
                    Err(error) => crate::error_notification(format!(
                        "The comment was not changed: {}",
                        describe(&error)
                    )),
                };
                window.push_notification(notification, cx);
                this.reload(window, cx);
            });
        })
        .detach();
    }

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

    /// The properties as shown, or why they are not; for tests.
    #[cfg(test)]
    pub fn properties(&self) -> Result<Vec<(String, String)>, String> {
        match &self.properties {
            Some(Ok(properties)) => Ok(properties
                .iter()
                .map(|(name, value)| (name.clone(), property_text(name, value.as_deref())))
                .collect()),
            Some(Err(error)) => Err(error.clone()),
            None => Err("none".into()),
        }
    }

    /// The sub-tab on screen; for tests.
    #[cfg(test)]
    pub fn section(&self) -> usize {
        self.section
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
            .when(self.kind.is_table(), |bar| {
                bar.child(
                    Button::new("structure-diagram")
                        .ghost()
                        .small()
                        .label("Diagram")
                        .tooltip("Draw this table and the tables linked to it by foreign keys")
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.emit(StructureTabEvent::ViewDiagram {
                                table: this.table.clone(),
                            })
                        })),
                )
            })
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
                    crate::advice_after(
                        error,
                        &format!(
                            "Check that the {} still exists, then try again.",
                            self.noun()
                        ),
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
            _ if self.section == PROPERTIES_SECTION => self.render_properties(cx),
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

    /// The properties, one per line, and the comment's "Edit…" on a table.
    fn render_properties(&self, cx: &mut Context<Self>) -> AnyElement {
        let properties = match &self.properties {
            None => {
                return self.render_notice(
                    "structure-no-properties",
                    "No properties",
                    "This server does not describe its tables here. The columns, keys and DDL \
                     are in the other sub-tabs."
                        .into(),
                    None,
                    cx,
                );
            }
            Some(Err(error)) => {
                let retry = Button::new("structure-properties-retry")
                    .outline()
                    .label("Try again")
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx)))
                    .into_any_element();
                return self.render_notice(
                    "structure-properties-error",
                    "The properties could not be read",
                    crate::advice_after(
                        error,
                        &format!(
                            "The account may lack the privilege to read the catalog: the other \
                             sub-tabs may still show the {}.",
                            self.noun()
                        ),
                    ),
                    Some(retry),
                    cx,
                );
            }
            Some(Ok(properties)) if properties.is_empty() => {
                return self.render_notice(
                    "structure-properties-missing",
                    "Not found",
                    format!(
                        "The server did not find {}.{}: it may have been renamed or dropped. \
                         Refresh the tree.",
                        self.table.schema, self.table.name
                    ),
                    None,
                    cx,
                );
            }
            Some(Ok(properties)) => properties,
        };
        // Only a table's comment is set from here: a view's is part of its
        // source, and MySQL views have none.
        let editable = self.kind.is_table() && self.read_only_reason(cx).is_none();
        v_flex()
            .id("structure-properties")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .gap_1()
            .children(properties.iter().enumerate().map(|(index, (name, value))| {
                let comment = name == "Comment";
                property_line(name, value.as_deref(), cx)
                    .id(("structure-property", index))
                    .test_support()
                    .when(comment && editable, |this| {
                        this.child(
                            Button::new("structure-edit-comment")
                                .ghost()
                                .xsmall()
                                .label("Edit…")
                                .tooltip("Set or remove the table's comment")
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.edit_comment(window, cx)
                                    }),
                                ),
                        )
                    })
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

/// One property on its line: its name, then its value for people.
fn property_line(name: &str, value: Option<&str>, cx: &App) -> gpui_kit::Div {
    let theme = cx.theme();
    h_flex()
        .gap_3()
        .items_start()
        .text_sm()
        .child(
            div()
                .flex_none()
                .w(px(160.))
                .text_color(theme.muted_foreground)
                .child(SharedString::from(name.to_string())),
        )
        // The value takes only its own width: an action after it (the
        // comment's "Edit…") sits beside it.
        .child(
            div()
                .min_w_0()
                .when(value.is_none(), |this| {
                    this.text_color(theme.muted_foreground)
                })
                .child(SharedString::from(property_text(name, value))),
        )
}

/// Shows the properties of a database or a schema (DBeaver's object
/// editor, F4), read with [`properties_of`]: one line each, as the
/// Properties sub-tab of a table shows them.
pub(crate) fn open_properties(
    title: String,
    properties: Properties,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let body = if properties.is_empty() {
            div()
                .id("object-properties-missing")
                .test_support()
                .text_sm()
                .child("Not found: it may have been renamed or dropped. Refresh the tree.")
                .into_any_element()
        } else {
            v_flex()
                .id("object-properties")
                .test_support()
                .gap_1()
                .children(properties.iter().enumerate().map(|(index, (name, value))| {
                    let label = format!("{name}: {}", property_text(name, value.as_deref()));
                    property_line(name, value.as_deref(), cx)
                        .id(("object-property", index))
                        .test_support()
                        .aria_label(SharedString::from(label))
                }))
                .into_any_element()
        };
        dialog.title(title.clone()).w(px(520.)).child(body)
    });
}

/// The properties in the one row of a properties query: its columns.
pub(crate) fn properties_of(collected: exec::Collected) -> Properties {
    let Some(result) = collected.results.into_iter().next() else {
        return Vec::new();
    };
    let names: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
    match result.rows().into_iter().next() {
        Some(row) => names.into_iter().zip(row).collect(),
        None => Vec::new(),
    }
}

/// What a property shows: sizes for people, flags as words, a dash for
/// what the server does not know.
fn property_text(name: &str, value: Option<&str>) -> String {
    match value {
        None => "—".into(),
        Some(text) if name.ends_with(" size") => match text.parse::<u64>() {
            Ok(bytes) => byte_size(bytes),
            Err(_) => text.into(),
        },
        Some("t") => "yes".into(),
        Some("f") => "no".into(),
        Some(text) => text.into(),
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

#[cfg(test)]
mod tests {
    use super::property_text;

    #[test]
    fn properties_read_for_people() {
        assert_eq!(property_text("Total size", Some("16384")), "16 kB");
        assert_eq!(property_text("Total size", Some("n/a")), "n/a");
        assert_eq!(property_text("Row-level security", Some("f")), "no");
        assert_eq!(property_text("Owner", Some("kerno")), "kerno");
        assert_eq!(property_text("Comment", None), "—");
    }
}
