//! The main toolbar under the title bar (CLAUDE.md §3 item 11). DBeaver's
//! main toolbars give the items, their order and their groups
//! (`org.jkiss.dbeaver.core/plugin.xml`: `dbeaver-general` New connection |
//! Connect, Invalidate/Reconnect, Disconnect | the SQL editor button of
//! `ui.editors.sql/plugin.xml`; `dbeaver-transactions` Commit, Rollback, the
//! transaction mode of `DataSourceTransactionModeContributor` with the
//! isolation levels; `dbeaver-transaction-monitor` Transaction log,
//! Read-only; then the active connection and schema selectors of
//! `SelectActiveDataSourceHandler` and `SelectActiveSchemaHandler`, and the
//! tools). Kerno draws them with its own components and theme.
//!
//! Each item runs a command of `actions::COMMANDS` (or opens a list), on
//! the connection the connection commands use: the active tab's, else the
//! one selected in the sidebar. An item that does not apply is greyed and
//! says why, with the words the menus use.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _, DropdownButton};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, AnyElement, App, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, TestSupportExt as _, Window, div, px,
};
use kerno_core::{Engine, TxState};

use super::{OpenTab, Workspace, connection_target};
use crate::actions;
use crate::connections::dialog::ConnectionDialog;
use crate::connections::manager::Status;
use crate::engine_logo::engine_logo;
use crate::navigator::actions::{tool_entries, tools_now};
use crate::sidebar::{SidebarAction, Subject};
use crate::state::AppState;
use crate::tabs::{TabAction, Target};

/// An item of the main toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    NewConnection,
    Connect,
    Reconnect,
    Disconnect,
    /// New SQL editor, with the recent scripts.
    NewEditor,
    Commit,
    Rollback,
    /// Auto or manual commit, and the isolation level.
    Mode,
    /// The statements of the open transaction.
    Log,
    ReadOnly,
    /// The active tab's connection.
    Connection,
    /// The active tab's database and schema, shown `schema@connection`.
    Target,
    Dashboard,
    Tools,
    /// Open table (DBeaver's search of database objects).
    Search,
}

use Item::*;

/// The groups of the toolbar, left to right, as DBeaver's toolbars.
pub const GROUPS: [&[Item]; 7] = [
    &[NewConnection],
    &[Connect, Reconnect, Disconnect],
    &[NewEditor],
    &[Commit, Rollback, Mode],
    &[Log, ReadOnly],
    &[Connection, Target],
    &[Dashboard, Tools, Search],
];

impl Item {
    /// Every item, in order.
    pub fn all() -> impl Iterator<Item = Item> {
        GROUPS.into_iter().flatten().copied()
    }

    /// Its id: `toolbar-<key>` is its element, and the key is what the
    /// settings store when it is hidden.
    pub fn key(self) -> &'static str {
        match self {
            NewConnection => "new-connection",
            Connect => "connect",
            Reconnect => "reconnect",
            Disconnect => "disconnect",
            NewEditor => "new-editor",
            Commit => "commit",
            Rollback => "rollback",
            Mode => "mode",
            Log => "log",
            ReadOnly => "read-only",
            Connection => "connection",
            Target => "target",
            Dashboard => "dashboard",
            Tools => "tools",
            Search => "search",
        }
    }

    /// The id of its element.
    pub fn element_id(self) -> SharedString {
        SharedString::from(format!("toolbar-{}", self.key()))
    }

    /// Its name, for screen readers, its tooltip and the toolbar's menu;
    /// worded apart from the menus' entries.
    pub fn name(self) -> &'static str {
        match self {
            NewConnection => "New database connection",
            Connect => "Connect to the server",
            Reconnect => "Reconnect to the server",
            Disconnect => "Disconnect from the server",
            NewEditor => "Open a SQL editor",
            Commit => "Commit the transaction",
            Rollback => "Roll back the transaction",
            Mode => "Transaction mode",
            Log => "Show the transaction log",
            ReadOnly => "Read-only connection",
            Connection => "Choose the active connection",
            Target => "Choose the active database and schema",
            Dashboard => "Open the dashboard",
            Tools => "Database tools",
            Search => "Find a table to open",
        }
    }

    /// The command it runs, whose shortcut its tooltip shows; `None` for
    /// the lists (the pickers and Tools ▸).
    pub fn command(self) -> Option<&'static str> {
        Some(match self {
            NewConnection => "new_connection",
            Connect => "connect",
            Reconnect => "reconnect_tab_connection",
            Disconnect => "disconnect_tab_connection",
            NewEditor => "new_query_tab",
            Commit => "commit_transaction",
            Rollback => "rollback_transaction",
            Mode => "toggle_commit_mode",
            Log => "transaction_log",
            ReadOnly => "toggle_read_only",
            Dashboard => "open_admin",
            Search => "quick_open",
            Connection | Target | Tools => return None,
        })
    }

    /// The action of its command, for the shortcut in its tooltip.
    fn action(self) -> Option<Box<dyn Action>> {
        Some(match self {
            NewConnection => Box::new(actions::NewConnection),
            Connect => Box::new(actions::Connect),
            Reconnect => Box::new(actions::ReconnectTabConnection),
            Disconnect => Box::new(actions::DisconnectTabConnection),
            NewEditor => Box::new(actions::NewQueryTab),
            Commit => Box::new(actions::CommitTransaction),
            Rollback => Box::new(actions::RollbackTransaction),
            Mode => Box::new(actions::ToggleCommitMode),
            Log => Box::new(actions::ShowTransactionLog),
            ReadOnly => Box::new(actions::ToggleReadOnly),
            Dashboard => Box::new(actions::OpenAdmin),
            Search => Box::new(actions::QuickOpen),
            Connection | Target | Tools => return None,
        })
    }

    fn icon(self) -> IconName {
        match self {
            NewConnection => IconName::DatabasePlus,
            Connect => IconName::PlugZap,
            Reconnect => IconName::RefreshCw,
            Disconnect => IconName::Unplug,
            NewEditor => IconName::FileCode,
            Commit => IconName::Check,
            Rollback => IconName::Undo2,
            Log => IconName::ScrollText,
            ReadOnly => IconName::Lock,
            Dashboard => IconName::LayoutDashboard,
            Tools => IconName::Wrench,
            Search => IconName::Search,
            Mode | Connection | Target => IconName::Database,
        }
    }

    /// Its width at its narrowest, in pixels: a 16 px glyph in a compact
    /// button, a caret beside a split one, a picker at its shortest.
    fn min_width(self) -> f32 {
        match self {
            NewConnection | NewEditor => ICON + CARET,
            Tools => ICON + CARET,
            Mode => 76.,
            Connection | Target => PICKER,
            _ => ICON,
        }
    }
}

