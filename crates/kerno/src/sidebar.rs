//! The sidebar: its header, its empty state, and the one tree of folders,
//! connections and their objects (see [`Navigator`]); the menus of the
//! empty area, of a folder and of a connection, modelled on DBeaver's
//! database navigator, are defined and carried out here.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, TestSupportExt as _, Window, div, px,
};
use kerno_core::config::{connection_url, in_folder, last_folder_name, parent_folder};
use kerno_core::storage::SavedConnection;

use crate::actions::NewConnection;
use crate::connections::dialog::ConnectionDialog;
use crate::connections::manager::{Connections, Status, folder_of};
use crate::folder_dialog::{self, Target};
use crate::navigator::actions::{ToolEntry, ToolGroups};
use crate::navigator::view::Navigator;
use crate::state::AppState;

/// What a sidebar menu is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// The empty area of the connections list.
    List,
    /// A folder, by path.
    Folder(String),
    /// A saved connection, by id.
    Connection(String),
}

/// An entry of a sidebar menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarAction {
    NewConnection,
    NewFolder,
    ImportConnections,
    /// Saves a copy of the connection copied last.
    Paste,
    Refresh,
    CollapseAll,
    /// DBeaver's toggle, as it is now (a checked entry).
    LinkWithEditor(bool),
    NewConnectionHere,
    NewSubfolder,
    /// A folder's name, colour and icon.
    EditFolder,
    Rename,
    Delete,
    Connect,
    /// A connection, or every connection open in a folder.
    Disconnect,
    Reconnect,
    /// Every connection open but this one.
    DisconnectOthers,
    /// Every connection open.
    DisconnectAll,
    /// The connection's read-only flag, as it is now (a checked entry).
    ReadOnly(bool),
    NewSqlEditor,
    /// An entry of the connection's Tools ▸ (the administration, its locks
    /// and sessions, the tool wizards, the tasks).
    Tool(ToolEntry),
    Edit,
    Duplicate,
    /// Into this folder; the top level when `None`.
    MoveTo(Option<String>),
    CopyName,
    CopyUrl,
    CopyHost,
    /// Stores a connection opened from a URL (`kerno <url>`), through the
    /// connection dialog.
    SaveConnection,
}

impl SidebarAction {
    pub fn label(&self) -> String {
        match self {
            Self::NewConnection => "New connection…",
            Self::NewFolder => "New folder…",
            Self::ImportConnections => "Import connections…",
            Self::Paste => "Paste",
            Self::Refresh => "Refresh",
            Self::CollapseAll => "Collapse all",
            Self::LinkWithEditor(_) => "Link with editor",
            Self::NewConnectionHere => "New connection here…",
            Self::NewSubfolder => "New subfolder…",
            Self::EditFolder => "Edit folder…",
            Self::Rename => "Rename…",
            Self::Delete => "Delete…",
            Self::Connect => "Connect",
            Self::Disconnect => "Disconnect",
            Self::Reconnect => "Reconnect",
            Self::DisconnectOthers => "Disconnect others",
            Self::DisconnectAll => "Disconnect all",
            Self::ReadOnly(_) => "Read-only",
            Self::NewSqlEditor => "New SQL editor",
            Self::Tool(entry) => return entry.label(),
            Self::Edit => "Edit…",
            Self::Duplicate => "Duplicate",
            Self::MoveTo(Some(folder)) => return folder.clone(),
            Self::MoveTo(None) => "No folder",
            Self::CopyName => "Name",
            Self::CopyUrl => "Connection URL",
            Self::CopyHost => "Host",
            Self::SaveConnection => "Save connection…",
        }
        .to_string()
    }
}

