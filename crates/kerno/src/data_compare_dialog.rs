//! "Compare data with…" (K60, D-126): the dialog that picks the two tables
//! of a data compare, a saved connection with its database, schema and
//! table each (the database alone on engines without schemas), the key
//! their rows are matched by and the columns compared, then opens the data
//! compare tab. It is the schema compare's dialog (K30) with a table: a
//! click picks a connection and connects it when it is not; the target
//! starts on the source's place and table; two engines of different
//! families, tables without a key and columns that cannot be compared are
//! said in plain words before anything is read. Both tables' structures
//! are read here, on a session opened for the read and closed after it.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input as TextInput, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, WeakEntity, Window, div, px,
};
use kerno_core::data_compare::{self, Request, Shape};
use kerno_core::schema::{TableRef, TableStructure};
use kerno_core::{Session, describe};

use crate::bridge;
use crate::compare_tab::compared_schema;
use crate::connections::manager::{Connections, Status};
use crate::data_compare_tab::Spec;
use crate::tabs::{Target, fit_window};
use crate::workspace::Workspace;

/// One side being chosen.
struct Picker {
    connection: Option<String>,
    database: Entity<InputState>,
    schema: Entity<InputState>,
    table: Entity<InputState>,
}

/// What was read of a side's place and table.
#[derive(Default)]
struct Read {
    /// The place and table read, or being read.
    of: Option<(Target, String)>,
    tables: Option<Result<Vec<String>, String>>,
    structure: Option<Result<TableStructure, String>>,
    task: Option<Task<()>>,
}

pub struct DataCompareDialog {
    connections: Entity<Connections>,
    workspace: WeakEntity<Workspace>,
    /// Source, then target.
    pickers: [Picker; 2],
    reads: [Read; 2],
    /// The key picked, of the shape's keys.
    key: usize,
    /// Columns left out of the compare.
    unticked: HashSet<String>,
    _subscriptions: Vec<Subscription>,
}

/// Waits this long after the last change of a field before reading.
const SETTLE: Duration = Duration::from_millis(250);