const ICON: f32 = 28.;
const CARET: f32 = 24.;
const PICKER: f32 = 96.;
/// The space between two groups (no rule is drawn: owner's item 1).
const GROUP_GAP: f32 = 12.;
/// The row's padding, left and right.
const PADDING: f32 = 16.;
/// The "»" button, and its gap.
const MORE: f32 = ICON + GROUP_GAP;

/// How many of the groups, whose narrowest widths are `groups`, fit in a
/// row `width` wide; when some do not, room is kept for "»", which holds
/// the rest. The pickers shrink before any group goes.
pub fn fitting(groups: &[f32], width: f32) -> usize {
    let gaps = GROUP_GAP * groups.len().saturating_sub(1) as f32;
    if PADDING + groups.iter().sum::<f32>() + gaps <= width {
        return groups.len();
    }
    let mut used = PADDING + MORE;
    let mut count = 0;
    for (index, group) in groups.iter().enumerate() {
        used += group + if index > 0 { GROUP_GAP } else { 0. };
        if used > width {
            break;
        }
        count += 1;
    }
    count
}

/// Where the connection the items act on stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Link {
    /// No tab on a connection, none selected in the sidebar.
    #[default]
    None,
    Closed,
    Connecting,
    Open,
}

impl Link {
    fn of(status: &Status) -> Self {
        match status {
            Status::Connected { .. } => Link::Open,
            Status::Connecting | Status::WaitingForInput => Link::Connecting,
            Status::Disconnected | Status::Failed { .. } => Link::Closed,
        }
    }
}

/// What the items depend on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Facts {
    pub link: Link,
    /// The active tab is a query tab: its transaction, whether a statement
    /// runs, and whether it runs on a connection.
    pub query: Option<QueryFacts>,
    /// The active tab is about one object (a table's data or structure, a
    /// table form, a server's administration): its place cannot change.
    pub fixed: bool,
    /// The connection is a database file (SQLite): no server to administer
    /// and no client tools yet.
    pub file: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryFacts {
    pub transaction: TxState,
    pub running: bool,
    pub target: bool,
}

/// Why `item` cannot be used now, when it cannot; the words of the
/// sidebar's connection menu and of the status bar's menus.
pub fn why_not(item: Item, facts: Facts) -> Option<&'static str> {
    let no_query = facts.query.is_none().then_some("no query tab");
    // The pickers show where any tab is; only a query tab's can change
    // (the tab menu's words).
    let picker = |query: Option<&'static str>| match facts.query {
        Some(_) => query,
        None if facts.fixed => Some("query tabs only"),
        None => Some("no query tab"),
    };
    let link = |open: Option<&'static str>| match facts.link {
        Link::None => Some("no connection"),
        Link::Open => open,
        Link::Closed | Link::Connecting => Some("not connected"),
    };
    let transaction = |commit: bool| {
        let query = facts.query?;
        match query.transaction {
            TxState::Idle => Some("no transaction open"),
            _ if query.running => Some("running"),
            TxState::Failed if commit => Some("failed: roll back"),
            _ => None,
        }
    };
    match item {
        NewConnection | NewEditor => None,
        Connect => match facts.link {
            Link::None => Some("no connection"),
            Link::Open => Some("connected"),
            Link::Connecting => Some("connecting"),
            Link::Closed => None,
        },
        Disconnect => match facts.link {
            Link::None => Some("no connection"),
            Link::Closed => Some("not connected"),
            Link::Open | Link::Connecting => None,
        },
        Dashboard | Tools if facts.file => Some("a database file has no server tools"),
        Reconnect | Dashboard | Search => link(None),
        ReadOnly | Tools => (facts.link == Link::None).then_some("no connection"),
        Commit => no_query.or_else(|| transaction(true)),
        Rollback => no_query.or_else(|| transaction(false)),
        Mode | Log => no_query,
        Connection => picker(None),
        Target => picker(if facts.query.is_some_and(|query| !query.target) {
            Some("no connection")
        } else {
            link(None)
        }),
    }
}

