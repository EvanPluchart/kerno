//! Server administration: the sessions on the server, which can be
//! cancelled or terminated; who waits for whose lock; the roles and what each was granted; the sizes
//! of the databases and tables; the server's settings. Each list is one of
//! the driver's administrative views (`Session::admin_query`) read on the
//! tab's own session. The two signals are the only statements that act on
//! the server, and they go through `sql_confirm`.
//!
//! Columns and actions follow DBeaver's session managers
//! (`PostgreSessionEditor`, `MySQLSessionEditor`): refresh, auto-refresh,
//! cancel query, terminate, each confirmed.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::base::FocusableExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::table::{
    Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableSelection, TableState,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, StyledExt as _,
    WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, AsyncWindowContext, ClipboardItem, Context, Div, Entity,
    EntityId, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Pixels, Render, SharedString, Stateful, Styled as _,
    Subscription, Task, TestSupportExt as _, WeakEntity, Window, div, px,
};
use kerno_core::exec::Collected;
use kerno_core::format::byte_size;
use kerno_core::result::Column as ResultColumn;
use kerno_core::sort::compare_cells;
use kerno_core::{AdminView, Engine, Error, Session, TxState, TypeKind, describe, exec};

use crate::bridge;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::query::tab::TabStatus;
use crate::sql_confirm::{self, Confirmation, Statement};
use crate::state::AppState;
use crate::tabs::{TabSnapshot, Target};

/// One list of the tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Sessions,
    /// Sessions waiting for a lock, with the session holding it.
    Locks,
    Roles,
    /// The grants of the role selected in `Roles`.
    Grants,
    Databases,
    Tables,
    Settings,
}

impl Pane {
    const ALL: [Pane; 7] = [
        Pane::Sessions,
        Pane::Locks,
        Pane::Roles,
        Pane::Grants,
        Pane::Databases,
        Pane::Tables,
        Pane::Settings,
    ];

    fn view(self) -> AdminView {
        match self {
            Pane::Sessions => AdminView::Sessions,
            Pane::Locks => AdminView::Locks,
            Pane::Roles => AdminView::Roles,
            Pane::Grants => AdminView::Grants,
            Pane::Databases => AdminView::DatabaseSizes,
            Pane::Tables => AdminView::TableSizes,
            Pane::Settings => AdminView::Settings,
        }
    }

    /// The id of its cells: unique in the window, two lists can be on screen.
    fn cell_id(self) -> &'static str {
        match self {
            Pane::Sessions => "admin-sessions-cell",
            Pane::Locks => "admin-locks-cell",
            Pane::Roles => "admin-roles-cell",
            Pane::Grants => "admin-grants-cell",
            Pane::Databases => "admin-databases-cell",
            Pane::Tables => "admin-tables-cell",
            Pane::Settings => "admin-settings-cell",
        }
    }

    /// What the list holds, for its messages.
    fn noun(self) -> &'static str {
        match self {
            Pane::Sessions => "sessions",
            Pane::Locks => "locks",
            Pane::Roles => "roles",
            Pane::Grants => "grants",
            Pane::Databases => "database sizes",
            Pane::Tables => "table sizes",
            Pane::Settings => "settings",
        }
    }
}

/// The sub-tabs, in order, with the lists each shows.
const SECTIONS: [(&str, &[Pane]); 5] = [
    ("Sessions", &[Pane::Sessions]),
    ("Locks", &[Pane::Locks]),
    ("Roles", &[Pane::Roles, Pane::Grants]),
    ("Sizes", &[Pane::Databases, Pane::Tables]),
    ("Settings", &[Pane::Settings]),
];

/// Index of each sub-tab.
pub const SESSIONS: usize = 0;
pub const LOCKS: usize = 1;
pub const ROLES: usize = 2;
pub const SIZES: usize = 3;
pub const SETTINGS: usize = 4;

/// What can be sent to another session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Stops the statement it runs; it stays connected.
    Cancel,
    /// Disconnects it.
    Terminate,
}

impl Signal {
    pub fn label(self) -> &'static str {
        match self {
            Signal::Cancel => "Cancel query",
            Signal::Terminate => "Terminate session",
        }
    }
}

/// Why no signal can be sent to a session now, when none can: never on a
/// read-only connection, never to this tab's own session.
pub fn signal_unavailable(read_only: bool, own: bool) -> Option<&'static str> {
    if read_only {
        Some("read-only connection")
    } else if own {
        Some("this tab's session")
    } else {
        None
    }
}

/// The auto-refresh periods offered for the sessions, `None` being off.
const AUTO_REFRESH: [Option<Duration>; 3] = [
    None,
    Some(Duration::from_secs(5)),
    Some(Duration::from_secs(30)),
];

fn every_label(every: Option<Duration>) -> String {
    match every {
        None => "off".into(),
        Some(every) => format!("{} s", every.as_secs()),
    }
}

/// Where a list is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PaneState {
    /// Not read yet.
    Idle,
    Loading,
    Ready,
    Failed(String),
}

struct PaneView {
    grid: Entity<TableState<AdminGrid>>,
    state: PaneState,
    /// Read at least once: its rows stay on screen while it reads again.
    loaded: bool,
}

