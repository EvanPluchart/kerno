//! The extensions manager of a PostgreSQL database (K27, the owner's
//! request of 2026-11-20): every extension the server has files for, with
//! what it is for (the server's own comment), its versions, what it requires
//! and who may install it, read from the catalog; a search and filters; and
//! Install, Update, Move and Drop, each shown as its SQL and run once
//! confirmed. DBeaver's `PostgreCreateExtensionDialog` was read for
//! behaviour (a schema picker defaulting to the server's); no logic is
//! ported, and the look and words are Kerno's (D-100).

use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, ClipboardItem, Context, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent,
    MouseButton, ParentElement as _, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _, Window,
    canvas, div, px,
};
use kerno_core::schema::{Change, ExtensionInfo, ExtensionState};
use kerno_core::{Error, Session, TxState, describe, exec};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::query::tab::TabStatus;
use crate::sql_confirm::{self, Confirmation, Reach, Statement};
use crate::tabs::Target;

/// What the manager asks of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionsTabEvent {
    /// An extension was installed, updated, moved or dropped: the tree
    /// reads the database's extensions again.
    Changed,
}

/// Which extensions the list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Installed,
    Available,
    ToUpdate,
}

impl Filter {
    pub const ALL: [Filter; 4] = [
        Filter::All,
        Filter::Installed,
        Filter::Available,
        Filter::ToUpdate,
    ];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Installed => "Installed",
            Filter::Available => "Available",
            Filter::ToUpdate => "To update",
        }
    }

    fn lets(self, extension: &ExtensionInfo) -> bool {
        match (self, extension.state()) {
            (Filter::All, _) => true,
            (Filter::Installed, state) => state != ExtensionState::Available,
            (Filter::Available, state) => state == ExtensionState::Available,
            (Filter::ToUpdate, state) => state == ExtensionState::UpdateAvailable,
        }
    }
}

/// A statement of the manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// `CREATE EXTENSION`; `brings`: the requirements not installed yet,
    /// installed with it (`CASCADE`).
    Install {
        schema: Option<String>,
        version: Option<String>,
        brings: Vec<String>,
    },
    /// `ALTER EXTENSION … UPDATE`, to the default version when `None`.
    Update { version: Option<String> },
    /// `ALTER EXTENSION … SET SCHEMA`.
    Move { schema: String },
    /// `DROP EXTENSION`, with `CASCADE` or not.
    Drop { cascade: bool },
}

impl Op {
    fn failed(&self, name: &str) -> String {
        match self {
            Op::Install { .. } => format!("The extension “{name}” was not installed."),
            Op::Update { .. } => format!("The extension “{name}” was not updated."),
            Op::Move { .. } => format!("The extension “{name}” was not moved."),
            Op::Drop { .. } => format!("The extension “{name}” was not dropped."),
        }
    }
}

/// Why the server refused `op` on `extension`, in plain words first, then
/// the server's own message with its detail and hint (K9).
pub fn refusal(op: &Op, extension: &ExtensionInfo, database: &str, error: &Error) -> String {
    let name = &extension.name;
    let (code, message) = match error {
        Error::Db(db) => (db.code.as_deref(), db.message.as_str()),
        _ => (None, ""),
    };
    let plain = if code == Some("42501") {
        match op {
            Op::Install { .. } if extension.needs_superuser() => format!(
                "Only a superuser can install “{name}”: it is not a trusted extension. Ask an \
                 administrator of the server to install it, or connect as a superuser."
            ),
            Op::Install { .. } => format!(
                "This user may not install “{name}”: that takes the right to create objects in \
                 the database “{database}” (CREATE on it). Ask the database's owner to grant \
                 it, or connect as a user who has it."
            ),
            _ => format!(
                "This user may not change “{name}”: only its owner or a superuser can. Connect \
                 as one of them."
            ),
        }
    } else if code == Some("58P01")
        || message.contains("no installation script")
        || message.contains("no update path")
        || message.contains("could not open extension")
    {
        format!(
            "The server has no files for this version of “{name}”: install the extension's \
             package for that version on the server's machine first, then try again."
        )
    } else {
        op.failed(name)
    };
    format!("{plain}\n{}", describe(error))
}

/// The extensions `name` requires that are not installed, followed through
/// their own requirements (what `CASCADE` installs), in the order the
/// server would install them; names the server has no files for included.
pub fn missing_requirements(name: &str, extensions: &[ExtensionInfo]) -> Vec<String> {
    let find = |name: &str| extensions.iter().find(|extension| extension.name == name);
    let mut missing = Vec::new();
    let mut seen = BTreeSet::new();
    let mut stack: Vec<String> = find(name)
        .map(|extension| extension.requires.clone())
        .unwrap_or_default();
    while let Some(required) = stack.pop() {
        if !seen.insert(required.clone()) {
            continue;
        }
        match find(&required) {
            Some(extension) if extension.installed_version.is_some() => {}
            Some(extension) => {
                missing.push(required);
                stack.extend(extension.requires.iter().cloned());
            }
            None => missing.push(required),
        }
    }
    missing.reverse();
    missing
}

