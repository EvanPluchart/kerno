//! A table as a form: its name, comment, columns, primary key, indexes and
//! foreign keys, with the statements that would make the server's table
//! match it, written live. Apply shows them and asks before anything runs.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, px,
};
use kerno_core::config::Environment;
use kerno_core::schema::{ColumnGeneration, ColumnInfo, TableRef, TableStructure};
use kerno_core::table_draft::{
    AutoIncrement, ColumnDraft, ForeignKeyDraft, IndexDraft, REFERENTIAL_ACTIONS, TableChange,
    TableDraft, changes,
};
use kerno_core::{Engine, Error, Session, TxState, exec};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::query::tab::TabStatus;
use crate::tabs::Target;
use crate::theme;

/// What a table editor tells the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableEditorEvent {
    /// The statements ran: the table `from` (`None` when it was created) is
    /// now `table`.
    Applied {
        from: Option<TableRef>,
        table: TableRef,
    },
}

/// Common types offered beside the type of a column.
fn common_types(engine: Engine) -> &'static [&'static str] {
    match engine {
        Engine::Postgres => &[
            "integer",
            "bigint",
            "numeric(10,2)",
            "text",
            "varchar(255)",
            "boolean",
            "date",
            "timestamptz",
            "uuid",
            "jsonb",
        ],
        Engine::MySql | Engine::MariaDb => &[
            "int",
            "bigint",
            "decimal(10,2)",
            "varchar(255)",
            "text",
            "tinyint(1)",
            "date",
            "datetime",
            "json",
        ],
    }
}

/// One column of the form.
struct ColumnRow {
    /// Ties the row to the keys and indexes that use it, across renames.
    id: u64,
    /// The column on the server; `None` for a new one.
    original: Option<ColumnInfo>,
    name: Entity<InputState>,
    data_type: Entity<InputState>,
    default: Entity<InputState>,
    comment: Entity<InputState>,
    nullable: bool,
    auto_increment: bool,
    _subscriptions: Vec<Subscription>,
}

impl ColumnRow {
    /// A stored or virtual column: its expression is not in the form.
    fn generated(&self) -> bool {
        self.original.as_ref().is_some_and(|column| {
            matches!(
                column.generation,
                Some(ColumnGeneration::Stored(_) | ColumnGeneration::Virtual(_))
            )
        })
    }
}

struct IndexRow {
    original: Option<String>,
    name: Entity<InputState>,
    /// Column rows, in index order.
    columns: Vec<u64>,
    unique: bool,
    _subscription: Subscription,
}

struct KeyRow {
    original: Option<String>,
    name: Entity<InputState>,
    /// Column rows, in key order.
    columns: Vec<u64>,
    /// `schema.table`, or a table of this schema.
    referenced_table: Entity<InputState>,
    /// Comma-separated.
    referenced_columns: Entity<InputState>,
    on_update: String,
    on_delete: String,
    _subscriptions: Vec<Subscription>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// The connection is not open.
    Waiting,
    Loading,
    Ready,
    Failed(String),
}

/// What Apply would run, or why it cannot.
type Statements = Result<(Vec<String>, bool), Vec<String>>;

/// A table being created, or an existing table being altered.
pub struct TableEditor {
    focus_handle: FocusHandle,
    connections: Entity<Connections>,
    target: Target,
    schema: String,
    /// The table on the server; `None` while it is being created.
    table: Option<TableRef>,
    structure: Option<TableStructure>,
    name: Entity<InputState>,
    comment: Entity<InputState>,
    columns: Vec<ColumnRow>,
    /// Column rows, in key order.
    primary_key: Vec<u64>,
    indexes: Vec<IndexRow>,
    keys: Vec<KeyRow>,
    /// Tables of the schema, offered to foreign keys.
    tables: Vec<String>,
    /// A new table as it was opened: what tells an untouched form.
    baseline: Option<TableDraft>,
    next_id: u64,
    state: State,
    applying: bool,
    /// Why the last apply failed, with the statement that did.
    error: Option<String>,
    load: Option<Task<()>>,
    generation: u64,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TableEditorEvent> for TableEditor {}

impl TableEditor {
    /// A form for `table` of `schema`, or for a new table when `None`.
    pub fn new(
        target: Target,
        schema: String,
        table: Option<TableRef>,
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = Self::input(window, cx, "", "Table name");
        let comment = Self::input(window, cx, "", "Comment");
        let follow = cx.observe_in(&connections, window, |this, _, window, cx| {
            match (&this.state, this.connected(cx)) {
                // A draft typed before the connection dropped is kept.
                (State::Waiting, true) => this.reload(false, window, cx),
                (State::Waiting, false) | (_, true) => {}
                (_, false) => {
                    this.state = State::Waiting;
                    cx.notify();
                }
            }
        });
        let mut editor = Self {
            focus_handle: cx.focus_handle(),
            connections,
            target,
            schema: schema.clone(),
            table,
            structure: None,
            name,
            comment,
            columns: Vec::new(),
            primary_key: Vec::new(),
            indexes: Vec::new(),
            keys: Vec::new(),
            tables: Vec::new(),
            baseline: None,
            next_id: 0,
            state: State::Waiting,
            applying: false,
            error: None,
            load: None,
            generation: 0,
            _subscriptions: vec![follow],
        };
        if editor.table.is_none() {
            editor.fill(
                &TableDraft::new(TableRef::new(schema, "")),
                None,
                window,
                cx,
            );
            editor.baseline = Some(editor.draft(cx));
        }
        editor.reload(false, window, cx);
        editor
    }