/// Server administration for one connection.
pub struct AdminTab {
    focus_handle: FocusHandle,
    connections: Entity<Connections>,
    target: Target,
    session: Option<Arc<dyn Session>>,
    /// Restored and not shown yet: nothing is read, no session is open.
    hidden: bool,
    /// The connection was open when last seen.
    was_connected: bool,
    /// One per pane, in `Pane::ALL` order.
    panes: Vec<PaneView>,
    /// The filter box of each sub-tab that has one.
    filters: [Option<Entity<InputState>>; 5],
    section: usize,
    auto_refresh: Option<Duration>,
    ticker: Option<Task<()>>,
    /// The tab each group of the workspace shows: the sessions refresh on
    /// their own only while this tab is one of them.
    shown: Rc<RefCell<Vec<EntityId>>>,
    /// The last job on the session; the next one waits for it, so the
    /// session runs one statement at a time and refreshes never pile up.
    job: Option<Task<()>>,
    /// What came of the last signal, and whether it failed.
    message: Option<(bool, String)>,
    /// The statement the last confirmation showed.
    asked: Option<String>,
    /// Lists read, for the tests of the auto-refresh.
    reads: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<()> for AdminTab {}

impl AdminTab {
    pub fn new(
        target: Target,
        connections: Entity<Connections>,
        shown: Rc<RefCell<Vec<EntityId>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The lists are read when the connection opens.
        let follow = cx.observe_in(&connections, window, |this, _, window, cx| {
            let connected = this.connected(cx);
            if connected == this.was_connected {
                return;
            }
            this.was_connected = connected;
            if connected {
                this.read_section(window, cx);
            } else {
                this.close_session(cx);
                cx.notify();
            }
        });
        let tab = cx.entity().downgrade();
        let panes: Vec<PaneView> = Pane::ALL
            .iter()
            .map(|&pane| {
                let mut grid = AdminGrid::new(pane, tab.clone());
                grid.hide_idle =
                    pane == Pane::Sessions && AppState::get(cx).settings.hide_idle_sessions;
                PaneView {
                    grid: cx.new(|cx| {
                        TableState::new(grid, window, cx)
                            .col_resizable(true)
                            .row_selectable(true)
                            .cell_selectable(true)
                            .row_header(false)
                    }),
                    state: PaneState::Idle,
                    loaded: false,
                }
            })
            .collect();
        let mut subscriptions = vec![follow];
        // The grants follow the role selected.
        subscriptions.push(cx.subscribe(
            &panes[Pane::Roles as usize].grid,
            |this, _, event: &TableEvent, cx| {
                if matches!(
                    event,
                    TableEvent::SelectRow(_)
                        | TableEvent::SelectCell(..)
                        | TableEvent::ClearSelection
                ) {
                    this.follow_role(cx);
                }
            },
        ));
        let filters = [SESSIONS, LOCKS, ROLES, SIZES, SETTINGS].map(|section| {
            (section != ROLES).then(|| {
                let input = cx.new(|cx| {
                    InputState::new(window, cx).placeholder(match section {
                        SESSIONS => "Filter sessions",
                        LOCKS => "Filter locks",
                        SIZES => "Filter databases and tables",
                        _ => "Filter settings",
                    })
                });
                subscriptions.push(cx.subscribe(&input, move |this, input, event, cx| {
                    if let InputEvent::Change = event {
                        let text = input.read(cx).value().to_string();
                        this.filter(section, &text, cx);
                    }
                }));
                input
            })
        });
        let mut tab = Self {
            focus_handle: cx.focus_handle(),
            connections,
            target,
            session: None,
            hidden: false,
            was_connected: false,
            panes,
            filters,
            section: SESSIONS,
            auto_refresh: None,
            ticker: None,
            shown,
            job: None,
            message: None,
            asked: None,
            reads: 0,
            _subscriptions: subscriptions,
        };
        tab.was_connected = tab.connected(cx);
        tab.read_section(window, cx);
        tab
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn title(&self, cx: &App) -> SharedString {
        SharedString::from(format!("{} · administration", self.connection_name(cx)))
    }

    pub fn snapshot(&self) -> TabSnapshot {
        TabSnapshot::Admin {
            target: self.target.clone(),
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

    fn read_only(&self, cx: &App) -> bool {
        self.connections
            .read(cx)
            .get(&self.target.connection)
            .is_some_and(|saved| saved.config.read_only)
    }

    fn engine(&self, cx: &App) -> Option<Engine> {
        self.connections
            .read(cx)
            .get(&self.target.connection)
            .map(|saved| saved.config.engine)
    }

    fn view(&self, pane: Pane) -> &PaneView {
        &self.panes[pane as usize]
    }

    fn busy(&self) -> bool {
        self.panes
            .iter()
            .any(|pane| pane.state == PaneState::Loading)
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
            running: self.busy(),
            rows: None,
            elapsed: None,
            notices: 0,
            pending: 0,
        }
    }

    // Reading -------------------------------------------------------------------

    /// This tab's session once `previous`, the job before, is done: the one
    /// a job opened, else a new one. `None` once the tab is gone.
    async fn turn(
        this: &WeakEntity<Self>,
        previous: Option<Task<()>>,
        cx: &mut AsyncWindowContext,
    ) -> Option<kerno_core::Result<Arc<dyn Session>>> {
        if let Some(previous) = previous {
            previous.await;
        }
        let opening = this
            .update(cx, |this, cx| match &this.session {
                Some(session) => Task::ready(Ok(session.clone())),
                None => this.connections.read(cx).open_session(
                    &this.target.connection,
                    this.target.database.clone(),
                    cx,
                ),
            })
            .ok()?;
        let session = opening.await;
        if let Ok(session) = &session {
            let session = session.clone();
            this.update(cx, |this, _| this.session = Some(session))
                .ok()?;
        }
        Some(session)
    }

    /// Reads the lists of the sub-tab on screen again.
    pub fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.read_section(window, cx);
    }

    fn read_section(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.read(SECTIONS[self.section].1.to_vec(), window, cx);
    }

    /// Closes the tab's session now: the tab is closing, or its connection
    /// closed. What is on screen stays.
    pub fn close_session(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session.take() {
            bridge::spawn(cx, async move { session.close().await }).detach();
        }
    }

    /// Reads nothing until shown (a restored tab): see `reveal`.
    pub fn defer_until_shown(&mut self, cx: &mut Context<Self>) {
        self.hidden = true;
        self.job = None;
        self.close_session(cx);
        for pane in &mut self.panes {
            if pane.state == PaneState::Loading {
                pane.state = PaneState::Idle;
            }
        }
        cx.notify();
    }

    /// The tab is on screen: a deferred tab reads its lists now.
    pub fn reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.hidden) {
            self.read_section(window, cx);
        }
    }