/// The tooltip of an item: its name, and why it is greyed.
fn tooltip(item: Item, reason: Option<&'static str>) -> SharedString {
    match reason {
        Some(reason) => format!("{} ({reason})", item.name()).into(),
        None => item.name().into(),
    }
}

impl Workspace {
    /// Registers the toolbar's commands on the window's root.
    pub(super) fn toolbar_actions(
        &self,
        root: gpui_kit::Stateful<gpui_kit::Div>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        root.on_action(cx.listener(|this, _: &actions::Connect, window, cx| {
            this.run_item(Item::Connect, window, cx)
        }))
        .on_action(cx.listener(|_, _: &actions::ToggleMainToolbar, _, cx| {
            AppState::update_settings(cx, |settings| {
                settings.toolbar_visible = !settings.toolbar_visible
            });
            cx.notify();
        }))
    }

    /// What the items depend on, now.
    pub(crate) fn toolbar_facts(&self, cx: &gpui_kit::App) -> Facts {
        let link = self.command_connection(cx).map_or(Link::None, |id| {
            Link::of(&self.connections.read(cx).status(&id))
        });
        let query = self.active_query_tab().map(|tab| {
            let tab = tab.read(cx);
            QueryFacts {
                transaction: tab.tx_state(),
                running: tab.running(),
                target: tab.target().is_some(),
            }
        });
        let fixed = matches!(
            self.active_open(),
            Some(
                OpenTab::Data(_)
                    | OpenTab::Structure(_)
                    | OpenTab::Admin(_)
                    | OpenTab::TableEditor(_)
            )
        );
        let file = self.command_connection(cx).is_some_and(|id| {
            self.connections
                .read(cx)
                .get(&id)
                .is_some_and(|saved| saved.config.engine.is_file())
        });
        Facts {
            link,
            query,
            fixed,
            file,
        }
    }

    /// Notes whether the keyboard was last in the tree or in a tab: the
    /// toolbar's "New SQL editor" opens on the tree's selected connection
    /// after the one, on the active tab's after the other, as DBeaver does
    /// (K56). A toolbar button taking the focus changes neither.
    pub(super) fn note_editor_place(&mut self, window: &Window, cx: &App) {
        let Some(focused) = window.focused(cx) else {
            return;
        };
        if self.navigator(cx).read(cx).is_tree_focus(&focused) {
            self.editor_on_tree = true;
        } else if self
            .tabs
            .iter()
            .any(|tab| tab.focus_handle(cx).contains(&focused, window))
        {
            self.editor_on_tree = false;
        }
    }

    /// The connection the toolbar's "New SQL editor" opens on: the tree's
    /// selected one when the keyboard was last there, otherwise the
    /// active tab's (or, without one, the tree's).
    pub(crate) fn editor_connection(&self, cx: &App) -> Option<String> {
        let tree = self
            .navigator(cx)
            .read(cx)
            .selected_connection(cx)
            .filter(|id| self.connections.read(cx).get(id).is_some());
        tree.filter(|_| self.editor_on_tree)
            .or_else(|| self.command_connection(cx))
    }

    /// Where the active tab works, whatever its kind; a table's tab is on
    /// its table's schema (its database on MySQL).
    fn shown_target(&self, cx: &App) -> Option<Target> {
        let tab = self.active_open()?;
        let mut target = tab.target(cx)?;
        let table = match tab {
            OpenTab::Data(tab) => Some(tab.read(cx).table().clone()),
            OpenTab::Structure(tab) => Some(tab.read(cx).table().clone()),
            OpenTab::TableEditor(tab) => tab.read(cx).table().cloned(),
            _ => None,
        };
        if let Some(table) = table {
            target.schema = Some(table.schema);
        }
        Some(target)
    }

    /// What a picker shows: the active tab's connection, or its
    /// `schema@connection` (`database@connection` without a schema).
    pub(crate) fn picker_text(&self, item: Item, cx: &App) -> String {
        let target = self.shown_target(cx);
        let connections = self.connections.read(cx);
        let name = target
            .as_ref()
            .and_then(|target| connections.get(&target.connection))
            .map(|saved| saved.config.name.clone());
        if item == Connection {
            return name.unwrap_or_else(|| "No connection".into());
        }
        let place = target.as_ref().and_then(|target| {
            target.schema.clone().or_else(|| {
                target.database.clone().or_else(|| {
                    connections
                        .meta_session(&target.connection)
                        .and_then(|meta| meta.database())
                })
            })
        });
        match (place, name) {
            (Some(place), Some(name)) => format!("{place}@{name}"),
            _ => "No database".into(),
        }
    }

