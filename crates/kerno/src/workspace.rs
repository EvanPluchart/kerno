//! The main window: title bar, sidebar, tabs and status bar, and the
//! commands that act on them.

use std::time::Duration;

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, TitleBar, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, BorrowAppContext as _, Context, Entity, FocusHandle, Focusable,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div, px,
};
use kerno_core::storage::SavedConnection;

use crate::actions::{
    self, About, CloseTab, NewQueryTab, NextTab, OpenCommandPalette, OpenSettings, PreviousTab,
    ToggleSidebar, UseDarkTheme, UseLightTheme, UseSystemTheme,
};
use crate::bridge;
use crate::state::{ACTIVE_TAB_KEY, AppState};
use crate::tabs::{QueryTab, TabSnapshot};
use crate::theme::{self, Appearance};

/// How long after the last change the open tabs are written to storage.
const SAVE_TABS_DELAY: Duration = Duration::from_millis(400);

/// The root view of a Kerno window.
pub struct Workspace {
    focus_handle: FocusHandle,
    connections: Vec<SavedConnection>,
    tabs: Vec<Entity<QueryTab>>,
    active_tab: usize,
    /// Number given to the next "Query N" tab.
    next_query_number: usize,
    palette: Entity<CommandState>,
    pending_save: Option<Task<()>>,
    tab_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    /// Creates the workspace with the saved connections and the tabs that
    /// were open last time.
    pub fn new(
        connections: Vec<SavedConnection>,
        tabs: Vec<TabSnapshot>,
        active_tab: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let palette = cx.new(|cx| CommandState::new(window, cx));
        let follow_system = cx.observe_window_appearance(window, |_, window, cx| {
            if AppState::get(cx).appearance() == Appearance::System {
                theme::apply(Appearance::System, Some(window), cx);
            }
        });
        // Text typed in the last moments before quitting is not lost.
        let flush_on_quit = cx.on_app_quit(|this, cx| this.save_tabs(cx));

        let mut workspace = Self {
            focus_handle: cx.focus_handle(),
            connections,
            tabs: Vec::new(),
            active_tab: 0,
            next_query_number: 1,
            palette,
            pending_save: None,
            tab_subscriptions: Vec::new(),
            _subscriptions: vec![follow_system, flush_on_quit],
        };
        for snapshot in tabs {
            match snapshot {
                TabSnapshot::Query { title, sql } => {
                    workspace.push_query_tab(title, &sql, window, cx)
                }
            }
        }
        // The tab that was active is active again.
        workspace.active_tab = active_tab.min(workspace.tabs.len().saturating_sub(1));
        workspace
    }

    /// Titles of the open tabs, in order.
    #[cfg(test)]
    pub fn tab_titles(&self, cx: &App) -> Vec<String> {
        self.tabs
            .iter()
            .map(|tab| tab.read(cx).title().to_string())
            .collect()
    }

    /// Index of the active tab, when there is one.
    #[cfg(test)]
    pub fn active_tab_index(&self) -> Option<usize> {
        (!self.tabs.is_empty()).then_some(self.active_tab)
    }

    /// The active query tab.
    #[cfg(test)]
    pub fn active_query_tab(&self) -> Option<&Entity<QueryTab>> {
        self.tabs.get(self.active_tab)
    }

    fn push_query_tab(
        &mut self,
        title: String,
        sql: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Keep "Query N" numbers unique across restored and new tabs.
        if let Some(number) = title
            .strip_prefix("Query ")
            .and_then(|n| n.parse::<usize>().ok())
        {
            self.next_query_number = self.next_query_number.max(number + 1);
        }
        let tab = cx.new(|cx| QueryTab::new(title, sql, window, cx));
        let editor = tab.read(cx).editor().clone();
        self.tab_subscriptions
            .push(cx.observe(&editor, |this, _, cx| this.schedule_save(cx)));
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(index) {
            self.active_tab = index;
            tab.focus_handle(cx).focus(window, cx);
            self.schedule_save(cx);
            cx.notify();
        }
    }

