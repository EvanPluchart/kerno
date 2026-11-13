//! The sidebar: saved connections in their folders, with their live status
//! and a context menu on every part (the empty area, a folder, a
//! connection), modelled on DBeaver's database navigator.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::assets::IconName as Glyph;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, Entity, EventEmitter,
    FocusHandle, Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _,
    Window, div, px,
};
use kerno_core::config::{connection_url, last_folder_name, parent_folder};
use kerno_core::storage::SavedConnection;

use crate::actions::NewConnection;
use crate::connections::dialog::ConnectionDialog;
use crate::connections::manager::{Connections, ConnectionsEvent, Status, folder_of};
use crate::engine_logo::engine_logo;
use crate::navigator::view::Navigator;
use crate::state::AppState;
use crate::theme;

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
    Refresh,
    CollapseAll,
    NewConnectionHere,
    NewSubfolder,
    Rename,
    Delete,
    Connect,
    Disconnect,
    Reconnect,
    NewSqlEditor,
    Administration,
    Edit,
    Duplicate,
    /// Into this folder; the top level when `None`.
    MoveTo(Option<String>),
    CopyName,
    CopyUrl,
    CopyHost,
}

impl SidebarAction {
    pub fn label(&self) -> String {
        match self {
            Self::NewConnection => "New connection…",
            Self::NewFolder => "New folder…",
            Self::ImportConnections => "Import connections…",
            Self::Refresh => "Refresh",
            Self::CollapseAll => "Collapse all",
            Self::NewConnectionHere => "New connection here…",
            Self::NewSubfolder => "New subfolder…",
            Self::Rename => "Rename…",
            Self::Delete => "Delete…",
            Self::Connect => "Connect",
            Self::Disconnect => "Disconnect",
            Self::Reconnect => "Reconnect",
            Self::NewSqlEditor => "New SQL editor",
            Self::Administration => "Administration",
            Self::Edit => "Edit…",
            Self::Duplicate => "Duplicate",
            Self::MoveTo(Some(folder)) => return folder.clone(),
            Self::MoveTo(None) => "No folder",
            Self::CopyName => "Name",
            Self::CopyUrl => "Connection URL",
            Self::CopyHost => "Host",
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
/// other subjects); `folders` are every folder; `folder` is where the
/// connection is.
pub fn menu(
    subject: &Subject,
    status: &Status,
    folders: &[String],
    folder: Option<&str>,
) -> Vec<Entry> {
    use Entry::{Item, Separator, Submenu};
    use SidebarAction::*;
    let enabled = |action| Item(action, None);
    match subject {
        Subject::List => vec![
            enabled(NewConnection),
            enabled(NewFolder),
            enabled(ImportConnections),
            Separator,
            enabled(Refresh),
            Item(CollapseAll, folders.is_empty().then_some("no folders")),
        ],
        Subject::Folder(_) => vec![
            enabled(NewConnectionHere),
            enabled(NewSubfolder),
            Separator,
            enabled(Rename),
            Separator,
            enabled(Delete),
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
                Separator,
                enabled(NewSqlEditor),
                Item(Administration, not_connected),
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

/// What the rest of the window does for the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarEvent {
    /// Open a query tab on this connection, which is open.
    NewEditor(String),
    /// Open the administration of this connection.
    OpenAdmin(String),
    /// Ask for a connections file to import.
    ImportConnections,
}

/// A connection row being dragged onto a folder.
#[derive(Clone)]
struct DraggedConnection {
    id: String,
    name: SharedString,
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
    /// The connection last clicked.
    selected: Option<String>,
    /// The schema tree of the selected connection.
    navigator: Entity<Navigator>,
    /// The list of connections, which takes Enter and F4 once a row is
    /// clicked (key context `ConnectionList`).
    focus_handle: FocusHandle,
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
        let navigator = cx.new(|cx| Navigator::new(connections.clone(), window, cx));
        let subscriptions = vec![
            cx.observe(&connections, |_, _, cx| cx.notify()),
            // A connection that has just opened is the one to browse.
            cx.subscribe(
                &connections,
                |this, connections, event: &ConnectionsEvent, cx| {
                    if let ConnectionsEvent::StatusChanged { id } = event
                        && matches!(connections.read(cx).status(id), Status::Connected { .. })
                    {
                        this.select(id.clone(), cx);
                    }
                },
            ),
        ];
        Self {
            connections,
            selected: None,
            navigator,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// The schema tree shown under the connections.
    pub fn navigator(&self) -> &Entity<Navigator> {
        &self.navigator
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        self.selected = Some(id.clone());
        self.navigator
            .update(cx, |navigator, cx| navigator.set_connection(Some(id), cx));
        cx.notify();
    }

    /// Browses a connection, connecting it first when it is closed: what a
    /// double click and Enter do (DBeaver's default). Disconnecting is the
    /// menu's.
    fn open(&mut self, id: String, cx: &mut Context<Self>) {
        self.select(id.clone(), cx);
        self.connections.update(cx, |connections, cx| {
            if matches!(
                connections.status(&id),
                Status::Disconnected | Status::Failed { .. }
            ) {
                connections.connect(&id, cx)
            }
        });
    }

    /// The connection selected in the list.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// What Enter does to the selected connection.
    pub fn open_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected.clone() {
            self.open(id, cx);
        }
    }

    /// What F4 does to the selected connection: opens its dialog.
    pub fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selected.clone() {
            Self::edit(self.connections.clone(), id, window, cx);
        }
    }

    /// The menu of `subject` as it stands now.
    pub fn menu_of(&self, subject: &Subject, cx: &App) -> Vec<Entry> {
        let connections = self.connections.read(cx);
        let (status, folder) = match subject {
            Subject::Connection(id) => (
                connections.status(id),
                connections
                    .get(id)
                    .and_then(|saved| folder_of(&saved.config)),
            ),
            _ => (Status::Disconnected, None),
        };
        menu(
            subject,
            &status,
            &connections.folders(cx),
            folder.as_deref(),
        )
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
        match (subject, action) {
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
                ask_name("New folder", String::new(), window, cx, move |name, cx| {
                    connections
                        .update(cx, |c, cx| c.create_folder(parent.as_deref(), name, cx))
                        .map(drop)
                });
            }
            (Subject::List, ImportConnections) => cx.emit(SidebarEvent::ImportConnections),
            (Subject::List, Refresh) => connections.update(cx, |c, cx| c.reload(cx)),
            (Subject::List, CollapseAll) => {
                let folders = connections.read(cx).folders(cx);
                AppState::update_settings(cx, |settings| {
                    settings.collapsed_folders = folders.into_iter().collect()
                });
                cx.notify();
            }
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
            (Subject::Connection(_), Disconnect) => {
                connections.update(cx, |c, cx| c.disconnect(&id, cx))
            }
            (Subject::Connection(_), Reconnect) => connections.update(cx, |c, cx| {
                c.disconnect(&id, cx);
                c.connect(&id, cx);
            }),
            (Subject::Connection(_), NewSqlEditor) => cx.emit(SidebarEvent::NewEditor(id)),
            (Subject::Connection(_), Administration) => cx.emit(SidebarEvent::OpenAdmin(id)),
            (Subject::Connection(_), Edit) => Self::edit(connections, id, window, cx),
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
                self.select(id, cx);
                self.navigator
                    .update(cx, |navigator, cx| navigator.reload(cx));
            }
            (Subject::Connection(_), CopyName) => {
                if let Some(saved) = saved {
                    copy(saved.config.name, cx)
                }
            }
            (Subject::Connection(_), CopyUrl) => {
                if let Some(saved) = saved {
                    copy(connection_url(&saved.config), cx)
                }
            }
            (Subject::Connection(_), CopyHost) => {
                if let Some(saved) = saved {
                    copy(saved.config.host, cx)
                }
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
                .on_ok(move |_, _, cx| {
                    connections.update(cx, |connections, cx| connections.delete(&id, cx));
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
    fn fill_menu(
        sidebar: Entity<Self>,
        subject: Subject,
        entries: Vec<Entry>,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        for entry in entries {
            menu = match entry {
                Entry::Separator => menu.separator(),
                Entry::Submenu(label, entries) => {
                    let (sidebar, subject) = (sidebar.clone(), subject.clone());
                    menu.submenu(label, window, cx, move |menu, window, cx| {
                        Self::fill_menu(
                            sidebar.clone(),
                            subject.clone(),
                            entries.clone(),
                            menu,
                            window,
                            cx,
                        )
                    })
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

    fn render_folder(
        &self,
        index: usize,
        path: &str,
        depth: usize,
        open: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let count = self.connections.read(cx).count_in(path);
        let toggled = path.to_string();
        let target = path.to_string();
        h_flex()
            .id(("conn-folder", index))
            .test_support()
            .pl(px(12. + 14. * depth as f32))
            .pr_3()
            .py_1()
            .gap_1p5()
            .hover(|style| style.bg(theme.sidebar_accent))
            .cursor_pointer()
            .drag_over::<DraggedConnection>(|style, _, _, cx| style.bg(cx.theme().drop_target))
            .on_drop(
                cx.listener(move |this, dragged: &DraggedConnection, _, cx| {
                    this.connections.update(cx, |c, cx| {
                        c.move_connection(&dragged.id, Some(&target), cx)
                    });
                }),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                set_collapsed(&toggled, open, cx);
                cx.notify();
            }))
            .context_menu(self.context_menu_for(Subject::Folder(path.to_string()), cx))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size_3()
                .flex_none()
                .text_color(theme.muted_foreground),
            )
            .child(
                Icon::new(Glyph::Folder)
                    .size_3p5()
                    .flex_none()
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .truncate()
                    .child(SharedString::from(last_folder_name(path).to_string())),
            )
            .when(count > 0, |this| {
                this.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(SharedString::from(count.to_string())),
                )
            })
    }

    fn render_row(
        &self,
        index: usize,
        saved: &SavedConnection,
        depth: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let config = &saved.config;
        let status = self.connections.read(cx).status(&saved.id);
        let tag = config
            .color
            .as_deref()
            .and_then(|key| theme::tag_color(key, cx))
            .unwrap_or_else(|| theme::environment_color(config.environment, cx));
        let (state_label, state_color) = match &status {
            Status::Disconnected => ("", theme.muted_foreground),
            Status::Connecting => ("connecting…", theme.muted_foreground),
            Status::WaitingForInput => ("waiting for you", theme.warning),
            Status::Connected { .. } => ("connected", theme.success),
            Status::Failed { .. } => ("failed", theme.danger),
        };
        let failure = match &status {
            Status::Failed { message } => Some(SharedString::from(message.clone())),
            _ => None,
        };
        let selected = self.selected.as_deref() == Some(saved.id.as_str());
        let indent = px(12. + 14. * depth as f32);
        let dragged = DraggedConnection {
            id: saved.id.clone(),
            name: SharedString::from(config.name.clone()),
        };

        let id = saved.id.clone();
        v_flex()
            .id(("connection", index))
            .test_support()
            .pl(indent)
            .pr_3()
            .py_1()
            .when(selected, |this| this.bg(theme.sidebar_accent))
            .hover(|style| style.bg(theme.sidebar_accent))
            .cursor_pointer()
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .on_click(cx.listener({
                let id = id.clone();
                move |this, event: &ClickEvent, window, cx| {
                    this.focus_handle.focus(window, cx);
                    if event.click_count() >= 2 {
                        this.open(id.clone(), cx);
                    } else {
                        this.select(id.clone(), cx);
                    }
                }
            }))
            .context_menu(self.context_menu_for(Subject::Connection(id), cx))
            .child(
                h_flex()
                    .gap_2()
                    // The tag (or environment) colour, then the engine.
                    .child(
                        div()
                            .flex_none()
                            .w(px(3.))
                            .h(px(16.))
                            .rounded_full()
                            .bg(tag),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(engine_logo(config.engine, px(16.), cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .child(SharedString::from(config.name.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(if state_label.is_empty() {
                                theme.muted_foreground
                            } else {
                                state_color
                            })
                            .child(if state_label.is_empty() {
                                config.engine.label()
                            } else {
                                state_label
                            }),
                    ),
            )
            .when_some(failure, |this, message| {
                this.child(
                    div()
                        .pl(px(16.))
                        .text_xs()
                        .text_color(theme.danger)
                        .child(message),
                )
            })
    }

    /// The rows of the folders and connections in `parent`, and of what
    /// open folders hold, depth first: folders, then connections.
    fn render_level(
        &self,
        parent: Option<&str>,
        depth: usize,
        folders: &[String],
        saved: &[SavedConnection],
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        let collapsed = AppState::get(cx).settings.collapsed_folders.clone();
        for (index, folder) in folders.iter().enumerate() {
            if parent_folder(folder) != parent {
                continue;
            }
            let open = !collapsed.contains(folder);
            rows.push(
                self.render_folder(index, folder, depth, open, cx)
                    .into_any_element(),
            );
            if open {
                self.render_level(Some(folder), depth + 1, folders, saved, rows, cx);
            }
        }
        for (index, connection) in saved.iter().enumerate() {
            if folder_of(&connection.config).as_deref() == parent {
                rows.push(
                    self.render_row(index, connection, depth, cx)
                        .into_any_element(),
                );
            }
        }
    }
}

/// Opens or closes a folder of the sidebar; remembered in the settings.
fn set_collapsed(path: &str, collapsed: bool, cx: &mut App) {
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
        let saved: Vec<SavedConnection> = self.connections.read(cx).saved().to_vec();
        let folders = self.connections.read(cx).folders(cx);
        let empty = saved.is_empty() && folders.is_empty();
        let mut rows = Vec::new();
        self.render_level(None, 0, &folders, &saved, &mut rows, cx);

        let theme = cx.theme();
        // The list takes what it needs, up to a share of the sidebar; the
        // schema tree gets the rest. Below the rows, an empty strip takes
        // drops to the top level and offers the list's own menu.
        let list = v_flex()
            .id("connection-list")
            .key_context("ConnectionList")
            .track_focus(&self.focus_handle)
            .flex_none()
            .max_h(gpui_kit::relative(0.45))
            .overflow_y_scroll()
            .children(rows)
            .child(
                div()
                    .id("connection-list-end")
                    .test_support()
                    .h(px(20.))
                    .flex_none()
                    .drag_over::<DraggedConnection>(|style, _, _, cx| {
                        style.bg(cx.theme().drop_target)
                    })
                    .on_drop(cx.listener(|this, dragged: &DraggedConnection, _, cx| {
                        this.connections
                            .update(cx, |c, cx| c.move_connection(&dragged.id, None, cx));
                    }))
                    .context_menu(self.context_menu_for(Subject::List, cx)),
            );

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
            .when(!empty, |this| {
                this.child(list).child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .border_t_1()
                        .border_color(theme.sidebar_border)
                        .child(self.navigator.clone()),
                )
            })
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
            labels(&menu(&Subject::List, &disconnected, &[], None)),
            [
                "New connection…",
                "New folder…",
                "Import connections…",
                "—",
                "Refresh",
                "Collapse all (no folders)",
            ]
        );
        let folders = ["Work".to_string()];
        assert_eq!(
            labels(&menu(&Subject::List, &disconnected, &folders, None))[5],
            "Collapse all"
        );
        assert_eq!(
            labels(&menu(
                &Subject::Folder("Work".into()),
                &disconnected,
                &folders,
                None
            )),
            [
                "New connection here…",
                "New subfolder…",
                "—",
                "Rename…",
                "—",
                "Delete…",
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
        ));
        assert_eq!(
            closed,
            [
                "Connect",
                "Disconnect (not connected)",
                "Reconnect (not connected)",
                "—",
                "New SQL editor",
                "Administration (not connected)",
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
        ));
        assert_eq!(
            open[..3],
            ["Connect (connected)", "Disconnect", "Reconnect"]
        );
        assert_eq!(open[5], "Administration");
        assert_eq!(open[14], "Refresh");
        assert!(open[10].starts_with("Move to folder ▸ [No folder (already there), Work,"));

        let connecting = labels(&menu(&subject, &Status::Connecting, &folders, None));
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
}
