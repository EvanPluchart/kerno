//! A query tab: a SQL editor bound to a connection, and what its
//! statements returned.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Glyph;
use gpui_kit::base::input;
use gpui_kit::base::input::Diagnostic;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    CompletionProvider, Editor, EditorState, InputEvent, Position, RopeExt as _,
};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    div, px,
};
use kerno_core::config::Environment;
use kerno_core::sql::{
    LexRules, Param, ParamKey, ParamValue, bind_params, find_params, format_sql, split_statements,
    statement_at,
};
use kerno_core::sql::{StatementClass, StatementKind, TxCommand, classify};
use kerno_core::storage::{HistoryEntry, HistoryStatus};
use kerno_core::{
    Dialect, Error, ExecEvent, Notice, Plan, ResultStore, RowLimit, Session, TxState, exec,
};

use super::comment;
use super::completion::{self, SharedCatalog, SqlCompletion};
use super::grid::{CopyFormat, Grid};
use super::params;
use super::{plan_view, viewer};
use crate::actions::{
    ExplainAnalyze, ExplainStatement, FindInEditor, FormatSql, ReplaceInEditor, RunAll,
    RunStatement, SaveFile, SaveFileAs, SaveQuery, ShowHistory, ShowSavedQueries, ToggleComment,
};
use crate::bridge;
use crate::connections::manager::{Connections, ConnectionsEvent, Status};
use crate::engine_logo::engine_logo;
use crate::export_dialog::{self, ExportDialog, Source};
use crate::sql_confirm::{self, Confirmation, Statement as ConfirmStatement};
use crate::state::AppState;
use crate::tabs::{self, EditorFacts, TabAction, TabSnapshot, Target};

/// A statement to run.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Statement {
    sql: String,
    /// Byte offset of the statement in the editor's text, when it runs as
    /// written there.
    origin: Option<usize>,
}

/// How a statement ended.
struct Ended {
    started_at_ms: i64,
    elapsed: Duration,
    status: HistoryStatus,
    failure: Option<Failure>,
    row_count: Option<u64>,
}

/// Line and column of a byte offset, as the editor counts them.
fn position_of(text: &str, offset: usize) -> Position {
    let before = &text[..offset.min(text.len())];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    Position {
        line: before.matches('\n').count() as u32,
        character: before[line_start..].encode_utf16().count() as u32,
    }
}

/// Which statements a run takes from the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The selection if there is one, otherwise the statement the cursor
    /// is in.
    AtCursor,
    /// The selection if there is one, otherwise the whole text.
    All,
}

/// Why a statement failed, as the server said it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub code: Option<String>,
    pub detail: Option<String>,
    pub hint: Option<String>,
    /// 1-based character position in the statement, when the server gave one.
    pub position: Option<u32>,
}

impl From<&Error> for Failure {
    fn from(error: &Error) -> Self {
        match error {
            Error::Db(db) => Self {
                message: db.message.clone(),
                code: db.code.clone(),
                detail: db.detail.clone(),
                hint: db.hint.clone(),
                position: db.position,
            },
            other => Self {
                message: other.to_string(),
                code: None,
                detail: None,
                hint: None,
                position: None,
            },
        }
    }
}

/// What a statement produced.
pub enum Outcome {
    /// Rows, in a grid.
    Rows(Entity<TableState<Grid>>),
    /// The plan of the statement (EXPLAIN).
    Plan {
        /// The plan as a tree; `None` when the output could not be read.
        plan: Option<Plan>,
        /// What the server returned, as it is.
        raw: String,
        /// Why there is no tree, when there is none.
        problem: Option<String>,
        /// The raw output is shown instead of the tree.
        show_raw: bool,
    },
    /// It ran and returned no rows.
    Done,
    /// The server refused it.
    Failed(Failure),
    /// It was cancelled.
    Cancelled,
}

/// One statement of a run and what came of it.
pub struct ResultPane {
    pub statement: String,
    pub outcome: Outcome,
    /// Count the server reported: rows changed, or rows returned.
    pub rows_affected: Option<u64>,
    /// From sending the statement to its end (or to now, while it runs).
    pub elapsed: Option<Duration>,
}

impl ResultPane {
    /// The grid of the pane, when it holds rows.
    fn grid(&self) -> Option<Entity<TableState<Grid>>> {
        match &self.outcome {
            Outcome::Rows(grid) => Some(grid.clone()),
            _ => None,
        }
    }
}

/// What the status bar shows of the active tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabStatus {
    pub connection: Option<String>,
    pub read_only: bool,
    pub database: Option<String>,
    pub transaction: TxState,
    pub running: bool,
    /// Rows of the result on screen.
    pub rows: Option<usize>,
    /// Duration of the statement whose result is on screen.
    pub elapsed: Option<Duration>,
    /// Messages the server sent during the last run.
    pub notices: usize,
    /// Rows with changes waiting to be applied (a table's data).
    pub pending: usize,
}

/// A statement on one line, cut short: enough to tell which one it is.
fn one_line(statement: &str) -> String {
    let mut line = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 120 {
        line = line.chars().take(119).collect::<String>() + "…";
    }
    line
}

/// `12 ms` or `1.25 s`.
pub fn duration_text(elapsed: Duration) -> String {
    let millis = elapsed.as_secs_f64() * 1000.;
    if millis < 1000. {
        format!("{millis:.0} ms")
    } else {
        format!("{:.2} s", millis / 1000.)
    }
}

/// What the tab is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Idle,
    /// Waiting for the server: statement `index` of `total`.
    Running {
        index: usize,
        total: usize,
        since: Instant,
    },
}

pub struct QueryTab {
    title: SharedString,
    editor: Entity<EditorState>,
    connections: Entity<Connections>,
    /// The connection, database and schema statements run on.
    target: Option<Target>,
    /// This tab's own session, opened at the first run.
    session: Option<Arc<dyn Session>>,
    /// Databases of the connection and schemas of the database, for the
    /// selectors.
    databases: Vec<String>,
    schemas: Vec<String>,
    results: Vec<ResultPane>,
    active_result: usize,
    notices: Vec<Notice>,
    activity: Activity,
    /// The run in progress. Dropping it stops the statement it waits on.
    run: Option<Task<()>>,
    /// The user asked to cancel the running statement: however the server
    /// words its end, it was cancelled.
    cancel_requested: bool,
    /// Statements run in a transaction that the user commits or rolls back.
    manual_commit: bool,
    /// What was typed for each parameter last time, by its label.
    param_values: HashMap<String, String>,
    /// The editor shows where the last statement failed.
    error_marked: bool,
    /// The `.sql` file the text was read from or written to.
    file: Option<PathBuf>,
    /// The text was changed since it was read or written.
    unsaved: bool,
    /// The run being prepared asks for a plan: `Some(analyze)`.
    explain_mode: Option<bool>,
    grid_subscriptions: Vec<Subscription>,
    /// Bumped at every run and target change, so late answers are dropped.
    generation: u64,
    /// What completion offers; empty until loaded.
    catalog: SharedCatalog,
    /// The catalog being loaded. Replacing it drops the older load.
    catalog_load: Option<Task<()>>,
    /// Source mode: the text replaces an object of this schema (MySQL
    /// database), and "Apply source" runs all of it.
    source: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// What a query tab tells the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryTabEvent {
    /// A source was applied, whether all of it ran or not: the objects of
    /// its schema may have changed.
    SourceApplied,
}

impl EventEmitter<QueryTabEvent> for QueryTab {}