    fn read(&mut self, panes: Vec<Pane>, window: &mut Window, cx: &mut Context<Self>) {
        if self.hidden {
            return;
        }
        if !self.connected(cx) {
            self.close_session(cx);
            cx.notify();
            return;
        }
        for &pane in &panes {
            self.panes[pane as usize].state = PaneState::Loading;
        }
        self.reads += panes.len();
        let previous = self.job.take();
        self.job = Some(cx.spawn_in(window, async move |this, cx| {
            let Some(turn) = Self::turn(&this, previous, cx).await else {
                return;
            };
            let session = match turn {
                Ok(session) => session,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        for pane in panes {
                            this.failed(pane, &error, cx);
                        }
                    });
                    return;
                }
            };
            for pane in panes {
                let outcome = match session.admin_query(pane.view()) {
                    None => Err(Error::Unsupported(format!(
                        "the {} of this server",
                        pane.noun()
                    ))),
                    Some(sql) => {
                        let session = session.clone();
                        let Ok(reading) =
                            cx.update(|_, cx| bridge::spawn(cx, exec::collect(session, sql)))
                        else {
                            return;
                        };
                        reading.await
                    }
                };
                let shown = this.update(cx, |this, cx| match outcome {
                    Ok(collected) => this.show(pane, collected, cx),
                    Err(error) => this.failed(pane, &error, cx),
                });
                if shown.is_err() {
                    return;
                }
            }
        }));
        cx.notify();
    }

    fn failed(&mut self, pane: Pane, error: &Error, cx: &mut Context<Self>) {
        if error.is_disconnect() {
            self.close_session(cx);
        }
        self.panes[pane as usize].state = PaneState::Failed(describe(error).to_string());
        cx.notify();
    }

    /// Puts a list on screen in place of what it showed, keeping the
    /// selected row (by its key), the scroll position and the sort.
    fn show(&mut self, pane: Pane, collected: Collected, cx: &mut Context<Self>) {
        let result = collected
            .results
            .into_iter()
            .find(|result| !result.columns.is_empty())
            .unwrap_or_default();
        let rows = result.rows();
        let columns = result.columns.to_vec();
        let view = &mut self.panes[pane as usize];
        view.state = PaneState::Ready;
        view.loaded = true;
        view.grid.update(cx, |table, cx| {
            let kept =
                selected(table).map(|(row, column)| (table.delegate().key(row), row, column));
            if table.delegate_mut().set_result(columns, rows) {
                table.refresh(cx);
            }
            if let Some((key, old, column)) = kept {
                match table.delegate().find(&key) {
                    Some(new) if new == old => {}
                    Some(new) => select_in_place(table, new, column, cx),
                    None => table.clear_selection(cx),
                }
            }
            cx.notify();
        });
        if pane == Pane::Roles {
            self.follow_role(cx);
        }
        cx.notify();
    }

    // Actions -------------------------------------------------------------------

    /// Shows a sub-tab, reading its lists the first time.
    pub fn select(&mut self, section: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.section = section.min(SECTIONS.len() - 1);
        let unread: Vec<Pane> = SECTIONS[self.section]
            .1
            .iter()
            .copied()
            .filter(|pane| self.view(*pane).state == PaneState::Idle)
            .collect();
        if !unread.is_empty() {
            self.read(unread, window, cx);
        }
        cx.notify();
    }

    /// Reads the sessions again every `every`, or stops when `None`.
    pub fn set_auto_refresh(
        &mut self,
        every: Option<Duration>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.auto_refresh = every;
        self.ticker = every.map(|every| {
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(every).await;
                    if this
                        .update_in(cx, |this, window, cx| this.tick(window, cx))
                        .is_err()
                    {
                        return;
                    }
                }
            })
        });
        cx.notify();
    }

    /// One period of the auto-refresh: the sessions are read again when
    /// they are on screen and not being read already.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.shown.borrow().contains(&cx.entity_id());
        let pane = match self.section {
            SESSIONS => Pane::Sessions,
            LOCKS => Pane::Locks,
            _ => return,
        };
        if shown && !self.busy() && self.connected(cx) {
            self.read(vec![pane], window, cx);
        }
    }

    fn filter(&mut self, section: usize, text: &str, cx: &mut Context<Self>) {
        for &pane in SECTIONS[section].1 {
            self.panes[pane as usize].grid.update(cx, |table, cx| {
                table.delegate_mut().set_filter(text);
                // The selection is an index into what is shown.
                table.clear_selection(cx);
                cx.notify();
            });
        }
    }

    /// Shows the grants of the role selected, or none.
    fn follow_role(&mut self, cx: &mut Context<Self>) {
        let mysql = self.engine(cx) != Some(Engine::Postgres);
        let grantee = {
            let roles = self.view(Pane::Roles).grid.read(cx);
            selected(roles).and_then(|(row, _)| {
                let roles = roles.delegate();
                let name = roles.raw(row, 0)?.to_string();
                // MySQL and MariaDB name an account `'user'@'host'`.
                if mysql {
                    Some(format!("'{name}'@'{}'", roles.raw(row, 1).unwrap_or("%")))
                } else {
                    Some(name)
                }
            })
        };
        self.view(Pane::Grants).grid.update(cx, |table, cx| {
            if table.delegate().only != grantee {
                table.delegate_mut().only = grantee;
                table.delegate_mut().recompute();
                table.clear_selection(cx);
                cx.notify();
            }
        });
        cx.notify();
    }

    /// The session selected in the sessions list: its id, and whether it is
    /// this tab's own.
    fn selected_session(&self, cx: &App) -> Option<(String, bool)> {
        let table = self.view(Pane::Sessions).grid.read(cx);
        let (row, _) = selected(table)?;
        let grid = table.delegate();
        Some((grid.raw(row, 0)?.to_string(), grid.is_own(row)))
    }

    /// The session holding the lock of the row selected in the locks list.
    fn selected_blocker(&self, cx: &App) -> Option<String> {
        let table = self.view(Pane::Locks).grid.read(cx);
        let (row, _) = selected(table)?;
        let grid = table.delegate();
        let column = grid.column_named("blocking_id")?;
        Some(grid.raw(row, column)?.to_string())
    }

    /// Leaves idle (PostgreSQL) or sleeping (MySQL, MariaDB) sessions out
    /// of the list, or shows them again; remembered in the settings.
    pub fn set_hide_idle(&mut self, hide: bool, cx: &mut Context<Self>) {
        AppState::update_settings(cx, |settings| settings.hide_idle_sessions = hide);
        self.view(Pane::Sessions).grid.update(cx, |table, cx| {
            table.delegate_mut().hide_idle = hide;
            table.delegate_mut().recompute();
            table.clear_selection(cx);
            cx.notify();
        });
        cx.notify();
    }

    /// Whether `id` is this tab's own session, as the last read said.
    fn is_own(&self, id: &str, cx: &App) -> bool {
        let grid = self.view(Pane::Sessions).grid.read(cx).delegate();
        (0..grid.rows.len())
            .any(|row| grid.rows[row][0].as_deref() == Some(id) && grid.own_row(row))
    }

    /// Who a session is, for the confirmation: " (kerno on shop)".
    fn who(&self, id: &str, cx: &App) -> String {
        let grid = self.view(Pane::Sessions).grid.read(cx).delegate();
        let column = |name: &str| grid.columns.iter().position(|column| column.name == name);
        let Some(row) = grid.rows.iter().find(|row| row[0].as_deref() == Some(id)) else {
            return String::new();
        };
        let cell = |name: &str| column(name).and_then(|column| row[column].clone());
        match (cell("user"), cell("database")) {
            (Some(user), Some(database)) => format!(" ({user} on {database})"),
            (Some(user), None) => format!(" ({user})"),
            _ => String::new(),
        }
    }

    /// Asks before sending `signal` to the session `id`, showing the
    /// statement that will run.
    pub fn ask(&mut self, signal: Signal, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(reason) = signal_unavailable(self.read_only(cx), self.is_own(&id, cx)) {
            window.push_notification(
                Notification::warning(SharedString::from(format!("{}: {reason}.", signal.label()))),
                cx,
            );
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let dialect = session.dialect();
        let statement = match signal {
            Signal::Cancel => dialect.cancel_session_sql(&id),
            Signal::Terminate => dialect.terminate_session_sql(&id),
        };
        let sql = match statement {
            Ok(sql) => sql,
            Err(error) => {
                window.push_notification(
                    Notification::error(SharedString::from(error.to_string())),
                    cx,
                );
                return;
            }
        };
        self.asked = Some(sql.clone());
        let who = self.who(&id, cx);
        let (title, explanation) = match signal {
            Signal::Cancel => (
                format!("Cancel the statement of session {id}?"),
                format!(
                    "The statement session {id}{who} is running stops with an error. The \
                     session stays connected."
                ),
            ),
            Signal::Terminate => (
                format!("Terminate session {id}?"),
                format!(
                    "Session {id}{who} is disconnected: what it is running stops and its open \
                     transaction is rolled back."
                ),
            ),
        };
        let environment = self
            .connections
            .read(cx)
            .get(&self.target.connection)
            .map(|saved| saved.config.environment.clone())
            .unwrap_or_default();
        let view = cx.entity();
        sql_confirm::open(
            Confirmation {
                title,
                explanation,
                statement: Statement::Fixed(sql),
                action: signal.label(),
                destructive: signal == Signal::Terminate,
                connection: self.connection_name(cx),
                environment,
                writes: true,
            },
            window,
            cx,
            // The id was taken when asking: a refresh may have moved the rows.
            move |_, window, cx| {
                let id = id.clone();
                view.update(cx, |this, cx| this.send(signal, id, window, cx));
            },
        );
    }

    /// Sends a confirmed signal, then reads the sessions again.
    fn send(&mut self, signal: Signal, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.job.take();
        self.job = Some(cx.spawn_in(window, async move |this, cx| {
            let Some(turn) = Self::turn(&this, previous, cx).await else {
                return;
            };
            let outcome = match turn {
                Ok(session) => {
                    let target = id.clone();
                    let Ok(sending) = cx.update(|_, cx| {
                        bridge::spawn(cx, async move {
                            match signal {
                                Signal::Cancel => session.cancel_session(&target).await,
                                Signal::Terminate => session.terminate_session(&target).await,
                            }
                        })
                    }) else {
                        return;
                    };
                    sending.await
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let (failed, text) = match outcome {
                    Ok(()) => (
                        false,
                        match signal {
                            Signal::Cancel => format!("Cancelled the statement of session {id}."),
                            Signal::Terminate => format!("Terminated session {id}."),
                        },
                    ),
                    Err(error) => (true, format!("Session {id}: {}", describe(&error))),
                };
                let notification = if failed {
                    crate::error_notification(text.clone())
                } else {
                    Notification::success(SharedString::from(text.clone()))
                };
                window.push_notification(notification, cx);
                this.message = Some((failed, text));
                let read: Vec<Pane> = [Pane::Sessions, Pane::Locks]
                    .into_iter()
                    .filter(|&pane| pane == Pane::Sessions || this.view(pane).loaded)
                    .collect();
                this.read(read, window, cx);
            });
        }));
        cx.notify();
    }

    /// Copies the selected cell, or the selected row, of the list that has
    /// the focus. Returns whether there was something to copy.
    fn copy_selection(&self, window: &Window, cx: &mut App) -> bool {
        let text = self.panes.iter().find_map(|pane| {
            let table = pane.grid.read(cx);
            if !table.focus_handle(cx).is_focused(window) {
                return None;
            }
            let grid = table.delegate();
            match table.selection() {
                TableSelection::Cell(row, column) => Some(grid.cell_text(row, column, cx)),
                TableSelection::Row(row) => Some(
                    (0..grid.shown.len())
                        .map(|column| grid.cell_text(row, column, cx))
                        .collect::<Vec<_>>()
                        .join("\t"),
                ),
                _ => None,
            }
        });
        text.is_some_and(|text| {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            true
        })
    }

    // Test accessors ------------------------------------------------------------

    /// The headers and rows of a list as the tab shows them; for tests.
    #[cfg(test)]
    pub fn shown(&self, pane: Pane, cx: &App) -> (Vec<String>, Vec<Vec<String>>) {
        let grid = self.view(pane).grid.read(cx).delegate();
        let headers = (0..grid.shown.len())
            .map(|column| grid.header(column))
            .collect();
        let rows = (0..grid.visible.len())
            .map(|row| {
                (0..grid.shown.len())
                    .map(|column| grid.label(row, column))
                    .collect()
            })
            .collect();
        (headers, rows)
    }

    /// The table of a list; for tests.
    #[cfg(test)]
    pub fn grid(&self, pane: Pane) -> Entity<TableState<AdminGrid>> {
        self.view(pane).grid.clone()
    }

    /// The sub-tab on screen; for tests.
    #[cfg(test)]
    pub fn section(&self) -> usize {
        self.section
    }

    /// The id of the session selected; for tests.
    #[cfg(test)]
    pub fn selected_id(&self, cx: &App) -> Option<String> {
        self.selected_session(cx).map(|(id, _)| id)
    }

    /// Where a list is, in words; for tests.
    #[cfg(test)]
    pub fn state(&self, pane: Pane) -> String {
        match &self.view(pane).state {
            PaneState::Idle => "idle".into(),
            PaneState::Loading => "loading".into(),
            PaneState::Ready => "ready".into(),
            PaneState::Failed(error) => format!("failed: {error}"),
        }
    }

    /// How many lists were read; for tests.
    #[cfg(test)]
    pub fn reads(&self) -> usize {
        self.reads
    }

    /// The statement the last confirmation showed; for tests.
    #[cfg(test)]
    pub fn asked(&self) -> Option<&str> {
        self.asked.as_deref()
    }

    /// What came of the last signal; for tests.
    #[cfg(test)]
    pub fn message(&self) -> Option<&(bool, String)> {
        self.message.as_ref()
    }

    /// Why a signal to the selected session cannot be sent; for tests.
    #[cfg(test)]
    pub fn why_not_signal(&self, cx: &App) -> Option<&'static str> {
        match self.selected_session(cx) {
            Some((_, own)) => signal_unavailable(self.read_only(cx), own),
            None => Some("no session selected"),
        }
    }

    // Rendering -----------------------------------------------------------------

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let version = self
            .session
            .as_ref()
            .map(|session| session.server().version.clone());
        let auto_menu = {
            let view = cx.entity();
            let current = self.auto_refresh;
            move |mut menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                for every in AUTO_REFRESH {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(SharedString::from(every_label(every)))
                            .checked(every == current)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| this.set_auto_refresh(every, window, cx))
                            }),
                    );
                }
                menu
            }
        };
        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .children(
                self.engine(cx)
                    .map(|engine| div().flex_none().child(engine_logo(engine, px(14.), cx))),
            )
            .child(
                div()
                    .id("admin-connection")
                    .test_support()
                    .px_1()
                    .text_sm()
                    .font_semibold()
                    .child(SharedString::from(self.connection_name(cx))),
            )
            .children(version.map(|version| {
                div()
                    .id("admin-version")
                    .test_support()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(version))
            }))
            .child(div().flex_1())
            .when(matches!(self.section, SESSIONS | LOCKS), |bar| {
                bar.child(
                    Button::new("admin-auto-refresh")
                        .ghost()
                        .small()
                        .label(SharedString::from(format!(
                            "Auto-refresh: {}",
                            every_label(self.auto_refresh)
                        )))
                        .tooltip("Read the list again on its own while this tab is shown")
                        .dropdown_caret(true)
                        .dropdown_menu(auto_menu),
                )
            })
            .child(
                Button::new("admin-refresh")
                    .ghost()
                    .small()
                    .label("Refresh")
                    .tooltip("Read the lists of this sub-tab again")
                    .disabled(self.busy())
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
    }

    /// The names of the sub-tabs with their counts: "Sessions (2 of 6)"
    /// when the filter or "Hide idle" leaves some out.
    pub fn section_titles(&self, cx: &App) -> Vec<String> {
        SECTIONS
            .iter()
            .map(|(label, panes)| {
                // The grants count with their role, not apart.
                let pane = self.view(panes[0]);
                if !pane.loaded {
                    return label.to_string();
                }
                let grid = pane.grid.read(cx).delegate();
                let (shown, all) = (grid.visible.len(), grid.rows.len());
                if shown < all {
                    format!("{label} ({shown} of {all})")
                } else {
                    format!("{label} ({all})")
                }
            })
            .collect()
    }

    fn render_sections(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let titles = self.section_titles(cx);
        TabBar::new("admin-sections")
            .selected_index(self.section)
            .on_click(
                cx.listener(|this, index: &usize, window, cx| this.select(*index, window, cx)),
            )
            .children(
                titles
                    .into_iter()
                    .map(|title| Tab::new().label(SharedString::from(title))),
            )
    }

    /// The filter box of the sub-tab and, for the sessions, what can be
    /// done to the one selected.
    fn render_section_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let filter = self.filters[self.section].as_ref()?;
        let locks = self.section == LOCKS;
        // On the locks, the buttons act on the session holding the lock.
        let selected = if locks {
            self.selected_blocker(cx).map(|id| (id, false))
        } else {
            self.selected_session(cx)
        };
        let read_only = self.read_only(cx);
        let signal_button = |signal: Signal, id: &'static str, cx: &mut Context<Self>| {
            let reason = match &selected {
                Some((_, own)) => signal_unavailable(read_only, *own),
                None if locks => Some("select a lock first"),
                None => Some("select a session first"),
            };
            let target = selected.as_ref().map(|(id, _)| id.clone());
            let label = match (locks, signal) {
                (true, Signal::Cancel) => "Cancel blocking query",
                (true, Signal::Terminate) => "Terminate blocking session",
                (false, signal) => signal.label(),
            };
            Button::new(id)
                .outline()
                .small()
                .label(label)
                .when_some(reason, |button, reason| button.tooltip(reason))
                .disabled(reason.is_some())
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Some(target) = target.clone() {
                        this.ask(signal, target, window, cx)
                    }
                }))
        };
        let sessions = self.section == SESSIONS;
        let buttons = (sessions || locks).then(|| {
            [
                signal_button(Signal::Cancel, "admin-cancel", cx),
                signal_button(Signal::Terminate, "admin-terminate", cx),
            ]
        });
        // DBeaver's "Show idle" (PostgreSQL) and "Hide sleeping" (MySQL).
        let hide_idle = sessions.then(|| {
            let grid = self.view(Pane::Sessions).grid.read(cx).delegate();
            let (hide, hidden) = (grid.hide_idle, grid.hidden_idle);
            let word = if self.engine(cx) == Some(Engine::Postgres) {
                "idle"
            } else {
                "sleeping"
            };
            Button::new("admin-hide-idle")
                .ghost()
                .small()
                .label(SharedString::from(if hide && hidden > 0 {
                    format!("Hide {word} ({hidden} hidden)")
                } else {
                    format!("Hide {word}")
                }))
                .selected(hide)
                .tooltip(if hide {
                    "Show every session again"
                } else {
                    "Leave out the sessions that wait for their next statement"
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_hide_idle(!hide, cx)))
        });
        let theme = cx.theme();
        let message = self
            .message
            .as_ref()
            .filter(|_| sessions || locks)
            .map(|(failed, text)| {
                div()
                    .id("admin-message")
                    .test_support()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(if *failed {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .child(SharedString::from(text.clone()))
            });
        Some(
            h_flex()
                .flex_none()
                .px_2()
                .py_1()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .child(
                    div().w(px(260.)).child(
                        Input::new(filter)
                            .small()
                            .id("admin-filter")
                            .cleanable(true),
                    ),
                )
                .children(hide_idle)
                .children(buttons.into_iter().flatten())
                .children(message)
                .into_any_element(),
        )
    }

    /// A message in place of a list, with what to do next.
    fn render_notice(
        &self,
        id: (&'static str, usize),
        title: String,
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
            .child(div().font_semibold().child(SharedString::from(title)))
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
        if !self.connected(cx) {
            let connect = Button::new("admin-connect")
                .primary()
                .label("Connect")
                .on_click(cx.listener(|this, _, _, cx| {
                    let id = this.target.connection.clone();
                    this.connections
                        .update(cx, |connections, cx| connections.connect(&id, cx));
                }))
                .into_any_element();
            return self.render_notice(
                ("admin-waiting", 0),
                "Not connected".into(),
                format!(
                    "“{}” is not connected. Connect it to see its sessions, roles, sizes and \
                     settings.",
                    self.connection_name(cx)
                ),
                Some(connect),
                cx,
            );
        }
        let theme = cx.theme();
        let panes = SECTIONS[self.section].1;
        let titled = panes.len() > 1;
        let titles: Vec<String> = panes
            .iter()
            .map(|pane| match pane {
                Pane::Grants => match &self.view(Pane::Grants).grid.read(cx).delegate().only {
                    Some(grantee) => format!("Grants of {grantee}"),
                    None => "Grants".into(),
                },
                Pane::Databases => "Databases".into(),
                Pane::Tables => "Tables of this database".into(),
                _ => String::new(),
            })
            .collect();
        let areas: Vec<AnyElement> = panes
            .iter()
            .zip(titles)
            .map(|(&pane, title)| {
                let view = self.view(pane);
                let content = match &view.state {
                    PaneState::Failed(error) => {
                        let retry = Button::new(("admin-retry", pane as usize))
                            .outline()
                            .label("Try again")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.read(vec![pane], window, cx)
                            }))
                            .into_any_element();
                        self.render_notice(
                            ("admin-error", pane as usize),
                            format!("The {} could not be read", pane.noun()),
                            crate::advice_after(
                                error,
                                "The account may lack the privilege to read them: connect as a \
                                 user who has it, then try again.",
                            ),
                            Some(retry),
                            cx,
                        )
                    }
                    _ if !view.loaded => self.render_notice(
                        ("admin-loading", pane as usize),
                        format!("Reading the {}…", pane.noun()),
                        format!("Asking “{}”.", self.connection_name(cx)),
                        None,
                        cx,
                    ),
                    // The lists are wider than the pane: the scrollbar stays
                    // in view so the columns past the edge are not missed.
                    _ => div()
                        .size_full()
                        .relative()
                        .child(DataTable::new(&view.grid).xsmall().focus_ring(false))
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(Scrollbar::width())
                                .child(
                                    Scrollbar::horizontal(
                                        &view.grid.read(cx).horizontal_scroll_handle,
                                    )
                                    .mode(ScrollbarMode::Always)
                                    .viewport_from_layout(),
                                ),
                        )
                        .into_any_element(),
                };
                v_flex()
                    // Each table keeps its own element state.
                    .id(("admin-pane", pane as usize))
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
                                .child(SharedString::from(title)),
                        )
                    })
                    .child(div().flex_1().min_h_0().child(content))
                    .into_any_element()
            })
            .collect();
        v_flex().size_full().children(areas).into_any_element()
    }
}