    pub fn title(&self) -> SharedString {
        match &self.table {
            None => "New table".into(),
            Some(table) => SharedString::from(format!("{} · edit", table.name)),
        }
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The table edited; `None` while creating one.
    pub fn table(&self) -> Option<&TableRef> {
        self.table.as_ref()
    }

    /// Whether the form differs from the table on the server.
    pub fn dirty(&self, cx: &App) -> bool {
        let draft = self.draft(cx);
        match (&self.table, &self.structure, &self.baseline) {
            (Some(table), Some(structure), _) => !changes(table, structure, &draft).is_empty(),
            (None, _, Some(baseline)) => *baseline != draft,
            _ => false,
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

    /// Why nothing can be applied from this form, when nothing can.
    pub fn read_only_reason(&self, cx: &App) -> Option<String> {
        let read_only = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .is_some_and(|saved| saved.config.read_only);
        read_only.then(|| {
            format!(
                "“{}” is a read-only connection: the table cannot be changed from here.",
                self.connection_name(cx)
            )
        })
    }

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
            rows: None,
            elapsed: None,
            notices: 0,
            pending: 0,
        }
    }

    // Loading -----------------------------------------------------------------

    /// A session for one load or one apply, closed once it is done: an
    /// open form holds no connection to the server.
    fn session(&self, cx: &App) -> Task<kerno_core::Result<Arc<dyn Session>>> {
        self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        )
    }