impl QueryTab {
    /// A query tab titled `title` with `sql` in its editor, running on
    /// `target`.
    pub fn new(
        title: impl Into<SharedString>,
        sql: &str,
        target: Option<Target>,
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sql = sql.to_string();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .soft_wrap(false)
                .default_value(sql)
                .placeholder("Write SQL here")
        });
        editor.update(cx, |editor, _| {
            editor
                .lsp_mut()
                .set_completion_highlighter(Rc::new(completion::highlight));
        });
        // An edit makes a file's tab differ from its file. (Moving the
        // cursor also notifies observers; only a change of text counts.)
        let edits = cx.subscribe(&editor, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && this.file.is_some() && !this.unsaved {
                this.unsaved = true;
                cx.notify();
            }
        });
        let subscription = cx.subscribe(&connections, |this, _, event: &ConnectionsEvent, cx| {
            if let ConnectionsEvent::StatusChanged { id } = event
                && this
                    .target
                    .as_ref()
                    .is_some_and(|target| target.connection == *id)
            {
                this.follow_connection(cx);
            }
        });
        let mut tab = Self {
            title: title.into(),
            editor,
            connections,
            target,
            session: None,
            databases: Vec::new(),
            schemas: Vec::new(),
            results: Vec::new(),
            active_result: 0,
            notices: Vec::new(),
            activity: Activity::Idle,
            run: None,
            cancel_requested: false,
            manual_commit: false,
            param_values: HashMap::new(),
            error_marked: false,
            file: None,
            unsaved: false,
            explain_mode: None,
            grid_subscriptions: Vec::new(),
            generation: 0,
            catalog: SharedCatalog::default(),
            catalog_load: None,
            source: None,
            _subscriptions: vec![subscription, edits],
        };
        tab.follow_connection(cx);
        tab
    }

    /// The tab's title.
    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// Names the tab.
    pub fn set_title(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.title = title.into();
        cx.notify();
    }

    /// The editor, to observe its changes.
    pub fn editor(&self) -> &Entity<EditorState> {
        &self.editor
    }

    /// The editor's text.
    pub fn sql(&self, cx: &App) -> String {
        self.editor.read(cx).text().to_string()
    }

    /// What to store of this tab.
    pub fn snapshot(&self, cx: &App) -> TabSnapshot {
        TabSnapshot::Query {
            title: self.title.to_string(),
            sql: self.sql(cx),
            target: self.target.clone(),
            file: self.file.clone(),
            unsaved: self.unsaved,
            source: self.source.clone(),
        }
    }

    /// Puts the tab in source mode for an object of `schema`, or out of it.
    pub fn set_source(&mut self, schema: Option<String>, cx: &mut Context<Self>) {
        self.source = schema;
        cx.notify();
    }

    /// The schema of the object whose source the tab holds, in source mode.
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    /// The `.sql` file of this tab, when it has one.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Whether the text differs from its file's (always, without a file).
    pub fn unsaved(&self) -> bool {
        self.unsaved
    }

    /// The tab is the file at `path` from now on, named after it; its text
    /// is what the file holds unless `unsaved`.
    pub fn set_file(&mut self, path: PathBuf, unsaved: bool, cx: &mut Context<Self>) {
        if let Some(name) = path.file_name() {
            self.title = SharedString::from(name.to_string_lossy().into_owned());
        }
        self.file = Some(path);
        self.unsaved = unsaved;
        cx.notify();
    }

    /// The connection the tab runs on.
    pub fn connection_id(&self) -> Option<&str> {
        self.target
            .as_ref()
            .map(|target| target.connection.as_str())
    }

    /// Where the tab runs its statements.
    pub fn target(&self) -> Option<&Target> {
        self.target.as_ref()
    }

    #[cfg(test)]
    pub fn activity(&self) -> Activity {
        self.activity
    }

    /// What the status bar says about this tab.
    pub fn status(&self, cx: &App) -> TabStatus {
        let connections = self.connections.read(cx);
        let saved = self
            .target
            .as_ref()
            .and_then(|target| connections.get(&target.connection));
        let database = self.target.as_ref().and_then(|target| {
            target.database.clone().or_else(|| {
                connections
                    .meta_session(&target.connection)
                    .and_then(|meta| meta.database())
            })
        });
        let pane = self
            .results
            .get(self.active_result.min(self.results.len().saturating_sub(1)));
        TabStatus {
            connection: saved.map(|saved| saved.config.name.clone()),
            read_only: saved.is_some_and(|saved| saved.config.read_only),
            database,
            transaction: self
                .session
                .as_ref()
                .map_or(TxState::Idle, |session| session.tx_state()),
            running: matches!(self.activity, Activity::Running { .. }),
            rows: pane
                .and_then(ResultPane::grid)
                .map(|grid| grid.read(cx).delegate().row_count()),
            elapsed: pane.and_then(|pane| pane.elapsed),
            notices: self.notices.len(),
            pending: 0,
        }
    }

    /// What each statement of the last run produced, in words; for tests.
    #[cfg(test)]
    pub fn outcomes(&self, cx: &App) -> Vec<String> {
        self.results
            .iter()
            .map(|pane| match &pane.outcome {
                Outcome::Rows(grid) => {
                    let grid = grid.read(cx).delegate();
                    format!(
                        "{} rows{}",
                        grid.row_count(),
                        if grid.is_complete() {
                            ""
                        } else if grid.has_more() {
                            ", more"
                        } else {
                            ", incomplete"
                        }
                    )
                }
                Outcome::Done => match pane.rows_affected {
                    Some(rows) => format!("done, {rows} affected"),
                    None => "done".into(),
                },
                Outcome::Failed(failure) => {
                    format!("failed [{}]", failure.code.clone().unwrap_or_default())
                }
                Outcome::Cancelled => "cancelled".into(),
                Outcome::Plan {
                    plan: Some(plan), ..
                } => {
                    format!("plan, {} steps", plan_view::rows(plan).len())
                }
                Outcome::Plan { problem, .. } => {
                    format!("plan unread: {}", problem.clone().unwrap_or_default())
                }
            })
            .collect()
    }

    /// How many places the editor marks as errors; for tests.
    #[cfg(test)]
    pub fn error_marks(&self, cx: &App) -> usize {
        self.editor
            .read(cx)
            .diagnostics()
            .map_or(0, |diagnostics| diagnostics.len())
    }

    /// The grid of a result; for tests.
    #[cfg(test)]
    pub fn grid_of(&self, result: usize) -> Option<Entity<TableState<Grid>>> {
        self.results.get(result).and_then(ResultPane::grid)
    }

    /// The server's messages of the last run; for tests.
    #[cfg(test)]
    pub fn messages(&self) -> Vec<String> {
        self.notices
            .iter()
            .map(|notice| format!("{}: {}", notice.severity, notice.message))
            .collect()
    }

    /// Column names and the first `limit` rows of a result; for tests.
    #[cfg(test)]
    pub fn rows_of(
        &self,
        result: usize,
        limit: usize,
        cx: &App,
    ) -> (Vec<String>, Vec<Vec<Option<String>>>) {
        let Some(grid) = self.results.get(result).and_then(ResultPane::grid) else {
            return Default::default();
        };
        let store = grid.read(cx).delegate().store();
        let columns: Vec<String> = store
            .columns()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        let rows = (0..store.row_count().min(limit))
            .map(|row| {
                (0..columns.len())
                    .map(|column| store.text(row, column).map(str::to_string))
                    .collect()
            })
            .collect();
        (columns, rows)
    }

    /// Whether the tab's connection is open.
    pub fn connected(&self, cx: &App) -> bool {
        self.target.as_ref().is_some_and(|target| {
            matches!(
                self.connections.read(cx).status(&target.connection),
                Status::Connected { .. }
            )
        })
    }

    /// Runs on `target` from now on. What was running is stopped.
    pub fn set_target(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        if self.target == target {
            return;
        }
        self.stop(cx);
        self.target = target;
        // The source replaces an object where it was read from.
        self.source = None;
        self.session = None;
        self.databases.clear();
        self.schemas.clear();
        self.generation += 1;
        *self.catalog.borrow_mut() = Arc::default();
        self.catalog_load = None;
        self.complete_with(None, cx);
        self.follow_connection(cx);
        cx.notify();
    }

    /// Loads the lists of the selectors when the connection is open; drops
    /// the session when it closed.
    fn follow_connection(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        if !self.connected(cx) {
            self.stop(cx);
            self.session = None;
            self.complete_with(None, cx);
            cx.notify();
            return;
        }
        let Some(meta) = self.connections.read(cx).meta_session(&target.connection) else {
            return;
        };
        self.complete_with(Some(meta.clone()), cx);
        self.load_catalog(cx);
        // The connection's own session lists the databases, and the
        // schemas when the tab is in its database.
        let own_database = target.database.is_none() || target.database == meta.database();
        let wants_schemas = meta.dialect().has_schemas() && own_database;
        let generation = self.generation;
        let listing = bridge::spawn(cx, async move {
            let databases = meta.databases().await?;
            let schemas = if wants_schemas {
                meta.schemas().await?
            } else {
                Vec::new()
            };
            Ok::<_, Error>((databases, schemas))
        });
        cx.spawn(async move |this, cx| {
            if let Ok((databases, schemas)) = listing.await {
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    this.databases = databases
                        .into_iter()
                        .filter(|database| !database.is_system)
                        .map(|database| database.name)
                        .collect();
                    if wants_schemas {
                        this.schemas = schemas
                            .into_iter()
                            .filter(|schema| !schema.is_system)
                            .map(|schema| schema.name)
                            .collect();
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Completion in the editor proposes from the catalog, with the dialect
    /// of `session`; none without a session.
    fn complete_with(&self, session: Option<Arc<dyn Session>>, cx: &mut Context<Self>) {
        let catalog = self.catalog.clone();
        self.editor.update(cx, |editor, _| {
            editor.lsp_mut().completion_provider = session.map(|session| {
                Rc::new(SqlCompletion { catalog, session }) as Rc<dyn CompletionProvider>
            });
        });
    }

    /// Loads (again) what completion offers: from the connection's own
    /// session when the tab is in its database, else from the tab's.
    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let Some(meta) = self.connections.read(cx).meta_session(&target.connection) else {
            return;
        };
        let session = if target.database.is_none() || target.database == meta.database() {
            Task::ready(Ok(meta))
        } else {
            self.session(cx)
        };
        self.catalog_load = Some(cx.spawn(async move |this, cx| {
            let Ok(session) = session.await else {
                return;
            };
            let loading = cx
                .update(|cx| bridge::spawn(cx, async move { session.completion_catalog().await }));
            match loading.await {
                Ok(mut catalog) => {
                    // The tab's schema is searched first.
                    if let Some(schema) = target.schema {
                        catalog.search_path.retain(|name| *name != schema);
                        catalog.search_path.insert(0, schema);
                    }
                    let _ = this.update(cx, |this, _| {
                        *this.catalog.borrow_mut() = Arc::new(catalog);
                    });
                }
                Err(error) => tracing::warn!(%error, "the completion catalog was not loaded"),
            }
        }));
    }

    /// Whether the completion catalog has arrived; for tests.
    #[cfg(test)]
    pub fn catalog_ready(&self) -> bool {
        !self.catalog.borrow().schemas.is_empty()
    }

    /// Labels and inserted texts completion proposes at byte `cursor` of
    /// `sql`; for tests.
    #[cfg(test)]
    pub fn completions(&self, sql: &str, cursor: usize, cx: &App) -> Vec<(String, String)> {
        let Some(session) = self
            .target
            .as_ref()
            .and_then(|target| self.connections.read(cx).meta_session(&target.connection))
        else {
            return Vec::new();
        };
        let provider = SqlCompletion {
            catalog: self.catalog.clone(),
            session,
        };
        provider
            .items(&gpui_kit::component::input::Rope::from(sql), cursor)
            .into_iter()
            .map(|item| {
                let insert = match item.text_edit {
                    Some(lsp_types::CompletionTextEdit::Edit(edit)) => edit.new_text,
                    _ => String::new(),
                };
                (item.label, insert)
            })
            .collect()
    }

    /// This tab's session, opened (and put in the tab's schema) if needed.
    fn session(&mut self, cx: &mut Context<Self>) -> Task<kerno_core::Result<Arc<dyn Session>>> {
        if let Some(session) = &self.session {
            return Task::ready(Ok(session.clone()));
        }
        let Some(target) = self.target.clone() else {
            return Task::ready(Err(Error::Config(
                "choose a connection for this tab first".into(),
            )));
        };
        if !self.connected(cx) {
            let name = self
                .connections
                .read(cx)
                .get(&target.connection)
                .map(|saved| saved.config.name.clone())
                .unwrap_or_else(|| "this tab's connection".into());
            return Task::ready(Err(Error::Closed(format!(
                "“{name}” is not connected. Double-click it in the sidebar, then run again."
            ))));
        }
        let opening =
            self.connections
                .read(cx)
                .open_session(&target.connection, target.database.clone(), cx);
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let session = opening.await?;
            if session.dialect().has_schemas() {
                if let Some(schema) = &target.schema {
                    let sql = format!(
                        "SET search_path TO {}, public",
                        session.dialect().quote_ident(schema)
                    );
                    let entering =
                        cx.update(|cx| bridge::spawn(cx, exec::collect(session.clone(), sql)));
                    entering.await?;
                }
                // The schemas of a database other than the connection's own
                // can only be listed from inside it.
                let listing = {
                    let session = session.clone();
                    cx.update(|cx| bridge::spawn(cx, async move { session.schemas().await }))
                };
                if let Ok(schemas) = listing.await {
                    let _ = this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.schemas = schemas
                                .into_iter()
                                .filter(|schema| !schema.is_system)
                                .map(|schema| schema.name)
                                .collect();
                            cx.notify();
                        }
                    });
                }
            }
            let _ = this.update(cx, |this, _| {
                if this.generation == generation {
                    this.session = Some(session.clone());
                }
            });
            Ok(session)
        })
    }

    /// Whether the result of `pane` can be exported: rows of a statement
    /// that only reads, since an export runs it again.
    fn exportable(&self, pane: &ResultPane, cx: &App) -> bool {
        matches!(pane.outcome, Outcome::Rows(_))
            && !classify(&pane.statement, self.lex_rules(cx)).writes
    }

    /// Opens the export of the result on screen; its statement runs again.
    pub fn export_result(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ExportDialog>> {
        let pane = self.results.get(self.active_result)?;
        if !self.exportable(pane, cx) {
            return None;
        }
        export_dialog::open(
            self.connections.clone(),
            self.target.clone()?,
            Source::Query {
                sql: pane.statement.clone(),
            },
            window,
            cx,
        )
    }

    /// The statements a run takes: the selection or, without one, the
    /// statement at the cursor or the whole text.
    fn lex_rules(&self, cx: &App) -> LexRules {
        self.target
            .as_ref()
            .and_then(|target| self.connections.read(cx).meta_session(&target.connection))
            .map_or(LexRules::POSTGRES, |meta| meta.dialect().lex_rules())
    }

    fn statements(&self, scope: Scope, cx: &App) -> Vec<Statement> {
        let rules = self.lex_rules(cx);
        let editor = self.editor.read(cx);
        let text = editor.text().to_string();
        let selection = editor.selected_range();
        let from = |source: &str, base: usize, span: &kerno_core::sql::StatementSpan| Statement {
            sql: span.text(source).to_string(),
            origin: Some(base + span.range.start),
        };
        if let Some(selected) = text.get(selection.clone())
            && !selected.trim().is_empty()
        {
            return split_statements(selected, rules)
                .iter()
                .map(|span| from(selected, selection.start, span))
                .collect();
        }
        let spans = split_statements(&text, rules);
        match scope {
            Scope::All => spans.iter().map(|span| from(&text, 0, span)).collect(),
            Scope::AtCursor => statement_at(&spans, editor.cursor())
                .map(|span| vec![from(&text, 0, span)])
                .unwrap_or_default(),
        }
    }

    /// Opens the editor's search panel, with its replace row when asked.
    pub fn find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            editor.focus_handle(cx).focus(window, cx);
            editor.open_search(replace, cx);
        });
    }

    /// Whether the editor's search panel is open, and with its replace row.
    #[cfg(test)]
    pub fn searching(&self, cx: &App) -> Option<bool> {
        let session = self.editor.read(cx).search_session();
        session.open.then_some(session.replace_mode)
    }

    /// Comments the lines of the selection (or the cursor's line) out, or
    /// back in. One step to undo.
    pub fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let selection = editor.selected_range();
            if text.get(selection.clone()).is_none() {
                return;
            }
            let lines = comment::lines_of(&text, selection.clone());
            let toggled = comment::toggle(&text[lines.clone()]);
            let cursor = position_of(&text, lines.start);
            let rewritten = format!("{}{toggled}{}", &text[..lines.start], &text[lines.end..]);
            editor.replace_all(rewritten, window, cx);
            editor.set_cursor_position(cursor, window, cx);
        });
    }

    /// Lays out the selection, or the whole text without one: keywords in
    /// upper case, one clause per line. One step to undo; text already
    /// laid out is left alone.
    pub fn format(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rules = self.lex_rules(cx);
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let mut range = editor.selected_range();
            let Some(selected) = text.get(range.clone()) else {
                return;
            };
            if selected.trim().is_empty() {
                range = 0..text.len();
            }
            let part = &text[range.clone()];
            let mut formatted = format_sql(part, rules);
            // A piece of the text keeps what followed it on its line.
            if range.len() < text.len() && !part.ends_with('\n') && formatted.ends_with('\n') {
                formatted.pop();
            }
            if formatted == part {
                return;
            }
            let rewritten = format!("{}{formatted}{}", &text[..range.start], &text[range.end..]);
            editor.replace_all(rewritten, window, cx);
            let cursor = editor.text().offset_to_position(range.start);
            editor.set_cursor_position(cursor, window, cx);
        });
    }

    /// Stops what is running or waiting to be paged, keeping what arrived.
    fn stop(&mut self, cx: &mut Context<Self>) {
        // Dropping the run drops the execution, which stops the statement.
        if self.run.take().is_some() {
            for pane in &mut self.results {
                if let Outcome::Rows(grid) = &pane.outcome {
                    grid.update(cx, |grid, cx| {
                        grid.delegate_mut().finish(false);
                        // A result that stopped arriving can be sorted: its
                        // columns are described again.
                        grid.refresh(cx);
                        cx.notify();
                    });
                }
            }
        }
        self.activity = Activity::Idle;
    }

    /// Asks the server to cancel the running statement. It ends with the
    /// server's cancellation error and keeps the rows that arrived.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.activity, Activity::Running { .. }) {
            // Nothing is being computed; a result waiting to be paged is
            // simply let go.
            self.stop(cx);
            cx.notify();
            return;
        }
        if let Some(session) = self.session.clone() {
            self.cancel_requested = true;
            bridge::spawn(cx, async move { session.cancel().await }).detach();
        }
    }

    /// Runs the statements of `scope`, one after the other, stopping at
    /// the first that fails.
    pub fn run(&mut self, scope: Scope, window: &mut Window, cx: &mut Context<Self>) {
        self.launch(scope, None, window, cx);
    }

    /// Shows the plan of the statement at the cursor. With `analyze` the
    /// statement is run and the plan carries what really happened.
    pub fn explain(&mut self, analyze: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.launch(Scope::AtCursor, Some(analyze), window, cx);
    }

    fn launch(
        &mut self,
        scope: Scope,
        explain: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut statements = self.statements(scope, cx);
        if explain.is_some() {
            // One plan at a time: the first statement of a selection.
            statements.truncate(1);
        }
        self.explain_mode = explain;
        if statements.is_empty() {
            return;
        }
        self.clear_error_mark(cx);
        // Parameters are asked for before anything runs.
        let rules = self.lex_rules(cx);
        let params: Vec<Vec<Param>> = statements
            .iter()
            .map(|statement| find_params(&statement.sql, rules))
            .collect();
        if params.iter().all(Vec::is_empty) {
            self.execute(statements, window, cx);
        } else {
            self.ask_params(statements, params, window, cx);
        }
    }

    /// Asks for the value of each parameter, then runs the statements with
    /// the values written in.
    fn ask_params(
        &mut self,
        statements: Vec<Statement>,
        params: Vec<Vec<Param>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(meta) = self
            .target
            .as_ref()
            .and_then(|target| self.connections.read(cx).meta_session(&target.connection))
        else {
            // No connection to run on: the run says so.
            self.execute(statements, window, cx);
            return;
        };
        // A named or numbered parameter is one value for the whole run; a
        // `?` is its own, in its own statement.
        let several = statements.len() > 1;
        let label = move |statement: usize, key: &ParamKey| match key {
            ParamKey::Positional(_) if several => {
                format!("{} · statement {}", key.label(), statement + 1)
            }
            key => key.label(),
        };
        let mut labels: Vec<String> = Vec::new();
        for (statement, params) in params.iter().enumerate() {
            for param in params {
                let label = label(statement, &param.key);
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
        }
        let remembered = labels
            .iter()
            .map(|label| self.param_values.get(label).cloned().unwrap_or_default())
            .collect();
        let view = cx.entity();
        params::open(
            labels.clone(),
            remembered,
            window,
            cx,
            move |values, window, cx| {
                let bound: Vec<Statement> = statements
                    .iter()
                    .zip(&params)
                    .enumerate()
                    .map(|(index, (statement, params))| Statement {
                        sql: bind_params(&statement.sql, params, |key| {
                            let position = labels
                                .iter()
                                .position(|known| *known == label(index, key))?;
                            Some(values[position].to_sql(meta.dialect()))
                        }),
                        // Positions the server reports are in the text it got,
                        // not in the editor's.
                        origin: if params.is_empty() {
                            statement.origin
                        } else {
                            None
                        },
                    })
                    .collect();
                let typed: Vec<(String, String)> = labels
                    .iter()
                    .zip(&values)
                    .filter_map(|(label, value)| match value {
                        ParamValue::Auto(text) => Some((label.clone(), text.clone())),
                        _ => None,
                    })
                    .collect();
                view.update(cx, |this, cx| {
                    this.param_values.extend(typed);
                    this.execute(bound, window, cx);
                });
                // Back to the editor once the prompt is gone, so the
                // keyboard keeps working without a click.
                let editor = view.read(cx).focus_handle(cx);
                window.defer(cx, move |window, cx| editor.focus(window, cx));
            },
        );
    }

    /// Runs `statements` one after the other, stopping at the first that
    /// fails.
    fn execute(&mut self, statements: Vec<Statement>, window: &mut Window, cx: &mut Context<Self>) {
        let rules = self.lex_rules(cx);
        let connections = self.connections.read(cx);
        let saved = self
            .target
            .as_ref()
            .and_then(|target| connections.get(&target.connection));
        let meta = self
            .target
            .as_ref()
            .and_then(|target| connections.meta_session(&target.connection));
        let (Some(saved), Some(meta)) = (saved, meta) else {
            // Nothing to run on: the run itself says so.
            self.start(statements, false, window, cx);
            return;
        };
        // What will be sent decides: EXPLAIN ANALYZE runs the statement.
        let explain = self.explain_mode;
        let classes: Vec<StatementClass> = statements
            .iter()
            .map(|statement| match explain {
                Some(analyze) => classify(&meta.dialect().explain(&statement.sql, analyze), rules),
                None => classify(&statement.sql, rules),
            })
            .collect();
        let name = saved.config.name.clone();
        if saved.config.read_only
            && let Some(blocked) = classes.iter().position(|class| class.writes)
        {
            self.refuse_read_only(&statements[blocked].sql, &name, cx);
            return;
        }
        // Statements that destroy something are shown and confirmed first;
        // on a production connection, so is anything that writes.
        let production = saved.config.environment == Environment::Production;
        let ask_destructive = AppState::get(cx).settings.confirm_destructive;
        let to_confirm: Vec<&Statement> = statements
            .iter()
            .zip(&classes)
            .filter(|(_, class)| {
                (class.destructive && ask_destructive) || (production && class.writes)
            })
            .map(|(statement, _)| statement)
            .collect();
        if to_confirm.is_empty() {
            self.start(statements, false, window, cx);
            return;
        }
        let destructive = classes.iter().any(|class| class.destructive);
        let confirmation = Confirmation {
            title: match to_confirm.len() {
                1 => "Run this statement?".to_string(),
                count => format!("Run these {count} statements?"),
            },
            explanation: if destructive {
                "It removes data or objects for good (a DROP, a TRUNCATE, an ALTER that drops, or a DELETE \
                 or UPDATE without a WHERE). This cannot be undone."
                    .to_string()
            } else {
                "It changes the database.".to_string()
            },
            statement: ConfirmStatement::Fixed(
                to_confirm
                    .iter()
                    .map(|statement| statement.sql.as_str())
                    .collect::<Vec<_>>()
                    .join(";\n\n"),
            ),
            action: "Run",
            destructive,
            connection: name,
            environment: saved.config.environment,
        };
        let view = cx.entity();
        sql_confirm::open(confirmation, window, cx, move |_, window, cx| {
            let statements = statements.clone();
            view.update(cx, |this, cx| this.start(statements, false, window, cx));
            let editor = view.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| editor.focus(window, cx));
        });
    }

    /// Shows why a statement was not run on a read-only connection.
    fn refuse_read_only(&mut self, statement: &str, connection: &str, cx: &mut Context<Self>) {
        self.stop(cx);
        self.results.clear();
        self.grid_subscriptions.clear();
        self.notices.clear();
        self.results.push(ResultPane {
            statement: statement.to_string(),
            outcome: Outcome::Failed(Failure {
                message: format!(
                    "“{connection}” is a read-only connection: this statement would change the database and was not run."
                ),
                code: None,
                detail: None,
                hint: Some(
                    "Untick “Read-only” in the connection's settings to allow changes.".into(),
                ),
                position: None,
            }),
            rows_affected: None,
            elapsed: None,
        });
        self.active_result = 0;
        cx.notify();
    }

    /// Runs the whole text, the script of a source, once its SQL was
    /// shown and confirmed. Parameters are not asked for: a source is
    /// sent as it is written. Whatever came of it, the window is told.
    pub fn apply_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.source.is_none() || matches!(self.activity, Activity::Running { .. }) {
            return;
        }
        let rules = self.lex_rules(cx);
        let text = self.sql(cx);
        let statements: Vec<Statement> = split_statements(&text, rules)
            .iter()
            .map(|span| Statement {
                sql: span.text(&text).to_string(),
                origin: Some(span.range.start),
            })
            .collect();
        if statements.is_empty() {
            return;
        }
        self.explain_mode = None;
        self.clear_error_mark(cx);
        let connections = self.connections.read(cx);
        let Some(saved) = self
            .target
            .as_ref()
            .and_then(|target| connections.get(&target.connection))
        else {
            // Nothing to run on: the run itself says so.
            self.start(statements, true, window, cx);
            return;
        };
        let (name, environment) = (saved.config.name.clone(), saved.config.environment);
        let classes: Vec<StatementClass> = statements
            .iter()
            .map(|statement| classify(&statement.sql, rules))
            .collect();
        if saved.config.read_only {
            let blocked = classes.iter().position(|class| class.writes).unwrap_or(0);
            self.refuse_read_only(&statements[blocked].sql, &name, cx);
            return;
        }
        let destructive = classes.iter().any(|class| class.destructive);
        let in_transaction = classes
            .iter()
            .any(|class| class.kind == StatementKind::Transaction(TxCommand::Begin));
        let confirmation = Confirmation {
            title: format!("Apply the source of “{}”?", self.title),
            explanation: match (destructive, in_transaction) {
                (true, true) => "The object is dropped, then created from this definition, in one \
                                 transaction: if the definition fails, the old object stays."
                    .to_string(),
                (true, false) => {
                    "The object is dropped, then created from this definition. If the \
                                  definition fails, the object stays dropped until it is fixed and \
                                  applied again."
                        .to_string()
                }
                _ => "The object is replaced by this definition.".to_string(),
            },
            statement: ConfirmStatement::Fixed(
                statements
                    .iter()
                    .map(|statement| statement.sql.as_str())
                    .collect::<Vec<_>>()
                    .join(";\n\n"),
            ),
            action: "Apply source",
            destructive,
            connection: name,
            environment,
        };
        let view = cx.entity();
        sql_confirm::open(confirmation, window, cx, move |_, window, cx| {
            let statements = statements.clone();
            view.update(cx, |this, cx| this.start(statements, true, window, cx));
            let editor = view.read(cx).focus_handle(cx);
            window.defer(cx, move |window, cx| editor.focus(window, cx));
        });
    }

    /// Runs the statements, guards passed. `applying` a source: the window
    /// is told once it ran.
    fn start(
        &mut self,
        statements: Vec<Statement>,
        applying: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop(cx);
        self.generation += 1;
        self.results.clear();
        self.grid_subscriptions.clear();
        self.notices.clear();
        self.active_result = 0;
        self.cancel_requested = false;
        let total = statements.len();
        self.activity = Activity::Running {
            index: 0,
            total,
            since: Instant::now(),
        };
        let page = u64::from(AppState::get(cx).settings.page_size).max(1);
        let manual = self.manual_commit;
        let explain = self.explain_mode;
        let rules = self.lex_rules(cx);
        let session = self.session(cx);
        self.run = Some(cx.spawn_in(window, async move |this, cx| {
            let session = match session.await {
                Ok(session) => session,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.results.push(ResultPane {
                            statement: String::new(),
                            outcome: Outcome::Failed(Failure::from(&error)),
                            rows_affected: None,
                            elapsed: None,
                        });
                        this.activity = Activity::Idle;
                        cx.notify();
                    });
                    return;
                }
            };
            // In manual-commit mode the first statement opens a transaction.
            if manual && session.tx_state() == TxState::Idle {
                let begin = if session.dialect().has_schemas() {
                    "BEGIN"
                } else {
                    "START TRANSACTION"
                };
                let Ok(beginning) =
                    cx.update(|_, cx| bridge::spawn(cx, exec::collect(session.clone(), begin)))
                else {
                    return;
                };
                if let Err(error) = beginning.await {
                    let _ = this.update(cx, |this, cx| {
                        this.results.push(ResultPane {
                            statement: begin.to_string(),
                            outcome: Outcome::Failed(Failure::from(&error)),
                            rows_affected: None,
                            elapsed: None,
                        });
                        this.activity = Activity::Idle;
                        this.run = None;
                        cx.notify();
                    });
                    return;
                }
            }
            let mut changed_schema = false;
            let mut any_failed = false;
            for (index, statement) in statements.into_iter().enumerate() {
                let started = Instant::now();
                let started_at_ms = kerno_core::storage::now_ms();
                // How the statement ended, for the history and the editor.
                let mut ended: Option<(HistoryStatus, Option<Failure>)> = None;
                let mut row_count: Option<u64> = None;
                let kind = classify(&statement.sql, rules).kind;
                // Only statements that change rows have a count worth showing.
                let counts_rows = matches!(
                    kind,
                    StatementKind::Insert
                        | StatementKind::Update
                        | StatementKind::Delete
                        | StatementKind::Merge
                        | StatementKind::Copy
                );
                // The pane the statement's rows went to, for its plan.
                let mut rows_pane: Option<usize> = None;
                let _ = this.update(cx, |this, cx| {
                    this.activity = Activity::Running {
                        index,
                        total,
                        since: started,
                    };
                    cx.notify();
                });
                // A plan is asked for by wrapping the statement.
                let sent = match explain {
                    Some(analyze) => session.dialect().explain(&statement.sql, analyze),
                    None => statement.sql.clone(),
                };
                let starting = {
                    let (session, sql) = (session.clone(), sent.clone());
                    cx.update(|_, cx| {
                        // `exec::run` spawns the statement: it needs the runtime.
                        bridge::spawn(
                            cx,
                            async move { exec::run(session, sql, RowLimit::Rows(page)) },
                        )
                    })
                };
                let Ok(starting) = starting else { return };
                let mut execution = starting.await;
                let demand = execution.demand();
                // The pane of the result set being received.
                let mut pane: Option<usize> = None;
                let mut failed = false;
                while let Some(event) = execution.next().await {
                    let last = index + 1 == total;
                    let keep_going = this.update_in(cx, |this, window, cx| {
                        match event {
                            ExecEvent::Columns(columns) => {
                                let grid = Grid::new(
                                    ResultStore::new(columns),
                                    demand.clone(),
                                    page,
                                    session.clone(),
                                );
                                pane = Some(this.open_grid(&statement.sql, grid, window, cx));
                                rows_pane = pane;
                            }
                            ExecEvent::Rows(batch) => {
                                if let Some(grid) = pane.and_then(|pane| this.results[pane].grid())
                                {
                                    let paused = grid.update(cx, |grid, cx| {
                                        grid.delegate_mut().push(batch);
                                        cx.notify();
                                        grid.delegate().has_more()
                                    });
                                    if paused {
                                        // The rows asked for are here: the tab is not
                                        // waiting for the server any more.
                                        this.activity = Activity::Idle;
                                        if let Some(pane) = pane {
                                            this.results[pane].elapsed = Some(started.elapsed());
                                        }
                                        // In a script the next statement needs the
                                        // session: this one stops at its first page.
                                        if !last {
                                            grid.update(cx, |grid, cx| {
                                                grid.delegate_mut().finish(false);
                                                grid.refresh(cx);
                                            });
                                            cx.notify();
                                            return false;
                                        }
                                    }
                                }
                            }
                            ExecEvent::Complete { rows_affected } => match pane.take() {
                                Some(pane) => {
                                    if let Outcome::Rows(grid) = &this.results[pane].outcome {
                                        grid.update(cx, |grid, cx| {
                                            grid.delegate_mut().finish(true);
                                            grid.refresh(cx);
                                            cx.notify();
                                        });
                                    }
                                    this.results[pane].rows_affected = rows_affected;
                                    this.results[pane].elapsed = Some(started.elapsed());
                                    row_count = rows_affected.or_else(|| {
                                        this.results[pane]
                                            .grid()
                                            .map(|grid| grid.read(cx).delegate().row_count() as u64)
                                    });
                                }
                                None => {
                                    row_count = rows_affected;
                                    this.results.push(ResultPane {
                                        statement: statement.sql.clone(),
                                        outcome: Outcome::Done,
                                        rows_affected: rows_affected.filter(|_| counts_rows),
                                        elapsed: Some(started.elapsed()),
                                    })
                                }
                            },
                            ExecEvent::Notice(notice) => this.notices.push(notice),
                            ExecEvent::Finished(Ok(())) => {
                                // Some statements end quietly when cancelled
                                // (MySQL's SLEEP returns early): it was still a
                                // cancellation, and the script stops there.
                                if std::mem::take(&mut this.cancel_requested) {
                                    failed = true;
                                    ended = Some((HistoryStatus::Cancelled, None));
                                    this.results.push(ResultPane {
                                        statement: statement.sql.clone(),
                                        outcome: Outcome::Cancelled,
                                        rows_affected: None,
                                        elapsed: Some(started.elapsed()),
                                    });
                                    this.active_result = this.results.len() - 1;
                                }
                            }
                            ExecEvent::Finished(Err(error)) => {
                                failed = true;
                                // Rows that arrived before the failure stay; a
                                // result without any goes.
                                if let Some(pane) = pane
                                    && let Some(grid) = this.results[pane].grid()
                                {
                                    let empty = grid.update(cx, |grid, cx| {
                                        grid.delegate_mut().finish(false);
                                        grid.refresh(cx);
                                        grid.delegate().row_count() == 0
                                    });
                                    if empty {
                                        this.results.remove(pane);
                                    }
                                }
                                // The server words a cancellation its own way
                                // (an error on PostgreSQL); the tab knows it asked.
                                let cancelled = std::mem::take(&mut this.cancel_requested)
                                    || matches!(error, Error::Cancelled);
                                ended = Some(if cancelled {
                                    (HistoryStatus::Cancelled, None)
                                } else {
                                    (HistoryStatus::Error, Some(Failure::from(&error)))
                                });
                                this.results.push(ResultPane {
                                    statement: statement.sql.clone(),
                                    outcome: if cancelled {
                                        Outcome::Cancelled
                                    } else {
                                        Outcome::Failed(Failure::from(&error))
                                    },
                                    rows_affected: None,
                                    elapsed: Some(started.elapsed()),
                                });
                                this.active_result = this.results.len() - 1;
                            }
                        }
                        cx.notify();
                        true
                    });
                    if !matches!(keep_going, Ok(true)) {
                        break;
                    }
                }
                if explain.is_some()
                    && let Some(pane) = rows_pane
                {
                    let dialect_of = session.clone();
                    let _ = this.update(cx, |this, cx| {
                        this.show_plan(pane, dialect_of.dialect(), explain == Some(true), cx)
                    });
                }
                let (status, failure) = ended.unwrap_or((HistoryStatus::Ok, None));
                // A table or view created, changed or dropped changes what
                // completion offers.
                changed_schema |= kind == StatementKind::Ddl
                    && explain.is_none()
                    && matches!(status, HistoryStatus::Ok);
                let _ = this.update(cx, |this, cx| {
                    let ended = Ended {
                        started_at_ms,
                        elapsed: started.elapsed(),
                        status,
                        failure,
                        // The lines of a plan are not rows of data.
                        row_count: row_count.filter(|_| explain.is_none()),
                    };
                    // The history holds what was sent: an EXPLAIN is one.
                    let recorded = Statement {
                        sql: sent.clone(),
                        origin: statement.origin.filter(|_| explain.is_none()),
                    };
                    this.statement_ended(&recorded, ended, cx)
                });
                if failed {
                    any_failed = true;
                    break;
                }
            }
            // A source that failed inside its own transaction leaves nothing
            // half done, and no lock held: the transaction is rolled back.
            if applying && any_failed && !manual && session.tx_state() == TxState::Failed {
                let rolling = cx
                    .update(|_, cx| bridge::spawn(cx, exec::collect(session.clone(), "ROLLBACK")));
                if let Ok(rolling) = rolling
                    && let Err(error) = rolling.await
                {
                    tracing::warn!(%error, "the failed source was not rolled back");
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.activity = Activity::Idle;
                this.run = None;
                if changed_schema {
                    this.load_catalog(cx);
                }
                if applying {
                    cx.emit(QueryTabEvent::SourceApplied);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Records a statement in the history and, when the server said where
    /// it failed, marks the place in the editor.
    fn statement_ended(&mut self, statement: &Statement, ended: Ended, cx: &mut Context<Self>) {
        let Ended {
            started_at_ms,
            elapsed,
            status,
            failure,
            row_count,
        } = ended;
        if let Some(target) = &self.target {
            let entry = HistoryEntry {
                id: 0,
                connection_id: target.connection.clone(),
                database: target.database.clone(),
                sql: statement.sql.clone(),
                started_at_ms,
                duration_ms: elapsed.as_millis() as i64,
                status,
                row_count: row_count.map(|rows| rows as i64),
                error: failure.as_ref().map(|failure| failure.message.clone()),
            };
            let (storage, rules) = (AppState::get(cx).storage.clone(), self.lex_rules(cx));
            bridge::spawn_blocking(cx, move || {
                if let Err(error) = storage.add_history(&entry, rules) {
                    tracing::warn!(%error, "the statement was not added to the history");
                }
            })
            .detach();
        }
        if let (Some(origin), Some(failure)) = (statement.origin, &failure)
            && let Some(position) = failure.position
        {
            self.mark_error(origin, &statement.sql, position, &failure.message, cx);
        }
    }

    /// Underlines the place of an error in the editor. `position` is the
    /// 1-based character the server pointed at in `statement`, which starts
    /// at byte `origin` of the editor's text.
    fn mark_error(
        &mut self,
        origin: usize,
        statement: &str,
        position: u32,
        message: &str,
        cx: &mut Context<Self>,
    ) {
        let at = statement
            .char_indices()
            .nth(position.saturating_sub(1) as usize)
            .map_or(statement.len(), |(byte, _)| byte);
        // The word (or single character) the server pointed at.
        let word = statement[at..]
            .char_indices()
            .find(|(_, c)| !(c.is_alphanumeric() || *c == '_'))
            .map_or(statement.len() - at, |(end, _)| end)
            .max(statement[at..].chars().next().map_or(0, char::len_utf8));
        let message = SharedString::from(message.to_string());
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().to_string();
            let (start, end) = (origin + at, (origin + at + word).min(text.len()));
            // The text changed since the run started: nothing sure to mark.
            if text.get(origin..origin + statement.len()) != Some(statement) {
                return;
            }
            let range = position_of(&text, start)..position_of(&text, end);
            if let Some(diagnostics) = editor.diagnostics_mut() {
                diagnostics.clear();
                diagnostics.push(Diagnostic::new(range, message));
            }
            cx.notify();
        });
        self.error_marked = true;
    }

    fn clear_error_mark(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.error_marked) {
            self.editor.update(cx, |editor, cx| {
                if let Some(diagnostics) = editor.diagnostics_mut() {
                    diagnostics.clear();
                }
                cx.notify();
            });
        }
    }

    /// Whether statements run in a transaction the user ends.
    pub fn manual_commit(&self) -> bool {
        self.manual_commit
    }

    /// The state of the tab's transaction.
    pub fn tx_state(&self) -> TxState {
        self.session
            .as_ref()
            .map_or(TxState::Idle, |session| session.tx_state())
    }

    /// Whether a statement is running.
    pub fn running(&self) -> bool {
        matches!(self.activity, Activity::Running { .. })
    }

    /// The databases the tab can switch to.
    pub fn databases(&self) -> &[String] {
        &self.databases
    }

    /// Runs in `database` of the same connection from now on.
    pub fn use_database(&mut self, database: String, cx: &mut Context<Self>) {
        let target = self.target.clone().map(|target| Target {
            database: Some(database),
            schema: None,
            ..target
        });
        self.set_target(target, cx)
    }

    /// What the editor's menu depends on.
    fn editor_facts(&self, cx: &App) -> EditorFacts {
        let editor = self.editor.read(cx);
        EditorFacts {
            connected: self.connected(cx),
            running: self.running(),
            selection: !editor.selected_range().is_empty(),
            empty: editor.text().len() == 0,
            saved_file: self.file.is_some() && !self.unsaved,
            source: self.source.is_some(),
        }
    }

    /// The editor's menu, built when it opens. Its entries run as their
    /// commands do: through the editor, which has the focus.
    fn editor_menu(
        &self,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.entity();
        move |menu, _, cx| {
            let entries = tabs::editor_menu(view.read(cx).editor_facts(cx));
            let view = view.clone();
            tabs::fill_menu(
                menu,
                entries,
                Rc::new(move |action, window, cx| {
                    let view = view.clone();
                    // Once the menu has given the focus back to the editor.
                    window.defer(cx, move |window, cx| {
                        run_editor_action(&view, action, window, cx)
                    });
                }),
            )
        }
    }

    /// Statements run in a transaction the user ends (`true`), or each is
    /// committed as it runs.
    pub fn set_manual_commit(&mut self, manual: bool, cx: &mut Context<Self>) {
        self.manual_commit = manual;
        cx.notify();
    }

    /// Commits or rolls back the tab's open transaction.
    pub fn end_transaction(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if session.tx_state() == TxState::Idle {
            return;
        }
        // A result still being paged belongs to the transaction.
        self.stop(cx);
        let sql = if commit { "COMMIT" } else { "ROLLBACK" };
        let ending = bridge::spawn(cx, exec::collect(session, sql));
        cx.spawn_in(window, async move |this, cx| {
            let outcome = ending.await;
            let _ = this.update_in(cx, |_, window, cx| {
                if let Err(error) = outcome {
                    window.push_notification(
                        Notification::error(SharedString::from(format!("{sql} failed: {error}"))),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Replaces the rows an EXPLAIN returned by its plan as a tree.
    fn show_plan(
        &mut self,
        pane: usize,
        dialect: &dyn Dialect,
        analyze: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(grid) = self.results.get(pane).and_then(ResultPane::grid) else {
            return;
        };
        // The output is one cell, or one line per row for text plans.
        let raw = {
            let grid = grid.read(cx).delegate();
            (0..grid.row_count())
                .filter_map(|row| grid.value_at(row, 1).and_then(|(_, value)| value))
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (plan, problem) = match dialect.parse_plan(&raw, analyze) {
            Ok(plan) => (Some(plan), None),
            Err(error) => (None, Some(error.to_string())),
        };
        self.results[pane].outcome = Outcome::Plan {
            show_raw: plan.is_none(),
            plan,
            raw,
            problem,
        };
        cx.notify();
    }

    /// Adds the pane of a result set that starts to arrive.
    fn open_grid(
        &mut self,
        statement: &str,
        grid: Grid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let table = cx.new(|cx| {
            TableState::new(grid, window, cx)
                .col_resizable(true)
                .col_movable(true)
                .sortable(true)
                .row_selectable(true)
                .col_selectable(true)
                .cell_selectable(true)
                // The grid numbers its rows itself.
                .row_header(false)
        });
        // A double click on a cell shows its whole value.
        self.grid_subscriptions.push(cx.subscribe_in(
            &table,
            window,
            |_, table, event: &TableEvent, window, cx| {
                // The grid stretches ranges from what the table has selected.
                if matches!(
                    event,
                    TableEvent::SelectCell(..)
                        | TableEvent::SelectRow(_)
                        | TableEvent::SelectColumn(_)
                        | TableEvent::ClearSelection
                ) {
                    table.update(cx, |table, _| {
                        let selection = table.selection();
                        table.delegate_mut().set_anchor(selection);
                    });
                }
                if let TableEvent::DoubleClickedCell(row, column) = event {
                    let grid = table.read(cx).delegate();
                    if let Some((column, value)) = grid.value_at(*row, *column) {
                        let (column, value) = (column.clone(), value.map(<[u8]>::to_vec));
                        viewer::open(&column, value.as_deref(), window, cx);
                    }
                }
            },
        ));
        self.results.push(ResultPane {
            statement: statement.to_string(),
            outcome: Outcome::Rows(table),
            rows_affected: None,
            elapsed: None,
        });
        self.results.len() - 1
    }

    /// Copies the selection of the result on screen (`None`: its column
    /// names). Returns whether there was something to copy.
    pub fn copy_selection(&mut self, format: Option<CopyFormat>, cx: &mut Context<Self>) -> bool {
        let Some(grid) = self
            .results
            .get(self.active_result)
            .and_then(ResultPane::grid)
        else {
            return false;
        };
        grid.update(cx, |table, cx| Grid::copy(table, format, None, cx))
    }

    /// Selects every cell of the result on screen.
    pub fn select_all_rows(&mut self, cx: &mut Context<Self>) {
        if let Some(grid) = self
            .results
            .get(self.active_result)
            .and_then(ResultPane::grid)
        {
            grid.update(cx, |table, cx| {
                table.delegate_mut().select_all();
                cx.notify();
            });
        }
    }

    // Rendering ---------------------------------------------------------------

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let connections = self.connections.read(cx);
        let saved = self
            .target
            .as_ref()
            .and_then(|target| connections.get(&target.connection));
        let connection_label = saved.map_or_else(
            || SharedString::from("Choose a connection"),
            |saved| SharedString::from(saved.config.name.clone()),
        );
        let engine = saved.map(|saved| saved.config.engine);
        let has_schemas = self
            .target
            .as_ref()
            .and_then(|target| connections.meta_session(&target.connection))
            .is_some_and(|meta| meta.dialect().has_schemas());
        let database_label = self
            .target
            .as_ref()
            .and_then(|target| {
                target.database.clone().or_else(|| {
                    connections
                        .meta_session(&target.connection)
                        .and_then(|meta| meta.database())
                })
            })
            .unwrap_or_else(|| "database".into());
        let schema_label = self
            .target
            .as_ref()
            .and_then(|target| target.schema.clone())
            .unwrap_or_else(|| "default schema".into());

        // What the menus offer, captured when they open.
        let choices: Vec<(String, String, bool)> = connections
            .saved()
            .iter()
            .map(|saved| {
                (
                    saved.id.clone(),
                    saved.config.name.clone(),
                    matches!(connections.status(&saved.id), Status::Connected { .. }),
                )
            })
            .collect();
        let (databases, schemas) = (self.databases.clone(), self.schemas.clone());
        let running = matches!(self.activity, Activity::Running { .. });
        let waiting = self.run.is_some();
        let transaction = self
            .session
            .as_ref()
            .map_or(TxState::Idle, |session| session.tx_state());
        let view = cx.entity();

        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("tab-connection")
                    .ghost()
                    .small()
                    // The engine's logo, then the name.
                    .child(
                        h_flex()
                            .gap_1p5()
                            .children(engine.map(|engine| {
                                div().flex_none().child(engine_logo(engine, px(14.), cx))
                            }))
                            .child(connection_label),
                    )
                    .dropdown_caret(true)
                    .dropdown_menu({
                        let view = view.clone();
                        move |mut menu, _, _| {
                            for (id, name, connected) in &choices {
                                let (view, id) = (view.clone(), id.clone());
                                let label = if *connected {
                                    name.clone()
                                } else {
                                    format!("{name} (not connected)")
                                };
                                menu = menu.item(
                                    PopupMenuItem::new(SharedString::from(label))
                                        .disabled(!connected)
                                        .on_click(move |_, _, cx| {
                                            view.update(cx, |this, cx| {
                                                this.set_target(
                                                    Some(Target {
                                                        connection: id.clone(),
                                                        database: None,
                                                        schema: None,
                                                    }),
                                                    cx,
                                                )
                                            })
                                        }),
                                );
                            }
                            menu
                        }
                    }),
            )
            .when(self.target.is_some(), |this| {
                this.child(
                    Button::new("tab-database")
                        .ghost()
                        .small()
                        .label(SharedString::from(database_label))
                        .dropdown_caret(true)
                        .dropdown_menu({
                            let view = view.clone();
                            move |mut menu, _, _| {
                                for database in &databases {
                                    let (view, database) = (view.clone(), database.clone());
                                    menu = menu.item(
                                        PopupMenuItem::new(SharedString::from(database.clone()))
                                            .on_click(move |_, _, cx| {
                                                view.update(cx, |this, cx| {
                                                    let target =
                                                        this.target.clone().map(|target| Target {
                                                            database: Some(database.clone()),
                                                            schema: None,
                                                            ..target
                                                        });
                                                    this.set_target(target, cx)
                                                })
                                            }),
                                    );
                                }
                                menu
                            }
                        }),
                )
            })
            .when(self.target.is_some() && has_schemas, |this| {
                this.child(
                    Button::new("tab-schema")
                        .ghost()
                        .small()
                        .label(SharedString::from(schema_label))
                        .dropdown_caret(true)
                        .dropdown_menu({
                            let view = view.clone();
                            move |mut menu, _, _| {
                                for schema in &schemas {
                                    let (view, schema) = (view.clone(), schema.clone());
                                    menu = menu.item(
                                        PopupMenuItem::new(SharedString::from(schema.clone()))
                                            .on_click(move |_, _, cx| {
                                                view.update(cx, |this, cx| {
                                                    let target =
                                                        this.target.clone().map(|target| Target {
                                                            schema: Some(schema.clone()),
                                                            ..target
                                                        });
                                                    this.set_target(target, cx)
                                                })
                                            }),
                                    );
                                }
                                menu
                            }
                        }),
                )
            })
            .child(
                Button::new("tab-commit-mode")
                    .ghost()
                    .small()
                    .label(if self.manual_commit {
                        "Manual commit"
                    } else {
                        "Auto-commit"
                    })
                    .dropdown_caret(true)
                    .dropdown_menu({
                        let view = view.clone();
                        move |menu, _, _| {
                            let (auto, manual) = (view.clone(), view.clone());
                            menu.item(
                                PopupMenuItem::new(
                                    "Auto-commit: every statement is committed as it runs",
                                )
                                .on_click(move |_, _, cx| {
                                    auto.update(cx, |this, cx| this.set_manual_commit(false, cx))
                                }),
                            )
                            .item(
                                PopupMenuItem::new(
                                    "Manual commit: statements run in a transaction you end",
                                )
                                .on_click(move |_, _, cx| {
                                    manual.update(cx, |this, cx| this.set_manual_commit(true, cx))
                                }),
                            )
                        }
                    }),
            )
            .when(transaction != TxState::Idle, |this| {
                this.child(
                    Button::new("tab-commit")
                        .outline()
                        .small()
                        .label("Commit")
                        .disabled(running || transaction == TxState::Failed)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.end_transaction(true, window, cx)
                        })),
                )
                .child(
                    Button::new("tab-rollback")
                        .outline()
                        .small()
                        .label("Rollback")
                        .disabled(running)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.end_transaction(false, window, cx)
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("tab-history")
                    .ghost()
                    .small()
                    .icon(Icon::new(Glyph::Clock))
                    .tooltip("Query history (Cmd+Y)")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(ShowHistory), cx)),
            )
            .child(
                Button::new("tab-saved")
                    .ghost()
                    .small()
                    .icon(Icon::new(Glyph::Bookmark))
                    .tooltip("Saved queries")
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ShowSavedQueries), cx)
                    }),
            )
            .when(waiting, |this| {
                this.child(
                    Button::new("tab-cancel")
                        .outline()
                        .small()
                        .label(if running { "Cancel" } else { "Stop" })
                        .tooltip("Cancel the running statement (Cmd+.)")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .child(
                Button::new("tab-explain")
                    .ghost()
                    .small()
                    .label("Explain")
                    .dropdown_caret(true)
                    .disabled(running)
                    .dropdown_menu({
                        let view = view.clone();
                        move |menu, _, _| {
                            let (plan, analyze) = (view.clone(), view.clone());
                            menu.item(
                                PopupMenuItem::new(
                                    "Explain: the plan, without running the statement",
                                )
                                .on_click(move |_, window, cx| {
                                    plan.update(cx, |this, cx| this.explain(false, window, cx))
                                }),
                            )
                            .item(
                                PopupMenuItem::new(
                                    "Explain analyze: runs the statement and times each step",
                                )
                                .on_click(move |_, window, cx| {
                                    analyze.update(cx, |this, cx| this.explain(true, window, cx))
                                }),
                            )
                        }
                    }),
            )
            .child(
                Button::new("tab-run-all")
                    .ghost()
                    .small()
                    .label("Run all")
                    .tooltip("Run every statement (Cmd+Shift+Enter)")
                    .disabled(running)
                    .on_click(cx.listener(|this, _, window, cx| this.run(Scope::All, window, cx))),
            )
            .when(self.source.is_some(), |this| {
                this.child(
                    Button::new("tab-apply-source")
                        .primary()
                        .small()
                        .label("Apply source")
                        .tooltip("Run the whole script, which replaces the object, once its SQL is confirmed")
                        .disabled(running)
                        .on_click(cx.listener(|this, _, window, cx| this.apply_source(window, cx))),
                )
            })
            .child(
                Button::new("tab-run")
                    .map(|button| {
                        // In source mode, applying is the main action.
                        if self.source.is_some() {
                            button.ghost()
                        } else {
                            button.primary()
                        }
                    })
                    .small()
                    .label("Run")
                    .tooltip("Run the statement at the cursor, or the selection (Cmd+Enter)")
                    .disabled(running)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.run(Scope::AtCursor, window, cx)),
                    ),
            )
    }

    fn render_pane(&self, pane: &ResultPane, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        match &pane.outcome {
            Outcome::Rows(grid) => DataTable::new(grid)
                .xsmall()
                .stripe(true)
                .into_any_element(),
            Outcome::Done => div()
                .id("result-done")
                .test_support()
                .p_3()
                .text_sm()
                .child(SharedString::from(match pane.rows_affected {
                    Some(1) => "Done. 1 row affected.".to_string(),
                    Some(rows) => format!("Done. {rows} rows affected."),
                    None => "Done.".to_string(),
                }))
                .into_any_element(),
            Outcome::Plan {
                plan,
                raw,
                problem,
                show_raw,
            } => {
                let show_raw = *show_raw || plan.is_none();
                let view = |raw_view: bool, id: &'static str, label: &'static str| {
                    Button::new(id)
                        .ghost()
                        .xsmall()
                        .label(label)
                        .selected(show_raw == raw_view)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let active = this.active_result;
                            if let Some(Outcome::Plan { show_raw, .. }) =
                                this.results.get_mut(active).map(|pane| &mut pane.outcome)
                            {
                                *show_raw = raw_view;
                            }
                            cx.notify();
                        }))
                };
                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .flex_none()
                            .px_2()
                            .py_1()
                            .gap_1()
                            .child(view(false, "plan-as-tree", "Plan").disabled(plan.is_none()))
                            .child(view(true, "plan-as-raw", "Raw output"))
                            .children(problem.clone().map(|problem| {
                                div().pl_2().text_xs().text_color(theme.warning).child(
                                    SharedString::from(format!(
                                        "The plan could not be read as a tree: {problem}"
                                    )),
                                )
                            })),
                    )
                    .child(
                        div().flex_1().min_h_0().child(match plan {
                            Some(plan) if !show_raw => plan_view::render(plan, cx),
                            _ => div()
                                .id("plan-raw")
                                .test_support()
                                .size_full()
                                .p_3()
                                .overflow_y_scroll()
                                .font_family(theme.mono_font_family.clone())
                                .text_sm()
                                .child(SharedString::from(raw.clone()))
                                .into_any_element(),
                        }),
                    )
                    .into_any_element()
            }
            Outcome::Cancelled => div()
                .id("result-cancelled")
                .test_support()
                .p_3()
                .text_sm()
                .text_color(theme.warning)
                .child("Cancelled. Rows that had arrived are kept in their result.")
                .into_any_element(),
            Outcome::Failed(failure) => v_flex()
                .id("result-error")
                .test_support()
                .p_3()
                .gap_1()
                .text_sm()
                .child(div().text_color(theme.danger).child(SharedString::from(
                    match &failure.code {
                        Some(code) => format!("{} [{code}]", failure.message),
                        None => failure.message.clone(),
                    },
                )))
                .children(
                    failure
                        .detail
                        .clone()
                        .map(|detail| div().child(SharedString::from(format!("Detail: {detail}")))),
                )
                .children(
                    failure
                        .hint
                        .clone()
                        .map(|hint| div().child(SharedString::from(format!("Hint: {hint}")))),
                )
                .into_any_element(),
        }
    }

    /// What the server said on the side during the last run.
    fn render_messages(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("result-messages")
            .test_support()
            .flex_1()
            .min_h_0()
            .p_3()
            .gap_2()
            .text_sm()
            .children(self.notices.iter().map(|notice| {
                let warning = notice.severity.to_uppercase().contains("WARN");
                v_flex()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(if warning {
                                        theme.warning
                                    } else {
                                        theme.muted_foreground
                                    })
                                    .child(SharedString::from(notice.severity.to_uppercase())),
                            )
                            .child(SharedString::from(match &notice.code {
                                Some(code) => format!("{} [{code}]", notice.message),
                                None => notice.message.clone(),
                            })),
                    )
                    .children(notice.detail.clone().map(|detail| {
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(detail))
                    }))
            }))
    }

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (border, muted, mono) = {
            let theme = cx.theme();
            (
                theme.border,
                theme.muted_foreground,
                theme.mono_font_family.clone(),
            )
        };
        // The server's messages get a tab of their own after the results.
        let messages = !self.notices.is_empty();
        let tab_count = self.results.len() + usize::from(messages);
        let active = self.active_result.min(tab_count.saturating_sub(1));
        let tabs = TabBar::new("results")
            .selected_index(active)
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                this.active_result = *index;
                cx.notify();
            }))
            .children(self.results.iter().enumerate().map(|(index, pane)| {
                Tab::new().label(SharedString::from(match &pane.outcome {
                    Outcome::Failed(_) => format!("Error {}", index + 1),
                    Outcome::Plan { .. } => format!("Plan {}", index + 1),
                    _ => format!("Result {}", index + 1),
                }))
            }))
            .children(messages.then(|| {
                Tab::new().label(SharedString::from(format!(
                    "Messages ({})",
                    self.notices.len()
                )))
            }));
        let Some(pane) = self.results.get(active) else {
            if messages {
                return v_flex()
                    .size_full()
                    .child(tabs)
                    .child(self.render_messages(cx));
            }
            let text = match self.activity {
                Activity::Running { .. } => "Running…",
                Activity::Idle => "Run a statement to see its result here.",
            };
            return v_flex()
                .size_full()
                .child(div().p_3().text_sm().text_color(muted).child(text));
        };
        // What the footer says about the result on screen.
        let (summary, has_more) = match &pane.outcome {
            Outcome::Rows(grid) => {
                let grid = grid.read(cx).delegate();
                let rows = grid.row_count();
                let more = grid.has_more();
                let text = if grid.is_complete() {
                    format!("{rows} row{}", if rows == 1 { "" } else { "s" })
                } else if more {
                    format!("first {rows} rows; more on the server")
                } else {
                    format!("{rows} rows so far")
                };
                (text, more)
            }
            _ => (String::new(), false),
        };
        let elapsed = pane.elapsed.map(duration_text);
        let grid = match &pane.outcome {
            Outcome::Rows(grid) => Some(grid.clone()),
            _ => None,
        };
        let exportable = self.exportable(pane, cx);
        v_flex()
            .size_full()
            .child(tabs)
            .child(div().flex_1().min_h_0().child(self.render_pane(pane, cx)))
            .child(
                h_flex()
                    .id("result-footer")
                    .test_support()
                    .flex_none()
                    .h(px(26.))
                    .px_3()
                    .gap_3()
                    .border_t_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(summary))
                    .when_some(grid.filter(|_| has_more), |this, grid| {
                        let all = grid.clone();
                        this.child(
                            Button::new("fetch-more")
                                .ghost()
                                .xsmall()
                                .label("Fetch more")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.activity = Activity::Running {
                                        index: 0,
                                        total: 1,
                                        since: Instant::now(),
                                    };
                                    grid.update(cx, |grid, cx| {
                                        grid.delegate_mut().fetch_more();
                                        cx.notify();
                                    });
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("fetch-all")
                                .ghost()
                                .xsmall()
                                .label("Fetch all")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.activity = Activity::Running {
                                        index: 0,
                                        total: 1,
                                        since: Instant::now(),
                                    };
                                    all.update(cx, |grid, cx| {
                                        grid.delegate_mut().fetch_all();
                                        cx.notify();
                                    });
                                    cx.notify();
                                })),
                        )
                    })
                    .when(exportable, |this| {
                        this.child(
                            Button::new("result-export")
                                .ghost()
                                .xsmall()
                                .label("Export…")
                                .tooltip("Run the statement again and write all its rows to a file")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.export_result(window, cx);
                                })),
                        )
                    })
                    // Which statement this is the result of.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_right()
                            .font_family(mono)
                            .child(SharedString::from(one_line(&pane.statement))),
                    )
                    .children(elapsed.map(SharedString::from)),
            )
    }
}

