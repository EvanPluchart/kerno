//! Copying tables with their data to another connection (CLAUDE.md §4 J13,
//! DBeaver #17767), as DBeaver's data transfer wizard (`DataTransferWizard`
//! with `DatabaseConsumerPageMapping`: the tables, the target container
//! and how each table and column maps, then the progress), drawn like
//! Kerno's tool wizards: the steps on the left, Back / Next / Start.
//!
//! 1. Tables: the source's schemas and their tables, ticked (the table or
//!    schema the wizard was opened on is ticked already).
//! 2. Target: the connection (it connects when it is not), its database
//!    and schema, Create or Append, what to do with a table that exists,
//!    what a failed table does. A read-only target is refused here.
//! 3. Mapping: per table, each source column's target name and type, both
//!    editable; a column without a counterpart is in the danger colour with
//!    the reason and blocks Start until it is skipped or given a type. The
//!    `CREATE TABLE` the copy runs is shown.
//! 4. Progress: per table, rows read and written, the rate, a bar when the
//!    source has an estimate; the log; Cancel; then the report.
//!
//! Nothing is written before the target's statements were shown and
//! confirmed (`sql_confirm`: twice on production). The copy runs on two
//! sessions of its own, off the UI thread; memory holds one batch.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input as TextInput, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, px,
};
use kerno_core::config::Engine;
use kerno_core::schema::TableRef;
use kerno_core::transfer::{
    self, IfExists, OnError, TableStatus, TableTransfer, TransferEvent, TransferMode, TransferPlan,
    TransferReport,
};
use kerno_core::{Error, Session};

use crate::backup_dialog::{SchemaEntry, chips, fetch_objects, wizard_steps};
use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::query::plan_view::number;
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::tabs::Target;

/// Lines of the log kept on screen: the last ones.
const LOG_LINES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Tables,
    Target,
    Mapping,
    Progress,
}

const PAGES: [(Page, &str); 4] = [
    (Page::Tables, "Tables"),
    (Page::Target, "Target"),
    (Page::Mapping, "Mapping"),
    (Page::Progress, "Progress"),
];

/// Something read from a server for a page.
enum Loaded<T> {
    Loading,
    Failed(String),
    Ready(T),
}

/// One table of the mapping page, with its fields.
struct TableRows {
    transfer: TableTransfer,
    name: Entity<InputState>,
    /// Per column: its target name and type.
    columns: Vec<(Entity<InputState>, Entity<InputState>)>,
}

/// A copy that runs or ran.
struct Running {
    /// Per table: the source's row estimate, rows read, rows written.
    tables: Vec<(Option<u64>, u64, u64)>,
    log: Vec<SharedString>,
    started: Instant,
    elapsed: Option<Duration>,
    report: Option<Result<TransferReport, String>>,
    /// The target session while the copy runs, to stop its statement.
    target: Option<Arc<dyn Session>>,
}

/// The plan and the `CREATE` preview of the table on the mapping page,
/// worked out when something they depend on changed, not at every frame.
struct Computed {
    plan: Option<TransferPlan>,
    /// The statements, or why there are none (empty: nothing to say).
    preview: Result<String, String>,
}

pub struct TransferDialog {
    connections: Entity<Connections>,
    /// The source connection and database.
    source: Target,
    source_name: String,
    engine: Engine,
    database: String,
    page: Page,
    objects: Loaded<Vec<SchemaEntry>>,
    picked: BTreeSet<TableRef>,
    /// The schema whose tables the right list shows.
    current: Option<String>,
    /// A schema to tick whole once the objects are read.
    preselect: Option<String>,
    /// The target connection.
    target: Option<String>,
    target_database: Entity<InputState>,
    target_schema: Entity<InputState>,
    mode: TransferMode,
    if_exists: IfExists,
    on_error: OnError,
    mapping: Loaded<Vec<TableRows>>,
    /// The target schema's tables, when the mapping was read.
    existing: Vec<String>,
    /// The table the mapping page shows.
    shown: usize,
    run: Option<Running>,
    cancel: Arc<AtomicBool>,
    tasks: Vec<Task<()>>,
    mapping_subscriptions: Vec<Subscription>,
    computed: Computed,
    /// An input changed since `computed` was worked out.
    stale: bool,
    /// How many times the plan was built; for tests.
    plan_builds: Cell<usize>,
    _subscriptions: Vec<Subscription>,
}

