//! The confirmation every statement that changes or destroys something
//! goes through: it shows the exact SQL before running it, and asks a
//! second time when the connection's environment says so (production does,
//! with no setup: CLAUDE.md §1).

use std::rc::Rc;

use gpui_kit::InteractiveElement as _;
use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, FontWeight, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, TestSupportExt as _, Window, div, px,
};
use kerno_core::environment;

use crate::state::AppState;

/// Writes the statement for a typed name, or says why there is none.
pub type BuildStatement = dyn Fn(&str) -> Result<String, String>;

/// Writes the statement for a typed name and list, or says why there is
/// none.
pub type BuildListStatement = dyn Fn(&str, &[String]) -> Result<String, String>;

/// Runs the confirmed statement.
type Run = dyn Fn(String, &mut Window, &mut App);

/// Where the statement comes from.
pub enum Statement {
    /// Known in advance.
    Fixed(String),
    /// Built from a name the user types (rename, create).
    Named {
        /// Label of the field.
        label: &'static str,
        /// What the field holds at first.
        initial: String,
        /// The statement for a name, or why there is none for it.
        build: Rc<BuildStatement>,
    },
    /// Built from a text the user types, which may be left empty (a
    /// comment, removed when empty).
    Text {
        /// Label of the field.
        label: &'static str,
        /// What the field holds at first.
        initial: String,
        /// The statement for the text, trimmed, or why there is none.
        build: Rc<BuildStatement>,
    },
    /// Built from a name and a list typed one item per line (an enum and
    /// its values).
    NamedList {
        /// Label of the list field.
        list_label: &'static str,
        /// The statement for a name and the non-empty lines, trimmed.
        build: Rc<BuildListStatement>,
    },
}

/// What is asked.
pub struct Confirmation {
    pub title: String,
    /// What running the statement does, in a sentence.
    pub explanation: String,
    pub statement: Statement,
    /// Label of the button that runs it ("Drop table").
    pub action: &'static str,
    /// Whether the statement destroys data (styles the button).
    pub destructive: bool,
    /// The connection it runs on.
    pub connection: String,
    /// Id of the connection's environment: its rules say whether a write
    /// asks twice.
    pub environment: String,
    /// Whether the statement writes: only a write asks a second time (a
    /// read confirmed because its environment confirms every run does not).
    pub writes: bool,
}

struct ConfirmView {
    confirmation: Confirmation,
    name: Option<Entity<InputState>>,
    list: Option<Entity<TextareaState>>,
    /// The environment's name when it asks twice: the first click on the
    /// action arms it and the second runs it.
    twice: Option<String>,
    /// The environment's name when its rules ask (every run, or a write
    /// twice): the title names it and the connection.
    asked_by: Option<String>,
    armed: bool,
    run: Rc<Run>,
    _subscriptions: Vec<Subscription>,
}

/// Asks, then calls `run` with the statement to execute.
pub fn open(
    confirmation: Confirmation,
    window: &mut Window,
    cx: &mut App,
    run: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let view = cx.new(|cx| ConfirmView::new(confirmation, Rc::new(run), window, cx));
    let title = SharedString::from(view.read(cx).title());
    let focus = view.read(cx).name.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = view.clone();
        dialog
            .title(
                div()
                    .id("sql-title")
                    .test_support()
                    .aria_label(title.clone())
                    .child(title.clone()),
            )
            .w(px(560.))
            // Enter never runs a statement: that takes a click on its button.
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    if let Some(name) = focus {
        window.defer(cx, move |window, cx| {
            name.read(cx).focus_handle(cx).focus(window, cx)
        });
    }
}