    /// Does what an item says.
    fn run_item(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        let connection = match item {
            NewEditor => self.editor_connection(cx),
            _ => self.command_connection(cx),
        };
        if let Some(reason) = why_not(item, self.toolbar_facts(cx)) {
            // From the palette: say why, do nothing.
            let label = actions::COMMANDS
                .iter()
                .find(|command| Some(command.id) == item.command())
                .map_or(item.name(), |command| command.label);
            window.push_notification(
                gpui_kit::component::notification::Notification::warning(SharedString::from(
                    format!("{}: {reason}.", label.trim_end_matches('…')),
                )),
                cx,
            );
            return;
        }
        let on_connection = |action: SidebarAction,
                             this: &mut Self,
                             window: &mut Window,
                             cx: &mut Context<Self>| {
            if let Some(id) = connection.clone() {
                this.sidebar.update(cx, |sidebar, cx| {
                    sidebar.perform(&Subject::Connection(id), action, window, cx)
                })
            }
        };
        match item {
            NewConnection => self.new_connection(&actions::NewConnection, window, cx),
            Connect => on_connection(SidebarAction::Connect, self, window, cx),
            Reconnect => on_connection(SidebarAction::Reconnect, self, window, cx),
            Disconnect => on_connection(SidebarAction::Disconnect, self, window, cx),
            NewEditor if connection.is_some() => {
                on_connection(SidebarAction::NewSqlEditor, self, window, cx)
            }
            NewEditor => self.new_query_tab(&actions::NewQueryTab, window, cx),
            Commit => self.perform(None, TabAction::Commit, window, cx),
            Rollback => self.perform(None, TabAction::Rollback, window, cx),
            Log => self.perform(None, TabAction::TransactionLog, window, cx),
            ReadOnly => self.toggle_read_only(&actions::ToggleReadOnly, window, cx),
            Dashboard => self.open_admin_command(&actions::OpenAdmin, window, cx),
            Search => self.quick_open(&actions::QuickOpen, window, cx),
            // Lists: their entries act.
            Mode | Connection | Target | Tools => {}
        }
    }
}

// Drawing ----------------------------------------------------------------------

/// A small ghost button holding the item's 16 px glyph, greyed with its
/// reason, its tooltip naming it and its shortcut (from the live keymap).
fn icon_button(item: Item, reason: Option<&'static str>) -> Button {
    with_tooltip(
        Button::new(item.element_id())
            .ghost()
            .small()
            .compact()
            .accessibility_label(item.name())
            .child(Icon::new(item.icon()).size_4())
            .disabled(reason.is_some()),
        item,
        reason,
    )
}

fn with_tooltip(button: Button, item: Item, reason: Option<&'static str>) -> Button {
    let text = tooltip(item, reason);
    match item.action() {
        Some(action) => button.tooltip_with_action(text, action.as_ref(), None),
        None => button.tooltip(text),
    }
}

/// Runs `item` on the workspace once the menu that chose it has closed.
fn later(
    workspace: &Entity<Workspace>,
    run: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let workspace = workspace.clone();
    let run = std::cell::RefCell::new(Some(run));
    move |_, window, cx| {
        if let Some(run) = run.borrow_mut().take() {
            let workspace = workspace.clone();
            window.defer(cx, move |window, cx| {
                workspace.update(cx, |this, cx| run(this, window, cx))
            });
        }
    }
}