impl Focusable for AdminTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AdminTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("admin-tab")
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
            .children(self.render_section_bar(cx))
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
    }
}

/// The row selected in a table, as a cell or as a row, and the column.
fn selected<D: TableDelegate>(table: &TableState<D>) -> Option<(usize, Option<usize>)> {
    match table.selection() {
        TableSelection::Row(row) => Some((row, None)),
        TableSelection::Cell(row, column) => Some((row, Some(column))),
        _ => None,
    }
}

/// Selects a row again where it moved to, without scrolling the list.
fn select_in_place(
    table: &mut TableState<AdminGrid>,
    row: usize,
    column: Option<usize>,
    cx: &mut Context<TableState<AdminGrid>>,
) {
    let offset = table.vertical_scroll_handle.0.borrow().base_handle.offset();
    match column {
        Some(column) => table.set_selected_cell(row, column, cx),
        None => table.set_selected_row(row, cx),
    }
    let mut scroll = table.vertical_scroll_handle.0.borrow_mut();
    scroll.deferred_scroll_to_item = None;
    scroll.base_handle.set_offset(offset);
}

/// Width of the dot that marks this tab's own session, with its gap.
const OWN_MARK_WIDTH: Pixels = px(10.);

/// A time without its fraction of a second: `02:37:45.132677+00` is
/// `02:37:45+00`.
fn to_the_second(text: &str) -> String {
    match text.find('.') {
        // The fraction follows the seconds of `hh:mm:ss`.
        Some(dot)
            if text[..dot].contains(':') && text[..dot].ends_with(|c: char| c.is_ascii_digit()) =>
        {
            let rest = &text[dot + 1..];
            let fraction = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            format!("{}{}", &text[..dot], &rest[fraction..])
        }
        _ => text.to_string(),
    }
}