/// One line of a menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// An action, and why it cannot be used now when it cannot.
    Item(SidebarAction, Option<&'static str>),
    Submenu(&'static str, Vec<Entry>),
    Separator,
}

/// The menu of `subject`. `status` is the connection's (ignored for the
/// other subjects); `checked` is the state of the menu's checked entry:
/// the connection's read-only flag, or Link with editor for the list;
/// `folders` are every folder; `folder` is where the connection is; `open`
/// counts the connections open that a disconnection would close: all of
/// them for the list, those in the folder, the others for a connection;
/// `copied` says a connection was copied, for Paste. A connection's Tools ▸
/// is left empty here: [`with_tools`] fills it.
pub fn menu(
    subject: &Subject,
    status: &Status,
    folders: &[String],
    folder: Option<&str>,
    open: usize,
    checked: bool,
    copied: bool,
) -> Vec<Entry> {
    use Entry::{Item, Separator, Submenu};
    use SidebarAction::*;
    let enabled = |action| Item(action, None);
    let none_open = (open == 0).then_some("none open");
    match subject {
        Subject::List => vec![
            enabled(NewConnection),
            enabled(NewFolder),
            enabled(ImportConnections),
            Separator,
            Item(Paste, (!copied).then_some("no connection copied")),
            Separator,
            enabled(Refresh),
            enabled(CollapseAll),
            enabled(LinkWithEditor(checked)),
            Separator,
            Item(DisconnectAll, none_open),
        ],
        Subject::Folder(_) => vec![
            enabled(NewConnectionHere),
            enabled(NewSubfolder),
            Separator,
            Item(Disconnect, none_open),
            Separator,
            enabled(EditFolder),
            enabled(Rename),
            Separator,
            enabled(Delete),
            Separator,
            enabled(Refresh),
        ],
        Subject::Connection(_) => {
            let connected = matches!(status, Status::Connected { .. });
            let busy = matches!(status, Status::Connecting | Status::WaitingForInput);
            let closed = matches!(status, Status::Disconnected | Status::Failed { .. });
            let not_connected = (!connected).then_some("not connected");
            let moves = std::iter::once(None)
                .chain(folders.iter().map(|folder| Some(folder.clone())))
                .map(|target| {
                    let here = target.as_deref() == folder;
                    Item(MoveTo(target), here.then_some("already there"))
                })
                .collect();
            vec![
                Item(
                    Connect,
                    if connected {
                        Some("connected")
                    } else {
                        busy.then_some("connecting")
                    },
                ),
                Item(Disconnect, closed.then_some("not connected")),
                Item(Reconnect, not_connected),
                Item(DisconnectOthers, (open == 0).then_some("no other open")),
                enabled(ReadOnly(checked)),
                Separator,
                enabled(NewSqlEditor),
                Submenu(TOOLS, Vec::new()),
                Separator,
                enabled(Edit),
                enabled(Duplicate),
                enabled(Rename),
                Submenu("Move to folder", moves),
                Separator,
                enabled(Delete),
                Separator,
                Item(Refresh, not_connected),
                Separator,
                Submenu(
                    "Copy",
                    vec![enabled(CopyName), enabled(CopyUrl), enabled(CopyHost)],
                ),
            ]
        }
    }
}

/// The label of a connection's Tools ▸.
pub const TOOLS: &str = "Tools";

/// `entries` with the connection's Tools ▸ filled with `tools`
/// ([`crate::navigator::actions::tool_entries`]), groups apart.
pub fn with_tools(entries: Vec<Entry>, tools: ToolGroups) -> Vec<Entry> {
    let items: Vec<Entry> = tools
        .into_iter()
        .enumerate()
        .flat_map(|(index, group)| {
            (index > 0).then_some(Entry::Separator).into_iter().chain(
                group
                    .into_iter()
                    .map(|(entry, reason)| Entry::Item(SidebarAction::Tool(entry), reason)),
            )
        })
        .collect();
    // An engine without tools (SQLite) has no Tools ▸ at all.
    entries
        .into_iter()
        .filter_map(|entry| match entry {
            Entry::Submenu(TOOLS, _) if items.is_empty() => None,
            Entry::Submenu(TOOLS, _) => Some(Entry::Submenu(TOOLS, items.clone())),
            entry => Some(entry),
        })
        .collect()
}

/// What the rest of the window does for the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarEvent {
    /// Open a query tab on this connection, which is open.
    NewEditor(String),
    /// Open a Tools ▸ entry on this connection's default database.
    Tool(String, ToolEntry),
    /// Ask for a connections file to import.
    ImportConnections,
    /// Close these connections, `action` (“Disconnect “prod””) once what
    /// their tabs hold is ended.
    Disconnect { ids: Vec<String>, action: String },
    /// Close this connection and open it again, the same way.
    Reconnect(String),
    /// Save this read-only flag on the connection, the same way.
    SetReadOnly(String, bool),
}