/// Opens the copy of tables of `source`'s database (its connection's
/// default one when it names none): `tables` ticked, or every table of
/// `source.schema`; or says why it cannot.
pub fn open(
    connections: Entity<Connections>,
    source: Target,
    tables: Vec<TableRef>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<TransferDialog>> {
    let checked = {
        let manager = connections.read(cx);
        let connected = matches!(manager.status(&source.connection), Status::Connected { .. });
        match (
            manager.get(&source.connection),
            manager.meta_session(&source.connection),
        ) {
            (Some(saved), Some(meta)) if connected => source
                .database
                .clone()
                .or_else(|| meta.database())
                .filter(|name| !name.is_empty())
                .map(|database| (saved.config.name.clone(), saved.config.engine, database))
                .ok_or("Choose a database first: right-click it in the tree ▸ Tools."),
            _ => Err("Connect first: the tables are read from the connection."),
        }
    };
    let (name, engine, database) = match checked {
        Ok(checked) => checked,
        Err(refusal) => {
            window.push_notification(Notification::warning(SharedString::from(refusal)), cx);
            return None;
        }
    };
    let title = format!("Copy tables of {database} to another connection");
    let view = cx.new(|cx| {
        TransferDialog::new(
            connections,
            Target {
                database: Some(database.clone()),
                ..source
            },
            (name, engine, database),
            tables,
            window,
            cx,
        )
    });
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title(SharedString::from(title.clone()))
            .w(px(900.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    Some(view)
}

/// The error with its code, detail and hint (C14).
fn error_text(error: &Error) -> String {
    kerno_core::describe(error).to_string()
}

fn value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().trim().to_string()
}

impl TransferDialog {
    fn new(
        connections: Entity<Connections>,
        source: Target,
        (source_name, engine, database): (String, Engine, String),
        tables: Vec<TableRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = |placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let target_database = input("database");
        let target_schema = input("schema");
        let mut subscriptions: Vec<Subscription> = [&target_database, &target_schema]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.changed(cx);
                    }
                })
            })
            .collect();
        // A target that connects, or fails to, is shown as it goes.
        subscriptions.push(cx.observe(&connections, |this, _, cx| this.changed(cx)));
        let current = tables
            .first()
            .map(|table| table.schema.clone())
            .or_else(|| source.schema.clone());
        let preselect = tables.is_empty().then(|| source.schema.clone()).flatten();
        let mut dialog = Self {
            connections,
            source,
            source_name,
            engine,
            database,
            page: Page::Tables,
            objects: Loaded::Loading,
            picked: tables.into_iter().collect(),
            current,
            preselect,
            target: None,
            target_database,
            target_schema,
            mode: TransferMode::Create,
            if_exists: IfExists::Stop,
            on_error: OnError::Stop,
            mapping: Loaded::Loading,
            existing: Vec::new(),
            shown: 0,
            run: None,
            cancel: Arc::new(AtomicBool::new(false)),
            tasks: Vec::new(),
            mapping_subscriptions: Vec::new(),
            computed: Computed {
                plan: None,
                preview: Err(String::new()),
            },
            stale: true,
            plan_builds: Cell::new(0),
            _subscriptions: subscriptions,
        };
        dialog.load_objects(cx);
        dialog
    }

    /// Lists the source's schemas and their tables on a session of its own.
    fn load_objects(&mut self, cx: &mut Context<Self>) {
        let (engine, database) = (self.engine, self.database.clone());
        let opening = self.connections.read(cx).open_session(
            &self.source.connection,
            Some(database.clone()),
            cx,
        );
        self.tasks.push(cx.spawn(async move |this, cx| {
            let loaded = match opening.await {
                Err(error) => Loaded::Failed(error_text(&error)),
                Ok(session) => {
                    let reading = cx.update(|cx| {
                        bridge::spawn(cx, async move {
                            let objects = fetch_objects(&*session, engine, &database).await;
                            session.close().await;
                            objects
                        })
                    });
                    match reading.await {
                        Ok(entries) => Loaded::Ready(entries),
                        Err(error) => Loaded::Failed(error_text(&error)),
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                if let Loaded::Ready(entries) = &loaded {
                    if this.current.is_none() {
                        this.current = entries.first().map(|entry| entry.name.clone());
                    }
                    if let Some(schema) = this.preselect.take()
                        && let Some(entry) = entries.iter().find(|entry| entry.name == schema)
                    {
                        this.picked.extend(tables_of(entry));
                    }
                }
                this.objects = loaded;
                cx.notify();
            });
        }));
    }

    fn entry(&self, schema: &str) -> Option<&SchemaEntry> {
        match &self.objects {
            Loaded::Ready(entries) => entries.iter().find(|entry| entry.name == schema),
            _ => None,
        }
    }

    fn toggle_table(&mut self, table: TableRef, cx: &mut Context<Self>) {
        if !self.picked.remove(&table) {
            self.picked.insert(table);
        }
        cx.notify();
    }

    /// Ticks or unticks every table of `schema`, or of every schema.
    fn tick_all(&mut self, schema: Option<String>, on: bool, cx: &mut Context<Self>) {
        let tables: Vec<TableRef> = match &self.objects {
            Loaded::Ready(entries) => entries
                .iter()
                .filter(|entry| schema.as_ref().is_none_or(|name| &entry.name == name))
                .flat_map(tables_of)
                .collect(),
            _ => Vec::new(),
        };
        for table in tables {
            if on {
                self.picked.insert(table);
            } else {
                self.picked.remove(&table);
            }
        }
        cx.notify();
    }

    /// Picks the connection to copy to, and connects it when it is not.
    pub fn choose_target(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let (engine, database) = match self.connections.read(cx).get(&id) {
            Some(saved) => (saved.config.engine, saved.config.database.clone()),
            None => return,
        };
        let meta_database = self
            .connections
            .read(cx)
            .meta_session(&id)
            .and_then(|meta| meta.database());
        let database = database.or(meta_database).unwrap_or_default();
        let (database, schema) = match engine {
            Engine::Postgres => (database, "public".to_string()),
            Engine::MySql | Engine::MariaDb => (String::new(), database),
        };
        self.target_database
            .update(cx, |input, cx| input.set_value(database, window, cx));
        self.target_schema
            .update(cx, |input, cx| input.set_value(schema, window, cx));
        if matches!(
            self.connections.read(cx).status(&id),
            Status::Disconnected | Status::Failed { .. }
        ) {
            self.connections
                .update(cx, |connections, cx| connections.connect(&id, cx));
        }
        self.target = Some(id);
        self.changed(cx);
    }

    fn target_engine(&self, cx: &App) -> Option<Engine> {
        let id = self.target.as_ref()?;
        Some(self.connections.read(cx).get(id)?.config.engine)
    }

    /// The database the target session opens in (`None`: the connection's
    /// default one) and the schema the tables go into.
    fn target_place(&self, cx: &App) -> (Option<String>, String) {
        let database = Some(value(&self.target_database, cx)).filter(|name| !name.is_empty());
        let schema = value(&self.target_schema, cx);
        match self.target_engine(cx) {
            Some(Engine::Postgres) => (database, schema),
            // The schema is the database there.
            _ => (None, schema),
        }
    }

    /// Reads the picked tables' structures and the target schema's tables,
    /// then shows the mapping.
    fn load_mapping(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(target), Some(to)) = (self.target.clone(), self.target_engine(cx)) else {
            return;
        };
        let from = self.engine;
        let tables: Vec<TableRef> = self.picked.iter().cloned().collect();
        let (database, schema) = self.target_place(cx);
        let connections = self.connections.read(cx);
        let source =
            connections.open_session(&self.source.connection, Some(self.database.clone()), cx);
        let target = connections.open_session(&target, database, cx);
        self.mapping = Loaded::Loading;
        self.stale = true;
        self.mapping_subscriptions.clear();
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let sessions = match (source.await, target.await) {
                (Ok(source), Ok(target)) => Ok((source, target)),
                (Err(error), _) | (_, Err(error)) => Err(error_text(&error)),
            };
            let read = match sessions {
                Err(error) => Err(error),
                Ok((source, target)) => {
                    let reading = cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            let read =
                                read_mapping(&*source, &*target, from, to, tables, &schema).await;
                            source.close().await;
                            target.close().await;
                            read
                        })
                    });
                    match reading {
                        Ok(reading) => reading.await,
                        Err(_) => return,
                    }
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.mapping = match read {
                    Err(error) => Loaded::Failed(error),
                    Ok((transfers, existing)) => {
                        this.existing = existing;
                        this.shown = 0;
                        Loaded::Ready(this.rows(transfers, window, cx))
                    }
                };
                this.changed(cx);
            });
        }));
    }

    /// The fields of the mapping page, one name per table and a name and a
    /// type per column.
    fn rows(
        &mut self,
        transfers: Vec<TableTransfer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<TableRows> {
        let mut field = |text: &str, placeholder: &'static str, cx: &mut Context<Self>| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(text.to_string())
            });
            self.mapping_subscriptions.push(cx.subscribe(
                &input,
                |this, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.changed(cx);
                    }
                },
            ));
            input
        };
        transfers
            .into_iter()
            .map(|transfer| TableRows {
                name: field(&transfer.target_name, "table name", cx),
                columns: transfer
                    .columns
                    .iter()
                    .map(|column| {
                        (
                            field(&column.target_name, "column name", cx),
                            field(&column.target_type, "type", cx),
                        )
                    })
                    .collect(),
                transfer,
            })
            .collect()
    }

    /// Skips a column of a table of the mapping, or copies it again.
    pub fn set_skip(&mut self, table: usize, column: usize, skip: bool, cx: &mut Context<Self>) {
        if let Loaded::Ready(rows) = &mut self.mapping
            && let Some(mapping) = rows
                .get_mut(table)
                .and_then(|row| row.transfer.columns.get_mut(column))
        {
            mapping.skip = skip;
        }
        self.changed(cx);
    }

    /// An input of the plan or of the preview changed: they are worked out
    /// again before the next frame.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        cx.notify();
    }

    /// Works the plan and the preview out again if an input changed.
    fn refresh(&mut self, cx: &App) {
        if std::mem::take(&mut self.stale) {
            let plan = self.plan(cx);
            let preview = self.preview(plan.as_ref(), cx);
            self.computed = Computed { plan, preview };
        }
    }

    /// The plan: the one worked out, or a new one while a change waits for
    /// the next frame.
    fn current_plan(&self, cx: &App) -> Option<Cow<'_, TransferPlan>> {
        match self.stale {
            true => self.plan(cx).map(Cow::Owned),
            false => self.computed.plan.as_ref().map(Cow::Borrowed),
        }
    }

    /// How many times the plan was built; for tests.
    #[cfg(test)]
    pub fn plan_builds(&self) -> usize {
        self.plan_builds.get()
    }

    /// The copy as the pages describe it, the fields read.
    fn plan(&self, cx: &App) -> Option<TransferPlan> {
        let Loaded::Ready(rows) = &self.mapping else {
            return None;
        };
        self.plan_builds.set(self.plan_builds.get() + 1);
        let tables = rows
            .iter()
            .map(|row| {
                let mut transfer = row.transfer.clone();
                transfer.target_name = value(&row.name, cx);
                for (column, (name, data_type)) in transfer.columns.iter_mut().zip(&row.columns) {
                    column.target_name = value(name, cx);
                    column.target_type = value(data_type, cx);
                }
                transfer
            })
            .collect();
        let mut plan = TransferPlan::new(tables, self.target_place(cx).1);
        plan.mode = self.mode;
        plan.if_exists = self.if_exists;
        plan.on_error = self.on_error;
        Some(plan)
    }

    /// The `CREATE TABLE` of the table on the mapping page under `plan`,
    /// or what happens to that table instead.
    fn preview(&self, plan: Option<&TransferPlan>, cx: &App) -> Result<String, String> {
        let Loaded::Ready(rows) = &self.mapping else {
            return Err(String::new());
        };
        let transfer = rows.get(self.shown).and_then(|row| {
            plan?
                .tables
                .iter()
                .find(|t| t.source == row.transfer.source)
        });
        match (plan, transfer, self.target.as_ref()) {
            (Some(plan), Some(transfer), Some(target)) => {
                let exists = self.exists(&transfer.target_name);
                if plan.mode == TransferMode::Append || exists {
                    let what = match (plan.mode, plan.if_exists) {
                        (TransferMode::Append, _) | (_, IfExists::Append) => {
                            "The table exists on the target: the rows are added to it."
                        }
                        (_, IfExists::Skip) => "The table exists on the target: it is skipped.",
                        (_, IfExists::Stop) => {
                            "The table exists on the target: the copy would stop here."
                        }
                    };
                    let what = if plan.mode == TransferMode::Append && !exists {
                        "The table does not exist on the target: Append needs it."
                    } else {
                        what
                    };
                    Ok(what.to_string())
                } else {
                    let meta = self.connections.read(cx).meta_session(target);
                    match meta {
                        Some(meta) => transfer::create_statements(
                            transfer,
                            &plan.target_schema,
                            meta.dialect(),
                        )
                        .map(|sql| format!("{};", sql.join(";\n")))
                        .map_err(|error| error_text(&error)),
                        None => Err("The target is not connected.".into()),
                    }
                }
            }
            _ => Err(String::new()),
        }
    }

    fn exists(&self, name: &str) -> bool {
        self.existing
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(name.trim()))
    }

    /// Why the page cannot go on, if it cannot.
    fn blocked(&self, cx: &App) -> Option<String> {
        match self.page {
            Page::Tables => match &self.objects {
                Loaded::Loading => Some("Reading the tables…".into()),
                Loaded::Failed(why) => Some(format!("The tables could not be read: {why}")),
                Loaded::Ready(_) if self.picked.is_empty() => {
                    Some("Tick at least one table.".into())
                }
                Loaded::Ready(_) => None,
            },
            Page::Target => self.target_problem(cx),
            Page::Mapping => {
                let plan = match &self.mapping {
                    Loaded::Loading => return Some("Reading the tables' columns…".into()),
                    Loaded::Failed(why) => return Some(why.clone()),
                    Loaded::Ready(_) => self.current_plan(cx)?,
                };
                if let Some(problem) = plan.problems().into_iter().next() {
                    return Some(problem);
                }
                if let Some(problem) = self.target_problem(cx) {
                    return Some(problem);
                }
                let (database, schema) = self.target_place(cx);
                let onto_itself = self.target.as_deref() == Some(self.source.connection.as_str())
                    && database.as_deref().is_none_or(|name| name == self.database);
                if let Some(table) = plan.tables.iter().find(|table| {
                    onto_itself
                        && table.source.schema == schema
                        && table
                            .source
                            .name
                            .eq_ignore_ascii_case(table.target_name.trim())
                }) {
                    return Some(format!(
                        "{}.{} would be copied onto itself: choose another schema or name.",
                        table.source.schema, table.source.name
                    ));
                }
                let stops = plan.mode == TransferMode::Create && plan.if_exists == IfExists::Stop;
                plan.tables
                    .iter()
                    .find(|table| stops && self.exists(&table.target_name))
                    .map(|table| {
                        format!(
                            "{} exists on the target: choose Append or Skip for the tables that \
                             exist, or another name.",
                            table.target_name.trim()
                        )
                    })
            }
            Page::Progress => None,
        }
    }

    /// Why the target cannot take the copy, if it cannot.
    fn target_problem(&self, cx: &App) -> Option<String> {
        let Some(id) = &self.target else {
            return Some("Choose the connection to copy to.".into());
        };
        let manager = self.connections.read(cx);
        let saved = manager.get(id)?;
        if saved.config.read_only {
            return Some(format!(
                "“{}” is a read-only connection: nothing can be written to it.",
                saved.config.name
            ));
        }
        match manager.status(id) {
            Status::Connected { .. } => {}
            Status::Failed { message } => {
                return Some(format!(
                    "“{}” did not connect: {message}",
                    saved.config.name
                ));
            }
            _ => return Some(format!("Connecting to “{}”…", saved.config.name)),
        }
        if self.target_place(cx).1.is_empty() {
            return Some("Type the schema the tables go into.".into());
        }
        None
    }

    fn next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked(cx).is_some() {
            return;
        }
        match self.page {
            Page::Tables => self.page = Page::Target,
            Page::Target => {
                self.page = Page::Mapping;
                self.load_mapping(window, cx);
            }
            Page::Mapping | Page::Progress => {}
        }
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.page = match self.page {
            Page::Tables | Page::Target => Page::Tables,
            Page::Mapping => Page::Target,
            Page::Progress => Page::Mapping,
        };
        cx.notify();
    }

    /// Shows the statements the copy runs on the target and, once
    /// confirmed, copies.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page != Page::Mapping || self.blocked(cx).is_some() {
            return;
        }
        let (Some(plan), Some(target)) = (self.plan(cx), self.target.clone()) else {
            return;
        };
        let manager = self.connections.read(cx);
        let (Some(saved), Some(meta)) = (manager.get(&target), manager.meta_session(&target))
        else {
            return;
        };
        let (name, environment) = (saved.config.name.clone(), saved.config.environment.clone());
        let sql = match transfer::statements(&plan, meta.dialect(), &self.existing) {
            Ok(sql) => format!("{};", sql.join(";\n\n")),
            Err(error) => {
                window.push_notification(crate::error_notification(error_text(&error)), cx);
                return;
            }
        };
        let count = plan.tables.len();
        let view = cx.entity();
        sql_confirm::open(
            Confirmation {
                title: format!(
                    "Copy {count} {} to {name}",
                    if count == 1 { "table" } else { "tables" }
                ),
                explanation: format!(
                    "The rows are written {} at a time, each batch in its own transaction: a \
                     batch that fails is rolled back, the batches before it stay. Indexes, \
                     foreign keys, checks, triggers and defaults are not copied.",
                    number(plan.batch_rows as f64)
                ),
                statement: Statement::Fixed(sql),
                action: "Copy",
                destructive: false,
                connection: name,
                environment,
                writes: true,
            },
            window,
            cx,
            move |_, window, cx| {
                let plan = plan.clone();
                view.update(cx, |this, cx| this.start(plan, window, cx));
            },
        );
    }

    /// Copies on two sessions of its own.
    fn start(&mut self, plan: TransferPlan, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let (database, _) = self.target_place(cx);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.page = Page::Progress;
        self.run = Some(Running {
            tables: vec![(None, 0, 0); plan.tables.len()],
            log: Vec::new(),
            started: Instant::now(),
            elapsed: None,
            report: None,
            target: None,
        });
        let cancel = self.cancel.clone();
        let connections = self.connections.read(cx);
        let source =
            connections.open_session(&self.source.connection, Some(self.database.clone()), cx);
        let target = connections.open_session(&target, database, cx);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<TransferEvent>();
        self.tasks.push(cx.spawn(async move |this, cx| {
            while let Some(event) = receiver.recv().await {
                let mut events = vec![event];
                while let Ok(event) = receiver.try_recv() {
                    events.push(event);
                }
                if this.update(cx, |this, cx| this.apply(events, cx)).is_err() {
                    return;
                }
            }
        }));
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let report = match (source.await, target.await) {
                (Err(error), _) | (_, Err(error)) => Err(error_text(&error)),
                (Ok(source), Ok(target)) => {
                    let _ = this.update(cx, |this, _| {
                        if let Some(run) = &mut this.run {
                            run.target = Some(target.clone());
                        }
                    });
                    let copying = cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            let progress = move |event| {
                                let _ = sender.send(event);
                            };
                            let report = transfer::run(
                                source.clone(),
                                target.clone(),
                                plan,
                                progress,
                                cancel,
                            )
                            .await;
                            source.close().await;
                            target.close().await;
                            report
                        })
                    });
                    match copying {
                        Ok(copying) => copying.await.map_err(|error| error_text(&error)),
                        Err(_) => return,
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(run) = &mut this.run {
                    run.elapsed = Some(run.started.elapsed());
                    run.target = None;
                    run.report = Some(report);
                }
                cx.notify();
            });
        }));
        // The elapsed time, while it runs.
        self.tasks.push(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        this.run.as_ref().is_some_and(|run| run.report.is_none())
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn apply(&mut self, events: Vec<TransferEvent>, cx: &mut Context<Self>) {
        let Some(run) = &mut self.run else {
            return;
        };
        for event in events {
            match event {
                TransferEvent::Table { index, estimate } => {
                    if let Some(table) = run.tables.get_mut(index) {
                        table.0 = estimate;
                    }
                }
                TransferEvent::Rows {
                    index,
                    read,
                    written,
                } => {
                    if let Some(table) = run.tables.get_mut(index) {
                        (table.1, table.2) = (read, written);
                    }
                }
                TransferEvent::Log(line) => run.log.push(SharedString::from(line)),
            }
        }
        if run.log.len() > LOG_LINES {
            run.log.drain(..run.log.len() - LOG_LINES);
        }
        cx.notify();
    }

    /// Stops the copy between two batches; the batch being written is
    /// rolled back.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Release);
        if let Some(target) = self.run.as_ref().and_then(|run| run.target.clone()) {
            bridge::spawn(cx, async move {
                if let Err(error) = target.cancel().await {
                    tracing::debug!(%error, "the copy's statement could not be cancelled");
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn running(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.report.is_none())
    }

    /// Where the wizard is, in words; for tests.
    #[cfg(test)]
    pub fn state(&self, cx: &App) -> String {
        let page = PAGES
            .iter()
            .find(|(page, _)| *page == self.page)
            .map_or("", |(_, label)| label)
            .to_lowercase();
        match (&self.run, self.blocked(cx)) {
            (Some(run), _) if self.page == Page::Progress => match &run.report {
                None => "running".into(),
                Some(Ok(report)) if report.succeeded() => format!("copied: {}", report.text()),
                Some(Ok(report)) => format!("ended: {}", report.text()),
                Some(Err(error)) => format!("failed: {error}"),
            },
            (_, Some(blocked)) => format!("{page}: {blocked}"),
            (_, None) => page,
        }
    }

    /// The report of the copy, once it ended; for tests.
    #[cfg(test)]
    pub fn report(&self) -> Option<TransferReport> {
        self.run.as_ref()?.report.clone()?.ok()
    }
}

impl Drop for TransferDialog {
    /// Closing the dialog stops a copy that still runs.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

/// The tables of a schema (views are not copied).
fn tables_of(entry: &SchemaEntry) -> Vec<TableRef> {
    entry
        .relations
        .iter()
        .filter(|(_, kind)| kind.is_table())
        .map(|(name, _)| TableRef::new(entry.name.clone(), name.clone()))
        .collect()
}

/// The transfers of `tables` (ordered by their foreign keys) and the
/// target schema's tables; or why they could not be read.
async fn read_mapping(
    source: &dyn Session,
    target: &dyn Session,
    from: Engine,
    to: Engine,
    tables: Vec<TableRef>,
    schema: &str,
) -> Result<(Vec<TableTransfer>, Vec<String>), String> {
    let failed = |error: Error| error_text(&error);
    let schemas: Vec<String> = if target.dialect().has_schemas() {
        let listed = target.schemas().await.map_err(failed)?;
        listed.into_iter().map(|schema| schema.name).collect()
    } else {
        let listed = target.databases().await.map_err(failed)?;
        listed.into_iter().map(|database| database.name).collect()
    };
    if !schemas.iter().any(|name| name == schema) {
        return Err(format!(
            "The schema “{schema}” does not exist on the target: create it first, or type \
             another one."
        ));
    }
    let existing = target
        .objects(schema)
        .await
        .map_err(failed)?
        .relations
        .into_iter()
        .map(|relation| relation.name)
        .collect();
    let mut transfers = Vec::with_capacity(tables.len());
    for table in tables {
        let structure = source.table_structure(&table).await.map_err(failed)?;
        transfers.push(TableTransfer::new(from, to, table, &structure));
    }
    Ok((transfer::order_by_foreign_keys(transfers), existing))
}

// Rendering ---------------------------------------------------------------

impl TransferDialog {
    fn muted(text: impl Into<SharedString>, cx: &Context<Self>) -> AnyElement {
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
            .into_any_element()
    }

    fn label(text: &'static str) -> AnyElement {
        div()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .child(text)
            .into_any_element()
    }

    fn render_steps(&self, cx: &Context<Self>) -> AnyElement {
        let labels: Vec<&str> = PAGES.iter().map(|(_, label)| *label).collect();
        let current = PAGES.iter().position(|(page, _)| *page == self.page);
        wizard_steps(&labels, current, cx)
    }

    fn render_tables(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let entries = match &self.objects {
            Loaded::Ready(entries) => entries,
            Loaded::Loading => return Self::muted("Reading the schemas and tables…", cx),
            Loaded::Failed(why) => {
                return div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(SharedString::from(format!(
                        "The tables could not be read: {why}"
                    )))
                    .into_any_element();
            }
        };
        let list = |id: &'static str| {
            div()
                .id(id)
                .h(px(280.))
                .overflow_y_scroll()
                .p_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
        };
        let header = |title: &'static str, ids: (&'static str, &'static str), schema| {
            let other = Option::clone(&schema);
            h_flex()
                .gap_1()
                .child(div().flex_1().child(Self::label(title)))
                .child(
                    Button::new(ids.0)
                        .ghost()
                        .small()
                        .label("All")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tick_all(Option::clone(&schema), true, cx)
                        })),
                )
                .child(
                    Button::new(ids.1)
                        .ghost()
                        .small()
                        .label("None")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tick_all(Option::clone(&other), false, cx)
                        })),
                )
        };
        let schemas =
            list("transfer-schemas").children(entries.iter().enumerate().map(|(index, entry)| {
                let tables = tables_of(entry);
                let ticked = tables.iter().filter(|t| self.picked.contains(t)).count();
                let (whole, pick) = (entry.name.clone(), entry.name.clone());
                let current = self.current.as_deref() == Some(entry.name.as_str());
                h_flex()
                    .gap_1()
                    .px_1()
                    .rounded(theme.radius)
                    .when(current, |this| this.bg(theme.accent))
                    .child(
                        Checkbox::new(("transfer-schema", index))
                            .checked(ticked > 0 && ticked == tables.len())
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                this.tick_all(Some(whole.clone()), *checked, cx)
                            })),
                    )
                    .child(
                        div()
                            .id(("transfer-schema-name", index))
                            .test_support()
                            .flex_1()
                            .text_sm()
                            .cursor_pointer()
                            .child(SharedString::from(if ticked > 0 {
                                format!("{} ({ticked})", entry.name)
                            } else {
                                entry.name.clone()
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.current = Some(pick.clone());
                                cx.notify();
                            })),
                    )
            }));
        let tables = self
            .current
            .as_deref()
            .and_then(|schema| self.entry(schema))
            .map(tables_of)
            .unwrap_or_default();
        let empty = tables.is_empty();
        let tables_list = list("transfer-tables")
            .children(tables.into_iter().enumerate().map(|(index, table)| {
                Checkbox::new(("transfer-table", index))
                    .label(SharedString::from(table.name.clone()))
                    .checked(self.picked.contains(&table))
                    .on_click(cx.listener(move |this, _: &bool, _, cx| {
                        this.toggle_table(table.clone(), cx)
                    }))
            }))
            .when(empty, |this| this.child(Self::muted("No table here.", cx)));
        let tables_column = v_flex()
            .flex_1()
            .gap_1()
            .child(header(
                "Tables",
                ("transfer-tables-all", "transfer-tables-none"),
                self.current.clone(),
            ))
            .child(tables_list);
        let picked = self.picked.len();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .when(self.engine == Engine::Postgres, |this| {
                        this.child(
                            v_flex()
                                .flex_1()
                                .gap_1()
                                .child(header(
                                    "Schemas",
                                    ("transfer-schemas-all", "transfer-schemas-none"),
                                    None,
                                ))
                                .child(schemas),
                        )
                    })
                    .child(tables_column),
            )
            .child(Self::muted(
                format!(
                    "{picked} {} ticked. Views are not copied.",
                    if picked == 1 { "table" } else { "tables" }
                ),
                cx,
            ))
            .into_any_element()
    }

    fn render_target(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let manager = self.connections.read(cx);
        let connections = v_flex()
            .id("transfer-targets")
            .max_h(px(200.))
            .overflow_y_scroll()
            .p_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .children(manager.saved().iter().enumerate().map(|(index, saved)| {
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
                let picked = self.target.as_deref() == Some(saved.id.as_str());
                let id = saved.id.clone();
                h_flex()
                    .id(("transfer-target", index))
                    .test_support()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .when(picked, |this| this.bg(theme.accent))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .child(SharedString::from(config.name.clone())),
                    )
                    .child(Self::muted(details.join(" · "), cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_target(id.clone(), window, cx)
                    }))
            }));
        let postgres = self.target_engine(cx) == Some(Engine::Postgres);
        let field = |label: &'static str, id: &'static str, input: &Entity<InputState>| {
            h_flex()
                .gap_3()
                .child(
                    div()
                        .w(px(120.))
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .w(px(260.))
                        .child(TextInput::new(input).small().id(id)),
                )
        };
        v_flex()
            .gap_3()
            .child(Self::label("Copy to"))
            .child(connections)
            .when(self.target.is_some(), |this| {
                this.when(postgres, |this| {
                    this.child(field(
                        "Database",
                        "transfer-database",
                        &self.target_database,
                    ))
                })
                .child(field(
                    if postgres { "Schema" } else { "Database" },
                    "transfer-schema-field",
                    &self.target_schema,
                ))
            })
            .child(Self::label("Tables"))
            .child(chips(
                "transfer-mode",
                &[
                    (TransferMode::Create, "Create them"),
                    (TransferMode::Append, "Add the rows to existing tables"),
                ],
                self.mode,
                |this, mode| {
                    this.mode = mode;
                    this.stale = true;
                },
                cx,
            ))
            .when(self.mode == TransferMode::Create, |this| {
                this.child(Self::muted("When a table exists already:", cx))
                    .child(chips(
                        "transfer-if-exists",
                        &[
                            (IfExists::Stop, "Stop"),
                            (IfExists::Append, "Add the rows to it"),
                            (IfExists::Skip, "Skip it"),
                        ],
                        self.if_exists,
                        |this, choice| {
                            this.if_exists = choice;
                            this.stale = true;
                        },
                        cx,
                    ))
            })
            .child(Self::label("When a table fails"))
            .child(chips(
                "transfer-on-error",
                &[
                    (OnError::Stop, "Stop the copy"),
                    (OnError::SkipTable, "Go on with the next table"),
                ],
                self.on_error,
                |this, choice| {
                    this.on_error = choice;
                    this.stale = true;
                },
                cx,
            ))
            .into_any_element()
    }

    fn render_mapping(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let rows = match &self.mapping {
            Loaded::Ready(rows) => rows,
            Loaded::Loading => return Self::muted("Reading the tables' columns…", cx),
            Loaded::Failed(why) => {
                return div()
                    .id("transfer-mapping-error")
                    .test_support()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(SharedString::from(why.clone()))
                    .into_any_element();
            }
        };
        let tables = v_flex()
            .id("transfer-map-tables")
            .flex_none()
            .w(px(170.))
            .max_h(px(420.))
            .overflow_y_scroll()
            .gap_0p5()
            .children(rows.iter().enumerate().map(|(index, row)| {
                let problem = row
                    .transfer
                    .columns
                    .iter()
                    .any(|column| column.problem().is_some());
                div()
                    .id(("transfer-map-table", index))
                    .test_support()
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radius)
                    .text_sm()
                    .cursor_pointer()
                    .when(index == self.shown, |this| this.bg(theme.accent))
                    .when(problem, |this| this.text_color(theme.danger))
                    .child(SharedString::from(row.transfer.source.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shown = index;
                        this.changed(cx);
                    }))
            }));
        let Some(row) = rows.get(self.shown) else {
            return tables.into_any_element();
        };
        let table = self.shown;
        let transfer = (self.computed.plan.as_ref())
            .and_then(|plan| plan.tables.iter().find(|t| t.source == row.transfer.source));
        let columns = v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().w(px(32.)).flex_none().child("Copy"))
                    .child(div().w(px(200.)).child("Source column"))
                    .child(div().w(px(160.)).child("Target name"))
                    .child(div().w(px(160.)).child("Target type")),
            )
            .children(
                row.transfer
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let (name, data_type) = &row.columns[index];
                        let typed = transfer
                            .and_then(|t| t.columns.get(index))
                            .cloned()
                            .unwrap_or_else(|| column.clone());
                        let problem = typed.problem();
                        let note = problem.clone().or_else(|| column.note.clone());
                        v_flex()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div().w(px(32.)).flex_none().child(
                                            Checkbox::new(("transfer-skip", index))
                                                .checked(!column.skip)
                                                .on_click(cx.listener(
                                                    move |this, checked: &bool, _, cx| {
                                                        this.set_skip(table, index, !*checked, cx)
                                                    },
                                                )),
                                        ),
                                    )
                                    .child(
                                        div()
                                            .w(px(200.))
                                            .text_sm()
                                            .when(column.skip, |this| {
                                                this.text_color(theme.muted_foreground)
                                            })
                                            .when(problem.is_some(), |this| {
                                                this.text_color(theme.danger)
                                            })
                                            .child(SharedString::from(format!(
                                                "{} {}",
                                                column.source.name, column.source.data_type
                                            ))),
                                    )
                                    .child(
                                        div().w(px(160.)).child(
                                            TextInput::new(name)
                                                .small()
                                                .id(("transfer-column-name", index))
                                                .disabled(column.skip),
                                        ),
                                    )
                                    .child(
                                        div().w(px(160.)).child(
                                            TextInput::new(data_type)
                                                .small()
                                                .id(("transfer-column-type", index))
                                                .disabled(column.skip),
                                        ),
                                    ),
                            )
                            .when_some(note, |this, note| {
                                this.child(
                                    div()
                                        .id(("transfer-column-note", index))
                                        .test_support()
                                        .pl(px(32.))
                                        .text_xs()
                                        .text_color(if problem.is_some() {
                                            theme.danger
                                        } else {
                                            theme.muted_foreground
                                        })
                                        .child(SharedString::from(note)),
                                )
                            })
                    }),
            );
        let preview = &self.computed.preview;
        let preview_text = match preview {
            Ok(text) | Err(text) => text.clone(),
        };
        let not_copied = (!row.transfer.not_copied.is_empty()).then(|| {
            Self::muted(
                format!("Not copied: {}.", row.transfer.not_copied.join(", ")),
                cx,
            )
        });
        h_flex()
            .gap_3()
            .items_start()
            .child(tables)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("Target table"),
                            )
                            .child(div().w(px(240.)).child(
                                TextInput::new(&row.name).small().id("transfer-table-name"),
                            )),
                    )
                    .child(columns)
                    .children(not_copied)
                    .child(
                        div()
                            .id("transfer-create-preview")
                            .test_support()
                            .p_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.muted)
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .when(preview.is_err(), |this| {
                                this.text_color(theme.muted_foreground)
                            })
                            .child(SharedString::from(preview_text.clone()))
                            .test_text(preview_text),
                    ),
            )
            .into_any_element()
    }

    fn render_progress(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(run) = &self.run else {
            return div().into_any_element();
        };
        let elapsed = run.elapsed.unwrap_or_else(|| run.started.elapsed());
        let report = run.report.as_ref().and_then(|report| report.as_ref().ok());
        let names: Vec<(String, String)> = match (&self.mapping, report) {
            (_, Some(report)) => report
                .tables
                .iter()
                .map(|t| {
                    (
                        t.source.name.clone(),
                        format!("{}.{}", t.target.schema, t.target.name),
                    )
                })
                .collect(),
            (Loaded::Ready(_), None) => self
                .current_plan(cx)
                .map(|plan| {
                    plan.tables
                        .iter()
                        .map(|t| {
                            (
                                t.source.name.clone(),
                                format!("{}.{}", plan.target_schema, t.target_name.trim()),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let tables = v_flex().gap_2().children(names.into_iter().enumerate().map(
            |(index, (source, target))| {
                let (estimate, read, written) = run.tables.get(index).copied().unwrap_or_default();
                let outcome = report.and_then(|report| report.tables.get(index));
                let seconds = outcome
                    .map_or(elapsed, |outcome| outcome.elapsed)
                    .as_secs_f64();
                let rate = if written > 0 && seconds > 0.0 {
                    format!(" · {} rows/s", number((written as f64 / seconds).round()))
                } else {
                    String::new()
                };
                let (read, written) = outcome.map_or((read, written), |o| (o.read, o.written));
                let (status, color) = match outcome.map(|outcome| &outcome.status) {
                    None => ("", theme.muted_foreground),
                    Some(TableStatus::Copied) => ("copied", theme.success),
                    Some(TableStatus::Skipped(_)) => ("skipped", theme.muted_foreground),
                    Some(TableStatus::Failed { .. }) => ("failed", theme.danger),
                    Some(TableStatus::Cancelled) => ("cancelled", theme.warning),
                    Some(TableStatus::NotStarted) => ("not started", theme.muted_foreground),
                };
                let text = format!(
                    "{source} → {target} · {} read, {} written{rate}{}",
                    number(read as f64),
                    number(written as f64),
                    if status.is_empty() {
                        String::new()
                    } else {
                        format!(" · {status}")
                    }
                );
                let fraction = match (outcome.map(|o| &o.status), estimate) {
                    (Some(TableStatus::Copied), _) => Some(100.),
                    (_, Some(estimate)) => {
                        Some((written as f32 * 100. / estimate as f32).min(100.))
                    }
                    _ => None,
                };
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .id(("transfer-table-progress", index))
                            .test_support()
                            .text_sm()
                            .text_color(color)
                            .child(SharedString::from(text.clone()))
                            .test_text(text),
                    )
                    .when_some(fraction, |this, fraction| {
                        this.child(Progress::new(("transfer-progress", index)).value(fraction))
                    })
            },
        ));
        let (summary, color) = match &run.report {
            None => (
                format!("Copying · {:.1} s", elapsed.as_secs_f64()),
                theme.muted_foreground,
            ),
            Some(Ok(report)) if report.succeeded() => (
                format!(
                    "Copied {} rows in {:.1} s.",
                    number(report.written() as f64),
                    elapsed.as_secs_f64()
                ),
                theme.success,
            ),
            Some(Ok(report)) => (
                format!(
                    "The copy ended before every table was copied: {} rows written in {:.1} s. \
                     The batches written before a failure stay on the target.",
                    number(report.written() as f64),
                    elapsed.as_secs_f64()
                ),
                theme.danger,
            ),
            Some(Err(error)) => (format!("Nothing was copied: {error}"), theme.danger),
        };
        let log = v_flex()
            .id("transfer-log")
            .test_support()
            .h(px(140.))
            .overflow_y_scroll()
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .font_family(theme.mono_font_family.clone())
            .text_xs()
            .children(run.log.iter().cloned());
        let details = report.map(|report| {
            div()
                .id("transfer-report")
                .test_support()
                .text_xs()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(SharedString::from(report.text()))
                .test_text(report.text())
        });
        v_flex()
            .gap_3()
            .child(
                div()
                    .id("transfer-summary")
                    .test_support()
                    .text_sm()
                    .text_color(color)
                    .child(SharedString::from(summary.clone()))
                    .test_text(summary),
            )
            .child(tables)
            .child(log)
            .children(details)
            .into_any_element()
    }

    fn render_footer(&self, cx: &Context<Self>) -> AnyElement {
        let close = |label: &'static str| {
            Button::new("transfer-close")
                .ghost()
                .label(label)
                .on_click(|_, window, cx| window.close_dialog(cx))
        };
        let footer = h_flex().gap_2().pt_2().justify_end();
        if self.page == Page::Progress {
            let report = self
                .run
                .as_ref()
                .and_then(|run| run.report.clone())
                .and_then(Result::ok);
            return if self.running() {
                footer
                    .child(
                        Button::new("transfer-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
                    .into_any_element()
            } else {
                footer
                    .when_some(report, |this, report| {
                        this.child(
                            Button::new("transfer-copy-report")
                                .outline()
                                .label("Copy report")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(report.text()))
                                }),
                        )
                    })
                    .child(
                        Button::new("transfer-back")
                            .outline()
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                    )
                    .child(close("Close").primary())
                    .into_any_element()
            };
        }
        let blocked = self.blocked(cx).is_some();
        footer
            .child(close("Cancel"))
            .child(
                Button::new("transfer-back")
                    .outline()
                    .label("Back")
                    .disabled(self.page == Page::Tables)
                    .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
            )
            .map(|this| {
                if self.page == Page::Mapping {
                    this.child(
                        Button::new("transfer-start")
                            .primary()
                            .label("Start…")
                            .disabled(blocked)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    )
                } else {
                    this.child(
                        Button::new("transfer-next")
                            .primary()
                            .label("Next")
                            .disabled(blocked)
                            .on_click(cx.listener(|this, _, window, cx| this.next(window, cx))),
                    )
                }
            })
            .into_any_element()
    }
}

impl Render for TransferDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh(cx);
        let theme = cx.theme();
        let intro = format!(
            "Copy tables of the database “{}” of “{}” with their rows to another connection.",
            self.database, self.source_name
        );
        let page = match self.page {
            Page::Tables => self.render_tables(cx),
            Page::Target => self.render_target(cx),
            Page::Mapping => self.render_mapping(cx),
            Page::Progress => self.render_progress(cx),
        };
        let blocked = (self.page != Page::Progress)
            .then(|| self.blocked(cx))
            .flatten()
            .map(|why| {
                div()
                    .id("transfer-blocked")
                    .test_support()
                    .text_sm()
                    .text_color(theme.warning)
                    .child(SharedString::from(why.clone()))
                    .test_text(why)
            });
        v_flex()
            .id("transfer-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(
                div()
                    .text_sm()
                    .child(SharedString::from(intro.clone()))
                    .test_text(intro),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_start()
                    .child(self.render_steps(cx))
                    .child(
                        // The tool wizards' page id: the tests scroll it.
                        div()
                            .id("wizard-page")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .child(
                                v_flex()
                                    .id("transfer-page")
                                    .gap_3()
                                    .max_h(px(540.))
                                    .child(page)
                                    .children(blocked)
                                    .overflow_y_scrollbar(),
                            ),
                    ),
            )
            .child(self.render_footer(cx))
    }
}