impl Workspace {
    /// The toolbar row: the items not hidden, in their groups, those that
    /// do not fit in "»".
    pub(super) fn render_toolbar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (background, border) = (cx.theme().title_bar, cx.theme().title_bar_border);
        let facts = self.toolbar_facts(cx);
        let hidden = &AppState::get(cx).settings.toolbar_hidden;
        let groups: Vec<Vec<Item>> = GROUPS
            .iter()
            .map(|group| {
                group
                    .iter()
                    .copied()
                    .filter(|item| !hidden.contains(item.key()))
                    .collect::<Vec<_>>()
            })
            .filter(|group| !group.is_empty())
            .collect();
        let widths: Vec<f32> = groups
            .iter()
            .map(|group| group.iter().map(|item| item.min_width()).sum())
            .collect();
        let shown = fitting(&widths, f32::from(window.viewport_size().width));
        let overflow: Vec<Item> = groups[shown..].iter().flatten().copied().collect();
        let workspace = cx.entity();
        let rows: Vec<AnyElement> = groups[..shown]
            .iter()
            .map(|group| {
                let pickers = group.contains(&Connection) || group.contains(&Target);
                h_flex()
                    .gap_0p5()
                    .items_center()
                    .map(|row| {
                        if pickers {
                            row.flex_shrink(1.).min_w_0()
                        } else {
                            row.flex_none()
                        }
                    })
                    .children(group.iter().map(|item| self.render_item(*item, facts, cx)))
                    .into_any_element()
            })
            .collect();
        h_flex()
            .id("main-toolbar")
            .test_support()
            .flex_none()
            .w_full()
            .h(px(34.))
            .px_2()
            .gap(px(GROUP_GAP))
            .items_center()
            .overflow_hidden()
            .bg(background)
            .border_b_1()
            .border_color(border)
            .context_menu({
                let workspace = workspace.clone();
                move |menu, _, cx| customise_menu(&workspace, menu, cx)
            })
            .children(rows)
            .when(!overflow.is_empty(), |row| {
                row.child(div().flex_1()).child(
                    Button::new("toolbar-more")
                        .ghost()
                        .small()
                        .compact()
                        .accessibility_label("More toolbar items")
                        .tooltip("More toolbar items")
                        .child(Icon::new(IconName::ChevronsRight).size_4())
                        .dropdown_menu(move |menu, window, cx| {
                            overflow_menu(&workspace, &overflow, menu, window, cx)
                        }),
                )
            })
    }

    fn render_item(&self, item: Item, facts: Facts, cx: &mut Context<Self>) -> AnyElement {
        let reason = why_not(item, facts);
        let workspace = cx.entity();
        match item {
            NewConnection | NewEditor => {
                let (menu_id, menu_label) = match item {
                    NewConnection => ("toolbar-new-connection-menu", "New connection of an engine"),
                    _ => ("toolbar-new-editor-menu", "Recent SQL scripts"),
                };
                // The editor's tooltip names the connection it opens on (K56).
                let button = match item {
                    NewEditor => {
                        let on = self.editor_connection(cx).and_then(|id| {
                            self.connections
                                .read(cx)
                                .get(&id)
                                .map(|saved| saved.config.name.clone())
                        });
                        let text = match on {
                            Some(name) => format!("{} on “{name}”", item.name()).into(),
                            None => tooltip(item, reason),
                        };
                        icon_button(item, reason).tooltip_with_action(
                            text,
                            &actions::NewQueryTab,
                            None,
                        )
                    }
                    _ => icon_button(item, reason),
                };
                DropdownButton::new(menu_id)
                    .ghost()
                    .small()
                    .button(button.on_click(
                        cx.listener(move |this, _, window, cx| this.run_item(item, window, cx)),
                    ))
                    .menu_accessibility_label(menu_label)
                    .dropdown_menu(move |menu, window, cx| match item {
                        NewConnection => engines_menu(&workspace, menu, cx),
                        _ => recent_menu(&workspace, menu, window, cx),
                    })
                    .into_any_element()
            }
            Mode => self.render_mode(reason, cx),
            Connection | Target => self.render_picker(item, reason, cx),
            Tools => icon_button(item, reason)
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, cx| tools_menu(&workspace, menu, cx))
                .into_any_element(),
            ReadOnly => {
                let read_only = self
                    .command_connection(cx)
                    .and_then(|id| {
                        self.connections
                            .read(cx)
                            .get(&id)
                            .map(|saved| saved.config.read_only)
                    })
                    .unwrap_or(false);
                let button = Button::new(item.element_id())
                    .ghost()
                    .small()
                    .compact()
                    .accessibility_label(item.name())
                    .child(
                        Icon::new(if read_only {
                            IconName::Lock
                        } else {
                            IconName::LockOpen
                        })
                        .size_4(),
                    )
                    .selected(read_only)
                    .toggled(read_only)
                    .disabled(reason.is_some())
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.run_item(item, window, cx)),
                    );
                with_tooltip(button, item, reason).into_any_element()
            }
            _ => icon_button(item, reason)
                .on_click(cx.listener(move |this, _, window, cx| this.run_item(item, window, cx)))
                .into_any_element(),
        }
    }

    /// "Auto" or "Manual"; tinted while a transaction is open, with the
    /// environment's colour when it has one.
    fn render_mode(&self, reason: Option<&'static str>, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.active_query_tab().map(|tab| tab.read(cx));
        let manual = tab.is_some_and(|tab| tab.manual_commit());
        let open = tab.is_some_and(|tab| tab.tx_state() != TxState::Idle);
        let isolation = tab.and_then(|tab| tab.isolation());
        let text = match (manual, isolation) {
            (false, _) => "Transaction mode: auto-commit".to_string(),
            (true, Some(level)) => format!("Transaction mode: manual commit ({})", level.label()),
            (true, None) => "Transaction mode: manual commit".to_string(),
        };
        let text = match reason {
            Some(reason) => format!("{text} ({reason})"),
            None => text,
        };
        let chrome = self.active_chrome(cx);
        let warning = cx.theme().warning;
        let workspace = cx.entity();
        let label = div()
            .px_1()
            .rounded_sm()
            .when(open, |label| match chrome {
                Some(chrome) => label.bg(chrome.bar).text_color(chrome.bar_text),
                None => label.text_color(warning),
            })
            .child(if manual { "Manual" } else { "Auto" });
        Button::new(Mode.element_id())
            .ghost()
            .small()
            .accessibility_label(Mode.name())
            .child(label)
            .dropdown_caret(true)
            .disabled(reason.is_some())
            .tooltip_with_action(SharedString::from(text), &actions::ToggleCommitMode, None)
            .dropdown_menu(move |menu, _, cx| mode_menu(&workspace, menu, cx))
            .into_any_element()
    }

    /// The active connection, or the database and schema
    /// (`schema@connection`): names cut to the room left, whole in the
    /// tooltip; the environment's wash behind them, as the tab toolbar.
    fn render_picker(
        &self,
        item: Item,
        reason: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = self.picker_text(item, cx);
        let target = self.shown_target(cx);
        let saved = target
            .as_ref()
            .and_then(|target| self.connections.read(cx).get(&target.connection));
        let engine = saved.map(|saved| saved.config.engine);
        // The whole name, even when greyed: the button may cut it.
        let tip = match reason {
            Some(reason) => format!("{}: {text} ({reason})", item.name()),
            None => format!("{}: {text}", item.name()),
        };
        let chrome = saved.and_then(|saved| crate::theme::environment_chrome(&saved.config, cx));
        let workspace = cx.entity();
        let logo = (item == Connection)
            .then_some(engine)
            .flatten()
            .map(|engine| div().flex_none().child(engine_logo(engine, px(14.), cx)));
        Button::new(item.element_id())
            .ghost()
            .small()
            .accessibility_label(item.name())
            .flex_shrink(1.)
            .min_w(px(PICKER))
            .max_w(px(240.))
            .when_some(chrome, |button, chrome| button.bg(chrome.wash))
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1p5()
                    .children(logo)
                    .child(div().min_w_0().truncate().child(SharedString::from(text))),
            )
            .dropdown_caret(true)
            .disabled(reason.is_some())
            .tooltip(SharedString::from(tip))
            .dropdown_menu(move |menu, _, cx| match item {
                Connection => connection_menu(&workspace, menu, cx),
                _ => target_menu(&workspace, menu, cx),
            })
            .into_any_element()
    }
}

