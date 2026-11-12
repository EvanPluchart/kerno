//! The sidebar: saved connections, grouped, with their live status and the
//! actions on them.

use std::collections::BTreeMap;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    TestSupportExt as _, Window, div, px,
};
use kerno_core::storage::SavedConnection;

use crate::actions::NewConnection;
use crate::connections::dialog::ConnectionDialog;
use crate::connections::manager::{Connections, Status};
use crate::engine_logo::engine_logo;
use crate::theme;

/// The sidebar view.
pub struct Sidebar {
    connections: Entity<Connections>,
    /// The connection last clicked.
    selected: Option<String>,
    _subscription: Subscription,
}

impl Sidebar {
    /// The sidebar for `connections`.
    pub fn new(connections: Entity<Connections>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&connections, |_, _, cx| cx.notify());
        Self {
            connections,
            selected: None,
            _subscription: subscription,
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

    fn render_row(
        &self,
        index: usize,
        saved: &SavedConnection,
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
        let connected = matches!(status, Status::Connected { .. });
        let busy = matches!(status, Status::Connecting | Status::WaitingForInput);
        let selected = self.selected.as_deref() == Some(saved.id.as_str());

        let id = saved.id.clone();
        let connections = self.connections.clone();
        let saved_for_menu = saved.clone();
        v_flex()
            .id(("connection", index))
            .test_support()
            .px_3()
            .py_1()
            .when(selected, |this| this.bg(theme.sidebar_accent))
            .hover(|style| style.bg(theme.sidebar_accent))
            .cursor_pointer()
            .on_click(cx.listener({
                let id = id.clone();
                move |this, event: &ClickEvent, _, cx| {
                    this.selected = Some(id.clone());
                    // A double click opens (or closes) the connection.
                    if event.click_count() >= 2 {
                        this.connections.update(cx, |connections, cx| {
                            match connections.status(&id) {
                                Status::Connected { .. } => connections.disconnect(&id, cx),
                                _ => connections.connect(&id, cx),
                            }
                        });
                    }
                    cx.notify();
                }
            }))
            .context_menu(move |menu, _, _| {
                let connections = connections.clone();
                let saved = saved_for_menu.clone();
                let id = saved.id.clone();
                menu.item(
                    PopupMenuItem::new(if connected { "Disconnect" } else { "Connect" })
                        .disabled(busy)
                        .on_click({
                            let (connections, id) = (connections.clone(), id.clone());
                            move |_, _, cx| {
                                connections.update(cx, |connections, cx| {
                                    if connected {
                                        connections.disconnect(&id, cx)
                                    } else {
                                        connections.connect(&id, cx)
                                    }
                                })
                            }
                        }),
                )
                .separator()
                .item(PopupMenuItem::new("Edit…").on_click({
                    let (connections, id) = (connections.clone(), id.clone());
                    move |_, window, cx| Sidebar::edit(connections.clone(), id.clone(), window, cx)
                }))
                .item(PopupMenuItem::new("Duplicate").on_click({
                    let (connections, id) = (connections.clone(), id.clone());
                    move |_, _, cx| {
                        connections.update(cx, |connections, cx| connections.duplicate(&id, cx))
                    }
                }))
                .separator()
                .item(PopupMenuItem::new("Delete…").on_click({
                    let connections = connections.clone();
                    move |_, window, cx| {
                        Sidebar::confirm_delete(connections.clone(), &saved, window, cx)
                    }
                }))
            })
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
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let saved: Vec<SavedConnection> = self.connections.read(cx).saved().to_vec();
        // Ungrouped connections first, then each group under its name.
        let mut groups: BTreeMap<String, Vec<(usize, &SavedConnection)>> = BTreeMap::new();
        for (index, connection) in saved.iter().enumerate() {
            groups
                .entry(connection.config.group.clone().unwrap_or_default())
                .or_default()
                .push((index, connection));
        }
        let muted = cx.theme().muted_foreground;
        let mut list = v_flex()
            .id("connection-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll();
        for (group, connections) in &groups {
            if !group.is_empty() {
                list = list.child(
                    div()
                        .px_3()
                        .pt_2()
                        .pb_1()
                        .text_xs()
                        .text_color(muted)
                        .child(SharedString::from(group.clone())),
                );
            }
            for (index, connection) in connections {
                list = list.child(self.render_row(*index, connection, cx));
            }
        }

        let theme = cx.theme();
        v_flex()
            .id("sidebar")
            .test_support()
            .size_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .justify_between()
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
                    ),
            )
            .when(saved.is_empty(), |this| {
                this.child(
                    v_flex()
                        .px_3()
                        .pt_2()
                        .gap_2()
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
            .when(!saved.is_empty(), |this| this.child(list))
    }
}