    /// Reads the table (when there is one) and the tables of the schema.
    /// With `refill`, the form is rebuilt from what was read; without, only
    /// what it is compared with changes.
    fn reload(&mut self, refill: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        if !self.connected(cx) {
            self.state = State::Waiting;
            cx.notify();
            return;
        }
        // A form already shown stays usable while it is read again.
        self.state = if self.columns.is_empty() {
            State::Loading
        } else {
            State::Ready
        };
        let previous = self.load.take();
        let opening = self.session(cx);
        let (table, schema) = (self.table.clone(), self.schema.clone());
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
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
                    let structure = match &table {
                        Some(table) => Some(session.table_structure(table).await),
                        None => None,
                    };
                    // The form works without them.
                    let tables = session
                        .objects(&schema)
                        .await
                        .map(|objects| {
                            objects
                                .relations
                                .into_iter()
                                .filter(|relation| relation.kind.is_table())
                                .map(|relation| relation.name)
                                .collect()
                        })
                        .unwrap_or_default();
                    session.close().await;
                    Ok::<_, Error>((structure.transpose()?, tables))
                })
            });
            let Ok(reading) = reading else {
                return;
            };
            let outcome = reading.await;
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok(_) if this.generation != generation => {}
                Ok((structure, tables)) => {
                    this.tables = tables;
                    this.state = State::Ready;
                    if let (Some(structure), Some(table)) = (structure, this.table.clone()) {
                        if refill || this.structure.is_none() {
                            let draft = TableDraft::from_structure(&table, &structure);
                            this.fill(&draft, Some(&structure), window, cx);
                        }
                        this.structure = Some(structure);
                    }
                    cx.notify();
                }
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

    // The form ----------------------------------------------------------------

    fn input(
        window: &mut Window,
        cx: &mut Context<Self>,
        value: &str,
        placeholder: &'static str,
    ) -> Entity<InputState> {
        let value = value.to_string();
        cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(value)
        })
    }

    /// Redraws (the preview with it) when `input` is typed in.
    fn follow(&self, input: &Entity<InputState>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(input, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        })
    }

    /// Replaces the form with `draft`; `structure` gives the columns as they
    /// are on the server.
    fn fill(
        &mut self,
        draft: &TableDraft,
        structure: Option<&TableStructure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.name.update(cx, |input, cx| {
            input.set_value(draft.table.name.clone(), window, cx)
        });
        let comment = draft.comment.clone().unwrap_or_default();
        self.comment
            .update(cx, |input, cx| input.set_value(comment, window, cx));
        self.columns.clear();
        for column in &draft.columns {
            let original = structure.and_then(|structure| {
                structure
                    .columns
                    .iter()
                    .find(|info| Some(&info.name) == column.original.as_ref())
                    .cloned()
            });
            self.push_column(column, original, window, cx);
        }
        // Keys and indexes name the rows of their columns.
        let ids = |names: &[String]| -> Vec<u64> {
            names
                .iter()
                .filter_map(|name| {
                    let position = draft.columns.iter().position(|c| &c.name == name)?;
                    Some(self.columns.get(position)?.id)
                })
                .collect()
        };
        self.primary_key = ids(&draft.primary_key);
        let index_columns: Vec<Vec<u64>> = draft.indexes.iter().map(|i| ids(&i.columns)).collect();
        let key_columns: Vec<Vec<u64>> =
            draft.foreign_keys.iter().map(|k| ids(&k.columns)).collect();
        self.indexes.clear();
        for (index, columns) in draft.indexes.iter().zip(index_columns) {
            self.push_index(index, columns, window, cx);
        }
        self.keys.clear();
        for (key, columns) in draft.foreign_keys.iter().zip(key_columns) {
            self.push_key(key, columns, window, cx);
        }
        self.error = None;
        cx.notify();
    }

    fn push_column(
        &mut self,
        column: &ColumnDraft,
        original: Option<ColumnInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> u64 {
        let name = Self::input(window, cx, &column.name, "name");
        let data_type = Self::input(window, cx, &column.data_type, "type");
        let default = Self::input(
            window,
            cx,
            column.default.as_deref().unwrap_or_default(),
            "default",
        );
        let comment = Self::input(
            window,
            cx,
            column.comment.as_deref().unwrap_or_default(),
            "comment",
        );
        let subscriptions = [&name, &data_type, &default, &comment]
            .into_iter()
            .map(|input| self.follow(input, cx))
            .collect();
        self.next_id += 1;
        self.columns.push(ColumnRow {
            id: self.next_id,
            original,
            name,
            data_type,
            default,
            comment,
            nullable: column.nullable,
            auto_increment: column.auto_increment.is_on(),
            _subscriptions: subscriptions,
        });
        self.next_id
    }

    fn push_index(
        &mut self,
        index: &IndexDraft,
        columns: Vec<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = Self::input(window, cx, &index.name, "index name");
        let subscription = self.follow(&name, cx);
        self.indexes.push(IndexRow {
            original: index.original.clone(),
            name,
            columns,
            unique: index.unique,
            _subscription: subscription,
        });
    }

    fn push_key(
        &mut self,
        key: &ForeignKeyDraft,
        columns: Vec<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = Self::input(window, cx, &key.name, "key name");
        let referenced = if key.referenced_table.name.is_empty() {
            String::new()
        } else {
            format!(
                "{}.{}",
                key.referenced_table.schema, key.referenced_table.name
            )
        };
        let referenced_table = Self::input(window, cx, &referenced, "schema.table");
        let referenced_columns = Self::input(
            window,
            cx,
            &key.referenced_columns.join(", "),
            "referenced columns",
        );
        let subscriptions = [&name, &referenced_table, &referenced_columns]
            .into_iter()
            .map(|input| self.follow(input, cx))
            .collect();
        self.keys.push(KeyRow {
            original: key.original.clone(),
            name,
            columns,
            referenced_table,
            referenced_columns,
            on_update: key.on_update.clone(),
            on_delete: key.on_delete.clone(),
            _subscriptions: subscriptions,
        });
    }

    /// The table as the form describes it.
    pub fn draft(&self, cx: &App) -> TableDraft {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let optional = |input: &Entity<InputState>| Some(text(input)).filter(|t| !t.is_empty());
        let names = |ids: &[u64]| -> Vec<String> {
            ids.iter()
                .filter_map(|id| self.columns.iter().find(|row| row.id == *id))
                .map(|row| text(&row.name))
                .collect()
        };
        TableDraft {
            table: TableRef::new(self.schema.clone(), text(&self.name)),
            comment: optional(&self.comment),
            columns: self
                .columns
                .iter()
                .map(|row| {
                    let data_type = text(&row.data_type);
                    let original = row.original.as_ref();
                    // `ON UPDATE` goes with the type it was set on.
                    let on_update = original
                        .filter(|info| info.data_type.trim().eq_ignore_ascii_case(&data_type))
                        .and_then(|info| info.on_update.clone());
                    ColumnDraft {
                        original: original.map(|info| info.name.clone()),
                        name: text(&row.name),
                        data_type,
                        nullable: row.nullable,
                        default: optional(&row.default),
                        comment: optional(&row.comment),
                        // An identity turned off and on keeps its kind.
                        auto_increment: AutoIncrement::new(row.auto_increment, original),
                        on_update,
                        invisible: original.is_some_and(|info| info.invisible),
                    }
                })
                .collect(),
            indexes: self
                .indexes
                .iter()
                .map(|index| IndexDraft {
                    original: index.original.clone(),
                    name: text(&index.name),
                    columns: names(&index.columns),
                    unique: index.unique,
                })
                .collect(),
            foreign_keys: self
                .keys
                .iter()
                .map(|key| {
                    let referenced = text(&key.referenced_table);
                    let referenced_table = match referenced.split_once('.') {
                        Some((schema, name)) => TableRef::new(schema.trim(), name.trim()),
                        None => TableRef::new(self.schema.clone(), referenced),
                    };
                    ForeignKeyDraft {
                        original: key.original.clone(),
                        name: text(&key.name),
                        columns: names(&key.columns),
                        referenced_table,
                        referenced_columns: text(&key.referenced_columns)
                            .split(',')
                            .map(str::trim)
                            .filter(|name| !name.is_empty())
                            .map(str::to_string)
                            .collect(),
                        on_update: key.on_update.clone(),
                        on_delete: key.on_delete.clone(),
                    }
                })
                .collect(),
            primary_key: names(&self.primary_key),
        }
    }

    /// The statements Apply would run and whether they drop something, or
    /// what keeps the form from being written as SQL.
    pub fn statements(&self, cx: &App) -> Statements {
        let Some(meta) = self
            .connections
            .read(cx)
            .meta_session(&self.target.connection)
        else {
            return Err(vec!["The connection is not open.".into()]);
        };
        let draft = self.draft(cx);
        let problems = draft.validate();
        if !problems.is_empty() {
            return Err(problems);
        }
        let dialect = meta.dialect();
        let written = match (&self.table, &self.structure) {
            (Some(table), Some(structure)) => {
                let changes = changes(table, structure, &draft);
                let destructive = changes.iter().any(|change| {
                    matches!(
                        change,
                        TableChange::DropColumn(_)
                            | TableChange::DropIndex(_)
                            | TableChange::DropPrimaryKey(_)
                            | TableChange::DropForeignKey(_)
                    )
                });
                dialect
                    .alter_table_sql(table, &changes)
                    .map(|statements| (statements, destructive))
            }
            (Some(_), None) => Ok((Vec::new(), false)),
            (None, _) => dialect
                .create_table_sql(&draft)
                .map(|statements| (statements, false)),
        };
        written.map_err(|error| vec![error.to_string()])
    }

    // Editing -----------------------------------------------------------------

    /// Adds an empty column at the end and puts the cursor in its name.
    pub fn add_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let empty = ColumnDraft {
            original: None,
            name: String::new(),
            data_type: String::new(),
            nullable: true,
            default: None,
            comment: None,
            auto_increment: AutoIncrement::Off,
            on_update: None,
            invisible: false,
        };
        self.push_column(&empty, None, window, cx);
        if let Some(row) = self.columns.last() {
            row.name.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Removes a column, and it from the keys and indexes.
    pub fn remove_column(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.columns.len() {
            return;
        }
        let id = self.columns.remove(index).id;
        self.primary_key.retain(|each| *each != id);
        for row in &mut self.indexes {
            row.columns.retain(|each| *each != id);
        }
        for row in &mut self.keys {
            row.columns.retain(|each| *each != id);
        }
        cx.notify();
    }

    /// Moves a column of a new table one place up (`-1`) or down (`1`).
    pub fn move_column(&mut self, index: usize, by: isize, cx: &mut Context<Self>) {
        let Some(to) = index.checked_add_signed(by) else {
            return;
        };
        if self.table.is_none() && to < self.columns.len() && index < self.columns.len() {
            self.columns.swap(index, to);
            cx.notify();
        }
    }

    pub fn set_type(
        &mut self,
        index: usize,
        data_type: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.columns.get(index) {
            let data_type = data_type.to_string();
            row.data_type
                .update(cx, |input, cx| input.set_value(data_type, window, cx));
            cx.notify();
        }
    }

    fn set_primary_key(&mut self, index: usize, on: bool, cx: &mut Context<Self>) {
        let Some(row) = self.columns.get_mut(index) else {
            return;
        };
        let id = row.id;
        self.primary_key.retain(|each| *each != id);
        if on {
            self.primary_key.push(id);
            // A key column holds a value.
            row.nullable = false;
        }
        cx.notify();
    }

    pub fn add_index(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = IndexDraft {
            original: None,
            name: String::new(),
            columns: Vec::new(),
            unique: false,
        };
        self.push_index(&index, Vec::new(), window, cx);
        if let Some(row) = self.indexes.last() {
            row.name.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    pub fn add_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = ForeignKeyDraft {
            original: None,
            name: String::new(),
            columns: Vec::new(),
            referenced_table: TableRef::new(self.schema.clone(), ""),
            referenced_columns: Vec::new(),
            on_update: "NO ACTION".into(),
            on_delete: "NO ACTION".into(),
        };
        self.push_key(&key, Vec::new(), window, cx);
        if let Some(row) = self.keys.last() {
            row.name.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Puts a column in an index or key, at the end, or takes it out.
    fn toggle(list: &mut Vec<u64>, id: u64) {
        if list.contains(&id) {
            list.retain(|each| *each != id);
        } else {
            list.push(id);
        }
    }

    /// Sets the referenced table of a foreign key to a table of the schema.
    pub fn reference(
        &mut self,
        key: usize,
        table: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.keys.get(key) {
            let text = format!("{}.{table}", self.schema);
            row.referenced_table
                .update(cx, |input, cx| input.set_value(text, window, cx));
            cx.notify();
        }
    }

    /// Sets what a foreign key does on update (`on_delete` false) or delete.
    pub fn set_action(
        &mut self,
        key: usize,
        on_delete: bool,
        action: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(row) = self.keys.get_mut(key) {
            let field = if on_delete {
                &mut row.on_delete
            } else {
                &mut row.on_update
            };
            *field = action.to_string();
            cx.notify();
        }
    }

    // Applying ----------------------------------------------------------------

    /// Shows the statements and, once confirmed, runs them.
    pub fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.applying || self.read_only_reason(cx).is_some() {
            return;
        }
        let Ok((statements, destructive)) = self.statements(cx) else {
            return;
        };
        if statements.is_empty() {
            return;
        }
        let saved = self.connections.read(cx).get(&self.target.connection);
        let environment = saved.map_or(Environment::Local, |saved| saved.config.environment);
        let engine = saved.map_or(Engine::Postgres, |saved| saved.config.engine);
        let explanation = match engine {
            Engine::Postgres => "These statements run in one transaction: all of them take \
                                 effect, or none does."
                .to_string(),
            _ if statements.len() > 1 => format!(
                "{} applies each statement at once; a failure leaves the earlier ones applied.",
                engine.label()
            ),
            _ => format!(
                "{} applies this statement at once, all of it or none of it; it cannot be \
                 rolled back afterwards.",
                engine.label()
            ),
        };
        let name = self.draft(cx).table.name;
        let (title, action) = match &self.table {
            None => (format!("Create table {name}"), "Create table"),
            Some(table) => (format!("Alter table {}", table.name), "Apply"),
        };
        let view = cx.entity();
        let run = statements.clone();
        crate::sql_confirm::open(
            crate::sql_confirm::Confirmation {
                title,
                explanation,
                statement: crate::sql_confirm::Statement::Fixed(script(&statements)),
                action,
                destructive,
                connection: self.connection_name(cx),
                environment,
            },
            window,
            cx,
            move |_, window, cx| {
                view.update(cx, |this, cx| this.run(run.clone(), window, cx));
            },
        );
    }

    /// Runs confirmed statements on a session of their own.
    fn run(&mut self, statements: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let opening = self.session(cx);
        self.applying = true;
        self.error = None;
        let from = self.table.clone();
        let table = self.draft(cx).table;
        // One statement at a time on the session: after a reload in flight.
        let previous = self.load.take();
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let session = match opening.await {
                Ok(session) => session,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.applying = false;
                        this.error = Some(format!("Nothing was changed: {error}"));
                        cx.notify();
                    });
                    return;
                }
            };
            let transactional = session.dialect().engine() == Engine::Postgres;
            let to_run = statements.clone();
            let running = cx.update(|_, cx| {
                bridge::spawn(cx, async move {
                    let outcome = run_statements(session.clone(), to_run).await;
                    session.close().await;
                    outcome
                })
            });
            let Ok(running) = running else {
                return;
            };
            let outcome = running.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.applying = false;
                this.load = None;
                match outcome {
                    Ok(()) => {
                        window.push_notification(
                            Notification::success(SharedString::from(match &from {
                                None => format!("Created table {}.", table.name),
                                Some(_) => format!("Altered table {}.", table.name),
                            })),
                            cx,
                        );
                        cx.emit(TableEditorEvent::Applied {
                            from: from.clone(),
                            table: table.clone(),
                        });
                        // The form shows the table as it is now.
                        if from.is_some() {
                            this.table = Some(table);
                            this.reload(true, window, cx);
                        }
                    }
                    Err((failed, error)) => {
                        let at = failed.and_then(|index| Some((index, statements.get(index)?)));
                        let partial = !transactional && failed.is_some_and(|index| index > 0);
                        let mut message = if partial {
                            "The statements before the one that failed were applied. ".to_string()
                        } else {
                            "Nothing was changed. ".to_string()
                        };
                        match at {
                            Some((index, sql)) => message.push_str(&format!(
                                "Statement {} failed: {error}\n{sql}",
                                index + 1
                            )),
                            None => message.push_str(&error.to_string()),
                        }
                        this.error = Some(message);
                        // What the form is compared with changed.
                        if partial {
                            this.reload(false, window, cx);
                        }
                        cx.notify();
                    }
                }
            });
        }));
        cx.notify();
    }

    /// Why the last apply failed; for tests.
    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the form is shown; for tests.
    #[cfg(test)]
    pub fn ready(&self) -> bool {
        self.state == State::Ready
    }

    // Rendering ---------------------------------------------------------------

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let engine = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .map(|saved| saved.config.engine);
        let ready = self.state == State::Ready;
        let can_apply = ready
            && !self.applying
            && self
                .statements(cx)
                .is_ok_and(|(statements, _)| !statements.is_empty());
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
                    .flex_none()
                    .pl_1()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(format!("{}.", self.schema))),
            )
            .child(
                div()
                    .w(px(200.))
                    .flex_none()
                    .child(Input::new(&self.name).small().id("editor-name")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(120.))
                    .child(Input::new(&self.comment).small().id("editor-comment")),
            )
            .child(
                Button::new("editor-add-column")
                    .ghost()
                    .small()
                    .label("Add column")
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, window, cx| this.add_column(window, cx))),
            )
            .child(
                Button::new("editor-add-index")
                    .ghost()
                    .small()
                    .label("Add index")
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, window, cx| this.add_index(window, cx))),
            )
            .child(
                Button::new("editor-add-key")
                    .ghost()
                    .small()
                    .label("Add foreign key")
                    .disabled(!ready)
                    .on_click(cx.listener(|this, _, window, cx| this.add_key(window, cx))),
            )
            .map(|bar| match self.read_only_reason(cx) {
                Some(reason) => bar.child(
                    div()
                        .id("editor-read-only")
                        .test_support()
                        .flex_none()
                        .max_w(px(260.))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(reason)),
                ),
                None => bar.child(
                    Button::new("editor-apply")
                        .primary()
                        .small()
                        .label(if self.table.is_none() {
                            "Create"
                        } else {
                            "Apply"
                        })
                        .tooltip("Show the statements, then run them")
                        .disabled(!can_apply)
                        .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
                ),
            })
    }

    /// A small label above a field of the column rows.
    fn heading(text: &'static str, width: f32, cx: &App) -> impl IntoElement {
        div()
            .flex_none()
            .w(px(width))
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }

    fn render_columns(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let creating = self.table.is_none();
        let engine = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .map_or(Engine::Postgres, |saved| saved.config.engine);
        let draft = self.draft(cx);
        let view = cx.entity();
        let count = self.columns.len();
        let rows = self.columns.iter().enumerate().map(|(index, row)| {
            let generated = row.generated();
            let in_key = self.primary_key.contains(&row.id);
            let types = {
                let view = view.clone();
                move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                    for data_type in common_types(engine) {
                        let view = view.clone();
                        menu = menu.item(PopupMenuItem::new(*data_type).on_click(
                            move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.set_type(index, data_type, window, cx)
                                })
                            },
                        ));
                    }
                    menu
                }
            };
            // What the server keeps on this column beyond the form.
            let on_update = row
                .original
                .as_ref()
                .and_then(|info| info.on_update.clone());
            let kept = draft
                .columns
                .get(index)
                .is_some_and(|column| column.on_update.is_some());
            let note = match (generated, on_update) {
                (true, _) => Some(("generated: not editable here".to_string(), false)),
                (false, Some(expression)) if kept => {
                    Some((format!("on update {expression}"), false))
                }
                (false, Some(expression)) => Some((
                    format!("on update {expression} is dropped with the new type"),
                    true,
                )),
                (false, None) => None,
            };
            h_flex()
                .gap_1()
                // Removing a row is the first thing on it, next to its name.
                .child(
                    Button::new(("column-remove", index))
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .tooltip("Remove column")
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_column(index, cx))),
                )
                .when(creating, |line| {
                    line.child(
                        Button::new(("column-up", index))
                            .ghost()
                            .xsmall()
                            .label("↑")
                            .tooltip("Move up")
                            .disabled(index == 0)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.move_column(index, -1, cx)),
                            ),
                    )
                    .child(
                        Button::new(("column-down", index))
                            .ghost()
                            .xsmall()
                            .label("↓")
                            .tooltip("Move down")
                            .disabled(index + 1 == count)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.move_column(index, 1, cx)),
                            ),
                    )
                })
                .child(
                    div().flex_none().w(px(150.)).child(
                        Input::new(&row.name)
                            .small()
                            .id(("column-name", index))
                            .disabled(generated),
                    ),
                )
                .child(
                    div().flex_none().w(px(150.)).child(
                        Input::new(&row.data_type)
                            .small()
                            .id(("column-type", index))
                            .disabled(generated),
                    ),
                )
                .child(
                    Button::new(("column-types", index))
                        .ghost()
                        .xsmall()
                        .tooltip("Common types")
                        .disabled(generated)
                        .dropdown_caret(true)
                        .dropdown_menu(types),
                )
                .child(
                    Checkbox::new(("column-null", index))
                        .label("NULL")
                        .tooltip("Allows NULL")
                        .checked(row.nullable)
                        .disabled(generated)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            if let Some(row) = this.columns.get_mut(index) {
                                row.nullable = *checked;
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div().flex_none().w(px(120.)).child(
                        Input::new(&row.default)
                            .small()
                            .id(("column-default", index))
                            .disabled(generated),
                    ),
                )
                .child(
                    Checkbox::new(("column-key", index))
                        .label("PK")
                        .tooltip("In the primary key")
                        .checked(in_key)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.set_primary_key(index, *checked, cx)
                        })),
                )
                .child(
                    Checkbox::new(("column-auto", index))
                        .label("Auto")
                        .tooltip(if engine == Engine::Postgres {
                            "Generated by default as identity"
                        } else {
                            "AUTO_INCREMENT"
                        })
                        .checked(row.auto_increment)
                        .disabled(generated)
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            if let Some(row) = this.columns.get_mut(index) {
                                row.auto_increment = *checked;
                                if *checked {
                                    row.nullable = false;
                                }
                            }
                            cx.notify();
                        })),
                )
                .child(
                    div().flex_1().min_w(px(100.)).child(
                        Input::new(&row.comment)
                            .small()
                            .id(("column-comment", index)),
                    ),
                )
                .children(note.map(|(text, warning)| {
                    div()
                        .id(("column-note", index))
                        .test_support()
                        .flex_none()
                        .max_w(px(220.))
                        .truncate()
                        .text_xs()
                        .text_color(if warning {
                            theme.warning
                        } else {
                            theme.muted_foreground
                        })
                        .child(SharedString::from(text))
                }))
        });
        let mover = if creating { 48. } else { 0. };
        v_flex()
            .gap_1()
            .child(section_title("Columns", cx))
            .child(
                h_flex()
                    .gap_1()
                    .pl(px(mover))
                    .child(Self::heading("Name", 150., cx))
                    .child(Self::heading("Type", 178., cx))
                    .child(Self::heading("", 58., cx))
                    .child(Self::heading("Default", 120., cx))
                    .child(Self::heading("", 102., cx))
                    .child(Self::heading("Comment", 100., cx)),
            )
            .children(rows)
            .when(!creating, |section| {
                section.child(hint(
                    "Columns keep their order on the server; new ones are added at the end.",
                    cx,
                ))
            })
    }

    /// Buttons that put columns in an index or key, numbered in key order.
    fn column_picker(
        &self,
        prefix: &str,
        selected: &[u64],
        on_click: impl Fn(&mut Self, u64, &mut Context<Self>) + Clone + 'static,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .flex_wrap()
            .gap_1()
            .children(self.columns.iter().enumerate().map(|(position, row)| {
                let name = row.name.read(cx).value().trim().to_string();
                let name = if name.is_empty() {
                    format!("column {}", position + 1)
                } else {
                    name
                };
                let rank = selected.iter().position(|id| *id == row.id);
                let label = match rank {
                    Some(rank) if selected.len() > 1 => format!("{}. {name}", rank + 1),
                    _ => name,
                };
                let (id, on_click) = (row.id, on_click.clone());
                theme::chip(format!("{prefix}-column-{position}"), rank.is_some())
                    .xsmall()
                    .label(SharedString::from(label))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        on_click(this, id, cx);
                        cx.notify();
                    }))
            }))
    }

    fn render_indexes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<AnyElement> = self
            .indexes
            .iter()
            .enumerate()
            .map(|(index, row)| {
                h_flex()
                    .gap_1()
                    .items_start()
                    .child(
                        Button::new(("index-remove", index))
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .tooltip("Remove index")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if index < this.indexes.len() {
                                    this.indexes.remove(index);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(180.))
                            .child(Input::new(&row.name).small().id(("index-name", index))),
                    )
                    .child(
                        Checkbox::new(("index-unique", index))
                            .label("Unique")
                            .checked(row.unique)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                if let Some(row) = this.indexes.get_mut(index) {
                                    row.unique = *checked;
                                }
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1().min_w_0().child(self.column_picker(
                        &format!("index-{index}"),
                        &row.columns,
                        move |this, id, _| {
                            if let Some(row) = this.indexes.get_mut(index) {
                                Self::toggle(&mut row.columns, id);
                            }
                        },
                        cx,
                    )))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_1()
            .child(section_title("Indexes", cx))
            .when(rows.is_empty(), |section| {
                section.child(hint("No index. “Add index” adds one.", cx))
            })
            .children(rows)
            .when(self.table.is_some(), |section| {
                section.child(hint(
                    "The indexes of the primary key and of unique constraints, and partial, \
                     expression or non-B-tree indexes, are not listed and stay as they are.",
                    cx,
                ))
            })
    }

    fn render_keys(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let rows: Vec<AnyElement> = self
            .keys
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let tables = {
                    let (view, tables) = (view.clone(), self.tables.clone());
                    move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                        for table in &tables {
                            let (view, table) = (view.clone(), table.clone());
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(table.clone())).on_click(
                                    move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.reference(index, &table, window, cx)
                                        })
                                    },
                                ),
                            );
                        }
                        menu
                    }
                };
                let actions = |on_delete: bool| {
                    let view = view.clone();
                    move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                        for action in REFERENTIAL_ACTIONS {
                            let view = view.clone();
                            menu =
                                menu.item(PopupMenuItem::new(action).on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        this.set_action(index, on_delete, action, cx)
                                    })
                                }));
                        }
                        menu
                    }
                };
                v_flex()
                    .gap_1()
                    .pb_1()
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(("key-remove", index))
                                    .ghost()
                                    .small()
                                    .icon(IconName::Close)
                                    .tooltip("Remove foreign key")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if index < this.keys.len() {
                                            this.keys.remove(index);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(180.))
                                    .child(Input::new(&row.name).small().id(("key-name", index))),
                            )
                            .child(div().flex_none().text_xs().child("references"))
                            .child(
                                div().flex_none().w(px(180.)).child(
                                    Input::new(&row.referenced_table)
                                        .small()
                                        .id(("key-table", index)),
                                ),
                            )
                            .child(
                                Button::new(("key-tables", index))
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Tables of this schema")
                                    .disabled(self.tables.is_empty())
                                    .dropdown_caret(true)
                                    .dropdown_menu(tables),
                            )
                            .child(
                                div().flex_1().min_w(px(100.)).child(
                                    Input::new(&row.referenced_columns)
                                        .small()
                                        .id(("key-referenced", index)),
                                ),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_start()
                            .child(
                                Button::new(("key-on-update", index))
                                    .outline()
                                    .xsmall()
                                    .label(SharedString::from(format!(
                                        "On update {}",
                                        row.on_update
                                    )))
                                    .dropdown_caret(true)
                                    .dropdown_menu(actions(false)),
                            )
                            .child(
                                Button::new(("key-on-delete", index))
                                    .outline()
                                    .xsmall()
                                    .label(SharedString::from(format!(
                                        "On delete {}",
                                        row.on_delete
                                    )))
                                    .dropdown_caret(true)
                                    .dropdown_menu(actions(true)),
                            )
                            .child(div().flex_1().min_w_0().child(self.column_picker(
                                &format!("key-{index}"),
                                &row.columns,
                                move |this, id, _| {
                                    if let Some(row) = this.keys.get_mut(index) {
                                        Self::toggle(&mut row.columns, id);
                                    }
                                },
                                cx,
                            ))),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_1()
            .child(section_title("Foreign keys", cx))
            .when(rows.is_empty(), |section| {
                section.child(hint("No foreign key. “Add foreign key” adds one.", cx))
            })
            .children(rows)
            .when(self.table.is_some(), |section| {
                section.child(hint(
                    "Check constraints and triggers are not in this form and stay as they are.",
                    cx,
                ))
            })
    }

    /// The statements, or why there are none.
    fn render_preview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let body = match self.statements(cx) {
            Err(problems) => div()
                .id("editor-problems")
                .test_support()
                .text_sm()
                .text_color(theme.danger)
                .children(
                    problems
                        .into_iter()
                        .map(|problem| div().child(SharedString::from(problem))),
                )
                .into_any_element(),
            Ok((statements, _)) if statements.is_empty() => div()
                .id("editor-no-change")
                .test_support()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No change: the form matches the table.")
                .into_any_element(),
            Ok((statements, _)) => div()
                .id("editor-preview")
                .test_support()
                .text_sm()
                .font_family(theme.mono_font_family.clone())
                .children(
                    script(&statements)
                        .lines()
                        .map(|line| div().child(SharedString::from(line.to_string())))
                        .collect::<Vec<_>>(),
                )
                .into_any_element(),
        };
        // The remaining height, at least 200 px: the form above takes what
        // it needs, so a short table leaves no empty band between the two.
        v_flex()
            .flex_1()
            .min_h(px(200.))
            .border_t_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("DDL preview"),
            )
            .child(
                v_flex()
                    .id("editor-preview-pane")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .pb_2()
                    .gap_2()
                    .children(self.error.clone().map(|error| {
                        div()
                            .id("editor-error")
                            .test_support()
                            .text_sm()
                            .text_color(theme.danger)
                            .children(
                                error
                                    .lines()
                                    .map(|line| div().child(SharedString::from(line.to_string())))
                                    .collect::<Vec<_>>(),
                            )
                    }))
                    .child(body),
            )
    }

    fn render_notice(&self, id: &'static str, title: &str, detail: String, cx: &App) -> AnyElement {
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
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let what = match &self.table {
            Some(table) => format!("{}.{}", table.schema, table.name),
            None => "the new table".into(),
        };
        match &self.state {
            State::Waiting => self.render_notice(
                "editor-waiting",
                "Not connected",
                format!(
                    "“{}” is not connected. Connect it in the sidebar to edit {what}.",
                    self.connection_name(cx)
                ),
                cx,
            ),
            State::Failed(error) => self.render_notice(
                "editor-failed",
                "The table could not be read",
                format!("{error} Check that it still exists, then open the form again."),
                cx,
            ),
            State::Loading if self.columns.is_empty() => self.render_notice(
                "editor-loading",
                "Loading the table…",
                format!("Reading {what}."),
                cx,
            ),
            _ => v_flex()
                .size_full()
                .child(
                    v_flex()
                        .id("editor-form")
                        .flex_initial()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_3()
                        .gap_4()
                        .child(self.render_columns(cx))
                        .child(self.render_indexes(cx))
                        .child(self.render_keys(cx)),
                )
                .child(self.render_preview(cx))
                .into_any_element(),
        }
    }
}