// Menus, built when they open ----------------------------------------------------

/// The engines, each opening the new-connection dialog on itself.
fn engines_menu(workspace: &Entity<Workspace>, mut menu: PopupMenu, cx: &mut App) -> PopupMenu {
    let connections = workspace.read(cx).connections.clone();
    for engine in Engine::ALL {
        let connections = connections.clone();
        menu = menu.item(
            PopupMenuItem::new(engine.label()).on_click(move |_, window, cx| {
                let connections = connections.clone();
                window.defer(cx, move |window, cx| {
                    ConnectionDialog::open_for_engine(connections, engine, window, cx)
                });
            }),
        );
    }
    menu
}

/// The last `.sql` files opened or saved; one that went away leaves the
/// list when it is picked (no file is looked at here: the UI thread never
/// waits on the disk).
fn recent_menu(
    workspace: &Entity<Workspace>,
    mut menu: PopupMenu,
    _: &mut Window,
    cx: &mut App,
) -> PopupMenu {
    let recent = AppState::get(cx).settings.recent_scripts.clone();
    if recent.is_empty() {
        menu = menu.item(PopupMenuItem::new("No recent scripts").disabled(true));
    }
    for path in recent {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let place = path
            .parent()
            .map(|dir| format!("  {}", dir.display()))
            .unwrap_or_default();
        menu = menu.item(
            PopupMenuItem::new(SharedString::from(format!("{name}{place}")))
                .on_click(later(workspace, move |this, window, cx| {
                    this.open_sql_file_from(path, window, cx)
                })),
        );
    }
    menu.separator().item(
        PopupMenuItem::new("Open SQL file…").on_click(later(workspace, |this, window, cx| {
            this.open_sql_file(&actions::OpenSqlFile, window, cx)
        })),
    )
}

/// Auto-commit or manual commit, then the engine's isolation levels with
/// the session's ticked (DBeaver's `DataSourceTransactionModeContributor`).
fn mode_menu(workspace: &Entity<Workspace>, menu: PopupMenu, cx: &mut App) -> PopupMenu {
    let this = workspace.read(cx);
    let Some(tab) = this.active_query_tab().cloned() else {
        return menu.item(PopupMenuItem::new("Open a query tab first").disabled(true));
    };
    let (manual, isolation, why_not, levels) = {
        let tab = tab.read(cx);
        let levels = tab
            .target()
            .and_then(|target| this.connections.read(cx).meta_session(&target.connection))
            .map_or(&[][..], |meta| meta.dialect().isolation_levels());
        (
            tab.manual_commit(),
            tab.isolation(),
            tab.isolation_why_not(cx),
            levels,
        )
    };
    let mode = |manual: bool, label: &'static str, checked: bool| {
        PopupMenuItem::new(label)
            .checked(checked)
            .on_click(later(workspace, move |this, window, cx| {
                this.perform(None, TabAction::ManualCommit(manual), window, cx)
            }))
    };
    let mut menu = menu
        .item(mode(false, "Auto-commit", !manual))
        .item(mode(true, "Manual commit", manual))
        .separator()
        .label("Isolation level");
    if levels.is_empty() {
        menu = menu.item(PopupMenuItem::new("Connect to list the levels").disabled(true));
    }
    for &level in levels {
        let label = match why_not {
            Some(reason) => format!("{} ({reason})", level.label()),
            None => level.label().to_string(),
        };
        let tab = tab.clone();
        menu = menu.item(
            PopupMenuItem::new(SharedString::from(label))
                .checked(isolation == Some(level))
                .disabled(why_not.is_some())
                .on_click(later(workspace, move |_, window, cx| {
                    tab.update(cx, |tab, cx| tab.set_isolation(level, window, cx))
                })),
        );
    }
    menu
}

/// The saved connections, the active tab's ticked, as the tab toolbar's
/// selector: the proposed one first while unbound, one not open said so and
/// connected at the tab's first run (C6, D-078).
fn connection_menu(workspace: &Entity<Workspace>, mut menu: PopupMenu, cx: &mut App) -> PopupMenu {
    let Some(tab) = workspace.read(cx).active_query_tab().cloned() else {
        return menu;
    };
    let choices = tab.read(cx).choices(cx);
    for (id, name, connected) in choices.connections {
        let label = if connected {
            name.clone()
        } else {
            format!("{name} (not connected)")
        };
        let checked = choices.connection.as_ref() == Some(&id);
        let tab = tab.clone();
        menu = menu.item(
            PopupMenuItem::new(SharedString::from(label))
                .checked(checked)
                // The one in use stays as it is (its database and schema).
                .on_click(later(workspace, move |_, window, cx| {
                    if !checked {
                        crate::query::tab::run_editor_action(
                            &tab,
                            TabAction::UseConnection(id, name),
                            window,
                            cx,
                        )
                    }
                })),
        );
    }
    menu
}