/// A connection row being dragged onto a folder.
#[derive(Clone)]
pub(crate) struct DraggedConnection {
    pub(crate) id: String,
    pub(crate) name: SharedString,
}

impl Render for DraggedConnection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .text_sm()
            .child(self.name.clone())
    }
}

/// The sidebar view.
pub struct Sidebar {
    connections: Entity<Connections>,
    /// The one tree of folders, connections and their objects.
    navigator: Entity<Navigator>,
    /// The connection whose Copy ▸ was used last: what Paste duplicates.
    /// Only its id is kept, never a secret, and nothing of it goes to the
    /// clipboard but what was copied.
    copied: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    /// The sidebar for `connections`.
    pub fn new(
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sidebar = cx.weak_entity();
        let navigator = cx.new(|cx| Navigator::new(connections.clone(), sidebar, window, cx));
        let subscriptions = vec![cx.observe(&connections, |_, _, cx| cx.notify())];
        Self {
            connections,
            navigator,
            copied: None,
            _subscriptions: subscriptions,
        }
    }

    /// The one tree of folders, connections and their objects.
    pub fn navigator(&self) -> &Entity<Navigator> {
        &self.navigator
    }

    /// The connection of the selected row, otherwise the one opened last.
    pub fn selected(&self, cx: &App) -> Option<String> {
        self.navigator.read(cx).connection().map(str::to_string)
    }