/// Runs an entry of the editor's menu as its command does: through the
/// editor, which has the focus.
fn run_editor_action(
    view: &Entity<QueryTab>,
    action: TabAction,
    window: &mut Window,
    cx: &mut App,
) {
    let focus = view.read(cx).focus_handle(cx);
    focus.focus(window, cx);
    let command: Box<dyn gpui_kit::Action> = match action {
        TabAction::ApplySource => {
            view.update(cx, |tab, cx| tab.apply_source(window, cx));
            return;
        }
        TabAction::RunStatement => Box::new(RunStatement),
        TabAction::RunScript => Box::new(RunAll),
        TabAction::Explain => Box::new(ExplainStatement),
        TabAction::ExplainAnalyze => Box::new(ExplainAnalyze),
        TabAction::Format => Box::new(FormatSql),
        // The editor's own actions, as its built-in menu does.
        TabAction::Undo => Box::new(input::Undo),
        TabAction::Redo => Box::new(input::Redo),
        TabAction::Cut => Box::new(input::Cut),
        TabAction::Copy => Box::new(input::Copy),
        TabAction::Paste => Box::new(input::Paste),
        TabAction::SelectAll => Box::new(input::SelectAll),
        TabAction::ToggleComment => Box::new(ToggleComment),
        TabAction::Find => Box::new(FindInEditor),
        TabAction::Replace => Box::new(ReplaceInEditor),
        TabAction::Save => Box::new(SaveFile),
        TabAction::SaveAs => Box::new(SaveFileAs),
        TabAction::SaveQuery => Box::new(SaveQuery),
        other => {
            tracing::warn!(?other, "not an editor action");
            return;
        }
    };
    focus.dispatch_action(command.as_ref(), window, cx);
}