// Lists ---------------------------------------------------------------------------

/// One list: the server's columns and rows, filtered and sorted on the
/// client.
pub struct AdminGrid {
    pane: Pane,
    tab: WeakEntity<AdminTab>,
    columns: Vec<ResultColumn>,
    /// The columns on screen, as indexes into `columns`.
    shown: Vec<usize>,
    /// The column that flags this tab's own session (sessions only).
    own: Option<usize>,
    rows: Vec<Vec<Option<String>>>,
    /// The rows on screen, in order, as indexes into `rows`.
    visible: Vec<usize>,
    /// Lowercase text a row must contain.
    filter: String,
    /// Grants only: the grantee whose grants are shown; none when no role
    /// is selected.
    only: Option<String>,
    /// Sessions only: idle and sleeping sessions are left out.
    hide_idle: bool,
    /// How many sessions `hide_idle` left out at the last count.
    hidden_idle: usize,
    sort: Option<(usize, ColumnSort)>,
    widths: Vec<Pixels>,
}

impl AdminGrid {
    fn new(pane: Pane, tab: WeakEntity<AdminTab>) -> Self {
        Self {
            pane,
            tab,
            columns: Vec::new(),
            shown: Vec::new(),
            own: None,
            rows: Vec::new(),
            visible: Vec::new(),
            filter: String::new(),
            only: None,
            hide_idle: false,
            hidden_idle: 0,
            sort: None,
            widths: Vec::new(),
        }
    }

