//! The schema tree of one connection: loads what is opened, filters,
//! refreshes, and offers each object's actions.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tree::{TreeEntry, TreeEvent, TreeItem, TreeState, tree};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div, px,
};
use kerno_core::schema::{ObjectRef, RelationKind, RoutineKind, TableRef};
use kerno_core::sql::{delete_template, insert_template, select_template, update_template};
use kerno_core::{Session, exec};

use super::actions::{Action, Template, actions};
use super::model::{self, Children, Group, Item, Load, LoadKind, Loaded, Node, NodeKind, Tree};
use crate::bridge;
use crate::connections::manager::{Connections, ConnectionsEvent, Status};
use crate::engine_logo::engine_logo;
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::state::AppState;

/// What the rest of the window does for the navigator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigatorEvent {
    /// Open a query tab holding `sql`.
    OpenQuery { title: String, sql: String },
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
            Action::CopyName => cx.write_to_clipboard(ClipboardItem::new_string(name_of(&node))),
            Action::NewQueryHere | Action::CreateTable | Action::CreateView => {
                self.open_template(action, &node, window, cx)
            }
            Action::Generate(template) => self.generate(template, &node, window, cx),
            Action::ShowDdl => self.show_ddl(&node, window, cx),
            Action::Rename
            | Action::Truncate
            | Action::Drop
            | Action::CreateSchema
            | Action::CreateDatabase => self.confirm(action, &node, window, cx),
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
        let session = self.session_for(&node.database, cx);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = session.await;
            let _ = this.update_in(cx, |_, window, cx| match outcome {
                Ok(session) => {
                    let dialect = session.dialect();
                    let (title, sql) = match action {
                        Action::CreateTable => (
                            "New table".to_string(),
                            format!(
                                "CREATE TABLE {} (\n    id BIGINT PRIMARY KEY,\n    name VARCHAR(255) NOT NULL\n);\n",
                                dialect.table_name(&TableRef::new(schema, "new_table"))
                            ),
                        ),
                        Action::CreateView => (
                            "New view".to_string(),
                            format!(
                                "CREATE VIEW {} AS\nSELECT 1 AS one;\n",
                                dialect.table_name(&TableRef::new(schema, "new_view"))
                            ),
                        ),
                        // The tab starts in this schema.
                        _ if dialect.has_schemas() => (
                            schema.clone(),
                            format!("SET search_path TO {};\n\n", dialect.quote_ident(&schema)),
                        ),
                        _ => (schema.clone(), format!("USE {};\n\n", dialect.quote_ident(&schema))),
                    };
                    cx.emit(NavigatorEvent::OpenQuery { title, sql });
                }
                Err(error) => window.push_notification(Notification::error(SharedString::from(error.to_string())), cx),
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
            NodeKind::Object(object) => object.clone(),
            _ => return,
        };
        let name = node.label.clone();
        let kind = kind_name(&object);
        // The statement is written by the dialect of the connection; the
        // session it runs on is found when it runs.
        let dialect_of = meta.clone();
        let above = model::parent_of(&node.id).map(str::to_string);
        // Databases are listed by the connection's own session, and a
        // database is dropped from outside it.
        let on_root = matches!(object, ObjectRef::Database(_));
        let (title, explanation, statement, button, destructive, after, done) = match action {
            Action::Drop => {
                let Ok(sql) = dialect_of.dialect().drop_sql(&object) else {
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
                let ObjectRef::Relation(table, _) = &object else {
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
                let (renamed, build_for) = (object.clone(), dialect_of.clone());
                let current = plain_name(&object);
                (
                    format!("Rename {kind} “{name}”"),
                    format!(
                        "Statements, views and functions that use the old name of the {kind} have to be changed too."
                    ),
                    Statement::Named {
                        label: "New name",
                        initial: plain_name(&object),
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
            (ObjectRef::Database(name), Action::Drop | Action::Rename) => Some(name.clone()),
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

    fn render_row(&self, index: usize, entry: &TreeEntry, cx: &App) -> ListItem {
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
            // A line of text under a node: loading, empty, or an error.
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
            Icon::new(if entry.is_expanded() {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .flex_none()
            .size_3()
            .text_color(theme.muted_foreground)
            .into_any_element()
        };
        base.child(
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
        NodeKind::Group(Group::Extensions) | NodeKind::Extension(_) => Glyph::Puzzle,
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
        | NodeKind::Extension(name) => name.clone(),
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
                tree(&self.tree_state, move |index, entry, _, _, cx| {
                    view.read(cx).render_row(index, entry, cx)
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
                .flex_1()
                .min_h_0()
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