/// The databases of the active tab's connection, then its schemas
/// (PostgreSQL), the ones in use ticked.
fn target_menu(workspace: &Entity<Workspace>, mut menu: PopupMenu, cx: &mut App) -> PopupMenu {
    let Some(tab) = workspace.read(cx).active_query_tab().cloned() else {
        return menu;
    };
    let choices = tab.read(cx).choices(cx);
    let lists = [
        (
            "Database",
            choices.databases,
            choices.database,
            TabAction::UseDatabase as fn(String) -> TabAction,
        ),
        (
            "Schema",
            choices.schemas,
            choices.schema,
            TabAction::UseSchema,
        ),
    ];
    for (index, (label, names, current, action)) in lists.into_iter().enumerate() {
        if names.is_empty() {
            continue;
        }
        if index > 0 {
            menu = menu.separator();
        }
        menu = menu.label(label);
        for name in names {
            let checked = current.as_ref() == Some(&name);
            let tab = tab.clone();
            menu = menu.item(
                PopupMenuItem::new(SharedString::from(name.clone()))
                    .checked(checked)
                    .on_click(later(workspace, move |_, window, cx| {
                        if !checked {
                            crate::query::tab::run_editor_action(&tab, action(name), window, cx)
                        }
                    })),
            );
        }
    }
    menu
}

/// Tools ▸ of the connection the items act on: the list of the sidebar's
/// connection menu ([`tool_entries`]), groups apart.
fn tools_menu(workspace: &Entity<Workspace>, mut menu: PopupMenu, cx: &mut App) -> PopupMenu {
    let this = workspace.read(cx);
    let Some(id) = this.command_connection(cx) else {
        return menu;
    };
    let connections = this.connections.read(cx);
    let Some(saved) = connections.get(&id) else {
        return menu;
    };
    let connected = matches!(connections.status(&id), Status::Connected { .. });
    let engine = saved.config.engine;
    let groups = tools_now(
        tool_entries(None, engine, &[]),
        connected,
        saved.config.read_only,
    );
    // The active tab's database when it is on this connection; not its
    // schema: a backup from here is of the whole database (K26).
    let target = this
        .active_open()
        .and_then(|tab| tab.target(cx))
        .filter(|target| target.connection == id)
        .map(|target| Target {
            schema: None,
            ..target
        })
        .unwrap_or_else(|| connection_target(&id));
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            menu = menu.separator();
        }
        for (entry, reason) in group {
            let label = match reason {
                Some(reason) => format!("{} ({reason})", entry.label()),
                None => entry.label(),
            };
            let target: Target = target.clone();
            menu = menu.item(
                PopupMenuItem::new(SharedString::from(label))
                    .disabled(reason.is_some())
                    .on_click(later(workspace, move |this, window, cx| {
                        this.run_tool(target, entry, Vec::new(), window, cx)
                    })),
            );
        }
    }
    menu
}

/// The items that did not fit: each as its button, a list as a submenu.
fn overflow_menu(
    workspace: &Entity<Workspace>,
    items: &[Item],
    mut menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let facts = workspace.read(cx).toolbar_facts(cx);
    for &item in items {
        let reason = why_not(item, facts);
        let label = tooltip(item, reason);
        let workspace = workspace.clone();
        menu = match item {
            Mode | Connection | Target | Tools if reason.is_none() => {
                menu.submenu(label, window, cx, move |menu, _, cx| match item {
                    Mode => mode_menu(&workspace, menu, cx),
                    Connection => connection_menu(&workspace, menu, cx),
                    Target => target_menu(&workspace, menu, cx),
                    _ => tools_menu(&workspace, menu, cx),
                })
            }
            _ => menu.item(
                PopupMenuItem::new(label)
                    .disabled(reason.is_some())
                    .on_click(later(&workspace, move |this, window, cx| {
                        this.run_item(item, window, cx)
                    })),
            ),
        };
    }
    menu
}

