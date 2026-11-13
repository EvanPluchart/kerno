//! The schema tree of one connection: loads what is opened, filters,
//! refreshes, and offers each object's actions.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName as Glyph;
use gpui_kit::base::actions::{SelectLeft, SelectRight};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tree::{TreeEntry, TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, px,
};
use kerno_core::browse::Filter;
use kerno_core::schema::{ObjectRef, RelationKind, RoutineKind, TableRef};
use kerno_core::sql::{delete_template, insert_template, select_template, update_template};
use kerno_core::{Session, exec};

use super::actions::{Action, Template, actions, default_action};
use super::model::{self, Children, Group, Item, Load, LoadKind, Loaded, Node, NodeKind, Tree};
use crate::bridge;
use crate::connections::manager::{Connections, ConnectionsEvent, Status};
use crate::engine_logo::engine_logo;
use crate::export_dialog::{self, Source};
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::state::AppState;
use crate::tabs::Target;

/// What the rest of the window does for the navigator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigatorEvent {
    /// Open an empty query tab running on `target`.
    NewQuery { target: Target },
    /// Open a query tab holding `sql`, running on `target`.
    OpenQuery {
        title: String,
        sql: String,
        target: Option<Target>,
    },
    /// Show the rows of a table or view.
    OpenData {
        target: Target,
        table: TableRef,
        kind: RelationKind,
    },
    /// Show the structure of a table or view.
    ShowStructure {
        target: Target,
        table: TableRef,
        kind: RelationKind,
    },
    /// Open the form of a new table in `schema`.
    CreateTable { target: Target, schema: String },
    /// Open a query tab in source mode holding `sql`, the script that
    /// replaces an object of `schema`.
    OpenSource {
        title: String,
        sql: String,
        target: Option<Target>,
        schema: String,
    },
    /// Import a CSV file into `table`, or into a new table of `schema`.
    ImportCsv {
        target: Target,
        schema: String,
        table: Option<TableRef>,
    },
}

/// The databases of the connection: the root of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Root {
    /// No connection is shown.
    None,
    Loading,
    Loaded,
    Failed(String),
}