/// Who may install `extension`, in plain words, from the catalog.
pub fn who_may_install(extension: &ExtensionInfo, trusted_known: bool) -> String {
    if !extension.superuser {
        "Any user who may create objects in this database (CREATE on it) can install it.".into()
    } else if extension.trusted {
        "A trusted extension: any user who may create objects in this database (CREATE on it) \
         can install it."
            .into()
    } else if trusted_known {
        "Only a superuser can install it: it is not a trusted extension.".into()
    } else {
        "Only a superuser can install it (servers before PostgreSQL 13 have no trusted \
         extensions)."
            .into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    /// The connection is not open, or the tab is not shown yet.
    Waiting,
    Loading,
    Ready,
    Failed(String),
}

/// What the manager read of the database.
#[derive(Debug, Clone, Default)]
struct Read {
    extensions: Vec<ExtensionInfo>,
    /// The database's own schemas, the system ones left out.
    schemas: Vec<String>,
    /// Where `CREATE EXTENSION` puts what has no schema of its own.
    current_schema: Option<String>,
    /// PostgreSQL 13 or later: `trusted` is read.
    trusted_known: bool,
}

/// A statement running.
struct Running {
    doing: String,
    session: Option<Arc<dyn Session>>,
    cancelled: bool,
}

/// What a picker of the details does with the value picked.
type OnPick = dyn Fn(&mut ExtensionsTab, Option<String>, &mut Window, &mut Context<ExtensionsTab>);

/// Under this width the details go under the list instead of beside it.
const NARROW: f32 = 640.;

/// Whether a tab this wide shows the details under the list (not before it
/// is measured).
fn is_narrow(width: f32) -> bool {
    width > 0. && width < NARROW
}

/// The extensions manager of one database.
pub struct ExtensionsTab {
    focus_handle: FocusHandle,
    connections: Entity<Connections>,
    /// The connection and the database (always named).
    target: Target,
    hidden: bool,
    state: State,
    read: Read,
    search: Entity<InputState>,
    query: String,
    filter: Filter,
    selected: Option<String>,
    /// The schema and version picked for an install, or the version for an
    /// update; reset when another extension is selected.
    schema_choice: Option<String>,
    version_choice: Option<String>,
    running: Option<Running>,
    /// The outcome of the last statement: whether it failed, and its text.
    message: Option<(bool, String)>,
    stop: bridge::Stop,
    generation: u64,
    load: Option<Task<()>>,
    /// The tab's width at the last frame.
    width: Rc<Cell<f32>>,
    list_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ExtensionsTabEvent> for ExtensionsTab {}

impl ExtensionsTab {
    pub fn new(
        target: Target,
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search names and descriptions"));
        let typed = cx.subscribe(&search, |this, input, event, cx| {
            if let InputEvent::Change = event {
                this.query = input.read(cx).value().to_lowercase();
                this.keep_selection_shown();
                cx.notify();
            }
        });
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
            hidden: false,
            state: State::Waiting,
            read: Read::default(),
            search,
            query: String::new(),
            filter: Filter::All,
            selected: None,
            schema_choice: None,
            version_choice: None,
            running: None,
            message: None,
            stop: bridge::Stop::default(),
            generation: 0,
            load: None,
            width: Rc::default(),
            list_scroll: ScrollHandle::new(),
            _subscriptions: vec![typed, follow],
        };
        tab.reload(window, cx);
        tab
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn database(&self) -> &str {
        self.target.database.as_deref().unwrap_or_default()
    }

    pub fn title(&self) -> SharedString {
        SharedString::from(format!("{} · extensions", self.database()))
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
            .map(|saved| saved.config.name.clone())
            .unwrap_or_default()
    }

    fn read_only(&self, cx: &App) -> bool {
        self.connections
            .read(cx)
            .get(&self.target.connection)
            .is_some_and(|saved| saved.config.read_only)
    }

    /// What the status bar says about this tab.
    pub fn status(&self, cx: &App) -> TabStatus {
        TabStatus {
            connection: Some(self.connection_name(cx)).filter(|name| !name.is_empty()),
            read_only: self.read_only(cx),
            database: self.target.database.clone(),
            transaction: TxState::Idle,
            running: self.state == State::Loading || self.running.is_some(),
            rows: None,
            elapsed: None,
            notices: 0,
            pending: 0,
        }
    }

    // Reading -----------------------------------------------------------------

    /// Reads nothing until shown (a tab opened in another group): see
    /// `reveal`.
    pub fn defer_until_shown(&mut self, cx: &mut Context<Self>) {
        self.hidden = true;
        self.stop_loading(cx);
        self.state = State::Waiting;
        cx.notify();
    }

    /// The tab is on screen: a deferred tab reads the list now.
    pub fn reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.hidden) {
            self.reload(window, cx);
        }
    }

    fn stop_loading(&mut self, cx: &App) {
        self.generation += 1;
        self.stop.stop(cx);
        self.stop = bridge::Stop::default();
        self.load = None;
    }

    /// Stops the read in flight (Cmd+., the tab closed), on the server too
    /// (C3, K15).
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.state == State::Loading {
            self.stop_loading(cx);
            self.state = State::Failed("Cancelled: the extensions were not read.".into());
            cx.notify();
        }
        if let Some(running) = &mut self.running {
            running.cancelled = true;
            if let Some(session) = running.session.clone() {
                bridge::spawn(cx, async move {
                    if let Err(error) = session.cancel().await {
                        tracing::debug!(%error, "the statement could not be cancelled");
                    }
                })
                .detach();
            }
        }
    }

    /// Reads the extensions and the schemas again, on a session of its own
    /// in the database, closed once they are read.
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_loading(cx);
        let generation = self.generation;
        if self.hidden || !self.connected(cx) {
            self.state = State::Waiting;
            cx.notify();
            return;
        }
        self.state = State::Loading;
        let stop = self.stop.clone();
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
            let outcome = match opening.await {
                Err(error) => Some(Err(error)),
                Ok(session) => {
                    let Ok(reading) = cx.update(|_, cx| {
                        stop.run(cx, session, |session| async move {
                            let read = async {
                                let extensions = session.extensions().await?;
                                let schemas = session.schemas().await?;
                                Ok::<_, Error>(Read {
                                    extensions,
                                    schemas: schemas
                                        .into_iter()
                                        .filter(|schema| !schema.is_system)
                                        .map(|schema| schema.name)
                                        .collect(),
                                    current_schema: session.schema(),
                                    trusted_known: session.server().version_number.0 >= 13,
                                })
                            }
                            .await;
                            session.close().await;
                            read
                        })
                    }) else {
                        return;
                    };
                    reading.await
                }
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match outcome {
                    // Stopped first.
                    None | Some(Err(Error::Cancelled)) => {}
                    Some(Ok(read)) => {
                        this.read = read;
                        this.state = State::Ready;
                        if this
                            .selected
                            .as_ref()
                            .is_some_and(|name| this.extension(name).is_none())
                        {
                            this.selected = None;
                        }
                        let at = this
                            .selected
                            .as_ref()
                            .and_then(|name| this.shown().iter().position(|e| &e.name == name));
                        if let Some(at) = at {
                            this.list_scroll.scroll_to_item(at);
                        }
                    }
                    Some(Err(error)) => {
                        this.state = State::Failed(describe(&error).to_string());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    // The list ----------------------------------------------------------------

    fn extension(&self, name: &str) -> Option<&ExtensionInfo> {
        self.read
            .extensions
            .iter()
            .find(|extension| extension.name == name)
    }

    fn matches(&self, extension: &ExtensionInfo) -> bool {
        self.query.is_empty()
            || extension.name.to_lowercase().contains(&self.query)
            || extension
                .comment
                .as_deref()
                .is_some_and(|comment| comment.to_lowercase().contains(&self.query))
    }

    /// The extensions the search and the filter let through, in the
    /// server's order (installed first, then by name).
    pub fn shown(&self) -> Vec<&ExtensionInfo> {
        self.read
            .extensions
            .iter()
            .filter(|extension| self.filter.lets(extension) && self.matches(extension))
            .collect()
    }

    /// How many extensions the search lets through under each filter.
    pub fn count(&self, filter: Filter) -> usize {
        self.read
            .extensions
            .iter()
            .filter(|extension| filter.lets(extension) && self.matches(extension))
            .count()
    }

    pub fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.keep_selection_shown();
        cx.notify();
    }

    /// A selection the list no longer shows is dropped.
    fn keep_selection_shown(&mut self) {
        let shown = self
            .selected
            .as_ref()
            .is_some_and(|name| self.shown().iter().any(|extension| &extension.name == name));
        if !shown {
            self.selected = None;
        }
    }

    pub fn selected(&self) -> Option<&ExtensionInfo> {
        self.selected
            .as_deref()
            .and_then(|name| self.extension(name))
    }

    /// Selects `name`; the search and the filter make room for it.
    pub fn select(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(name) {
            self.schema_choice = None;
            self.version_choice = None;
        }
        self.selected = Some(name.to_string());
        if self.state == State::Ready && !self.shown().iter().any(|e| e.name == name) {
            self.filter = Filter::All;
            self.query.clear();
            self.search
                .update(cx, |search, cx| search.set_value("", window, cx));
            self.selected = Some(name.to_string());
        }
        if let Some(index) = self.shown().iter().position(|e| e.name == name) {
            self.list_scroll.scroll_to_item(index);
        }
        cx.notify();
    }

    /// Selects `name` once the list is read, for "Show in the extensions
    /// manager" on a tab that is opening.
    pub fn select_when_read(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(name.clone());
        if self.state == State::Ready {
            self.select(&name, window, cx);
        }
    }

    /// Moves the selection by `step` rows of the list.
    fn step_selection(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let names: Vec<String> = self.shown().iter().map(|e| e.name.clone()).collect();
        if names.is_empty() {
            return;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|name| names.iter().position(|n| n == name));
        let next = match at {
            None if step > 0 => 0,
            None => names.len() - 1,
            Some(at) => (at as isize + step).clamp(0, names.len() as isize - 1) as usize,
        };
        let name = names[next].clone();
        self.select(&name, window, cx);
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "down" => self.step_selection(1, window, cx),
            "up" => self.step_selection(-1, window, cx),
            "f" if keystroke.modifiers.secondary(cx) => {
                self.search
                    .update(cx, |search, cx| search.focus(window, cx));
            }
            "enter" if self.focus_handle.is_focused(window) => {
                if let Some(name) = self.selected.clone() {
                    self.main_action(&name, window, cx);
                }
            }
            "escape" => self.focus_handle.focus(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// What Enter does to a row: Install… what is available, Update what
    /// has an update; nothing to the rest.
    pub fn main_action(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(extension) = self.extension(name) else {
            return;
        };
        match extension.state() {
            ExtensionState::Available => self.ask_install(name, window, cx),
            ExtensionState::UpdateAvailable => {
                self.ask(name, Op::Update { version: None }, window, cx)
            }
            ExtensionState::Installed => {}
        }
    }

    // Statements --------------------------------------------------------------

    /// Why no statement can run now, when none can.
    pub fn why_not_write(&self, cx: &App) -> Option<&'static str> {
        if self.read_only(cx) {
            Some("Read-only connection: extensions cannot be installed, updated, moved or dropped.")
        } else if self.running.is_some() {
            Some("A statement of this tab is running.")
        } else if !self.connected(cx) {
            Some("Not connected.")
        } else {
            None
        }
    }

    /// The schema an install of `extension` would use: its own when its
    /// control file fixes one, else the one picked, else the server's.
    fn install_schema(&self, extension: &ExtensionInfo) -> Option<String> {
        extension
            .fixed_schema
            .clone()
            .or_else(|| self.schema_choice.clone())
            .or_else(|| self.read.current_schema.clone())
    }

    pub fn ask_install(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(extension) = self.extension(name) else {
            return;
        };
        // The SCHEMA and VERSION clauses only when they differ from what the
        // server would choose on its own.
        let schema = match &extension.fixed_schema {
            Some(_) => None,
            None => self
                .schema_choice
                .clone()
                .filter(|schema| Some(schema) != self.read.current_schema.as_ref()),
        };
        let version = self
            .version_choice
            .clone()
            .filter(|version| Some(version) != extension.default_version.as_ref());
        let brings = missing_requirements(name, &self.read.extensions);
        self.ask(
            name,
            Op::Install {
                schema,
                version,
                brings,
            },
            window,
            cx,
        );
    }

    /// Shows the SQL of `op` on `name` in the usual confirmation and runs
    /// it once confirmed; a drop lists what it reaches (K14).
    pub fn ask(&mut self, name: &str, op: Op, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(reason) = self.why_not_write(cx) {
            window.push_notification(Notification::warning(SharedString::from(reason)), cx);
            return;
        }
        let Some(extension) = self.extension(name).cloned() else {
            return;
        };
        let connections = self.connections.read(cx);
        let (Some(saved), Some(meta)) = (
            connections.get(&self.target.connection),
            connections.meta_session(&self.target.connection),
        ) else {
            return;
        };
        let environment = saved.config.environment.clone();
        let dialect = meta.dialect();
        let database = self.database().to_string();
        let sql = match &op {
            Op::Install {
                schema,
                version,
                brings,
            } => dialect.create_extension_sql(
                name,
                schema.as_deref(),
                version.as_deref(),
                !brings.is_empty(),
            ),
            Op::Update { version } => dialect.update_extension_sql(name, version.as_deref()),
            Op::Move { schema } => dialect.move_extension_sql(name, schema),
            Op::Drop { cascade } => dialect
                .drop_extension_sql(name)
                .map(|sql| if *cascade { sql + " CASCADE" } else { sql }),
        };
        let Ok(sql) = sql else {
            return;
        };
        let (title, explanation, button, destructive) = match &op {
            Op::Install { brings, .. } => {
                let schema = self.install_schema(&extension).unwrap_or_default();
                let mut explanation = format!(
                    "Its types, functions and operators are added to the database “{database}”, in \
                     the schema “{schema}”."
                );
                if !brings.is_empty() {
                    let one = brings.len() == 1;
                    explanation += &format!(
                        " It requires {}, which {}: {} installed with it (CASCADE), in that \
                         schema too unless {} control file fixes another.",
                        quoted_list(brings),
                        if one {
                            "is not installed"
                        } else {
                            "are not installed"
                        },
                        if one { "it is" } else { "they are" },
                        if one { "its" } else { "their" },
                    );
                }
                (
                    format!("Install extension “{name}”?"),
                    explanation,
                    "Install",
                    false,
                )
            }
            Op::Update { version } => {
                let to = version
                    .clone()
                    .or(extension.default_version.clone())
                    .unwrap_or_default();
                (
                    format!("Update extension “{name}” to {to}?"),
                    format!(
                        "The server runs the extension's update scripts from version {} to {to}.",
                        extension.installed_version.clone().unwrap_or_default()
                    ),
                    "Update",
                    false,
                )
            }
            Op::Move { schema } => (
                format!("Move extension “{name}” to “{schema}”?"),
                format!(
                    "Its objects move from the schema “{}” to “{schema}”: statements that name them \
                     with the old schema have to be changed.",
                    extension.schema.clone().unwrap_or_default()
                ),
                "Move",
                false,
            ),
            Op::Drop { cascade: false } => (
                format!("Drop extension “{name}”?"),
                "Its types, functions and operators are removed from the database for good. \
                 This cannot be undone."
                    .to_string(),
                "Drop",
                true,
            ),
            Op::Drop { cascade: true } => (
                format!("Drop extension “{name}” with cascade?"),
                "Its objects and every object that depends on them (columns of its types, \
                 views, …) are removed for good. This cannot be undone."
                    .to_string(),
                "Drop with cascade",
                true,
            ),
        };
        let confirmation = Confirmation {
            title,
            explanation,
            statement: Statement::Fixed(sql),
            action: button,
            destructive,
            connection: saved.config.name.clone(),
            environment,
            writes: true,
        };
        let view = cx.entity();
        let run_name = name.to_string();
        let dropping = match op {
            Op::Drop { cascade } => Some(cascade),
            _ => None,
        };
        let run = move |sql: String, window: &mut Window, cx: &mut App| {
            let (name, op) = (run_name.clone(), op.clone());
            view.update(cx, |this, cx| this.execute(name, op, sql, window, cx));
        };
        if let Some(cascade) = dropping {
            let reach = Reach {
                reading: crate::navigator::view::read_dependents(
                    &self.connections,
                    &self.target.connection,
                    self.target.database.clone(),
                    Change::DropExtension {
                        name: name.to_string(),
                        cascade,
                    },
                    cx,
                ),
                empty: "Nothing else depends on it.".into(),
                waits: cascade,
            };
            sql_confirm::open_with_reach(confirmation, reach, window, cx, run);
        } else {
            sql_confirm::open(confirmation, window, cx, run);
        }
    }

    /// Runs a confirmed statement on a session of its own in the database,
    /// closed once it ends; Cancel (the tab's, Cmd+.) stops it on the server.
    fn execute(
        &mut self,
        name: String,
        op: Op,
        sql: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(extension) = self.extension(&name).cloned() else {
            return;
        };
        self.message = None;
        let doing = match &op {
            Op::Install { .. } => format!("Installing {name}…"),
            Op::Update { .. } => format!("Updating {name}…"),
            Op::Move { .. } => format!("Moving {name}…"),
            Op::Drop { .. } => format!("Dropping {name}…"),
        };
        self.running = Some(Running {
            doing,
            session: None,
            cancelled: false,
        });
        let opening = self.connections.read(cx).open_session(
            &self.target.connection,
            self.target.database.clone(),
            cx,
        );
        let database = self.database().to_string();
        cx.spawn_in(window, async move |this, cx| {
            let outcome = match opening.await {
                Err(error) => Err(error),
                Ok(session) => {
                    // A Cancel that came while it opened: nothing is sent.
                    let go_on = this.update(cx, |this, _| match &mut this.running {
                        Some(running) if !running.cancelled => {
                            running.session = Some(session.clone());
                            true
                        }
                        _ => false,
                    });
                    let go_on = matches!(go_on, Ok(true));
                    let Ok(running) = cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            let outcome = if go_on {
                                exec::collect(session.clone(), sql).await.map(drop)
                            } else {
                                Err(Error::Cancelled)
                            };
                            session.close().await;
                            outcome
                        })
                    }) else {
                        return;
                    };
                    running.await
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let cancelled = this.running.take().is_some_and(|running| running.cancelled);
                let (failed, text) = match outcome {
                    Ok(()) => (
                        false,
                        match &op {
                            Op::Install { brings, .. } if !brings.is_empty() => {
                                format!("Installed {name}, with {}.", brings.join(", "))
                            }
                            Op::Install { .. } => format!("Installed {name}."),
                            Op::Update { .. } => format!("Updated {name}."),
                            Op::Move { schema } => format!("Moved {name} to {schema}."),
                            Op::Drop { .. } => format!("Dropped {name}."),
                        },
                    ),
                    Err(_) if cancelled => (false, "Cancelled: nothing was changed.".to_string()),
                    Err(error) => (true, refusal(&op, &extension, &database, &error)),
                };
                window.push_notification(
                    if failed {
                        crate::error_notification(text.clone())
                    } else {
                        Notification::success(SharedString::from(text.clone()))
                    },
                    cx,
                );
                this.message = Some((failed, text));
                if !failed {
                    this.schema_choice = None;
                    this.version_choice = None;
                    cx.emit(ExtensionsTabEvent::Changed);
                }
                this.reload(window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    // For tests ---------------------------------------------------------------

    /// The state of the list, in words.
    #[cfg(test)]
    pub fn state_text(&self) -> String {
        match &self.state {
            State::Waiting => "waiting".into(),
            State::Loading => "loading".into(),
            State::Ready => "ready".into(),
            State::Failed(message) => format!("failed: {message}"),
        }
    }

    #[cfg(test)]
    pub fn message(&self) -> Option<&(bool, String)> {
        self.message.as_ref()
    }

    #[cfg(test)]
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn choose_schema(&mut self, schema: Option<String>, cx: &mut Context<Self>) {
        self.schema_choice = schema;
        cx.notify();
    }

    pub fn choose_version(&mut self, version: Option<String>, cx: &mut Context<Self>) {
        self.version_choice = version;
        cx.notify();
    }

    #[cfg(test)]
    pub fn search_for(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.set_value(text, window, cx));
        self.query = text.to_lowercase();
        self.keep_selection_shown();
        cx.notify();
    }

    // View --------------------------------------------------------------------

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let engine = self
            .connections
            .read(cx)
            .meta_session(&self.target.connection)
            .map(|meta| meta.engine());
        h_flex()
            .flex_none()
            // Wraps in a narrow group of a split instead of being cut (K19).
            .flex_wrap()
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
                    .id("extensions-title")
                    .test_support()
                    .px_1()
                    .text_sm()
                    .font_semibold()
                    .test_text(format!("Extensions of {}", self.database()))
                    .child(SharedString::from(format!(
                        "Extensions of {}",
                        self.database()
                    ))),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(self.connection_name(cx))),
            )
            .child(div().flex_1())
            .child(
                div().w(px(220.)).min_w(px(140.)).child(
                    Input::new(&self.search)
                        .small()
                        .id("extensions-search")
                        .cleanable(true),
                ),
            )
            .children(Filter::ALL.into_iter().enumerate().map(|(index, filter)| {
                let selected = self.filter == filter;
                let label = format!("{} {}", filter.label(), self.count(filter));
                Button::new(("extensions-filter", index))
                    .small()
                    .map(|button| {
                        if selected {
                            button.primary()
                        } else {
                            button.ghost()
                        }
                    })
                    .label(SharedString::from(label))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)))
            }))
            .child(
                Button::new("extensions-refresh")
                    .ghost()
                    .small()
                    .label("Refresh")
                    .tooltip("Read the extensions of this database again")
                    .disabled(self.state == State::Loading)
                    .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
            )
    }

    /// A short line under the toolbar: the case where an extension is
    /// missing from the list, what runs, what came of the last statement.
    fn render_notes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let missing = "An extension that is not listed is not on the server yet: install its \
                       package on the server's machine first, and it shows up here.";
        v_flex()
            .flex_none()
            .px_3()
            .py_1()
            .gap_1()
            .text_xs()
            .child(
                div()
                    .id("extensions-missing")
                    .test_support()
                    .text_color(theme.muted_foreground)
                    .test_text(missing)
                    .child(missing),
            )
            .children(
                self.why_not_write(cx)
                    .filter(|_| self.read_only(cx))
                    .map(|reason| {
                        div()
                            .id("extensions-read-only")
                            .test_support()
                            .text_color(theme.warning)
                            .test_text(reason)
                            .child(reason)
                    }),
            )
            .children(self.running.as_ref().map(|running| {
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("extensions-running")
                            .test_support()
                            .child(SharedString::from(running.doing.clone())),
                    )
                    .child(
                        Button::new("extensions-cancel")
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    )
            }))
            .children(self.message.as_ref().map(|(failed, text)| {
                div()
                    .id("extensions-message")
                    .test_support()
                    .whitespace_normal()
                    .text_color(if *failed {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .test_text(text.clone())
                    .child(SharedString::from(text.clone()))
            }))
    }

    fn render_row(
        &self,
        index: usize,
        extension: &ExtensionInfo,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let selected = self.selected.as_deref() == Some(extension.name.as_str());
        let (state, color) = match extension.state() {
            ExtensionState::Installed => ("Installed", theme.success),
            ExtensionState::UpdateAvailable => ("Update available", theme.warning),
            ExtensionState::Available => ("Available", theme.muted_foreground),
        };
        let versions = match (&extension.installed_version, &extension.default_version) {
            (Some(installed), Some(default)) if installed != default => {
                format!("{installed} → {default}")
            }
            (Some(installed), _) => installed.clone(),
            (None, Some(default)) => default.clone(),
            (None, None) => String::new(),
        };
        let name = extension.name.clone();
        let (click_name, menu_name, press_name) = (name.clone(), name.clone(), name.clone());
        let view = cx.entity();
        let hover = theme.list_hover;
        div()
            .id(("extension-row", index))
            .test_support()
            .test_text(format!("{name} {state}"))
            .px_3()
            .py_1p5()
            .border_b_1()
            .border_color(theme.border)
            .cursor_pointer()
            .map(|row| {
                if selected {
                    row.bg(theme.list_active)
                } else {
                    row.hover(move |row| row.bg(hover))
                }
            })
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, window, cx| {
                    this.select(&click_name, window, cx);
                    this.focus_handle.focus(window, cx);
                    if event.click_count() == 2 {
                        this.main_action(&click_name, window, cx);
                    }
                }),
            )
            // A right-click selects the row the menu is about.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| this.select(&press_name, window, cx)),
            )
            .context_menu(move |menu, _, cx| view.read(cx).row_menu(&menu_name, menu, &view, cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(name.clone())),
                    )
                    .child(div().flex_none().text_xs().text_color(color).child(state))
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(versions)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(SharedString::from(
                        extension.comment.clone().unwrap_or_default(),
                    )),
            )
            .into_any_element()
    }

    /// The menu of a row: its actions, each with why it cannot be used, and
    /// Copy name.
    fn row_menu(&self, name: &str, menu: PopupMenu, view: &Entity<Self>, cx: &App) -> PopupMenu {
        let Some(extension) = self.extension(name).cloned() else {
            return menu;
        };
        // Why each action cannot be used, as the tree's menus say it.
        let refused = if self.read_only(cx) {
            Some("read-only connection")
        } else if self.running.is_some() {
            Some("a statement is running")
        } else if !self.connected(cx) {
            Some("not connected")
        } else {
            None
        };
        let entry = |menu: PopupMenu, label: &str, op: Option<Op>| {
            let (view, name) = (view.clone(), extension.name.clone());
            let label = match refused {
                Some(reason) => format!("{label} ({reason})"),
                None => label.to_string(),
            };
            menu.item(
                PopupMenuItem::new(SharedString::from(label))
                    .disabled(refused.is_some())
                    .on_click(move |_, window, cx| {
                        let op = op.clone();
                        view.update(cx, |this, cx| match op {
                            Some(op) => this.ask(&name, op, window, cx),
                            None => this.ask_install(&name, window, cx),
                        })
                    }),
            )
        };
        let mut menu = match extension.state() {
            ExtensionState::Available => entry(menu, "Install…", None),
            state => {
                let mut menu = menu;
                if state == ExtensionState::UpdateAvailable {
                    menu = entry(menu, "Update", Some(Op::Update { version: None }));
                }
                menu = entry(menu, "Drop…", Some(Op::Drop { cascade: false }));
                entry(menu, "Drop with cascade…", Some(Op::Drop { cascade: true }))
            }
        };
        let copied = extension.name.clone();
        menu = menu
            .separator()
            .item(PopupMenuItem::new("Copy name").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
            }));
        menu
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let empty = |id: &'static str, text: String| {
            div()
                .id(id)
                .test_support()
                .p_4()
                .text_sm()
                .whitespace_normal()
                .text_color(theme.muted_foreground)
                .test_text(text.clone())
                .child(SharedString::from(text))
                .into_any_element()
        };
        match &self.state {
            State::Waiting => {
                return empty(
                    "extensions-waiting",
                    "Connect to list the extensions of this database.".into(),
                );
            }
            State::Loading if self.read.extensions.is_empty() => {
                return empty("extensions-loading", "Reading the extensions…".into());
            }
            State::Failed(message) => {
                return empty(
                    "extensions-error",
                    format!("The extensions could not be read. Refresh to try again.\n{message}"),
                );
            }
            _ => {}
        }
        let shown = self.shown();
        if shown.is_empty() {
            let text = if self.read.extensions.is_empty() {
                "This server has no extension files.".to_string()
            } else if self.query.is_empty() {
                format!(
                    "No extension is {}.",
                    match self.filter {
                        Filter::Installed => "installed",
                        Filter::ToUpdate => "waiting for an update",
                        _ => "available",
                    }
                )
            } else {
                format!(
                    "No extension matches “{}”. It may not be on the server yet: install its \
                     package on the server's machine first.",
                    self.search.read(cx).value()
                )
            };
            return empty("extensions-empty", text);
        }
        let rows: Vec<AnyElement> = shown
            .iter()
            .enumerate()
            .map(|(index, extension)| self.render_row(index, extension, cx))
            .collect();
        div()
            .id("extensions-list")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.list_scroll)
            .children(rows)
            .into_any_element()
    }

    fn render_details(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(extension) = self.selected().cloned() else {
            return div()
                .id("extension-details-empty")
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Select an extension to see what it needs and what can be done with it.")
                .into_any_element();
        };
        let name = extension.name.clone();
        let refused = self.why_not_write(cx);
        let line = |label: &'static str, text: String, id: &'static str| {
            v_flex()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .id(id)
                        .test_support()
                        .text_sm()
                        .whitespace_normal()
                        .test_text(text.clone())
                        .child(SharedString::from(text)),
                )
        };
        let installed = extension.installed_version.is_some();
        let schema_text = match (&extension.schema, &extension.fixed_schema) {
            (Some(schema), _) => format!("In the schema “{schema}”."),
            (None, Some(fixed)) => format!("Goes to “{fixed}”: its control file fixes it."),
            (None, None) => format!(
                "Goes to the schema picked here (the server's default: “{}”).",
                self.read.current_schema.clone().unwrap_or_default()
            ),
        };
        let move_text = if extension.relocatable {
            "Can be moved to another schema."
        } else {
            "Cannot be moved to another schema once installed."
        };
        let versions_text = extension
            .versions
            .iter()
            .map(|version| {
                let mut text = version.clone();
                let marks: Vec<&str> = [
                    (Some(version) == extension.default_version.as_ref()).then_some("default"),
                    (Some(version) == extension.installed_version.as_ref()).then_some("installed"),
                ]
                .into_iter()
                .flatten()
                .collect();
                if !marks.is_empty() {
                    text += &format!(" ({})", marks.join(", "));
                }
                text
            })
            .collect::<Vec<_>>()
            .join(", ");
        let requires_text = if extension.requires.is_empty() {
            "Nothing else.".to_string()
        } else {
            extension
                .requires
                .iter()
                .map(|required| match self.extension(required) {
                    Some(other) if other.installed_version.is_some() => {
                        format!("{required} (installed)")
                    }
                    Some(_) => format!("{required} (not installed: installed with it)"),
                    None => format!("{required} (not on this server)"),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let view = cx.entity();
        let picker = |id: &'static str,
                      label: String,
                      items: Vec<(String, Option<String>, bool)>,
                      disabled: bool,
                      on_pick: Rc<OnPick>| {
            let view = view.clone();
            Button::new(id)
                .small()
                .outline()
                .label(SharedString::from(label))
                .disabled(disabled)
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu: PopupMenu, _, _| {
                    for (text, value, checked) in items.clone() {
                        let (view, on_pick) = (view.clone(), on_pick.clone());
                        menu = menu.item(
                            PopupMenuItem::new(SharedString::from(text))
                                .checked(checked)
                                .on_click(move |_, window, cx| {
                                    let value = value.clone();
                                    view.update(cx, |this, cx| on_pick(this, value, window, cx))
                                }),
                        );
                    }
                    menu
                })
        };
        let mut actions: Vec<AnyElement> = Vec::new();
        let tooltip = refused.unwrap_or("");
        if !installed {
            if extension.fixed_schema.is_none() {
                let chosen = self.install_schema(&extension).unwrap_or_default();
                let items = self
                    .read
                    .schemas
                    .iter()
                    .map(|schema| (schema.clone(), Some(schema.clone()), *schema == chosen))
                    .collect();
                actions.push(
                    picker(
                        "extension-schema",
                        format!("Schema: {chosen}"),
                        items,
                        false,
                        Rc::new(|this, value, _, cx| this.choose_schema(value, cx)),
                    )
                    .into_any_element(),
                );
            }
            let chosen = self
                .version_choice
                .clone()
                .or(extension.default_version.clone())
                .unwrap_or_default();
            if extension.versions.len() > 1 {
                let items = extension
                    .versions
                    .iter()
                    .rev()
                    .map(|version| (version.clone(), Some(version.clone()), *version == chosen))
                    .collect();
                actions.push(
                    picker(
                        "extension-version",
                        format!("Version: {chosen}"),
                        items,
                        false,
                        Rc::new(|this, value, _, cx| this.choose_version(value, cx)),
                    )
                    .into_any_element(),
                );
            }
            let install_name = name.clone();
            actions.push(
                Button::new("extension-install")
                    .small()
                    .primary()
                    .label("Install…")
                    .disabled(refused.is_some())
                    .when(refused.is_some(), |button| button.tooltip(tooltip))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ask_install(&install_name, window, cx)
                    }))
                    .into_any_element(),
            );
        } else {
            if !extension.update_targets.is_empty() {
                let update_name = name.clone();
                let to_default = extension.state() == ExtensionState::UpdateAvailable;
                if to_default {
                    actions.push(
                        Button::new("extension-update")
                            .small()
                            .primary()
                            .label(SharedString::from(format!(
                                "Update to {}",
                                extension.default_version.clone().unwrap_or_default()
                            )))
                            .disabled(refused.is_some())
                            .when(refused.is_some(), |button| button.tooltip(tooltip))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.ask(&update_name, Op::Update { version: None }, window, cx)
                            }))
                            .into_any_element(),
                    );
                }
                let items = extension
                    .update_targets
                    .iter()
                    .rev()
                    .map(|version| (version.clone(), Some(version.clone()), false))
                    .collect();
                let pick_name = name.clone();
                actions.push(
                    picker(
                        "extension-update-to",
                        "Update to…".into(),
                        items,
                        refused.is_some(),
                        Rc::new(move |this, value, window, cx| {
                            this.ask(&pick_name, Op::Update { version: value }, window, cx)
                        }),
                    )
                    .into_any_element(),
                );
            }
            if extension.relocatable {
                let items = self
                    .read
                    .schemas
                    .iter()
                    .filter(|schema| Some(*schema) != extension.schema.as_ref())
                    .map(|schema| (schema.clone(), Some(schema.clone()), false))
                    .collect();
                let move_name = name.clone();
                actions.push(
                    picker(
                        "extension-move",
                        "Move to schema…".into(),
                        items,
                        refused.is_some(),
                        Rc::new(move |this, value, window, cx| {
                            if let Some(schema) = value {
                                this.ask(&move_name, Op::Move { schema }, window, cx)
                            }
                        }),
                    )
                    .into_any_element(),
                );
            }
            for (id, label, cascade) in [
                ("extension-drop", "Drop…", false),
                ("extension-drop-cascade", "Drop with cascade…", true),
            ] {
                let drop_name = name.clone();
                actions.push(
                    Button::new(id)
                        .small()
                        .ghost()
                        .label(label)
                        .disabled(refused.is_some())
                        .when(refused.is_some(), |button| button.tooltip(tooltip))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.ask(&drop_name, Op::Drop { cascade }, window, cx)
                        }))
                        .into_any_element(),
                );
            }
        }
        v_flex()
            .id("extension-details")
            .test_support()
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_size(theme.font_size * 1.15)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(name.clone())),
                    )
                    .child(
                        div()
                            .id("extension-comment")
                            .test_support()
                            .text_sm()
                            .whitespace_normal()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(
                                extension
                                    .comment
                                    .clone()
                                    .unwrap_or_else(|| "The server gives no description.".into()),
                            )),
                    ),
            )
            .child(h_flex().flex_wrap().gap_1().children(actions))
            .child(line(
                "Schema",
                format!("{schema_text} {move_text}"),
                "extension-schema-text",
            ))
            .child(line(
                "Versions on this server",
                versions_text,
                "extension-versions",
            ))
            .child(line("Requires", requires_text, "extension-requires"))
            .child(line(
                "Who may install it",
                who_may_install(&extension, self.read.trusted_known),
                "extension-who",
            ))
            .into_any_element()
    }
}