    /// The index of the server's column named `name`.
    fn column_named(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| column.name == name)
    }

    /// Whether a row of `rows` is a session waiting for its next statement:
    /// PostgreSQL's `idle` (not `idle in transaction`, which holds locks),
    /// MySQL's `Sleep` command.
    fn is_idle(&self, row: usize) -> bool {
        let cell = |name: &str| {
            self.column_named(name)
                .and_then(|column| self.rows[row][column].as_deref())
        };
        match self.column_named("command") {
            Some(_) => cell("command") == Some("Sleep"),
            None => cell("state") == Some("idle"),
        }
    }

    /// Takes a new result. Returns whether the columns changed (the table
    /// must then lay out its headers again).
    fn set_result(&mut self, columns: Vec<ResultColumn>, rows: Vec<Vec<Option<String>>>) -> bool {
        let names = |columns: &[ResultColumn]| -> Vec<String> {
            columns.iter().map(|column| column.name.clone()).collect()
        };
        let changed = names(&columns) != names(&self.columns);
        self.columns = columns;
        self.rows = rows;
        if changed {
            let has = |name: &str| self.columns.iter().any(|column| column.name == name);
            self.own = self
                .columns
                .iter()
                .position(|column| column.name == "this_session");
            // The flag shows as a mark on the row; PostgreSQL's
            // `pg_size_pretty` copies of a size are shown from its bytes.
            self.shown = (0..self.columns.len())
                .filter(|&index| {
                    let name = &self.columns[index].name;
                    let pretty =
                        matches!(name.as_str(), "size" | "total") && has(&format!("{name}_bytes"));
                    Some(index) != self.own && !pretty
                })
                .collect();
            self.sort = None;
            self.visible = (0..self.rows.len()).collect();
            // ponytail: width from the longest text, about 7 px a character;
            // measure with the text system if columns look off.
            self.widths = (0..self.shown.len())
                .map(|column| {
                    let longest = (0..self.rows.len().min(200))
                        .map(|row| self.text_of(row, column).chars().count())
                        .chain([self.header(column).chars().count() + 3])
                        .max()
                        .unwrap_or(0);
                    px(longest.clamp(4, 60) as f32 * 7. + 24.)
                })
                .collect();
            // Room for the dot that marks this tab's own session.
            if self.own.is_some()
                && let Some(first) = self.widths.first_mut()
            {
                *first += OWN_MARK_WIDTH;
            }
        }
        self.recompute();
        changed
    }

    fn set_filter(&mut self, text: &str) {
        self.filter = text.trim().to_lowercase();
        self.recompute();
    }

    /// Works out the rows on screen from the filter and the sort.
    fn recompute(&mut self) {
        let hide_idle = self.pane == Pane::Sessions && self.hide_idle;
        let idle: Vec<bool> = (0..self.rows.len())
            .map(|row| hide_idle && self.is_idle(row))
            .collect();
        self.hidden_idle = idle.iter().filter(|idle| **idle).count();
        let mut visible: Vec<usize> = (0..self.rows.len())
            .filter(|&row| !idle[row])
            .filter(|&row| {
                if self.pane == Pane::Grants && self.rows[row][0].as_deref() != self.only.as_deref()
                {
                    return false;
                }
                self.filter.is_empty()
                    || (0..self.shown.len()).any(|column| {
                        self.text_of(row, column)
                            .to_lowercase()
                            .contains(&self.filter)
                    })
            })
            .collect();
        if let Some((column, sort)) = self.sort
            && sort != ColumnSort::Default
            && let Some(&source) = self.shown.get(column)
        {
            let kind = self.columns[source].kind;
            visible.sort_by(|&a, &b| {
                let (a, b) = (
                    self.rows[a][source].as_deref(),
                    self.rows[b][source].as_deref(),
                );
                // What the server could not tell goes last either way.
                match (a, b) {
                    (None, Some(_)) => Ordering::Greater,
                    (Some(_), None) => Ordering::Less,
                    _ => {
                        let order = compare_cells(a.map(str::as_bytes), b.map(str::as_bytes), kind);
                        if sort == ColumnSort::Descending {
                            order.reverse()
                        } else {
                            order
                        }
                    }
                }
            });
        }
        self.visible = visible;
    }

    /// What identifies a row on screen across reads: its first two cells.
    fn key(&self, row: usize) -> Vec<Option<String>> {
        self.visible
            .get(row)
            .map(|&row| self.rows[row].iter().take(2).cloned().collect())
            .unwrap_or_default()
    }

    /// The row on screen with this key.
    fn find(&self, key: &[Option<String>]) -> Option<usize> {
        self.visible
            .iter()
            .position(|&row| self.rows[row].iter().take(2).eq(key.iter()))
    }

    /// The server's text of a cell of a row on screen, by the server's
    /// column index.
    fn raw(&self, row: usize, source: usize) -> Option<&str> {
        self.rows
            .get(*self.visible.get(row)?)?
            .get(source)?
            .as_deref()
    }

    fn own_row(&self, row: usize) -> bool {
        self.own
            .is_some_and(|own| matches!(self.rows[row][own].as_deref(), Some("t" | "1" | "true")))
    }

    /// Whether a row on screen is this tab's own session.
    fn is_own(&self, row: usize) -> bool {
        self.visible.get(row).is_some_and(|&row| self.own_row(row))
    }

    /// A header for people: `size_bytes` is "Size", `index_bytes` is
    /// "Index size", `waiting_on` is "Waiting on".
    fn header(&self, column: usize) -> String {
        let name = &self.columns[self.shown[column]].name;
        let words = match name.strip_suffix("_bytes") {
            Some("size") => "size".to_string(),
            Some(what) => format!("{what} size"),
            None => name.clone(),
        }
        .replace('_', " ");
        let mut chars = words.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }

    /// Whether a column of `columns` holds numbers, right-aligned. The
    /// connection limit reads "no limit" more often than not: it stays with
    /// its neighbours, on the left.
    fn right_aligned(&self, source: usize) -> bool {
        let column = &self.columns[source];
        column.name != "connection_limit"
            && (column.kind.is_numeric() || column.name.ends_with("_bytes"))
    }

    /// What a cell shows, by a row of `rows` and a column on screen.
    fn text_of(&self, row: usize, column: usize) -> String {
        let source = self.shown[column];
        let described = &self.columns[source];
        let Some(text) = self.rows[row][source].as_deref() else {
            // The server sends no limit as NULL.
            return if described.name == "connection_limit" {
                "no limit".into()
            } else {
                String::new()
            };
        };
        if described.name.ends_with("_bytes")
            && let Ok(bytes) = text.parse::<u64>()
        {
            return byte_size(bytes);
        }
        let shown = kerno_core::format::display(Some(text.as_bytes()), described, 300).text;
        // To the second on screen; the copy keeps the fraction.
        match described.kind {
            TypeKind::DateTime | TypeKind::DateTimeTz | TypeKind::Time => to_the_second(&shown),
            _ => shown.into_owned(),
        }
    }

    /// What a cell on screen shows.
    fn text(&self, row: usize, column: usize) -> String {
        self.visible
            .get(row)
            .map(|&row| self.text_of(row, column))
            .unwrap_or_default()
    }

    /// What a cell on screen says: its text, and on this tab's own session
    /// a mark.
    fn label(&self, row: usize, column: usize) -> String {
        let text = self.text(row, column);
        if column == 0 && self.is_own(row) {
            format!("{text} · this tab")
        } else {
            text
        }
    }

    /// The menu of a cell: copy, and the signals on a session.
    fn cell_menu(
        table: &Entity<TableState<Self>>,
        row: usize,
        column: usize,
        mut menu: PopupMenu,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let (pane, tab, text, id, own) = {
            let grid = table.read(cx).delegate();
            (
                grid.pane,
                grid.tab.clone(),
                grid.cell_text(row, column, cx),
                grid.raw(row, 0).map(str::to_string),
                grid.is_own(row),
            )
        };
        menu = menu.item(PopupMenuItem::new("Copy").on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
        }));
        let (Pane::Sessions | Pane::Locks, Some(id), Some(view)) = (pane, id, tab.upgrade()) else {
            return menu;
        };
        let reason = signal_unavailable(view.read(cx).read_only(cx), own);
        menu = menu.separator();
        // A lock names two sessions: the one holding it first (DBeaver's
        // lock manager acts on the holder), then the one waiting.
        let targets: Vec<(Signal, String, String)> = if pane == Pane::Locks {
            let blocker = {
                let grid = table.read(cx).delegate();
                grid.column_named("blocking_id")
                    .and_then(|column| grid.raw(row, column))
                    .map(str::to_string)
            };
            let mut targets = Vec::new();
            if let Some(blocker) = blocker {
                targets.push((
                    Signal::Cancel,
                    blocker.clone(),
                    "Cancel blocking query".into(),
                ));
                targets.push((
                    Signal::Terminate,
                    blocker,
                    "Terminate blocking session".into(),
                ));
            }
            targets.push((Signal::Cancel, id.clone(), "Cancel waiting query".into()));
            targets.push((Signal::Terminate, id, "Terminate waiting session".into()));
            targets
        } else {
            [Signal::Cancel, Signal::Terminate]
                .map(|signal| (signal, id.clone(), signal.label().to_string()))
                .into()
        };
        for (signal, id, label) in targets {
            let label = match reason {
                Some(reason) => format!("{label} ({reason})"),
                None => label,
            };
            let (view, id) = (view.clone(), id.clone());
            menu = menu.item(
                PopupMenuItem::new(SharedString::from(label))
                    .disabled(reason.is_some())
                    .on_click(move |_, window, cx| {
                        let (view, id) = (view.clone(), id.clone());
                        // Once the menu has closed.
                        window.defer(cx, move |window, cx| {
                            view.update(cx, |this, cx| this.ask(signal, id, window, cx))
                        });
                    }),
            );
        }
        menu
    }

    /// What the list says when it shows nothing.
    fn empty_text(&self) -> String {
        if !self.filter.is_empty() && !self.rows.is_empty() {
            return format!(
                "Nothing matches “{}”. Clear the filter to see every row.",
                self.filter
            );
        }
        match (self.pane, &self.only) {
            (Pane::Grants, None) => "Select a role above to see what it was granted.".into(),
            (Pane::Grants, Some(grantee)) => format!(
                "No privilege granted explicitly to {grantee}. Owners hold every privilege on \
                 their own objects without a grant."
            ),
            (Pane::Tables, _) => "No table in this database.".into(),
            (Pane::Sessions, _) if self.hidden_idle > 0 => format!(
                "Every session waits for its next statement: {} hidden. Turn the filter of \
                 idle sessions off to see them.",
                self.hidden_idle
            ),
            (Pane::Locks, _) => "No session is waiting for a lock. A session shows here, with \
                                 the one in its way, while it waits for a row or table another \
                                 session holds. Refresh to look again."
                .into(),
            (pane, _) => format!("No {} on this server.", pane.noun()),
        }
    }
}