pub struct Navigator {
    connections: Entity<Connections>,
    /// The connection whose tree is shown.
    connection: Option<String>,
    root: Root,
    tree: Tree,
    /// Sessions by database, opened as databases are browsed.
    sessions: HashMap<String, Arc<dyn Session>>,
    /// Bumped whenever the tree is replaced, so late answers are dropped.
    generation: u64,
    tree_state: Entity<TreeState>,
    filter: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NavigatorEvent> for Navigator {}

impl Navigator {
    pub fn new(
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tree_state = cx.new(|cx| TreeState::new(cx));
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter objects"));
        let subscriptions = vec![
            cx.subscribe(&tree_state, |this, _, event: &TreeEvent, cx| match event {
                TreeEvent::Expanded(id) => this.expand(id, cx),
                TreeEvent::Collapsed(id) => this.tree.collapse(id),
            }),
            cx.subscribe(&filter, |this, filter, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = filter.read(cx).value().to_string();
                    this.tree.set_filter(&text);
                    this.sync(cx);
                }
            }),
            cx.subscribe(&connections, |this, _, event: &ConnectionsEvent, cx| {
                if let ConnectionsEvent::StatusChanged { id } = event
                    && this.connection.as_deref() == Some(id.as_str())
                {
                    this.follow_status(cx);
                }
            }),
        ];
        Self {
            connections,
            connection: None,
            root: Root::None,
            tree: Tree::default(),
            sessions: HashMap::new(),
            generation: 0,
            tree_state,
            filter,
            _subscriptions: subscriptions,
        }
    }

    /// Shows the tree of this connection (none when `None`).
    pub fn set_connection(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.connection == id {
            return;
        }
        self.connection = id;
        self.reset(cx);
        self.follow_status(cx);
    }

    /// The connection whose tree is shown.
    pub fn connection(&self) -> Option<&str> {
        self.connection.as_deref()
    }

    /// Where a tab opened from `node` runs: its database, and its schema
    /// when `in_schema`.
    fn target_of(&self, node: &Node, in_schema: bool) -> Option<Target> {
        Some(Target {
            connection: self.connection.clone()?,
            database: Some(node.database.clone()),
            // Without schemas the database is the schema.
            schema: node
                .schema
                .clone()
                .filter(|schema| in_schema && *schema != node.database),
        })
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.root = Root::None;
        self.tree = Tree::default();
        self.sessions.clear();
        self.sync(cx);
    }

    /// Loads the tree once the connection is open; empties it when it closes.
    fn follow_status(&mut self, cx: &mut Context<Self>) {
        let connected = self.connection.as_deref().is_some_and(|id| {
            matches!(
                self.connections.read(cx).status(id),
                Status::Connected { .. }
            )
        });
        match (connected, &self.root) {
            (true, Root::None) => self.load_databases(cx),
            (false, Root::None) => {}
            (false, _) => self.reset(cx),
            (true, _) => {}
        }
        cx.notify();
    }

    /// Fetches the list of databases again, from scratch.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.reset(cx);
        self.follow_status(cx);
    }

    fn load_databases(&mut self, cx: &mut Context<Self>) {
        let Some(meta) = self
            .connection
            .as_deref()
            .and_then(|id| self.connections.read(cx).meta_session(id))
        else {
            return;
        };
        self.root = Root::Loading;
        self.tree = Tree::new(meta.dialect().has_schemas());
        let generation = self.generation;
        let current = meta.database();
        // The connection's own session answers for its own database.
        if let Some(database) = &current {
            self.sessions.insert(database.clone(), meta.clone());
        }
        let listing = bridge::spawn(cx, async move { meta.databases().await });
        cx.spawn(async move |this, cx| {
            let outcome = listing.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match outcome {
                    Ok(databases) => {
                        this.root = Root::Loaded;
                        if let Some(load) = this.tree.set_databases(databases, current.as_deref()) {
                            this.run(load, cx);
                        }
                    }
                    Err(error) => this.root = Root::Failed(error.to_string()),
                }
                this.sync(cx);
            });
        })
        .detach();
    }

    fn expand(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(load) = self.tree.expand(id) {
            self.run(load, cx);
        }
    }

    /// Closes every open node.
    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        fn open(items: &[Item], ids: &mut Vec<String>) {
            for item in items {
                if item.expanded {
                    ids.push(item.id.clone());
                }
                open(&item.children, ids);
            }
        }
        let mut ids = Vec::new();
        open(&self.tree.items(), &mut ids);
        for id in ids {
            self.tree.collapse(&id);
        }
        self.sync(cx);
    }

    /// Reloads the node (or the nearest one above it that the server
    /// answers for).
    pub fn refresh(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(target) = self.tree.refresh_target(id)
            && let Some(load) = self.tree.refresh(&target)
        {
            self.run(load, cx);
            self.sync(cx);
        }
    }

    /// Reads again the tables of `schema` (in `database`, or in any database
    /// when `None`) when the tree of `connection` shows them.
    pub fn refresh_tables(
        &mut self,
        connection: &str,
        database: Option<&str>,
        schema: &str,
        cx: &mut Context<Self>,
    ) {
        if self.connection.as_deref() != Some(connection) {
            return;
        }
        fn ids(items: &[model::Item], out: &mut Vec<String>) {
            for item in items {
                out.push(item.id.clone());
                ids(&item.children, out);
            }
        }
        // ponytail: walks the rows shown, so a folder hidden by the filter
        // is not refreshed; look nodes up in the model if that matters.
        let mut all = Vec::new();
        ids(&self.tree.items(), &mut all);
        let folders: Vec<String> = all
            .into_iter()
            .filter(|id| {
                self.tree.node(id).is_some_and(|node| {
                    node.kind == NodeKind::Group(Group::Tables)
                        && node.schema.as_deref() == Some(schema)
                        && database.is_none_or(|database| node.database == database)
                })
            })
            .collect();
        for folder in folders {
            self.refresh(&folder, cx);
        }
    }

    /// The session that answers for `database`, opened if it is not yet.
    fn session_for(
        &mut self,
        database: &str,
        cx: &mut Context<Self>,
    ) -> Task<kerno_core::Result<Arc<dyn Session>>> {
        let closed = || kerno_core::Error::Closed("the connection is not open".into());
        let Some(connection) = self.connection.clone() else {
            return Task::ready(Err(closed()));
        };
        if let Some(session) = self.sessions.get(database) {
            return Task::ready(Ok(session.clone()));
        }
        let connections = self.connections.read(cx);
        match connections.meta_session(&connection) {
            None => return Task::ready(Err(closed())),
            // Without schemas, any session sees every database.
            Some(meta) if !meta.dialect().has_schemas() => return Task::ready(Ok(meta)),
            Some(_) => {}
        }
        let opening = connections.open_session(&connection, Some(database.to_string()), cx);
        let generation = self.generation;
        let database = database.to_string();
        cx.spawn(async move |this, cx| {
            let session = opening.await?;
            let _ = this.update(cx, |this, _| {
                if this.generation == generation {
                    this.sessions.insert(database, session.clone());
                }
            });
            Ok(session)
        })
    }

    /// Runs a load on the session of its database.
    fn run(&mut self, load: Load, cx: &mut Context<Self>) {
        let generation = self.generation;
        let session = self.session_for(&load.database, cx);
        cx.spawn(async move |this, cx| {
            let outcome = match session.await {
                Err(error) => Err(error),
                Ok(session) => {
                    let what = load.what.clone();
                    let fetching = cx.update(|cx| {
                        bridge::spawn(cx, async move {
                            Ok(match what {
                                LoadKind::Schemas => Loaded::Schemas(session.schemas().await?),
                                LoadKind::Objects { schema } => {
                                    Loaded::Objects(Box::new(session.objects(&schema).await?))
                                }
                                LoadKind::Structure { table } => Loaded::Structure(Box::new(
                                    session.table_structure(&table).await?,
                                )),
                                LoadKind::Extensions => {
                                    Loaded::Extensions(session.extensions().await?)
                                }
                            })
                        })
                    });
                    fetching.await
                }
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.tree.finish(
                        &load.id,
                        outcome.map_err(|error: kerno_core::Error| error.to_string()),
                    );
                    this.sync(cx);
                }
            });
        })
        .detach();
    }

    // Actions -----------------------------------------------------------------

    /// Does what a menu entry says for the node `id`.
    pub fn perform(
        &mut self,
        action: Action,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(node) = self.tree.node(id).cloned() else {
            return;
        };
        match action {
            Action::Refresh => self.refresh(id, cx),
            Action::OpenData | Action::OpenStructure => {
                if let (NodeKind::Object(ObjectRef::Relation(table, kind)), Some(target)) =
                    (&node.kind, self.target_of(&node, false))
                {
                    let (table, kind) = (table.clone(), *kind);
                    cx.emit(match action {
                        Action::OpenData => NavigatorEvent::OpenData {
                            target,
                            table,
                            kind,
                        },
                        _ => NavigatorEvent::ShowStructure {
                            target,
                            table,
                            kind,
                        },
                    });
                }
            }
            Action::CopyName => cx.write_to_clipboard(ClipboardItem::new_string(name_of(&node))),
            Action::Export => {
                if let (NodeKind::Object(ObjectRef::Relation(table, _)), Some(target)) =
                    (&node.kind, self.target_of(&node, false))
                {
                    let source = Source::Table {
                        table: table.clone(),
                        filter: Filter::default(),
                        sort: None,
                    };
                    export_dialog::open(self.connections.clone(), target, source, window, cx);
                }
            }
            Action::ImportCsv => {
                if self.unavailable(action, &node, cx).is_none()
                    && let (Some(target), Some(schema)) =
                        (self.target_of(&node, false), node.schema.clone())
                {
                    let table = match &node.kind {
                        NodeKind::Object(ObjectRef::Relation(table, _)) => Some(table.clone()),
                        _ => None,
                    };
                    cx.emit(NavigatorEvent::ImportCsv {
                        target,
                        schema,
                        table,
                    });
                }
            }
            Action::CreateTable => {
                if self.unavailable(action, &node, cx).is_none()
                    && let (Some(target), Some(schema)) =
                        (self.target_of(&node, false), node.schema.clone())
                {
                    cx.emit(NavigatorEvent::CreateTable { target, schema });
                }
            }
            Action::NewQueryHere | Action::CreateView => {
                self.open_template(action, &node, window, cx)
            }
            Action::Generate(template) => self.generate(template, &node, window, cx),
            Action::ShowDdl => self.show_ddl(&node, window, cx),
            Action::EditSource => {
                if self.unavailable(action, &node, cx).is_none() {
                    self.edit_source(&node, window, cx)
                }
            }
            Action::Rename
            | Action::Truncate
            | Action::Drop
            | Action::CreateSchema
            | Action::CreateDatabase
            | Action::InstallExtension
            | Action::CreateEnum
            | Action::AddEnumValue
            | Action::RenameEnumValue => self.confirm(action, &node, window, cx),
        }
    }

    /// Why a menu entry cannot be used on this node, when it cannot.
    pub fn unavailable(&self, action: Action, node: &Node, cx: &App) -> Option<&'static str> {
        let connection = self.connection.as_deref()?;
        let connections = self.connections.read(cx);
        let saved = connections.get(connection)?;
        let meta = connections.meta_session(connection)?;
        let dialect = meta.dialect();
        if action.writes() && saved.config.read_only {
            return Some("read-only connection");
        }
        let object = match &node.kind {
            NodeKind::Object(object) => Some(object),
            _ => None,
        };
        match (action, object) {
            (Action::Rename, Some(object)) if dialect.rename_sql(object, "name").is_err() => {
                Some("not supported by this database")
            }
            (Action::Drop, Some(object)) if dialect.drop_sql(object).is_err() => {
                Some("not supported by this database")
            }
            (Action::EditSource, Some(object))
                if dialect.replace_source_sql(object, "").is_err() =>
            {
                Some("not supported by this database")
            }
            // PostgreSQL cannot drop the database a session is in.
            (Action::Drop, Some(ObjectRef::Database(name)))
                if dialect.has_schemas() && meta.database().as_deref() == Some(name.as_str()) =>
            {
                Some("the connection is using it")
            }
            _ => None,
        }
    }

    /// Opens a query tab with a statement to complete: nothing runs.
    fn open_template(
        &mut self,
        action: Action,
        node: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(schema) = node.schema.clone() else {
            return;
        };
        let target = self.target_of(node, action == Action::NewQueryHere);
        let session = self.session_for(&node.database, cx);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = session.await;
            let _ = this.update_in(cx, |_, window, cx| match outcome {
                Ok(session) => {
                    let dialect = session.dialect();
                    let (title, sql) = match action {
                        Action::CreateView => (
                            "New view".to_string(),
                            format!(
                                "CREATE VIEW {} AS\nSELECT 1 AS one;\n",
                                dialect.table_name(&TableRef::new(schema, "new_view"))
                            ),
                        ),
                        // An empty tab that runs in this schema.
                        _ => (schema.clone(), String::new()),
                    };
                    cx.emit(NavigatorEvent::OpenQuery { title, sql, target });
                }
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(error.to_string())),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Writes a statement for a table into a new query tab.
    fn generate(
        &mut self,
        template: Template,
        node: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let NodeKind::Object(ObjectRef::Relation(table, _)) = &node.kind else {
            return;
        };
        let table = table.clone();
        let target = self.target_of(node, false);
        let page_size = AppState::get(cx).settings.page_size;
        let session = self.session_for(&node.database, cx);
        cx.spawn_in(window, async move |this, cx| {
            let written = match session.await {
                Err(error) => Err(error),
                Ok(session) => {
                    let table = table.clone();
                    let writing = cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            let structure = session.table_structure(&table).await?;
                            let dialect = session.dialect();
                            Ok(match template {
                                Template::Select => select_template(
                                    dialect,
                                    &table,
                                    &structure.columns,
                                    u64::from(page_size),
                                ),
                                Template::Insert => {
                                    insert_template(dialect, &table, &structure.columns)
                                }
                                Template::Update => update_template(dialect, &table, &structure),
                                Template::Delete => delete_template(dialect, &table, &structure),
                            })
                        })
                    });
                    match writing {
                        Ok(writing) => writing.await,
                        Err(_) => return,
                    }
                }
            };
            let _ = this.update_in(cx, |_, window, cx| match written {
                Ok(sql) => cx.emit(NavigatorEvent::OpenQuery {
                    title: table.name.clone(),
                    sql,
                    target,
                }),
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "The statement for {} could not be written: {error}",
                        table.name
                    ))),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Opens the `CREATE` statement of an object in a query tab.
    fn show_ddl(&mut self, node: &Node, window: &mut Window, cx: &mut Context<Self>) {
        let NodeKind::Object(object) = &node.kind else {
            return;
        };
        let (object, name) = (object.clone(), node.label.clone());
        let target = self.target_of(node, false);
        let session = self.session_for(&node.database, cx);
        cx.spawn_in(window, async move |this, cx| {
            let ddl = match session.await {
                Err(error) => Err(error),
                Ok(session) => {
                    match cx.update(|_, cx| {
                        bridge::spawn(cx, async move { session.ddl(&object).await })
                    }) {
                        Ok(reading) => reading.await,
                        Err(_) => return,
                    }
                }
            };
            let _ = this.update_in(cx, |_, window, cx| match ddl {
                Ok(sql) => cx.emit(NavigatorEvent::OpenQuery {
                    title: format!("DDL {name}"),
                    sql,
                    target,
                }),
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "The DDL of {name} could not be read: {error}"
                    ))),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Opens the script that replaces a view or routine by its edited
    /// definition, in a query tab in source mode. Nothing runs.
    fn edit_source(&mut self, node: &Node, window: &mut Window, cx: &mut Context<Self>) {
        let (NodeKind::Object(object), Some(schema)) = (&node.kind, node.schema.clone()) else {
            return;
        };
        let (object, name) = (object.clone(), node.label.clone());
        let target = self.target_of(node, false);
        let session = self.session_for(&node.database, cx);
        cx.spawn_in(window, async move |this, cx| {
            let script = match session.await {
                Err(error) => Err(error),
                Ok(session) => {
                    match cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            let ddl = session.ddl(&object).await?;
                            session.dialect().replace_source_sql(&object, &ddl)
                        })
                    }) {
                        Ok(reading) => reading.await,
                        Err(_) => return,
                    }
                }
            };
            let _ = this.update_in(cx, |_, window, cx| match script {
                Ok(sql) => cx.emit(NavigatorEvent::OpenSource {
                    title: name,
                    sql,
                    target,
                    schema,
                }),
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "The source of {name} could not be read: {error}"
                    ))),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Reads again the objects of `schema` after a source was applied
    /// from a tab on `target`, when this tree shows that connection.
    pub fn refresh_source(&mut self, target: &Target, schema: &str, cx: &mut Context<Self>) {
        self.refresh_tables(&target.connection, target.database.as_deref(), schema, cx);
    }

    /// Shows the statement a destructive or creating action would run and
    /// runs it once confirmed.
    fn confirm(
        &mut self,
        action: Action,
        node: &Node,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(connection) = self.connection.clone() else {
            return;
        };
        let connections = self.connections.read(cx);
        let (Some(saved), Some(meta)) = (
            connections.get(&connection),
            connections.meta_session(&connection),
        ) else {
            return;
        };
        if self.unavailable(action, node, cx).is_some() {
            return;
        }
        let (connection_name, environment) = (saved.config.name.clone(), saved.config.environment);
        let object = match &node.kind {
            NodeKind::Object(object) => Some(object.clone()),
            _ => None,
        };
        let name = node.label.clone();
        let kind = match (&node.kind, &object) {
            (NodeKind::Extension { .. }, _) => "extension",
            (_, Some(object)) => kind_name(object),
            _ => "object",
        };
        // The statement is written by the dialect of the connection; the
        // session it runs on is found when it runs.
        let dialect_of = meta.clone();
        let above = model::parent_of(&node.id).map(str::to_string);
        // Databases are listed by the connection's own session, and a
        // database is dropped from outside it.
        let on_root = matches!(object, Some(ObjectRef::Database(_)));
        let (title, explanation, statement, button, destructive, after, done) = match action {
            Action::Drop => {
                let sql = match (&node.kind, &object) {
                    (NodeKind::Extension { name, .. }, _) => {
                        dialect_of.dialect().drop_extension_sql(name)
                    }
                    (_, Some(object)) => dialect_of.dialect().drop_sql(object),
                    _ => return,
                };
                let Ok(sql) = sql else {
                    return;
                };
                (
                    format!("Drop {kind} “{name}”?"),
                    format!(
                        "The {kind} and what it holds are removed for good. This cannot be undone."
                    ),
                    Statement::Fixed(sql),
                    "Drop",
                    true,
                    if on_root {
                        After::Reload
                    } else {
                        After::Refresh(above.clone())
                    },
                    format!("Dropped {kind} {name}."),
                )
            }
            Action::Truncate => {
                let Some(ObjectRef::Relation(table, _)) = &object else {
                    return;
                };
                (
                    format!("Truncate table “{name}”?"),
                    "Every row of the table is deleted. This cannot be undone.".to_string(),
                    Statement::Fixed(dialect_of.dialect().truncate_sql(table)),
                    "Truncate",
                    true,
                    After::Refresh(above.clone()),
                    format!("Truncated table {name}."),
                )
            }
            Action::Rename => {
                let Some(object) = &object else {
                    return;
                };
                let (renamed, build_for) = (object.clone(), dialect_of.clone());
                let current = plain_name(object);
                (
                    format!("Rename {kind} “{name}”"),
                    format!(
                        "Statements, views and functions that use the old name of the {kind} have to be changed too."
                    ),
                    Statement::Named {
                        label: "New name",
                        initial: plain_name(object),
                        build: Rc::new(move |new_name| {
                            if new_name == current {
                                return Err("Type a different name.".into());
                            }
                            build_for
                                .dialect()
                                .rename_sql(&renamed, new_name)
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Rename",
                    false,
                    if on_root {
                        After::Reload
                    } else {
                        After::Refresh(above.clone())
                    },
                    format!("Renamed {kind} {name}."),
                )
            }
            Action::CreateSchema => {
                let build_for = dialect_of.clone();
                (
                    "Create schema".to_string(),
                    format!("A new, empty schema in the database “{}”.", node.database),
                    Statement::Named {
                        label: "Name",
                        initial: String::new(),
                        build: Rc::new(move |name| {
                            build_for
                                .dialect()
                                .create_schema_sql(name)
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Create schema",
                    false,
                    // The schemas are the children of the database node.
                    After::Refresh(Some(format!("db:{}", node.database))),
                    "Created the schema.".to_string(),
                )
            }
            Action::CreateDatabase => {
                let build_for = dialect_of.clone();
                (
                    "Create database".to_string(),
                    "A new, empty database on this server.".to_string(),
                    Statement::Named {
                        label: "Name",
                        initial: String::new(),
                        build: Rc::new(move |name| {
                            build_for
                                .dialect()
                                .create_database_sql(name)
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Create database",
                    false,
                    After::Reload,
                    "Created the database.".to_string(),
                )
            }
            Action::InstallExtension => {
                let NodeKind::Extension {
                    name: extension, ..
                } = &node.kind
                else {
                    return;
                };
                let Ok(sql) = dialect_of.dialect().create_extension_sql(extension) else {
                    return;
                };
                (
                    format!("Install extension “{name}”?"),
                    format!(
                        "Its types, functions and operators are added to the database “{}”.",
                        node.database
                    ),
                    Statement::Fixed(sql),
                    "Install",
                    false,
                    After::Refresh(above.clone()),
                    format!("Installed extension {name}."),
                )
            }
            Action::CreateEnum => {
                let Some(schema) = node.schema.clone() else {
                    return;
                };
                let build_for = dialect_of.clone();
                (
                    "Create enum".to_string(),
                    format!("A new enum type in the schema “{schema}”, with its values in order."),
                    Statement::NamedList {
                        list_label: "Values, one per line",
                        build: Rc::new(move |name, values| {
                            build_for
                                .dialect()
                                .create_enum_sql(&TableRef::new(schema.clone(), name), values)
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Create enum",
                    false,
                    After::Refresh(Some(node.id.clone())),
                    "Created the enum.".to_string(),
                )
            }
            Action::AddEnumValue => {
                // On a value: the new one goes before it.
                let (ty, before) = match &node.kind {
                    NodeKind::Object(ObjectRef::Type(ty)) => (ty.clone(), None),
                    NodeKind::EnumValue { ty, value } => (ty.clone(), Some(value.clone())),
                    _ => return,
                };
                let explanation = match &before {
                    Some(value) => format!("The new value goes just before “{value}”."),
                    None => "The new value goes after the others.".to_string(),
                };
                let (title, build_for) =
                    (format!("Add a value to “{}”", ty.name), dialect_of.clone());
                (
                    title,
                    explanation,
                    Statement::Named {
                        label: "Value",
                        initial: String::new(),
                        build: Rc::new(move |value| {
                            build_for
                                .dialect()
                                .add_enum_value_sql(&ty, value, before.as_deref())
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Add value",
                    false,
                    After::Refresh(Some(node.id.clone())),
                    "Added the value.".to_string(),
                )
            }
            Action::RenameEnumValue => {
                let NodeKind::EnumValue { ty, value } = &node.kind else {
                    return;
                };
                let (ty, current, build_for) = (ty.clone(), value.clone(), dialect_of.clone());
                (
                    format!("Rename value “{value}”"),
                    format!("Rows holding “{value}” hold the new value from then on."),
                    Statement::Named {
                        label: "New value",
                        initial: value.clone(),
                        build: Rc::new(move |new_value| {
                            if new_value == current {
                                return Err("Type a different value.".into());
                            }
                            build_for
                                .dialect()
                                .rename_enum_value_sql(&ty, &current, new_value)
                                .map_err(|error| error.to_string())
                        }),
                    },
                    "Rename value",
                    false,
                    After::Refresh(Some(node.id.clone())),
                    "Renamed the value.".to_string(),
                )
            }
            _ => return,
        };
        // Creating a database and anything done to a database runs on the
        // connection's own session.
        let database = if on_root || action == Action::CreateDatabase {
            None
        } else {
            Some(node.database.clone())
        };
        // A session of the navigator's own inside a database would keep
        // PostgreSQL from dropping or renaming it.
        let release = match (&object, action) {
            (Some(ObjectRef::Database(name)), Action::Drop | Action::Rename) => Some(name.clone()),
            _ => None,
        };
        let plan = Plan {
            database,
            release,
            after,
            done,
        };
        let view = cx.entity();
        sql_confirm::open(
            Confirmation {
                title,
                explanation,
                statement,
                action: button,
                destructive,
                connection: connection_name,
                environment,
            },
            window,
            cx,
            move |sql, window, cx| {
                let plan = plan.clone();
                view.update(cx, |this, cx| this.execute(sql, plan, window, cx));
            },
        );
    }

    /// Runs a confirmed statement and shows what came of it.
    fn execute(&mut self, sql: String, plan: Plan, window: &mut Window, cx: &mut Context<Self>) {
        let Plan {
            database,
            release,
            after,
            done,
        } = plan;
        let Some(connection) = self.connection.clone() else {
            return;
        };
        if let Some(released) = release {
            self.sessions.remove(&released);
        }
        let session = match &database {
            Some(database) => self.session_for(database, cx),
            None => match self.connections.read(cx).meta_session(&connection) {
                Some(meta) => Task::ready(Ok(meta)),
                None => return,
            },
        };
        let generation = self.generation;
        cx.spawn_in(window, async move |this, cx| {
            let outcome = match session.await {
                Err(error) => Err(error),
                Ok(session) => {
                    match cx.update(|_, cx| bridge::spawn(cx, exec::collect(session, sql))) {
                        Ok(running) => running.await.map(|_| ()),
                        Err(_) => return,
                    }
                }
            };
            let _ = this.update_in(cx, |this, window, cx| match outcome {
                Ok(()) => {
                    window.push_notification(Notification::success(SharedString::from(done)), cx);
                    if this.generation != generation {
                        return;
                    }
                    match after {
                        After::Reload => this.reload(cx),
                        After::Refresh(Some(id)) => this.refresh(&id, cx),
                        After::Refresh(None) => {}
                    }
                }
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(error.to_string())),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Shows the model in the tree widget, keeping the selection.
    fn sync(&mut self, cx: &mut Context<Self>) {
        fn convert(item: &Item) -> TreeItem {
            TreeItem::new(item.id.clone(), item.label.clone())
                .expanded(item.expanded)
                .children(item.children.iter().map(convert))
        }
        let items: Vec<TreeItem> = self.tree.items().iter().map(convert).collect();
        self.tree_state.update(cx, |state, cx| {
            let selected = state.selected_item().map(|item| item.id.clone());
            state.set_items(items, cx);
            let index = selected.and_then(|id| state.index_of(&id));
            state.set_selected_index(index, cx);
        });
        cx.notify();
    }

    // Clicks and keys --------------------------------------------------------

    fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        self.tree_state
            .update(cx, |state, cx| state.set_selected_index(Some(index), cx));
    }

    /// Opens the row's node, or closes it.
    fn toggle_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((id, open)) = self
            .tree_state
            .read(cx)
            .entry(index)
            .filter(|entry| entry.is_folder())
            .map(|entry| (entry.item().id.to_string(), entry.is_expanded()))
        else {
            return;
        };
        if open {
            self.tree.collapse(&id);
        } else {
            self.expand(&id, cx);
        }
        self.sync(cx);
    }

    /// What a double click or Enter does: opens the row's object (see
    /// [`default_action`]), or opens or closes the row.
    fn open_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .tree_state
            .read(cx)
            .entry(index)
            .map(|entry| entry.item().id.to_string())
        else {
            return;
        };
        let Some(node) = self.tree.node(&id).cloned() else {
            return;
        };
        let has_schemas = self
            .connection
            .as_deref()
            .and_then(|connection| self.connections.read(cx).meta_session(connection))
            .is_some_and(|meta| meta.dialect().has_schemas());
        let table = AppState::get(cx).settings.table_double_click;
        match default_action(&node, has_schemas, table) {
            // A source that cannot be edited here (read-only connection)
            // is shown as DDL.
            Some(Action::EditSource)
                if self.unavailable(Action::EditSource, &node, cx).is_some() =>
            {
                self.perform(Action::ShowDdl, &id, window, cx)
            }
            Some(action) => self.perform(action, &id, window, cx),
            None => self.toggle_row(index, cx),
        }
    }

    /// Right: opens the selected row, or goes to its first child when open.
    fn key_right(&mut self, cx: &mut Context<Self>) {
        let state = self.tree_state.read(cx);
        let Some(index) = state.selected_index() else {
            return;
        };
        let Some(entry) = state.entry(index) else {
            return;
        };
        if !entry.is_folder() {
            return;
        }
        if !entry.is_expanded() {
            self.toggle_row(index, cx);
        } else if state
            .entry(index + 1)
            .is_some_and(|child| child.depth() > entry.depth())
        {
            self.select_row(index + 1, cx);
        }
    }

    /// Left: closes the selected row, or goes to its parent when closed.
    fn key_left(&mut self, cx: &mut Context<Self>) {
        let state = self.tree_state.read(cx);
        let Some(index) = state.selected_index() else {
            return;
        };
        let Some(entry) = state.entry(index) else {
            return;
        };
        if entry.is_folder() && entry.is_expanded() {
            self.toggle_row(index, cx);
            return;
        }
        let depth = entry.depth();
        let parent = (0..index)
            .rev()
            .find(|&above| state.entry(above).is_some_and(|row| row.depth() < depth));
        if let Some(parent) = parent {
            self.select_row(parent, cx);
        }
    }

    /// The selected row; for tests.
    #[cfg(test)]
    pub fn selected_row(&self, cx: &App) -> Option<usize> {
        self.tree_state.read(cx).selected_index()
    }

    /// The rows now shown, indented by depth; for tests.
    #[cfg(test)]
    pub fn outline(&self, cx: &App) -> Vec<String> {
        let state = self.tree_state.read(cx);
        (0..)
            .map_while(|index| state.entry(index))
            .map(|entry| format!("{}{}", "  ".repeat(entry.depth()), entry.item().label))
            .collect()
    }

    /// The id of the node on a row; for tests.
    #[cfg(test)]
    pub fn id_at(&self, row: usize, cx: &App) -> String {
        self.tree_state
            .read(cx)
            .entry(row)
            .expect("a row")
            .item()
            .id
            .to_string()
    }

    /// Whether the node of a row is open; for tests.
    #[cfg(test)]
    pub fn is_open(&self, row: usize, cx: &App) -> bool {
        self.tree_state
            .read(cx)
            .entry(row)
            .is_some_and(|entry| entry.is_expanded())
    }

    /// Why an action cannot be used on the node of a row; for tests.
    #[cfg(test)]
    pub fn why_not(&self, action: Action, row: usize, cx: &App) -> Option<&'static str> {
        let node = self.tree.node(&self.id_at(row, cx))?;
        self.unavailable(action, node, cx)
    }

    /// The row of the node with this id; for tests.
    #[cfg(test)]
    pub fn row_of(&self, label_path: &[&str], cx: &App) -> Option<usize> {
        let state = self.tree_state.read(cx);
        let mut depth = 0;
        let mut found = None;
        for index in 0.. {
            let entry = state.entry(index)?;
            if entry.depth() < depth {
                return None;
            }
            if entry.depth() == depth && entry.item().label.as_ref() == label_path[depth] {
                found = Some(index);
                depth += 1;
                if depth == label_path.len() {
                    return found;
                }
            }
        }
        found
    }

    // Rendering ---------------------------------------------------------------

    fn render_row(
        &self,
        view: &Entity<Self>,
        index: usize,
        entry: &TreeEntry,
        cx: &App,
    ) -> ListItem {
        let theme = cx.theme();
        let id = entry.item().id.as_ref();
        let indent = px(14.) * entry.depth() + px(8.);
        // An id of its own: the tree gives the bare index to the row around it.
        let base = ListItem::new(("node", index))
            .w_full()
            .py_0p5()
            .pr_2()
            .pl(indent)
            .text_sm();
        let Some(node) = self.tree.node(id) else {
            // A line of text under a node: loading, empty, or an error. The
            // tree's own click selects it; there is nothing to open.
            let failed = model::parent_of(id)
                .and_then(|parent| self.tree.node(parent))
                .is_some_and(|parent| matches!(parent.children, Children::Failed(_)));
            return base.child(
                div()
                    .pl(px(20.))
                    .text_xs()
                    .text_color(if failed {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .child(entry.item().label.clone()),
            );
        };
        let chevron = if !entry.is_folder() {
            div().flex_none().size_3().into_any_element()
        } else {
            let view = view.clone();
            div()
                .id(("node-chevron", index))
                .test_support()
                .flex_none()
                .child(
                    Icon::new(if entry.is_expanded() {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size_3()
                    .text_color(theme.muted_foreground),
                )
                // The chevron opens and closes, and nothing else.
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    view.update(cx, |this, cx| {
                        this.select_row(index, cx);
                        this.toggle_row(index, cx);
                    });
                })
                .into_any_element()
        };
        // Clicks are the navigator's: the tree's own would open or close
        // the row on a single click.
        let view = view.clone();
        base.on_click(move |event, window, cx| {
            cx.stop_propagation();
            let count = event.click_count();
            view.update(cx, |this, cx| {
                this.select_row(index, cx);
                if count >= 2 {
                    this.open_row(index, window, cx);
                }
            });
        })
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_1p5()
                .child(chevron)
                .child(
                    Icon::new(glyph(&node.kind))
                        .flex_none()
                        .size_3p5()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    // The name keeps its room: the hint only gets what is
                    // left, and the name is cut only when alone too long.
                    div()
                        .min_w_0()
                        .truncate()
                        .when(node.muted, |this| this.text_color(theme.muted_foreground))
                        .child(entry.item().label.clone()),
                )
                .when_some(node.hint.clone(), |this, hint| {
                    this.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_right()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(hint)),
                    )
                }),
        )
    }

    fn render_header(&self, connection: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let saved = self.connections.read(cx).get(connection);
        let name = saved
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default();
        let engine = saved.map(|saved| saved.config.engine);
        h_flex()
            .px_3()
            .gap_2()
            .when_some(engine, |this, engine| {
                this.child(div().flex_none().child(engine_logo(engine, px(16.), cx)))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(SharedString::from(name)),
            )
            .child(
                Button::new("navigator-reload")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Glyph::RefreshCw))
                    .tooltip("Reload the schema tree")
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
    }
}

/// How a confirmed statement is run.
#[derive(Debug, Clone)]
struct Plan {
    /// The database whose session runs it; the connection's own session
    /// when `None`.
    database: Option<String>,
    /// A database the navigator must not be connected to meanwhile.
    release: Option<String>,
    after: After,
    /// What to say once it ran.
    done: String,
}

/// What to show again once a statement ran.
#[derive(Debug, Clone)]
enum After {
    /// The list of databases.
    Reload,
    /// The node with this id (nothing when there is none).
    Refresh(Option<String>),
}

/// What a kind of object is called in a sentence.
fn kind_name(object: &ObjectRef) -> &'static str {
    match object {
        ObjectRef::Relation(_, RelationKind::View) => "view",
        ObjectRef::Relation(_, RelationKind::MaterializedView) => "materialized view",
        ObjectRef::Relation(..) => "table",
        ObjectRef::Routine {
            kind: RoutineKind::Procedure,
            ..
        } => "procedure",
        ObjectRef::Routine { .. } => "function",
        ObjectRef::Sequence(_) => "sequence",
        ObjectRef::Type(_) => "type",
        ObjectRef::Trigger { .. } => "trigger",
        ObjectRef::Event(_) => "event",
        ObjectRef::Index { .. } => "index",
        ObjectRef::Schema(_) => "schema",
        ObjectRef::Database(_) => "database",
    }
}

/// The name of an object without its schema: what a rename starts from.
fn plain_name(object: &ObjectRef) -> String {
    match object {
        ObjectRef::Relation(table, _)
        | ObjectRef::Sequence(table)
        | ObjectRef::Type(table)
        | ObjectRef::Event(table) => table.name.clone(),
        ObjectRef::Routine { name, .. }
        | ObjectRef::Trigger { name, .. }
        | ObjectRef::Index { name, .. }
        | ObjectRef::Schema(name)
        | ObjectRef::Database(name) => name.clone(),
    }
}

/// The glyph of a kind of node.
fn glyph(kind: &NodeKind) -> Glyph {
    match kind {
        NodeKind::Object(object) => match object {
            ObjectRef::Database(_) => Glyph::Database,
            ObjectRef::Schema(_) => Glyph::Boxes,
            ObjectRef::Relation(_, RelationKind::View) => Glyph::Eye,
            ObjectRef::Relation(_, RelationKind::MaterializedView) => Glyph::Layers,
            ObjectRef::Relation(..) => Glyph::Table,
            ObjectRef::Routine {
                kind: RoutineKind::Procedure,
                ..
            } => Glyph::Workflow,
            ObjectRef::Routine { .. } => Glyph::SquareFunction,
            ObjectRef::Sequence(_) => Glyph::ListOrdered,
            ObjectRef::Type(_) => Glyph::Shapes,
            ObjectRef::Trigger { .. } => Glyph::Zap,
            ObjectRef::Event(_) => Glyph::CalendarClock,
            ObjectRef::Index { .. } => Glyph::KeyRound,
        },
        NodeKind::Group(Group::Extensions) | NodeKind::Extension { .. } => Glyph::Puzzle,
        NodeKind::EnumValue { .. } => Glyph::ListOrdered,
        NodeKind::Group(_) => Glyph::Folder,
        NodeKind::Column { .. } => Glyph::Columns3,
    }
}

/// The name to copy for a node: qualified where the object has a schema.
fn name_of(node: &Node) -> String {
    match &node.kind {
        NodeKind::Object(ObjectRef::Relation(table, _))
        | NodeKind::Object(ObjectRef::Sequence(table))
        | NodeKind::Object(ObjectRef::Type(table))
        | NodeKind::Object(ObjectRef::Event(table)) => format!("{}.{}", table.schema, table.name),
        NodeKind::Object(ObjectRef::Routine { schema, name, .. }) => format!("{schema}.{name}"),
        NodeKind::Object(ObjectRef::Trigger { name, .. })
        | NodeKind::Object(ObjectRef::Index { name, .. })
        | NodeKind::Object(ObjectRef::Schema(name))
        | NodeKind::Object(ObjectRef::Database(name))
        | NodeKind::Column { name, .. }
        | NodeKind::Extension { name, .. }
        | NodeKind::EnumValue { value: name, .. } => name.clone(),
        NodeKind::Group(group) => group.label().to_string(),
    }
}

impl Render for Navigator {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let Some(connection) = self.connection.clone() else {
            return v_flex()
                .id("navigator")
                .test_support()
                .flex_1()
                .min_h_0()
                .px_3()
                .py_2()
                .child(div().text_xs().text_color(muted).child(
                    "Connect to a server to browse its databases, tables and other objects here.",
                ));
        };
        let body = match &self.root {
            Root::None => div()
                .px_3()
                .text_xs()
                .text_color(muted)
                .child("Not connected. Double-click the connection to open it.")
                .into_any_element(),
            Root::Loading => div()
                .px_3()
                .text_xs()
                .text_color(muted)
                .child("Loading databases…")
                .into_any_element(),
            Root::Failed(message) => v_flex()
                .id("navigator-error")
                .test_support()
                .px_3()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(SharedString::from(format!(
                            "The databases could not be listed: {message}"
                        ))),
                )
                .child(
                    h_flex().child(
                        Button::new("navigator-retry")
                            .outline()
                            .small()
                            .label("Try again")
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
                )
                .into_any_element(),
            Root::Loaded => {
                let view = cx.entity();
                let menu_view = cx.entity();
                let keys = cx.entity();
                let tree_view = tree(&self.tree_state, move |index, entry, _, _, cx| {
                    view.read(cx).render_row(&view, index, entry, cx)
                })
                .context_menu(move |_, entry, mut menu, _, cx| {
                    let id = entry.item().id.to_string();
                    let navigator = menu_view.read(cx);
                    let Some(node) = navigator.tree.node(&id) else {
                        return menu;
                    };
                    let has_schemas = navigator
                        .connection
                        .as_deref()
                        .and_then(|connection| {
                            navigator.connections.read(cx).meta_session(connection)
                        })
                        .is_some_and(|meta| meta.dialect().has_schemas());
                    for (index, group) in actions(node, has_schemas).into_iter().enumerate() {
                        if index > 0 {
                            menu = menu.separator();
                        }
                        for action in group {
                            // An entry that cannot be used says why.
                            let reason = navigator.unavailable(action, node, cx);
                            let label = match reason {
                                Some(reason) => format!("{} ({reason})", action.label()),
                                None => action.label().to_string(),
                            };
                            let (view, id) = (menu_view.clone(), id.clone());
                            menu = menu.item(
                                PopupMenuItem::new(SharedString::from(label))
                                    .disabled(reason.is_some())
                                    .on_click(move |_, window, cx| {
                                        view.update(cx, |this, cx| {
                                            this.perform(action, &id, window, cx)
                                        })
                                    }),
                            );
                        }
                    }
                    menu
                })
                .size_full();
                // Right and Left also move to the first child and the
                // parent; Enter does what a double click does. They run
                // before the tree's own handling of the keys.
                // Below the objects, the tree's own menu (a row's menu
                // takes the click first).
                let area = cx.entity();
                div()
                    .id("navigator-tree")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .capture_action(cx.listener(|this, _: &SelectRight, _, cx| {
                        cx.stop_propagation();
                        this.key_right(cx);
                    }))
                    .capture_action(cx.listener(|this, _: &SelectLeft, _, cx| {
                        cx.stop_propagation();
                        this.key_left(cx);
                    }))
                    .on_key_down(move |event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified() {
                            cx.stop_propagation();
                            keys.update(cx, |this, cx| {
                                if let Some(index) = this.tree_state.read(cx).selected_index() {
                                    this.open_row(index, window, cx);
                                }
                            });
                        }
                    })
                    .child(tree_view)
                    .context_menu(move |menu, _, _| {
                        let (refresh, collapse, query) = (area.clone(), area.clone(), area.clone());
                        menu.item(PopupMenuItem::new("Refresh").on_click(move |_, _, cx| {
                            refresh.update(cx, |this, cx| this.reload(cx))
                        }))
                        .item(
                            PopupMenuItem::new("Collapse all").on_click(move |_, _, cx| {
                                collapse.update(cx, |this, cx| this.collapse_all(cx))
                            }),
                        )
                        .separator()
                        .item(
                            PopupMenuItem::new("New query here").on_click(move |_, _, cx| {
                                query.update(cx, |this, cx| {
                                    if let Some(connection) = this.connection.clone() {
                                        cx.emit(NavigatorEvent::NewQuery {
                                            target: Target {
                                                connection,
                                                database: None,
                                                schema: None,
                                            },
                                        });
                                    }
                                })
                            }),
                        )
                    })
                    .into_any_element()
            }
        };
        v_flex()
            .id("navigator")
            .test_support()
            .flex_1()
            .min_h_0()
            .gap_2()
            .py_2()
            .child(self.render_header(&connection, cx))
            .when(self.root == Root::Loaded, |this| {
                this.child(
                    div().px_3().child(
                        Input::new(&self.filter)
                            .id("navigator-filter")
                            .small()
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).size_3p5().text_color(muted)),
                    ),
                )
            })
            .child(body)
    }
}