/// `“a”, “b” and “c”`.
fn quoted_list(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("“{name}”")).collect();
    match quoted.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

impl Focusable for ExtensionsTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ExtensionsTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = is_narrow(self.width.get());
        let width = self.width.clone();
        let theme = cx.theme();
        let border = theme.border;
        let list = self.render_list(cx);
        let details = self.render_details(cx);
        let body = if narrow {
            v_flex()
                .size_full()
                .child(div().flex_1().min_h_0().child(list))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .border_t_1()
                        .border_color(border)
                        .child(details),
                )
        } else {
            h_flex()
                .size_full()
                .child(div().flex_1().min_w_0().h_full().child(list))
                .child(
                    div()
                        .w(px(340.))
                        .flex_none()
                        .h_full()
                        .border_l_1()
                        .border_color(border)
                        .child(details),
                )
        };
        v_flex()
            .id("extensions-tab")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.key_down(event, window, cx)
            }))
            .child(
                // The tab's width, for the layout of the next frame.
                canvas(
                    move |bounds: Bounds<Pixels>, window, _| {
                        let now = bounds.size.width.as_f32();
                        // A new frame only when the layout changes.
                        if is_narrow(width.replace(now)) != is_narrow(now) {
                            window.refresh();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(self.render_toolbar(cx))
            .child(self.render_notes(cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extension(name: &str, installed: bool, requires: &[&str]) -> ExtensionInfo {
        ExtensionInfo {
            name: name.into(),
            installed_version: installed.then(|| "1.0".into()),
            default_version: Some("1.0".into()),
            requires: requires.iter().map(|r| r.to_string()).collect(),
            ..ExtensionInfo::default()
        }
    }

    /// K27: CASCADE installs what is missing, followed through, in the
    /// order the server installs it; what is installed is left out.
    #[test]
    fn missing_requirements_are_followed_through() {
        let all = [
            extension("earthdistance", false, &["cube"]),
            extension("cube", false, &[]),
            extension("top", false, &["middle", "plpgsql"]),
            extension("middle", false, &["cube"]),
            extension("plpgsql", true, &[]),
        ];
        assert_eq!(missing_requirements("earthdistance", &all), ["cube"]);
        assert_eq!(missing_requirements("top", &all), ["cube", "middle"]);
        assert!(missing_requirements("cube", &all).is_empty());
        assert_eq!(
            missing_requirements("lost", &[extension("lost", false, &["gone"])]),
            ["gone"]
        );
    }

    /// K27: a refusal says what happened first, then the server's words.
    #[test]
    fn refusals_say_what_happened_first() {
        let db = |code: &str, message: &str| {
            Error::Db(Box::new(kerno_core::DbError {
                message: message.into(),
                code: Some(code.into()),
                hint: Some("Must be superuser to create this extension.".into()),
                ..Default::default()
            }))
        };
        let mut earth = extension("earthdistance", false, &["cube"]);
        earth.superuser = true;
        let install = Op::Install {
            schema: None,
            version: None,
            brings: vec!["cube".into()],
        };
        let denied = db(
            "42501",
            "permission denied to create extension \"earthdistance\"",
        );
        let text = refusal(&install, &earth, "shop", &denied);
        assert!(
            text.starts_with("Only a superuser can install “earthdistance”"),
            "{text}"
        );
        assert!(text.contains("permission denied") && text.contains("Must be superuser"));
        earth.trusted = true;
        let text = refusal(&install, &earth, "shop", &denied);
        assert!(text.starts_with("This user may not install") && text.contains("“shop”"));
        let text = refusal(&Op::Drop { cascade: false }, &earth, "shop", &denied);
        assert!(text.starts_with("This user may not change"), "{text}");
        let files = db(
            "22023",
            "extension \"citext\" has no installation script nor update path for version \"9.9\"",
        );
        let text = refusal(&install, &earth, "shop", &files);
        assert!(text.starts_with("The server has no files"), "{text}");
        let other = db("XX000", "something else");
        assert!(
            refusal(&Op::Update { version: None }, &earth, "shop", &other)
                .starts_with("The extension “earthdistance” was not updated.")
        );
    }

    #[test]
    fn who_may_install_is_read_from_the_catalog() {
        let mut ext = extension("x", false, &[]);
        assert!(who_may_install(&ext, true).starts_with("Any user"));
        ext.superuser = true;
        assert!(who_may_install(&ext, true).starts_with("Only a superuser"));
        assert!(who_may_install(&ext, false).contains("before PostgreSQL 13"));
        ext.trusted = true;
        assert!(who_may_install(&ext, true).starts_with("A trusted extension"));
        assert_eq!(
            quoted_list(&["a".into(), "b".into(), "c".into()]),
            "“a”, “b” and “c”"
        );
    }
}