/// The statements as one script.
fn script(statements: &[String]) -> String {
    statements
        .iter()
        .map(|statement| format!("{statement};"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn section_title(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_semibold()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn hint(text: &'static str, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// Runs `statements` in order; on PostgreSQL in one transaction, rolled
/// back when one fails. The error names the statement that failed, when
/// one did.
async fn run_statements(
    session: Arc<dyn Session>,
    statements: Vec<String>,
) -> Result<(), (Option<usize>, Error)> {
    // DDL is transactional on PostgreSQL only; MySQL commits each statement.
    let transactional = session.dialect().engine() == Engine::Postgres;
    if transactional {
        exec::collect(session.clone(), "START TRANSACTION")
            .await
            .map_err(|error| (None, error))?;
    }
    for (index, sql) in statements.iter().enumerate() {
        if let Err(error) = exec::collect(session.clone(), sql.as_str()).await {
            if transactional && let Err(error) = exec::collect(session.clone(), "ROLLBACK").await {
                tracing::warn!(%error, "rollback after a failed DDL statement did not succeed");
            }
            return Err((Some(index), error));
        }
    }
    if transactional {
        exec::collect(session, "COMMIT")
            .await
            .map_err(|error| (None, error))?;
    }
    Ok(())
}

impl Focusable for TableEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TableEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("table-editor")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
    }
}