    fn close_tab_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        drop(self.tab_subscriptions.remove(index));
        if self.active_tab > index || self.active_tab >= self.tabs.len() {
            self.active_tab = self.active_tab.saturating_sub(1);
        }
        match self.tabs.get(self.active_tab) {
            Some(tab) => tab.focus_handle(cx).focus(window, cx),
            None => self.focus_handle.focus(window, cx),
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// Writes the open tabs to storage shortly after the last change.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.pending_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_TABS_DELAY).await;
            if let Ok(save) = this.update(cx, |this, cx| this.save_tabs(cx)) {
                save.await;
            }
        }));
    }

    /// Writes the open tabs to storage now.
    fn save_tabs(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let snapshots: Vec<TabSnapshot> = self
            .tabs
            .iter()
            .map(|tab| tab.read(cx).snapshot(cx))
            .collect();
        let active_tab = self.active_tab;
        let storage = AppState::get(cx).storage.clone();
        bridge::spawn_blocking(cx, move || {
            let saved = storage
                .save_tabs(&snapshots)
                .and_then(|()| storage.set_setting(ACTIVE_TAB_KEY, &active_tab));
            if let Err(error) = saved {
                tracing::error!(%error, "could not store the open tabs");
            }
        })
    }

    // Commands ----------------------------------------------------------------

    fn new_query_tab(&mut self, _: &NewQueryTab, window: &mut Window, cx: &mut Context<Self>) {
        let title = format!("Query {}", self.next_query_number);
        self.push_query_tab(title, "", window, cx);
        self.activate_tab(self.tabs.len() - 1, window, cx);
        self.schedule_save(cx);
    }

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab_at(self.active_tab, window, cx);
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.activate_tab((self.active_tab + 1) % self.tabs.len(), window, cx);
        }
    }

    fn previous_tab(&mut self, _: &PreviousTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            let count = self.tabs.len();
            self.activate_tab((self.active_tab + count - 1) % count, window, cx);
        }
    }

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        AppState::update_settings(cx, |settings| {
            settings.sidebar_visible = !settings.sidebar_visible
        });
        cx.notify();
    }

    fn set_appearance(appearance: Appearance, window: &mut Window, cx: &mut App) {
        // An explicit choice ends a theme pinned for this run.
        cx.update_global::<AppState, _>(|state, _| state.appearance_override = None);
        AppState::update_settings(cx, |settings| settings.appearance = appearance);
        theme::apply(appearance, Some(window), cx);
    }

    fn use_system_theme(
        &mut self,
        _: &UseSystemTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Self::set_appearance(Appearance::System, window, cx);
    }

    fn use_light_theme(&mut self, _: &UseLightTheme, window: &mut Window, cx: &mut Context<Self>) {
        Self::set_appearance(Appearance::Light, window, cx);
    }

    fn use_dark_theme(&mut self, _: &UseDarkTheme, window: &mut Window, cx: &mut Context<Self>) {
        Self::set_appearance(Appearance::Dark, window, cx);
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        window.open_dialog(cx, |dialog, _, _| {
            dialog
                .title("Settings")
                .w(px(440.))
                .content(|content, _, cx| {
                    let current = AppState::get(cx).appearance();
                    let selected = Appearance::ALL.iter().position(|a| *a == current);
                    content.child(
                        v_flex()
                            .gap_2()
                            .child(div().font_weight(FontWeight::MEDIUM).child("Appearance"))
                            .child(
                                RadioGroup::new("appearance")
                                    .children(Appearance::ALL.map(Appearance::label))
                                    .selected_index(selected)
                                    .on_change(|index, window, cx| {
                                        if let Some(appearance) = Appearance::ALL.get(*index) {
                                            Workspace::set_appearance(*appearance, window, cx);
                                        }
                                    }),
                            ),
                    )
                })
        });
    }

    fn about(&mut self, _: &About, window: &mut Window, cx: &mut Context<Self>) {
        window.open_alert_dialog(cx, |alert, _, _| {
            alert.title("Kerno").description(format!(
                "Version {}\n\nA fast, native database client.\nLicensed under MIT or Apache-2.0, at your option.",
                env!("CARGO_PKG_VERSION")
            ))
        });
    }

    fn open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette
            .update(cx, |palette, cx| palette.set_query("", window, cx));
        let palette = self.palette.clone();
        // Commands are handled by the workspace, which the dialog is not a
        // child of: on confirm the action is dispatched from here.
        let target = self.focus_handle.clone();
        let focused = std::rc::Rc::new(std::cell::Cell::new(false));
        window.open_dialog(cx, move |dialog, _, _| {
            let palette = palette.clone();
            let target = target.clone();
            let focused = focused.clone();
            dialog
                .close_button(false)
                .p_0()
                .content(move |content, window, cx| {
                    if !focused.replace(true) {
                        let palette = palette.clone();
                        window.defer(cx, move |window, cx| {
                            palette.read(cx).focus_handle(cx).focus(window, cx)
                        });
                    }
                    let target = target.clone();
                    let mut command = Command::new(&palette)
                        .bordered(false)
                        .rounded(cx.theme().radius_lg)
                        .placeholder("Type a command")
                        .max_h(px(420.))
                        .on_confirm(move |index, window, cx| {
                            window.close_dialog(cx);
                            if let Some(command) = palette_command(index.section, index.row) {
                                let action = (command.action)();
                                let target = target.clone();
                                window.defer(cx, move |window, cx| {
                                    target.focus(window, cx);
                                    target.dispatch_action(action.as_ref(), window, cx);
                                });
                            }
                        });
                    for (group, commands) in palette_groups() {
                        command = command.group(CommandGroup::new().label(group).items(
                            commands.into_iter().map(|command| {
                                CommandItem::new()
                                    .label(command.label)
                                    .keywords(command.keywords.iter().copied())
                            }),
                        ));
                    }
                    content.child(command)
                })
        });
    }

    // Rendering ---------------------------------------------------------------

    fn render_sidebar(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("sidebar")
            .test_support()
            .size_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(
                div()
                    .px_3()
                    .pt_3()
                    .pb_2()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("CONNECTIONS"),
            )
            .when(self.connections.is_empty(), |this| {
                this.child(
                    v_flex()
                        .px_3()
                        .gap_1()
                        .child(div().text_sm().child("No saved connections"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Connections you save are listed here."),
                        ),
                )
            })
            .children(self.connections.iter().map(|connection| {
                let config = &connection.config;
                let tag = config
                    .color
                    .as_deref()
                    .and_then(|key| theme::tag_color(key, cx))
                    .unwrap_or_else(|| theme::environment_color(config.environment, cx));
                h_flex()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .child(div().size(px(8.)).rounded_full().bg(tag))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .truncate()
                            .child(SharedString::from(config.name.clone())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(config.engine.label()),
                    )
            }))
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        if self.tabs.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    v_flex()
                        .items_center()
                        .gap_1()
                        .child(div().text_lg().font_semibold().child("No tab open"))
                        .child(div().text_color(theme.muted_foreground).child(
                            "Open a query tab to write SQL. Its text is kept between runs.",
                        )),
                )
                .child(
                    Button::new("empty-new-query")
                        .primary()
                        .label("New Query Tab")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(NewQueryTab), cx)
                        }),
                )
                .into_any_element();
        }

        let tab_bar = TabBar::new("tabs")
            .selected_index(self.active_tab)
            .on_click(
                cx.listener(|this, index: &usize, window, cx| {
                    this.activate_tab(*index, window, cx)
                }),
            )
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                Tab::new().label(tab.read(cx).title()).suffix(
                    Button::new(("close-tab", index))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Close tab")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            // The click must not also reach the tab, which
                            // would activate the tab being closed.
                            cx.stop_propagation();
                            this.close_tab_at(index, window, cx)
                        })),
                )
            }))
            .suffix(
                Button::new("new-tab")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("New query tab")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewQueryTab), cx)),
            );
        v_flex()
            .size_full()
            .child(tab_bar)
            .children(
                self.tabs
                    .get(self.active_tab)
                    .map(|tab| div().flex_1().min_h_0().child(tab.clone())),
            )
            .into_any_element()
    }

    fn render_status_bar(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let memory_only = !AppState::get(cx).secrets.is_persistent();
        h_flex()
            .h(px(24.))
            .px_3()
            .gap_3()
            .justify_between()
            .border_t_1()
            .border_color(theme.status_bar_border)
            .bg(theme.status_bar)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child("Not connected")
            .when(memory_only, |this| {
                this.child(
                    div()
                        .text_color(theme.warning)
                        .child("Passwords are kept in memory only (no system secret store in use)"),
                )
            })
    }
}