/// Opens the dialog, both sides on `source` (a table of the tree, the
/// active data tab's) when given.
pub fn open(
    connections: Entity<Connections>,
    workspace: WeakEntity<Workspace>,
    source: Option<(Target, String)>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DataCompareDialog> {
    let view = cx.new(|cx| DataCompareDialog::new(connections, workspace, source, window, cx));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title("Compare data")
            .w(px(860.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    view
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

/// The tables of `schema` and the structure of `table` in it, on a
/// session of `connection` in `database` opened for the read.
async fn read_side(
    session: Arc<dyn Session>,
    schema: String,
    table: String,
) -> (
    Result<Vec<String>, String>,
    Option<Result<TableStructure, String>>,
) {
    let tables = session
        .objects(&schema)
        .await
        .map(|objects| {
            objects
                .relations
                .into_iter()
                .filter(|relation| relation.kind.is_table() && !relation.is_partition)
                .map(|relation| relation.name)
                .collect()
        })
        .map_err(|error| describe(&error).to_string());
    let structure = if table.is_empty() {
        None
    } else {
        let read = session
            .table_structure(&TableRef::new(schema, table))
            .await
            .map_err(|error| describe(&error).to_string());
        Some(read)
    };
    session.close().await;
    (tables, structure)
}

impl DataCompareDialog {
    fn new(
        connections: Entity<Connections>,
        workspace: WeakEntity<Workspace>,
        source: Option<(Target, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut picker = |cx: &mut Context<Self>| Picker {
            connection: None,
            database: cx.new(|cx| InputState::new(window, cx).placeholder("database")),
            schema: cx.new(|cx| InputState::new(window, cx).placeholder("schema")),
            table: cx.new(|cx| InputState::new(window, cx).placeholder("table")),
        };
        let pickers = [picker(cx), picker(cx)];
        let mut subscriptions: Vec<Subscription> = pickers
            .iter()
            .enumerate()
            .flat_map(|(index, picker)| {
                [&picker.database, &picker.schema, &picker.table].map(|input| (index, input))
            })
            .map(|(index, input)| {
                cx.subscribe(input, move |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.read(index, cx);
                        cx.notify();
                    }
                })
            })
            .collect();
        // A connection that connects fills what its session knows.
        subscriptions.push(cx.observe_in(&connections, window, |this, _, window, cx| {
            for index in 0..2 {
                this.fill_from_session(index, window, cx);
                this.read(index, cx);
            }
            cx.notify();
        }));
        let mut dialog = Self {
            connections,
            workspace,
            pickers,
            reads: Default::default(),
            key: 0,
            unticked: HashSet::new(),
            _subscriptions: subscriptions,
        };
        if let Some((target, table)) = source {
            for index in 0..2 {
                dialog.choose(index, target.connection.clone(), window, cx);
                dialog.set_place(index, &target, &table, window, cx);
            }
        }
        dialog
    }

    fn set_place(
        &mut self,
        index: usize,
        target: &Target,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picker = &self.pickers[index];
        let fields = [
            (&picker.database, target.database.clone()),
            (&picker.schema, target.schema.clone()),
            (&picker.table, Some(table.to_string())),
        ];
        for (input, text) in fields {
            if let Some(text) = text {
                input.update(cx, |input, cx| input.set_value(text, window, cx));
            }
        }
        self.read(index, cx);
    }

    /// Whether the side's connection has schemas; `None` before it is open.
    fn has_schemas(&self, index: usize, cx: &App) -> Option<bool> {
        let id = self.pickers[index].connection.as_ref()?;
        Some(
            self.connections
                .read(cx)
                .meta_session(id)?
                .dialect()
                .has_schemas(),
        )
    }

    /// Empty fields take what the connection's session says: its database,
    /// its current schema.
    fn fill_from_session(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.pickers[index].connection.clone() else {
            return;
        };
        let Some(meta) = self.connections.read(cx).meta_session(&id) else {
            return;
        };
        let picker = &self.pickers[index];
        if value(&picker.database, cx).is_empty()
            && let Some(database) = meta.database()
        {
            picker
                .database
                .update(cx, |input, cx| input.set_value(database, window, cx));
        }
        if value(&picker.schema, cx).is_empty()
            && let Some(schema) = meta.schema()
        {
            picker
                .schema
                .update(cx, |input, cx| input.set_value(schema, window, cx));
        }
    }

    /// Picks the connection of the side `index` (0: source), and connects
    /// it when it is not. The table's name stays: the same table elsewhere
    /// is the usual compare.
    pub fn choose(
        &mut self,
        index: usize,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(saved) = self.connections.read(cx).get(&id) else {
            return;
        };
        let database = saved.config.database.clone().unwrap_or_default();
        let picker = &mut self.pickers[index];
        picker.connection = Some(id.clone());
        picker
            .database
            .update(cx, |input, cx| input.set_value(database, window, cx));
        picker
            .schema
            .update(cx, |input, cx| input.set_value("", window, cx));
        if matches!(
            self.connections.read(cx).status(&id),
            Status::Disconnected | Status::Failed { .. }
        ) {
            self.connections
                .update(cx, |connections, cx| connections.connect(&id, cx));
        }
        self.fill_from_session(index, window, cx);
        self.read(index, cx);
        cx.notify();
    }

    /// Exchanges the source and the target.
    pub fn swap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values: Vec<(Option<String>, [String; 3])> = self
            .pickers
            .iter()
            .map(|picker| {
                (
                    picker.connection.clone(),
                    [&picker.database, &picker.schema, &picker.table].map(|input| value(input, cx)),
                )
            })
            .collect();
        for (picker, (connection, texts)) in self.pickers.iter_mut().zip(values.into_iter().rev()) {
            picker.connection = connection;
            for (input, text) in [&picker.database, &picker.schema, &picker.table]
                .into_iter()
                .zip(texts)
            {
                input.update(cx, |input, cx| input.set_value(text, window, cx));
            }
        }
        self.reads.swap(0, 1);
        self.key = 0;
        cx.notify();
    }

    #[cfg(test)]
    pub fn set_fields(
        &mut self,
        index: usize,
        database: &str,
        schema: &str,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picker = &self.pickers[index];
        for (input, text) in [
            (&picker.database, database),
            (&picker.schema, schema),
            (&picker.table, table),
        ] {
            input.update(cx, |input, cx| {
                input.set_value(text.to_string(), window, cx)
            });
        }
        self.read(index, cx);
        cx.notify();
    }

    /// The place of the side `index`, if it is named.
    fn place(&self, index: usize, cx: &App) -> Option<Target> {
        let picker = &self.pickers[index];
        let connection = picker.connection.clone()?;
        let database = Some(value(&picker.database, cx)).filter(|name| !name.is_empty());
        let schema = Some(value(&picker.schema, cx)).filter(|name| !name.is_empty());
        let schema = match self.has_schemas(index, cx) {
            Some(false) => None,
            Some(true) => Some(schema?),
            None => return None,
        };
        database.as_ref()?;
        Some(Target {
            connection,
            database,
            schema,
        })
    }

    fn table(&self, index: usize, cx: &App) -> String {
        value(&self.pickers[index].table, cx)
    }

    /// Reads the tables of the side's place and the structure of its table,
    /// once its fields stop changing, when they changed since the last read.
    fn read(&mut self, index: usize, cx: &mut Context<Self>) {
        let place = self.place(index, cx);
        let connected = place.as_ref().is_some_and(|place| {
            matches!(
                self.connections.read(cx).status(&place.connection),
                Status::Connected { .. }
            )
        });
        let of = place.map(|place| (place, self.table(index, cx)));
        if !connected || of.is_none() {
            self.reads[index] = Read::default();
            return;
        }
        if self.reads[index].of == of {
            return;
        }
        let Some((place, table)) = of.clone() else {
            return;
        };
        let read = &mut self.reads[index];
        // The table list stays while the place is the same.
        if read.of.as_ref().map(|(place, _)| place) != Some(&place) {
            read.tables = None;
        }
        read.of = of;
        read.structure = None;
        let schema = compared_schema(&place);
        read.task = Some(cx.spawn(async move |this, cx| {
            let settle = async { tokio::time::sleep(SETTLE).await };
            cx.update(|cx| bridge::spawn(cx, settle)).await;
            let opening = this.update(cx, |this, cx| {
                this.connections.read(cx).open_session(
                    &place.connection,
                    place.database.clone(),
                    cx,
                )
            });
            let Ok(opening) = opening else {
                return;
            };
            let session = match opening.await {
                Ok(session) => session,
                Err(error) => {
                    let message = describe(&error).to_string();
                    let _ = this.update(cx, |this, cx| {
                        this.reads[index].tables = Some(Err(message));
                        cx.notify();
                    });
                    return;
                }
            };
            let (tables, structure) = cx
                .update(|cx| bridge::spawn(cx, read_side(session, schema, table)))
                .await;
            let _ = this.update(cx, |this, cx| {
                let read = &mut this.reads[index];
                read.tables = Some(tables);
                read.structure = structure;
                cx.notify();
            });
        }));
    }

    /// The two structures, once both are read.
    fn structures(&self) -> Option<(&TableStructure, &TableStructure)> {
        match (&self.reads[0].structure, &self.reads[1].structure) {
            (Some(Ok(source)), Some(Ok(target))) => Some((source, target)),
            _ => None,
        }
    }

    /// What can be compared of the two tables, or why nothing can.
    pub fn shape(&self) -> Option<Result<Shape, String>> {
        let (source, target) = self.structures()?;
        Some(Shape::of(source, target))
    }

    /// The key the rows are matched by.
    fn key(&self, shape: &Shape) -> Vec<String> {
        shape
            .keys
            .get(self.key)
            .or(shape.keys.first())
            .cloned()
            .unwrap_or_default()
    }

    /// What the compare would be asked, once everything is in place.
    pub fn request(&self, cx: &App) -> Option<Request> {
        let shape = self.shape()?.ok()?;
        let key = self.key(&shape);
        let columns = shape
            .shared
            .iter()
            .filter(|column| column.refused.is_none())
            .filter(|column| !key.contains(&column.name))
            .filter(|column| !self.unticked.contains(&column.name))
            .map(|column| column.name.clone())
            .collect();
        let table = |index: usize| -> Option<TableRef> {
            let place = self.place(index, cx)?;
            Some(TableRef::new(
                compared_schema(&place),
                self.table(index, cx),
            ))
        };
        Some(Request {
            source: table(0)?,
            target: table(1)?,
            key,
            columns,
        })
    }

    pub fn pick_key(&mut self, key: usize, cx: &mut Context<Self>) {
        self.key = key;
        cx.notify();
    }

    pub fn set_ticked(&mut self, column: &str, ticked: bool, cx: &mut Context<Self>) {
        if ticked {
            self.unticked.remove(column);
        } else {
            self.unticked.insert(column.to_string());
        }
        cx.notify();
    }

    /// Why Compare cannot be clicked yet, in plain words.
    pub fn problem(&self, cx: &App) -> Option<String> {
        let manager = self.connections.read(cx);
        let engine = |index: usize| {
            let id = self.pickers[index].connection.as_ref()?;
            Some(manager.get(id)?.config.engine)
        };
        let (source, target) = match (engine(0), engine(1)) {
            (Some(source), Some(target)) => (source, target),
            (Some(_), None) => return Some("Pick the target connection.".into()),
            (None, Some(_)) => return Some("Pick the source connection.".into()),
            (None, None) => {
                return Some("Pick the connection of the source and of the target.".into());
            }
        };
        if let Some(refusal) = data_compare::refusal(source, target) {
            return Some(refusal);
        }
        for (index, role) in [(0, "source"), (1, "target")] {
            let id = self.pickers[index]
                .connection
                .as_deref()
                .unwrap_or_default();
            match manager.status(id) {
                Status::Connected { .. } => {}
                Status::Failed { .. } => {
                    return Some(format!(
                        "The {role} connection could not connect: pick it again to retry."
                    ));
                }
                _ => return Some(format!("Connecting the {role}…")),
            }
            if self.place(index, cx).is_none() || self.table(index, cx).is_empty() {
                return Some(match self.has_schemas(index, cx) {
                    Some(false) => format!("Name the {role} database and table."),
                    _ => format!("Name the {role} database, schema and table."),
                });
            }
        }
        let side = |index| (self.place(index, cx), self.table(index, cx));
        if side(0) == side(1) {
            return Some(
                "The source and the target are the same table: pick another table, schema, \
                 database or connection for the target."
                    .into(),
            );
        }
        for (index, role) in [(0, "source"), (1, "target")] {
            match &self.reads[index].structure {
                None => return Some(format!("Reading the {role} table…")),
                Some(Err(error)) => {
                    return Some(format!("The {role} table could not be read: {error}"));
                }
                Some(Ok(structure)) if structure.columns.is_empty() => {
                    return Some(format!(
                        "There is no table “{}” there: pick the {role} table in the list.",
                        self.table(index, cx)
                    ));
                }
                Some(Ok(_)) => {}
            }
        }
        self.shape()?.err()
    }

    /// Opens the data compare tab and closes the dialog.
    pub fn compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.problem(cx).is_some() {
            return;
        }
        let (Some(source), Some(target), Some(request)) =
            (self.place(0, cx), self.place(1, cx), self.request(cx))
        else {
            return;
        };
        window.close_dialog(cx);
        let spec = Spec {
            source,
            target,
            request,
        };
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.open_data_compare(spec, window, cx);
        });
    }

    fn render_picker(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let manager = self.connections.read(cx);
        let picker = &self.pickers[index];
        let (heading, hint) = if index == 0 {
            ("Source", "the model")
        } else {
            ("Target", "what the script changes")
        };
        let list = v_flex()
            .id(("data-compare-connections", index))
            .h(px(150.))
            .overflow_y_scroll()
            .p_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .children(manager.saved().iter().enumerate().map(|(row, saved)| {
                let config = &saved.config;
                let status = match manager.status(&saved.id) {
                    Status::Connected { .. } => "connected",
                    Status::Connecting | Status::WaitingForInput => "connecting…",
                    Status::Failed { .. } => "failed to connect",
                    Status::Disconnected => "not connected",
                };
                let mut details = vec![config.engine.label(), status];
                if config.read_only {
                    details.push("read-only");
                }
                let picked = picker.connection.as_deref() == Some(saved.id.as_str());
                let id = saved.id.clone();
                h_flex()
                    .id(("data-compare-connection", index * 1000 + row))
                    .test_support()
                    .test_text(config.name.clone())
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .when(picked, |this| this.bg(theme.accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(SharedString::from(config.name.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(details.join(" · "))),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose(index, id.clone(), window, cx)
                    }))
            }));
        let label = |text: &'static str| {
            div()
                .w(px(70.))
                .flex_none()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let field = |text: &'static str, id: (&'static str, usize), input: &Entity<InputState>| {
            h_flex()
                .gap_2()
                .child(label(text))
                .child(div().flex_1().child(TextInput::new(input).small().id(id)))
        };
        let tables = match &self.reads[index].tables {
            Some(Ok(tables)) => tables.clone(),
            _ => Vec::new(),
        };
        let view = cx.entity();
        let input = picker.table.clone();
        let menu = move |mut menu: PopupMenu, window: &mut Window, _: &mut Context<PopupMenu>| {
            for name in &tables {
                let (input, name) = (input.clone(), name.clone());
                let view = view.clone();
                menu = menu.item(
                    PopupMenuItem::new(SharedString::from(name.clone())).on_click(
                        move |_, window, cx| {
                            input.update(cx, |input, cx| input.set_value(name.clone(), window, cx));
                            view.update(cx, |this, cx| this.read(index, cx));
                        },
                    ),
                );
            }
            fit_window(menu, window)
        };
        let listed = matches!(&self.reads[index].tables, Some(Ok(tables)) if !tables.is_empty());
        let schemas = self.has_schemas(index, cx) != Some(false);
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(heading))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(hint),
                    ),
            )
            .child(list)
            .child(field(
                "Database",
                ("data-compare-database", index),
                &picker.database,
            ))
            .when(schemas, |this| {
                this.child(field(
                    "Schema",
                    ("data-compare-schema", index),
                    &picker.schema,
                ))
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(label("Table"))
                    .child(
                        div().flex_1().child(
                            TextInput::new(&picker.table)
                                .small()
                                .id(("data-compare-table", index)),
                        ),
                    )
                    .child(
                        Button::new(("data-compare-tables", index))
                            .outline()
                            .small()
                            .label("Tables")
                            .dropdown_caret(true)
                            .tooltip("The tables of this database and schema")
                            .disabled(!listed)
                            .dropdown_menu(menu),
                    ),
            )
            .into_any_element()
    }

    /// The key and the columns compared, once both tables are read.
    fn render_columns(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let shape = self.shape()?.ok()?;
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let key = self.key(&shape);
        let shown_key = |key: &[String]| format!("({})", key.join(", "));
        let view = cx.entity();
        let keys = shape.keys.clone();
        let picked = self.key;
        let key_menu = move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
            for (index, key) in keys.iter().enumerate() {
                let view = view.clone();
                menu = menu.item(
                    PopupMenuItem::new(SharedString::from(shown_key(key)))
                        .checked(index == picked)
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| this.pick_key(index, cx))
                        }),
                );
            }
            menu
        };
        let several = shape.keys.len() > 1;
        let notes: Vec<String> = [
            (&shape.only_in_source, "Only in the source, not compared"),
            (&shape.only_in_target, "Only in the target, not compared"),
        ]
        .into_iter()
        .filter(|(names, _)| !names.is_empty())
        .map(|(names, words)| format!("{words}: {}.", names.join(", ")))
        .chain(shape.shared.iter().filter_map(|column| {
            column
                .refused
                .as_ref()
                .map(|why| format!("“{}” is not compared: {why}.", column.name))
        }))
        .chain(shape.shared.iter().filter_map(|column| {
            column
                .unwritten
                .as_ref()
                .filter(|_| column.refused.is_none())
                .map(|why| format!("“{}”: {why}.", column.name))
        }))
        .collect();
        let ticks = shape
            .shared
            .iter()
            .enumerate()
            .filter(|(_, column)| column.refused.is_none() && !key.contains(&column.name))
            .map(|(index, column)| {
                let name = column.name.clone();
                Checkbox::new(("data-compare-column", index))
                    .label(SharedString::from(name.clone()))
                    .checked(!self.unticked.contains(&name))
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set_ticked(&name, *checked, cx)
                    }))
            })
            .collect::<Vec<_>>();
        Some(
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().w(px(70.)).text_sm().text_color(muted).child("Key"))
                        .map(|this| {
                            if several {
                                this.child(
                                    Button::new("data-compare-key")
                                        .outline()
                                        .small()
                                        .label(SharedString::from(shown_key(&key)))
                                        .dropdown_caret(true)
                                        .dropdown_menu(key_menu),
                                )
                            } else {
                                this.child(
                                    div()
                                        .id("data-compare-key")
                                        .test_support()
                                        .text_sm()
                                        .test_text(shown_key(&key))
                                        .child(SharedString::from(shown_key(&key))),
                                )
                            }
                        })
                        .child(div().text_xs().text_color(muted).child(
                            "a row of one table is matched with the row that has the same key",
                        )),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_start()
                        .child(
                            div()
                                .w(px(70.))
                                .flex_none()
                                .text_sm()
                                .text_color(muted)
                                .child("Columns"),
                        )
                        .child(
                            h_flex()
                                .id("data-compare-columns")
                                .flex_1()
                                .flex_wrap()
                                .gap_x_3()
                                .gap_y_1()
                                .max_h(px(96.))
                                .overflow_y_scroll()
                                .children(ticks),
                        ),
                )
                .when(!notes.is_empty(), |this| {
                    let text = notes.join("\n");
                    this.child(
                        div()
                            .id("data-compare-notes")
                            .test_support()
                            .text_xs()
                            .text_color(muted)
                            .whitespace_normal()
                            .test_text(text.clone())
                            .children(
                                notes
                                    .into_iter()
                                    .map(|note| div().child(SharedString::from(note))),
                            ),
                    )
                })
                .into_any_element(),
        )
    }
}