impl ConfirmView {
    fn new(
        confirmation: Confirmation,
        run: Rc<Run>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = match &confirmation.statement {
            Statement::Fixed(_) => None,
            Statement::Named { initial, .. } | Statement::Text { initial, .. } => {
                Some(initial.clone())
            }
            Statement::NamedList { .. } => Some(String::new()),
        };
        let name =
            initial.map(|initial| cx.new(|cx| InputState::new(window, cx).default_value(initial)));
        let list = matches!(confirmation.statement, Statement::NamedList { .. })
            .then(|| cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8)));
        // A changed statement is confirmed afresh.
        let disarm = |this: &mut Self, event: &InputEvent, cx: &mut Context<Self>| {
            if matches!(event, InputEvent::Change) {
                this.armed = false;
                cx.notify();
            }
        };
        let mut subscriptions = Vec::new();
        if let Some(name) = &name {
            subscriptions
                .push(cx.subscribe(name, move |this, _, event, cx| disarm(this, event, cx)));
        }
        if let Some(list) = &list {
            subscriptions
                .push(cx.subscribe(list, move |this, _, event, cx| disarm(this, event, cx)));
        }
        let environments = &AppState::get(cx).settings.environments;
        let environment = environment::environment(environments, &confirmation.environment);
        let twice = (confirmation.writes && environment.rules.confirm_writes_twice)
            .then(|| environment.name.clone());
        let asked_by = (twice.is_some() || environment.rules.confirm_execute)
            .then(|| environment.name.clone());
        Self {
            confirmation,
            name,
            list,
            twice,
            asked_by,
            armed: false,
            run,
            _subscriptions: subscriptions,
        }
    }

    /// The dialog's title: the caller's, naming the environment and the
    /// connection when the environment's rules are why it asks ("Run this
    /// statement on Production: Reporting?").
    fn title(&self) -> String {
        let title = &self.confirmation.title;
        match &self.asked_by {
            Some(environment) => format!(
                "{} on {environment}: {}?",
                title.trim_end_matches('?'),
                self.confirmation.connection
            ),
            None => title.clone(),
        }
    }

    /// The statement as it stands, or why there is none.
    fn statement(&self, cx: &App) -> Result<String, String> {
        let typed = match &self.name {
            Some(name) => name.read(cx).value().trim().to_string(),
            None => String::new(),
        };
        match &self.confirmation.statement {
            Statement::Fixed(sql) => Ok(sql.clone()),
            Statement::Text { build, .. } => build(&typed),
            Statement::Named { label, .. } if typed.is_empty() => {
                Err(format!("Type a {}.", label.to_lowercase()))
            }
            _ if typed.is_empty() => Err("Type a name.".into()),
            Statement::Named { build, .. } => build(&typed),
            Statement::NamedList { build, .. } => {
                let lines: Vec<String> = self
                    .list
                    .as_ref()
                    .map(|list| list.read(cx).value().to_string())
                    .unwrap_or_default()
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect();
                build(&typed, &lines)
            }
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(sql) = self.statement(cx) else {
            return;
        };
        if self.twice.is_some() && !self.armed {
            self.armed = true;
            cx.notify();
            return;
        }
        window.close_dialog(cx);
        (self.run)(sql, window, cx);
    }
}

impl Render for ConfirmView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let statement = self.statement(cx);
        let label = match &self.twice {
            Some(environment) if self.armed => SharedString::from(format!("Run on {environment}")),
            _ => SharedString::from(self.confirmation.action),
        };
        let labelled = |label: &'static str, field: gpui_kit::AnyElement| {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(field)
        };
        let name_label = match &self.confirmation.statement {
            Statement::Named { label, .. } | Statement::Text { label, .. } => label,
            _ => "Name",
        };
        let field = self.name.as_ref().map(|name| {
            labelled(
                name_label,
                Input::new(name).id("sql-name").into_any_element(),
            )
        });
        let list = match (&self.confirmation.statement, &self.list) {
            (Statement::NamedList { list_label, .. }, Some(list)) => Some(labelled(
                list_label,
                div()
                    .id("sql-list")
                    .test_support()
                    .child(Textarea::new(list))
                    .into_any_element(),
            )),
            _ => None,
        };
        // The environment's colour, as its chrome carries it (the bar's
        // colours: its text reads on it in both themes).
        let band = self.asked_by.as_ref().and_then(|environment| {
            let chrome = crate::theme::environment_chrome_of(&self.confirmation.environment, cx)?;
            let text = format!("{environment} · {}", self.confirmation.connection);
            Some(
                div()
                    .id("sql-environment")
                    .test_support()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .bg(chrome.bar)
                    .text_color(chrome.bar_text)
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .aria_label(SharedString::from(text.clone()))
                    .child(SharedString::from(text)),
            )
        });
        v_flex()
            .gap_3()
            .pt_1()
            .children(band)
            .child(
                div()
                    .text_sm()
                    .child(SharedString::from(self.confirmation.explanation.clone()))
                    .test_text(self.confirmation.explanation.clone()),
            )
            .children(field)
            .children(list)
            .child(
                div()
                    .id("sql-preview")
                    .test_support()
                    .p_2()
                    .max_h(px(240.))
                    .overflow_hidden()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .map(|this| match &statement {
                        Ok(sql) => this.child(
                            div()
                                .child(SharedString::from(sql.clone()))
                                .test_text(sql.clone()),
                        ),
                        Err(reason) => this
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(reason.clone())),
                    }),
            )
            .when_some(self.twice.as_ref(), |this, environment| {
                // The id names the rule's oldest case, production.
                this.child(
                    div()
                        .id("sql-production")
                        .test_support()
                        .text_sm()
                        .text_color(if self.armed {
                            theme.danger
                        } else {
                            theme.warning
                        })
                        .child(SharedString::from(if self.armed {
                            format!(
                                "“{}” is a {environment} connection. Click again to run this on \
                                 {environment}.",
                                self.confirmation.connection
                            )
                        } else {
                            format!(
                                "“{}” is a {environment} connection: you will be asked twice.",
                                self.confirmation.connection
                            )
                        })),
                )
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("sql-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("sql-confirm")
                            .map(|button| {
                                if self.confirmation.destructive || self.armed {
                                    button.danger()
                                } else {
                                    button.primary()
                                }
                            })
                            .label(label)
                            .disabled(statement.is_err())
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    ),
            )
    }
}