    /// What Enter does to the selected connection: browses it, connecting
    /// it first when it is closed (DBeaver's default).
    pub fn open_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected(cx) {
            self.navigator
                .update(cx, |navigator, cx| navigator.open_connection(&id, cx));
        }
    }

    /// What F4 does to the selected connection: opens its dialog.
    pub fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selected(cx) {
            Self::edit(self.connections.clone(), id, window, cx);
        }
    }

    /// The menu of `subject` as it stands now.
    pub fn menu_of(&self, subject: &Subject, cx: &App) -> Vec<Entry> {
        let connections = self.connections.read(cx);
        let (status, folder, checked) = match subject {
            Subject::Connection(id) => {
                let saved = connections.get(id);
                (
                    connections.status(id),
                    saved.and_then(|saved| folder_of(&saved.config)),
                    saved.is_some_and(|saved| saved.config.read_only),
                )
            }
            Subject::List => (
                Status::Disconnected,
                None,
                AppState::get(cx).settings.link_with_editor,
            ),
            Subject::Folder(_) => (Status::Disconnected, None, false),
        };
        let entries = menu(
            subject,
            &status,
            &connections.folders(cx),
            folder.as_deref(),
            self.closing(subject, cx).len(),
            checked,
            self.copied_connection(cx).is_some(),
        );
        let Subject::Connection(id) = subject else {
            return entries;
        };
        // A connection opened from a URL says first that it is not saved.
        let entries = match connections.is_unsaved(id) {
            true => [
                Entry::Item(SidebarAction::SaveConnection, None),
                Entry::Separator,
            ]
            .into_iter()
            .chain(entries)
            .collect(),
            false => entries,
        };
        let Some(engine) = connections.get(id).map(|saved| saved.config.engine) else {
            return entries;
        };
        let connected = matches!(status, Status::Connected { .. });
        let tools = crate::navigator::actions::tool_entries(None, engine, &[]);
        with_tools(
            entries,
            crate::navigator::actions::tools_now(tools, connected, checked),
        )
    }

    /// The connection copied last, while it is still saved.
    fn copied_connection(&self, cx: &App) -> Option<String> {
        self.copied
            .clone()
            .filter(|id| self.connections.read(cx).get(id).is_some())
    }

    /// The open connections a Disconnect on `subject` closes: every one
    /// for the list, those in a folder, the others for a connection.
    pub fn closing(&self, subject: &Subject, cx: &App) -> Vec<String> {
        let connections = self.connections.read(cx);
        connections
            .saved()
            .iter()
            .filter(|saved| {
                !matches!(
                    connections.status(&saved.id),
                    Status::Disconnected | Status::Failed { .. }
                )
            })
            .filter(|saved| match subject {
                Subject::List => true,
                Subject::Folder(path) => {
                    folder_of(&saved.config).is_some_and(|folder| in_folder(&folder, path))
                }
                Subject::Connection(id) => saved.id != *id,
            })
            .map(|saved| saved.id.clone())
            .collect()
    }

    /// Does what a menu entry says.
    pub fn perform(
        &mut self,
        subject: &Subject,
        action: SidebarAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use SidebarAction::*;
        let connections = self.connections.clone();
        let id = match subject {
            Subject::Connection(id) => id.clone(),
            _ => String::new(),
        };
        let saved = connections.read(cx).get(&id).cloned();
        let copy = |text: String, cx: &mut Context<Self>| {
            cx.write_to_clipboard(ClipboardItem::new_string(text))
        };
        match (subject, action.clone()) {
            (Subject::List, NewConnection) => ConnectionDialog::open(connections, None, window, cx),
            (Subject::Folder(path), NewConnectionHere) => {
                ConnectionDialog::open_in(connections, path.clone(), window, cx)
            }
            (Subject::List, NewFolder) | (Subject::Folder(_), NewSubfolder) => {
                let parent = match subject {
                    Subject::Folder(path) => Some(path.clone()),
                    _ => None,
                };
                if let Some(parent) = &parent {
                    set_collapsed(parent, false, cx);
                }
                folder_dialog::open(connections, Target::New(parent), window, cx);
            }
            (Subject::Folder(path), EditFolder) => {
                folder_dialog::open(connections, Target::Edit(path.clone()), window, cx);
            }
            (Subject::List, ImportConnections) => cx.emit(SidebarEvent::ImportConnections),
            // The list is read again, and every tree being browsed.
            (Subject::List, Refresh) => {
                connections.update(cx, |c, cx| c.reload(cx));
                self.navigator
                    .update(cx, |navigator, cx| navigator.reload_all(cx));
            }
            // DBeaver pastes a copied connection as a new one.
            (Subject::List, Paste) => {
                if let Some(copied) = self.copied_connection(cx) {
                    connections.update(cx, |c, cx| c.duplicate(&copied, cx))
                }
            }
            // The folder's list is read again, and the trees of the
            // connections in it; nothing is reconnected.
            (Subject::Folder(path), Refresh) => {
                connections.update(cx, |c, cx| c.reload(cx));
                let inside: Vec<String> = connections
                    .read(cx)
                    .saved()
                    .iter()
                    .filter(|saved| {
                        folder_of(&saved.config).is_some_and(|folder| in_folder(&folder, path))
                    })
                    .map(|saved| saved.id.clone())
                    .collect();
                self.navigator.update(cx, |navigator, cx| {
                    for id in &inside {
                        navigator.reload(id, cx);
                    }
                });
            }
            // Folders and connections close; nothing is disconnected.
            (Subject::List, CollapseAll) => {
                let folders = connections.read(cx).folders(cx);
                AppState::update_settings(cx, |settings| {
                    settings.collapsed_folders = folders.into_iter().collect()
                });
                self.navigator
                    .update(cx, |navigator, cx| navigator.collapse_connections(cx));
            }
            // The window runs it, once the menu has given the focus back.
            (Subject::List, LinkWithEditor(_)) => window.defer(cx, |window, cx| {
                window.dispatch_action(Box::new(crate::actions::LinkWithEditor), cx)
            }),
            (Subject::Folder(path), Rename) => {
                let path = path.clone();
                let current = last_folder_name(&path).to_string();
                ask_name("Rename folder", current, window, cx, move |name, cx| {
                    connections.update(cx, |c, cx| c.rename_folder(&path, name, cx))
                });
            }
            (Subject::Folder(path), Delete) => {
                Self::confirm_delete_folder(connections, path.clone(), window, cx)
            }
            (Subject::Connection(_), Connect) => connections.update(cx, |c, cx| c.connect(&id, cx)),
            // What closes a session goes through the window, which asks
            // about the transactions and pending changes it would end.
            (Subject::Connection(_), Disconnect) => {
                let name = saved.map(|saved| saved.config.name).unwrap_or_default();
                cx.emit(SidebarEvent::Disconnect {
                    ids: vec![id],
                    action: format!("Disconnect “{name}”"),
                })
            }
            (Subject::Connection(_), Reconnect) => cx.emit(SidebarEvent::Reconnect(id)),
            (Subject::Connection(_), DisconnectOthers) => {
                let name = saved.map(|saved| saved.config.name).unwrap_or_default();
                cx.emit(SidebarEvent::Disconnect {
                    ids: self.closing(subject, cx),
                    action: format!("Disconnect the connections other than “{name}”"),
                })
            }
            (Subject::Connection(_), ReadOnly(current)) => {
                cx.emit(SidebarEvent::SetReadOnly(id, !current))
            }
            (Subject::Folder(path), Disconnect) => cx.emit(SidebarEvent::Disconnect {
                ids: self.closing(subject, cx),
                action: format!("Disconnect the connections in “{}”", last_folder_name(path)),
            }),
            (Subject::List, DisconnectAll) => cx.emit(SidebarEvent::Disconnect {
                ids: self.closing(subject, cx),
                action: "Disconnect all connections".into(),
            }),
            (Subject::Connection(_), NewSqlEditor) => cx.emit(SidebarEvent::NewEditor(id)),
            (Subject::Connection(_), Tool(entry)) => cx.emit(SidebarEvent::Tool(id, entry)),
            (Subject::Connection(_), Edit) => Self::edit(connections, id, window, cx),
            (Subject::Connection(_), SaveConnection) => Self::edit(connections, id, window, cx),
            (Subject::Connection(_), Duplicate) => {
                connections.update(cx, |c, cx| c.duplicate(&id, cx))
            }
            (Subject::Connection(_), Rename) => {
                let current = saved.map(|saved| saved.config.name).unwrap_or_default();
                ask_name("Rename connection", current, window, cx, move |name, cx| {
                    connections.update(cx, |c, cx| c.rename_connection(&id, name, cx))
                });
            }
            (Subject::Connection(_), MoveTo(folder)) => {
                connections.update(cx, |c, cx| c.move_connection(&id, folder.as_deref(), cx))
            }
            (Subject::Connection(_), Delete) => {
                if let Some(saved) = saved {
                    Self::confirm_delete(connections, &saved, window, cx)
                }
            }
            (Subject::Connection(_), Refresh) => {
                self.navigator.update(cx, |navigator, cx| {
                    navigator.select_connection(&id, cx);
                    navigator.reload(&id, cx);
                });
            }
            (Subject::Connection(_), CopyName | CopyUrl | CopyHost) => {
                let Some(saved) = saved else {
                    return;
                };
                let text = match action {
                    CopyName => saved.config.name,
                    // A database file's "URL" and "host" are its path.
                    CopyUrl => connection_url(&saved.config),
                    _ if saved.config.engine.is_file() => connection_url(&saved.config),
                    _ => saved.config.host,
                };
                copy(text, cx);
                // Paste in the list saves a copy of it.
                self.copied = Some(id);
            }
            (subject, action) => {
                tracing::warn!(?subject, ?action, "a sidebar action on the wrong subject")
            }
        }
    }

    /// Opens the dialog to edit a connection, once its secrets are read.
    pub fn edit(connections: Entity<Connections>, id: String, window: &mut Window, cx: &mut App) {
        let Some(saved) = connections.read(cx).get(&id).cloned() else {
            return;
        };
        let loading = connections.read(cx).secrets_of(&id, cx);
        window
            .spawn(cx, async move |cx| {
                let secrets = loading.await;
                let _ = cx.update(|window, cx| match secrets {
                    Ok(secrets) => {
                        // An unsaved connection is saved as a new one is:
                        // its password goes to the secret store.
                        let mut saved = saved;
                        if connections.read(cx).is_unsaved(&saved.id) {
                            saved.config.save_password = true;
                        }
                        ConnectionDialog::open(connections, Some((saved, secrets)), window, cx)
                    }
                    Err(error) => window.push_notification(
                        format!("The connection's secrets could not be read: {error}"),
                        cx,
                    ),
                });
            })
            .detach();
    }

    /// Asks before deleting a connection, naming what goes with it.
    pub fn confirm_delete(
        connections: Entity<Connections>,
        saved: &SavedConnection,
        window: &mut Window,
        cx: &mut App,
    ) {
        let id = saved.id.clone();
        let name = saved.config.name.clone();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let connections = connections.clone();
            let id = id.clone();
            alert
                .title(SharedString::from(format!("Delete “{name}”?")))
                .description("Its saved password, query history and saved queries are deleted too. The database itself is not touched.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                // The window ends what the connection's tabs hold first.
                .on_ok(move |_, _, cx| {
                    let id = id.clone();
                    connections.update(cx, |_, cx| {
                        cx.emit(crate::connections::manager::ConnectionsEvent::DeleteRequested {
                            id,
                        })
                    });
                    true
                })
        });
    }

    /// Deletes an empty folder; asks first when connections would move.
    fn confirm_delete_folder(
        connections: Entity<Connections>,
        path: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        let count = connections.read(cx).count_in(&path);
        if count == 0 {
            connections.update(cx, |c, cx| c.delete_folder(&path, cx));
            return;
        }
        let destination = match parent_folder(&path) {
            Some(parent) => format!("the folder “{parent}”"),
            None => "the top level".to_string(),
        };
        let text = SharedString::from(format!(
            "Its {count} connection{} and its folders move to {destination}. No connection is deleted.",
            if count == 1 { "" } else { "s" }
        ));
        let title = SharedString::from(format!("Delete the folder “{}”?", last_folder_name(&path)));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (connections, path) = (connections.clone(), path.clone());
            alert
                .title(title.clone())
                .description(text.clone())
                .confirm()
                .ok_text("Delete folder")
                .on_ok(move |_, _, cx| {
                    connections.update(cx, |c, cx| c.delete_folder(&path, cx));
                    true
                })
        });
    }

    /// Fills `menu` with `entries`, each running on `subject`.
    pub(crate) fn fill_menu(
        sidebar: Entity<Self>,
        subject: Subject,
        entries: Vec<Entry>,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut App,
    ) -> PopupMenu {
        for entry in entries {
            menu = match entry {
                Entry::Separator => menu.separator(),
                Entry::Submenu(label, entries) => {
                    let (sidebar, subject) = (sidebar.clone(), subject.clone());
                    let submenu = PopupMenu::build(window, cx, move |menu, window, cx| {
                        Self::fill_menu(sidebar, subject, entries, menu, window, cx)
                    });
                    menu.item(PopupMenuItem::submenu(label, submenu))
                }
                Entry::Item(action, reason) => {
                    // An entry that cannot be used says why.
                    let label = match reason {
                        Some(reason) => format!("{} ({reason})", action.label()),
                        None => action.label(),
                    };
                    let (sidebar, subject) = (sidebar.clone(), subject.clone());
                    menu.item(
                        PopupMenuItem::new(SharedString::from(label))
                            .checked(matches!(
                                action,
                                SidebarAction::ReadOnly(true) | SidebarAction::LinkWithEditor(true)
                            ))
                            .disabled(reason.is_some())
                            .on_click(move |_, window, cx| {
                                sidebar.update(cx, |this, cx| {
                                    this.perform(&subject, action.clone(), window, cx)
                                })
                            }),
                    )
                }
            };
        }
        menu
    }

    /// The context menu of `subject`, built when it opens.
    fn context_menu_for(
        &self,
        subject: Subject,
        cx: &Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let sidebar = cx.entity();
        move |menu, window, cx| {
            let entries = sidebar.read(cx).menu_of(&subject, cx);
            Self::fill_menu(sidebar.clone(), subject.clone(), entries, menu, window, cx)
        }
    }
}