impl Render for DataCompareDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let problem = self.problem(cx);
        let muted = theme.muted_foreground;
        let source = self.render_picker(0, cx);
        let target = self.render_picker(1, cx);
        let columns = self.render_columns(cx);
        v_flex()
            .gap_3()
            .child(div().text_sm().text_color(muted).whitespace_normal().child(
                "Compares the rows of two tables by their key: rows only in the source, only in \
                 the target, or different. Both tables are only read; nothing is written. The \
                 script that makes the target like the source opens in a query tab, and nothing \
                 runs by itself.",
            ))
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(source)
                    .child(
                        div().pt(px(80.)).child(
                            Button::new("data-compare-dialog-swap")
                                .ghost()
                                .small()
                                .label("⇄")
                                .tooltip("Exchange the source and the target")
                                .on_click(cx.listener(|this, _, window, cx| this.swap(window, cx))),
                        ),
                    )
                    .child(target),
            )
            .children(columns)
            .when_some(problem.clone(), |this, problem| {
                this.child(
                    div()
                        .id("data-compare-problem")
                        .test_support()
                        .text_sm()
                        .whitespace_normal()
                        .text_color(muted)
                        .test_text(problem.clone())
                        .child(SharedString::from(problem)),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("data-compare-dialog-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("data-compare-dialog-start")
                            .primary()
                            .label("Compare")
                            .disabled(problem.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.compare(window, cx))),
                    ),
            )
    }
}