impl TableDelegate for AdminGrid {
    fn columns_count(&self, _: &App) -> usize {
        self.shown.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.visible.len()
    }

    fn column(&self, index: usize, _: &App) -> Column {
        let source = self.shown[index];
        let column = Column::new(format!("column-{index}"), self.header(index))
            .width(self.widths[index])
            .min_width(px(32.))
            .resizable(true)
            .movable(false)
            .sortable();
        let numeric = self.right_aligned(source);
        if numeric { column.text_right() } else { column }
    }

    fn perform_sort(
        &mut self,
        column: usize,
        sort: ColumnSort,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let before = self.visible.clone();
        self.sort = Some((column, sort));
        self.recompute();
        // The table keeps the selected index: the selection follows its row,
        // so an action on it never lands on another session.
        cx.defer_in(window, move |table, _, cx| {
            let Some((row, column)) = selected(table) else {
                return;
            };
            let moved = before.get(row).and_then(|source| {
                table
                    .delegate()
                    .visible
                    .iter()
                    .position(|shown| shown == source)
            });
            match (moved, column) {
                (Some(new), _) if new == row => {}
                (Some(new), Some(column)) => table.set_selected_cell(new, column, cx),
                (Some(new), None) => table.set_selected_row(new, cx),
                (None, _) => table.clear_selection(cx),
            }
        });
        cx.notify();
    }