/// Opens or closes a folder of the sidebar; remembered in the settings.
pub(crate) fn set_collapsed(path: &str, collapsed: bool, cx: &mut App) {
    let path = path.to_string();
    AppState::update_settings(cx, move |settings| {
        if collapsed {
            settings.collapsed_folders.insert(path);
        } else {
            settings.collapsed_folders.remove(&path);
        }
    });
}

/// Asks for a name; `submit` checks and uses it, and says what is wrong
/// with it to keep the dialog open.
fn ask_name(
    title: &'static str,
    initial: String,
    window: &mut Window,
    cx: &mut App,
    submit: impl Fn(&str, &mut App) -> Result<(), String> + 'static,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
    let problem: Rc<RefCell<Option<String>>> = Rc::default();
    let submit = {
        let (input, problem) = (input.clone(), problem.clone());
        Rc::new(move |window: &mut Window, cx: &mut App| {
            let name = input.read(cx).value().to_string();
            let outcome = submit(&name, cx);
            let done = outcome.is_ok();
            *problem.borrow_mut() = outcome.err();
            window.refresh();
            done
        })
    };
    let focus = input.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let (input, problem, on_enter) = (input.clone(), problem.clone(), submit.clone());
        let on_click = submit.clone();
        dialog
            .title(title)
            .w(px(380.))
            .on_ok(move |_, window, cx| on_enter(window, cx))
            .content(move |content, _, cx| {
                let problem = problem.borrow().clone();
                let on_click = on_click.clone();
                content.child(
                    v_flex()
                        .gap_3()
                        .child(Input::new(&input).id("name-input"))
                        .when_some(problem, |this, problem| {
                            this.child(
                                div()
                                    .id("name-error")
                                    .test_support()
                                    .text_xs()
                                    .text_color(cx.theme().danger)
                                    .child(SharedString::from(problem)),
                            )
                        })
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("name-cancel")
                                        .ghost()
                                        .label("Cancel")
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(Button::new("name-ok").primary().label("OK").on_click(
                                    move |_, window, cx| {
                                        if on_click(window, cx) {
                                            window.close_dialog(cx);
                                        }
                                    },
                                )),
                        ),
                )
            })
    });
    window.defer(cx, move |window, cx| {
        focus.read(cx).focus_handle(cx).focus(window, cx)
    });
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let connections = self.connections.read(cx);
        let empty = connections.saved().is_empty() && connections.folders(cx).is_empty();
        let theme = cx.theme();
        let header = h_flex()
            .id("connections-header")
            .test_support()
            .px_3()
            .pt_2()
            .pb_1()
            .justify_between()
            .context_menu(self.context_menu_for(Subject::List, cx))
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("CONNECTIONS"),
            )
            .child(
                Button::new("new-connection")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("New connection")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewConnection), cx)),
            );

        v_flex()
            .id("sidebar")
            .test_support()
            .size_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(header)
            .when(empty, |this| {
                this.child(
                    v_flex()
                        .id("connections-empty")
                        .flex_1()
                        .px_3()
                        .pt_2()
                        .gap_2()
                        .context_menu(self.context_menu_for(Subject::List, cx))
                        .child(div().text_sm().child("No connections yet"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Add a PostgreSQL, MySQL or MariaDB server to browse it and run queries."),
                        )
                        .child(
                            h_flex().child(
                                Button::new("empty-new-connection")
                                    .primary()
                                    .small()
                                    .label("New connection")
                                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewConnection), cx)),
                            ),
                        ),
                )
            })
            .when(!empty, |this| this.child(self.navigator.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Item(action, None) => action.label(),
                Entry::Item(action, Some(reason)) => format!("{} ({reason})", action.label()),
                Entry::Submenu(label, entries) => {
                    format!("{label} ▸ [{}]", labels(entries).join(", "))
                }
                Entry::Separator => "—".into(),
            })
            .collect()
    }

    #[test]
    fn the_empty_area_and_a_folder_offer_what_dbeaver_does() {
        let disconnected = Status::Disconnected;
        assert_eq!(
            labels(&menu(
                &Subject::List,
                &disconnected,
                &[],
                None,
                0,
                false,
                false
            )),
            [
                "New connection…",
                "New folder…",
                "Import connections…",
                "—",
                "Paste (no connection copied)",
                "—",
                "Refresh",
                "Collapse all",
                "Link with editor",
                "—",
                "Disconnect all (none open)",
            ]
        );
        // Link with editor is a checked entry that carries its state.
        let linked = menu(&Subject::List, &disconnected, &[], None, 0, true, false);
        assert!(linked.contains(&Entry::Item(SidebarAction::LinkWithEditor(true), None)));
        let folders = ["Work".to_string()];
        assert_eq!(
            labels(&menu(
                &Subject::List,
                &disconnected,
                &folders,
                None,
                2,
                false,
                false
            ))[7],
            "Collapse all"
        );
        assert_eq!(
            labels(&menu(
                &Subject::List,
                &disconnected,
                &folders,
                None,
                2,
                false,
                false
            ))[10],
            "Disconnect all"
        );
        assert_eq!(
            labels(&menu(
                &Subject::Folder("Work".into()),
                &disconnected,
                &folders,
                None,
                0,
                false,
                false
            )),
            [
                "New connection here…",
                "New subfolder…",
                "—",
                "Disconnect (none open)",
                "—",
                "Edit folder…",
                "Rename…",
                "—",
                "Delete…",
                "—",
                "Refresh",
            ]
        );
    }

    #[test]
    fn a_connection_disables_what_does_not_apply_to_its_status() {
        let folders = ["Work".to_string(), "Work/Clients".to_string()];
        let subject = Subject::Connection("id".into());
        let closed = labels(&menu(
            &subject,
            &Status::Disconnected,
            &folders,
            Some("Work"),
            0,
            false,
            false,
        ));
        assert_eq!(
            closed,
            [
                "Connect",
                "Disconnect (not connected)",
                "Reconnect (not connected)",
                "Disconnect others (no other open)",
                "Read-only",
                "—",
                "New SQL editor",
                "Tools ▸ []",
                "—",
                "Edit…",
                "Duplicate",
                "Rename…",
                "Move to folder ▸ [No folder, Work (already there), Work/Clients]",
                "—",
                "Delete…",
                "—",
                "Refresh (not connected)",
                "—",
                "Copy ▸ [Name, Connection URL, Host]",
            ]
        );
        let open = labels(&menu(
            &subject,
            &Status::Connected {
                version: "17".into(),
            },
            &folders,
            None,
            1,
            true,
            false,
        ));
        assert_eq!(
            open[..3],
            ["Connect (connected)", "Disconnect", "Reconnect"]
        );
        assert_eq!(open[3], "Disconnect others");
        // Administration and Lock Manager moved under Tools ▸, filled by
        // `with_tools` (see `a_connection_s_tools_say_what_its_state_allows`).
        assert_eq!(open[7], "Tools ▸ []");
        assert_eq!(open[16], "Refresh");
        assert!(open[12].starts_with("Move to folder ▸ [No folder (already there), Work,"));
        // The read-only entry carries the flag as it is, checked.
        let entries = menu(
            &subject,
            &Status::Disconnected,
            &folders,
            None,
            0,
            true,
            false,
        );
        assert!(entries.contains(&Entry::Item(SidebarAction::ReadOnly(true), None)));

        let connecting = labels(&menu(
            &subject,
            &Status::Connecting,
            &folders,
            None,
            0,
            false,
            false,
        ));
        assert_eq!(
            connecting[..3],
            [
                "Connect (connecting)",
                "Disconnect",
                "Reconnect (not connected)"
            ]
        );
        let failed = labels(&menu(
            &subject,
            &Status::Failed {
                message: "no".into(),
            },
            &folders,
            None,
            0,
            false,
            false,
        ));
        assert_eq!(
            failed[..3],
            [
                "Connect",
                "Disconnect (not connected)",
                "Reconnect (not connected)"
            ]
        );
    }

    #[test]
    fn a_connection_s_tools_say_what_its_state_allows() {
        use crate::navigator::actions::{tool_entries, tools_now};
        let subject = Subject::Connection("id".into());
        let tools_of = |postgres: bool, connected: bool, read_only: bool| {
            let status = if connected {
                Status::Connected {
                    version: "17".into(),
                }
            } else {
                Status::Disconnected
            };
            let entries = menu(&subject, &status, &[], None, 0, read_only, false);
            let tools = tools_now(
                tool_entries(
                    None,
                    if postgres {
                        kerno_core::Engine::Postgres
                    } else {
                        kerno_core::Engine::MySql
                    },
                    &[],
                ),
                connected,
                read_only,
            );
            labels(&with_tools(entries, tools))
                .into_iter()
                .find(|label| label.starts_with("Tools ▸"))
                .unwrap()
        };
        assert_eq!(
            tools_of(true, true, false),
            "Tools ▸ [Back up…, Global backup…, Restore…, Execute script…, —, Foreign data \
             wrappers…, —, Open dashboard, Session manager, Lock manager, —, Create new task…, \
             Tasks]"
        );
        // Read-only: what writes is refused; MySQL has no global backup and
        // no foreign data wrappers.
        assert_eq!(
            tools_of(false, true, true),
            "Tools ▸ [Back up…, Restore… (read-only connection), Execute script… (read-only \
             connection), —, Open dashboard, Session manager, Lock manager, —, Create new \
             task…, Tasks]"
        );
        // Closed: only the Tasks view opens.
        let closed = tools_of(true, false, false);
        assert!(
            closed.contains("Back up… (not connected)")
                && closed.contains("Open dashboard (not connected)")
                && closed.ends_with("Create new task… (not connected), Tasks]"),
            "{closed}"
        );
    }
}
