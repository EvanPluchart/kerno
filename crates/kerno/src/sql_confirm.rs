//! The confirmation every statement that changes or destroys something
//! goes through: it shows the exact SQL before running it, and asks a
//! second time on a production connection.

use std::rc::Rc;

use gpui_kit::InteractiveElement as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, TestSupportExt as _, Window, div, px,
};
use kerno_core::config::Environment;

/// Writes the statement for a typed name, or says why there is none.
pub type BuildStatement = dyn Fn(&str) -> Result<String, String>;

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
    pub environment: Environment,
}

struct ConfirmView {
    confirmation: Confirmation,
    name: Option<Entity<InputState>>,
    /// On production, the first click on the action arms it and the second
    /// runs it.
    armed: bool,
    run: Rc<Run>,
    _subscription: Option<Subscription>,
}

/// Asks, then calls `run` with the statement to execute.
pub fn open(
    confirmation: Confirmation,
    window: &mut Window,
    cx: &mut App,
    run: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    let title = SharedString::from(confirmation.title.clone());
    let view = cx.new(|cx| ConfirmView::new(confirmation, Rc::new(run), window, cx));
    let focus = view.read(cx).name.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = view.clone();
        dialog
            .title(title.clone())
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
        let name = match &confirmation.statement {
            Statement::Fixed(_) => None,
            Statement::Named { initial, .. } => {
                let initial = initial.clone();
                Some(cx.new(|cx| InputState::new(window, cx).default_value(initial)))
            }
        };
        let subscription = name.as_ref().map(|name| {
            cx.subscribe(name, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    // A changed statement is confirmed afresh.
                    this.armed = false;
                    cx.notify();
                }
            })
        });
        Self {
            confirmation,
            name,
            armed: false,
            run,
            _subscription: subscription,
        }
    }

    /// The statement as it stands, or why there is none.
    fn statement(&self, cx: &App) -> Result<String, String> {
        match (&self.confirmation.statement, &self.name) {
            (Statement::Named { build, .. }, Some(name)) => {
                let typed = name.read(cx).value().trim().to_string();
                if typed.is_empty() {
                    return Err("Type a name.".into());
                }
                build(&typed)
            }
            (Statement::Fixed(sql), _) => Ok(sql.clone()),
            (Statement::Named { .. }, None) => Err("No name.".into()),
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(sql) = self.statement(cx) else {
            return;
        };
        if self.confirmation.environment == Environment::Production && !self.armed {
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
        let production = self.confirmation.environment == Environment::Production;
        let label = if self.armed {
            "Run on production"
        } else {
            self.confirmation.action
        };
        let field = match (&self.confirmation.statement, &self.name) {
            (Statement::Named { label, .. }, Some(name)) => Some(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(*label),
                    )
                    .child(Input::new(name).id("sql-name")),
            ),
            _ => None,
        };
        v_flex()
            .gap_3()
            .pt_1()
            .child(
                div()
                    .text_sm()
                    .child(SharedString::from(self.confirmation.explanation.clone())),
            )
            .children(field)
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
                        Ok(sql) => this.child(SharedString::from(sql.clone())),
                        Err(reason) => this
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(reason.clone())),
                    }),
            )
            .when(production, |this| {
                this.child(
                    div()
                        .id("sql-production")
                        .test_support()
                        .text_sm()
                        .text_color(if self.armed { theme.danger } else { theme.warning })
                        .child(SharedString::from(if self.armed {
                            format!(
                                "“{}” is a production connection. Click again to run this on production.",
                                self.confirmation.connection
                            )
                        } else {
                            format!(
                                "“{}” is a production connection: you will be asked twice.",
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