    fn render_tr(
        &mut self,
        row: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let tr = div().id(("row", row));
        if self.is_own(row) {
            tr.bg(cx.theme().accent)
        } else {
            tr
        }
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let own = column == 0 && self.is_own(row);
        let text = self.text(row, column);
        let table = cx.entity();
        let source = self.shown[column];
        let numeric = self.right_aligned(source);
        // This tab's own session: a dot before its first cell (its words,
        // " · this tab", are the cell's label), never cutting the cell.
        h_flex()
            .id((self.pane.cell_id(), (row << 12) | column))
            .test_support()
            .w_full()
            .gap_1()
            .when(numeric, |cell| cell.justify_end())
            .when(own, |cell| {
                cell.child(
                    div()
                        .flex_none()
                        .size(px(6.))
                        .rounded_full()
                        .bg(theme.primary),
                )
            })
            // The text's style stays on the text: the menu, drawn inside the
            // element that opens it, would inherit it.
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .when(own, |text| text.font_semibold())
                    .when(self.raw(row, source).is_none(), |text| {
                        text.text_color(theme.muted_foreground)
                    })
                    .child(SharedString::from(text)),
            )
            // The table keeps a right click on a cell to itself: each cell
            // opens its own menu, built when it opens.
            .context_menu(move |menu, _, cx| Self::cell_menu(&table, row, column, menu, cx))
    }

    fn cell_label(&self, row: usize, column: usize, _: &App) -> Option<SharedString> {
        Some(SharedString::from(self.label(row, column)))
    }

    /// The server's text: a size in bytes, a whole statement.
    fn cell_text(&self, row: usize, column: usize, _: &App) -> String {
        self.shown
            .get(column)
            .and_then(|&source| self.raw(row, source))
            .unwrap_or_default()
            .to_string()
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .id(("admin-empty", self.pane as usize))
            .test_support()
            .size_full()
            .flex()
            .justify_center()
            .pt_6()
            .px_4()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from(self.empty_text()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_are_refused_on_a_read_only_connection_and_to_this_tab() {
        assert_eq!(signal_unavailable(false, false), None);
        assert_eq!(signal_unavailable(false, true), Some("this tab's session"));
        assert_eq!(
            signal_unavailable(true, false),
            Some("read-only connection")
        );
        assert_eq!(signal_unavailable(true, true), Some("read-only connection"));
    }

    fn grid(pane: Pane, columns: &[(&str, TypeKind)], rows: &[&[Option<&str>]]) -> AdminGrid {
        let mut grid = AdminGrid::new(pane, WeakEntity::new_invalid());
        grid.set_result(
            columns
                .iter()
                .map(|(name, kind)| ResultColumn::new(*name, "t", *kind))
                .collect(),
            rows.iter()
                .map(|row| row.iter().map(|cell| cell.map(str::to_string)).collect())
                .collect(),
        );
        grid
    }

    #[test]
    fn sizes_show_for_people_and_sort_by_bytes() {
        let mut sizes = grid(
            Pane::Databases,
            &[
                ("database", TypeKind::Text),
                ("size_bytes", TypeKind::Integer),
                ("size", TypeKind::Text),
            ],
            &[
                &[Some("small"), Some("900"), Some("900 bytes")],
                &[Some("big"), Some("3145728"), Some("3072 kB")],
                &[Some("mid"), Some("20480"), Some("20 kB")],
                &[Some("closed"), None, None],
            ],
        );
        // The server's own pretty size is left out for the one from bytes.
        assert_eq!(sizes.shown, [0, 1]);
        assert_eq!(sizes.header(1), "Size");
        assert_eq!(sizes.text(1, 1), "3.0 MB");
        assert_eq!(sizes.raw(1, 1), Some("3145728"));
        sizes.sort = Some((1, ColumnSort::Descending));
        sizes.recompute();
        let order: Vec<String> = (0..4).map(|row| sizes.text(row, 0)).collect();
        assert_eq!(order, ["big", "mid", "small", "closed"]);
        sizes.set_filter(" KB ");
        assert_eq!(
            (0..sizes.visible.len())
                .map(|row| sizes.text(row, 0))
                .collect::<Vec<_>>(),
            ["mid"]
        );
    }

    #[test]
    fn the_own_session_is_a_mark_and_rows_are_found_again_by_key() {
        let sessions = grid(
            Pane::Sessions,
            &[
                ("pid", TypeKind::Integer),
                ("user", TypeKind::Text),
                ("this_session", TypeKind::Boolean),
                ("waiting_on", TypeKind::Text),
            ],
            &[
                &[Some("12"), Some("kerno"), Some("f"), None],
                &[Some("34"), Some("kerno"), Some("t"), None],
            ],
        );
        assert_eq!(sessions.shown, [0, 1, 3]);
        assert_eq!(sessions.header(2), "Waiting on");
        assert!(!sessions.is_own(0) && sessions.is_own(1));
        assert_eq!(sessions.find(&sessions.key(1)), Some(1));
        assert_eq!(
            sessions.find(&[Some("99".into()), Some("kerno".into())]),
            None
        );
    }

    #[test]
    fn times_show_to_the_second_and_copy_in_full() {
        assert_eq!(
            to_the_second("2026-11-13 02:37:45.132677+00"),
            "2026-11-13 02:37:45+00"
        );
        assert_eq!(to_the_second("02:37:45.5"), "02:37:45");
        assert_eq!(to_the_second("2026-11-13 02:37:45"), "2026-11-13 02:37:45");
        let sessions = grid(
            Pane::Sessions,
            &[
                ("pid", TypeKind::Integer),
                ("connected_at", TypeKind::DateTimeTz),
                ("this_session", TypeKind::Boolean),
            ],
            &[&[
                Some("129864"),
                Some("2026-11-13 02:37:45.132677+00"),
                Some("t"),
            ]],
        );
        assert_eq!(sessions.text(0, 1), "2026-11-13 02:37:45+00");
        // What a copy takes (`cell_text`): the server's text.
        assert_eq!(
            sessions.raw(0, sessions.shown[1]),
            Some("2026-11-13 02:37:45.132677+00")
        );
    }

    #[test]
    fn the_own_session_mark_takes_room_of_its_own() {
        let columns = [
            ("pid", TypeKind::Integer),
            ("this_session", TypeKind::Boolean),
        ];
        let row: &[Option<&str>] = &[Some("129864"), Some("t")];
        let marked = grid(Pane::Sessions, &columns, &[row]);
        let plain = grid(Pane::Sessions, &columns[..1], &[&row[..1]]);
        // The pid keeps the width it needs; the dot has its own room.
        assert_eq!(marked.widths[0], plain.widths[0] + OWN_MARK_WIDTH);
        assert_eq!(marked.text(0, 0), "129864");
        assert_eq!(marked.label(0, 0), "129864 · this tab");
    }

    #[test]
    fn no_connection_limit_says_so() {
        let roles = grid(
            Pane::Roles,
            &[
                ("role", TypeKind::Text),
                ("connection_limit", TypeKind::Integer),
            ],
            &[&[Some("kerno"), None], &[Some("app"), Some("10")]],
        );
        assert_eq!(roles.text(0, 1), "no limit");
        assert_eq!(roles.text(1, 1), "10");
        // Aligned with the role's other columns, not as a number.
        assert!(!roles.right_aligned(1));
    }

    #[test]
    fn grants_show_the_selected_role_only() {
        let mut grants = grid(
            Pane::Grants,
            &[("grantee", TypeKind::Text), ("object", TypeKind::Text)],
            &[
                &[Some("a"), Some("orders")],
                &[Some("b"), Some("audit_log")],
            ],
        );
        assert!(grants.visible.is_empty());
        assert!(grants.empty_text().starts_with("Select a role"));
        grants.only = Some("b".into());
        grants.recompute();
        assert_eq!(grants.text(0, 1), "audit_log");
        assert_eq!(grants.visible.len(), 1);
    }
}