/// The commands shown in the palette, grouped, in display order.
fn palette_groups() -> Vec<(&'static str, Vec<&'static actions::Command>)> {
    let mut groups: Vec<(&'static str, Vec<&'static actions::Command>)> = Vec::new();
    for command in actions::COMMANDS {
        match groups.iter_mut().find(|(group, _)| *group == command.group) {
            Some((_, commands)) => commands.push(command),
            None => groups.push((command.group, vec![command])),
        }
    }
    groups
}

/// The command at a palette position.
fn palette_command(section: usize, row: usize) -> Option<&'static actions::Command> {
    palette_groups().get(section)?.1.get(row).copied()
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar_visible = AppState::get(cx).settings.sidebar_visible;
        let background = cx.theme().background;
        let foreground = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;

        let main = resizable_panel().child(self.render_tabs(cx));
        let body = if sidebar_visible {
            h_resizable("workspace")
                .child(
                    resizable_panel()
                        .size(px(260.))
                        .size_range(px(200.)..px(480.))
                        .child(self.render_sidebar(cx)),
                )
                .child(main)
        } else {
            h_resizable("workspace").child(main)
        };

        v_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::new_query_tab))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::previous_tab))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::use_system_theme))
            .on_action(cx.listener(Self::use_light_theme))
            .on_action(cx.listener(Self::use_dark_theme))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::about))
            .on_action(cx.listener(Self::open_command_palette))
            .size_full()
            .bg(background)
            .text_color(foreground)
            .child(
                TitleBar::new().child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(muted)
                        .child("Kerno"),
                ),
            )
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_status_bar(cx))
    }
}