impl Focusable for QueryTab {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl Render for QueryTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The editor's own menu is native and closed to tests: an empty one
        // keeps its cursor placement, and the drawn menu around it shows.
        let editor = div()
            .id("query-editor")
            .test_support()
            .size_full()
            .context_menu(self.editor_menu(cx))
            .child(
                Editor::new(&self.editor)
                    .h_full()
                    .bordered(false)
                    .appearance(false)
                    .context_menu(|menu, _, _| menu),
            );
        let shows_results = !self.results.is_empty() || self.run.is_some();
        v_flex()
            .id("query-tab")
            .test_support()
            .size_full()
            .child(self.render_toolbar(cx))
            .child(div().flex_1().min_h_0().map(|area| {
                if shows_results {
                    area.child(
                        v_resizable("query-split")
                            .child(resizable_panel().child(editor))
                            .child(
                                resizable_panel().child(
                                    div()
                                        .id("results")
                                        .size_full()
                                        // With the grid focused: copy what is
                                        // selected, select everything.
                                        .on_key_down(cx.listener(
                                            |this, event: &KeyDownEvent, _, cx| {
                                                let keystroke = &event.keystroke;
                                                if !keystroke.modifiers.secondary(cx)
                                                    || keystroke.modifiers.shift
                                                {
                                                    return;
                                                }
                                                match keystroke.key.as_str() {
                                                    "c" => {
                                                        if this.copy_selection(
                                                            Some(CopyFormat::Tsv),
                                                            cx,
                                                        ) {
                                                            cx.stop_propagation();
                                                        }
                                                    }
                                                    "a" => {
                                                        this.select_all_rows(cx);
                                                        cx.stop_propagation();
                                                    }
                                                    _ => {}
                                                }
                                            },
                                        ))
                                        .child(self.render_results(cx)),
                                ),
                            ),
                    )
                } else {
                    area.child(editor)
                }
            }))
    }
}