/// The toolbar's right-click: every item with a tick, unticking hides it
/// (DBeaver's toolbar customisation), then Reset toolbar.
fn customise_menu(workspace: &Entity<Workspace>, mut menu: PopupMenu, cx: &mut App) -> PopupMenu {
    let hidden = AppState::get(cx).settings.toolbar_hidden.clone();
    for item in Item::all() {
        let shown = !hidden.contains(item.key());
        menu = menu.item(
            PopupMenuItem::new(item.name())
                .checked(shown)
                .on_click(later(workspace, move |_, _, cx| {
                    AppState::update_settings(cx, |settings| {
                        if shown {
                            settings.toolbar_hidden.insert(item.key().to_string());
                        } else {
                            settings.toolbar_hidden.remove(item.key());
                        }
                    });
                    cx.notify();
                })),
        );
    }
    menu.separator().item(
        PopupMenuItem::new("Reset toolbar")
            .disabled(hidden.is_empty())
            .on_click(later(workspace, |_, _, cx| {
                AppState::update_settings(cx, |settings| settings.toolbar_hidden.clear());
                cx.notify();
            })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar::{Entry, menu};

    #[test]
    fn every_item_runs_a_command_or_is_a_list() {
        for item in Item::all() {
            match (item.command(), item.action()) {
                (Some(id), Some(action)) => {
                    let command = actions::COMMANDS
                        .iter()
                        .find(|command| command.id == id)
                        .unwrap_or_else(|| panic!("{item:?}: no command {id}"));
                    assert_eq!((command.action)().name(), action.name(), "{item:?}");
                }
                (None, None) => assert!(
                    matches!(item, Connection | Target | Tools),
                    "{item:?} runs no command"
                ),
                _ => panic!("{item:?}: a command without its action"),
            }
        }
        assert_eq!(Item::all().count(), 15);
    }

    #[test]
    fn ids_are_unique_and_names_are_not_the_menus_words() {
        let mut keys: Vec<&str> = Item::all().map(Item::key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 15);
        // Tests and screen readers find a menu's entries by their words:
        // the toolbar never takes them.
        let words = [
            "Connect",
            "Disconnect",
            "Reconnect",
            "New SQL editor",
            "Read-only",
            "Tools",
            "Open dashboard",
            "Transaction log…",
            "Commit transaction",
            "Roll back transaction",
            "New connection…",
            "Active connection",
            "Active schema",
            "Administration",
        ];
        for item in Item::all() {
            assert!(!words.contains(&item.name()), "{item:?}");
        }
    }

    fn query(transaction: TxState, running: bool) -> Facts {
        Facts {
            link: Link::Open,
            query: Some(QueryFacts {
                transaction,
                running,
                target: true,
            }),
            fixed: false,
            file: false,
        }
    }

    fn reasons(facts: Facts) -> Vec<(Item, &'static str)> {
        Item::all()
            .filter_map(|item| why_not(item, facts).map(|reason| (item, reason)))
            .collect()
    }

    #[test]
    fn with_nothing_open_only_the_new_items_apply() {
        assert_eq!(
            reasons(Facts::default()),
            [
                (Connect, "no connection"),
                (Reconnect, "no connection"),
                (Disconnect, "no connection"),
                (Commit, "no query tab"),
                (Rollback, "no query tab"),
                (Mode, "no query tab"),
                (Log, "no query tab"),
                (ReadOnly, "no connection"),
                (Connection, "no query tab"),
                (Target, "no query tab"),
                (Dashboard, "no connection"),
                (Tools, "no connection"),
                (Search, "no connection"),
            ]
        );
    }

    #[test]
    fn the_transaction_items_follow_the_tab_s_transaction() {
        // Connected, nothing open: only Connect and the transaction's ends
        // are greyed.
        assert_eq!(
            reasons(query(TxState::Idle, false)),
            [
                (Connect, "connected"),
                (Commit, "no transaction open"),
                (Rollback, "no transaction open"),
            ]
        );
        assert_eq!(
            reasons(query(TxState::Open, false)),
            [(Connect, "connected")]
        );
        assert_eq!(
            reasons(query(TxState::Open, true)),
            [
                (Connect, "connected"),
                (Commit, "running"),
                (Rollback, "running"),
            ]
        );
        assert_eq!(
            reasons(query(TxState::Failed, false)),
            [(Connect, "connected"), (Commit, "failed: roll back")]
        );
        // A query tab on no connection chooses one; its database cannot be.
        let no_target = Facts {
            link: Link::None,
            query: Some(QueryFacts {
                transaction: TxState::Idle,
                running: false,
                target: false,
            }),
            fixed: false,
            file: false,
        };
        assert_eq!(why_not(Connection, no_target), None);
        assert_eq!(why_not(Target, no_target), Some("no connection"));
        // A table's tab shows where it is, but only a query tab chooses;
        // the connection items act on its connection.
        let table_tab = Facts {
            link: Link::Open,
            query: None,
            fixed: true,
            file: false,
        };
        assert_eq!(
            reasons(table_tab),
            [
                (Connect, "connected"),
                (Commit, "no query tab"),
                (Rollback, "no query tab"),
                (Mode, "no query tab"),
                (Log, "no query tab"),
                (Connection, "query tabs only"),
                (Target, "query tabs only"),
            ]
        );
    }

    #[test]
    fn the_connection_items_say_what_the_sidebar_menu_says() {
        for status in [
            Status::Disconnected,
            Status::Connecting,
            Status::WaitingForInput,
            Status::Connected {
                version: "17".into(),
            },
            Status::Failed {
                message: "lost".into(),
            },
        ] {
            let entries = menu(
                &Subject::Connection("id".into()),
                &status,
                &[],
                None,
                0,
                false,
                false,
            );
            let sidebar = |wanted: SidebarAction| {
                entries.iter().find_map(|entry| match entry {
                    Entry::Item(action, reason) if *action == wanted => Some(*reason),
                    _ => None,
                })
            };
            let facts = Facts {
                link: Link::of(&status),
                query: None,
                fixed: false,
                file: false,
            };
            for (item, action) in [
                (Connect, SidebarAction::Connect),
                (Disconnect, SidebarAction::Disconnect),
                (Reconnect, SidebarAction::Reconnect),
            ] {
                assert_eq!(Some(why_not(item, facts)), sidebar(action), "{status:?}");
            }
        }
    }

    #[test]
    fn narrow_rows_send_the_rightmost_groups_to_the_overflow() {
        let groups: Vec<f32> = GROUPS
            .iter()
            .map(|group| group.iter().map(|item| item.min_width()).sum())
            .collect();
        assert_eq!(fitting(&groups, 1200.), GROUPS.len());
        let all = PADDING + groups.iter().sum::<f32>() + GROUP_GAP * (groups.len() - 1) as f32;
        assert_eq!(fitting(&groups, all), GROUPS.len());
        // One pixel less: the last group goes, and "»" takes its room.
        let fit = fitting(&groups, all - 1.);
        assert!(fit < GROUPS.len());
        let used = PADDING
            + MORE
            + groups[..fit].iter().sum::<f32>()
            + GROUP_GAP * fit.saturating_sub(1) as f32;
        assert!(used <= all - 1., "{used}");
        assert_eq!(fitting(&groups, 40.), 0);
    }
}
