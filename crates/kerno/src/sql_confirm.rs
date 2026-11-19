//! The confirmation every statement that changes or destroys something
//! goes through: it shows the exact SQL before running it, and asks a
//! second time when the connection's environment says so (production does,
//! with no setup: CLAUDE.md §1).

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::InteractiveElement as _;
use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, Focusable as _, FontWeight, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, TestSupportExt as _, Window, div, px,
};
use kerno_core::schema::{Dependent, Tie};
use kerno_core::{Error, describe, environment};

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

/// What else the statement reaches, read while the dialog is open (C11):
/// what a cascade drops with the object, what refuses a plain drop, what a
/// rename leaves naming the old name.
pub struct Reach {
    /// The read, off the UI thread. [`Error::Unsupported`]: the engine reads
    /// nothing for this statement, and the dialog shows nothing.
    pub reading: Task<kerno_core::Result<Vec<Dependent>>>,
    /// What an empty list says ("Nothing else depends on it.").
    pub empty: String,
    /// Whether the button waits for the list (at most [`REACH_WAIT`]): set
    /// where the list is what the statement destroys (a cascade), so it is
    /// not confirmed blind.
    pub waits: bool,
}

/// How long the button waits for the list of a [`Reach::waits`] before it
/// can be clicked without it.
pub const REACH_WAIT: Duration = Duration::from_secs(10);

/// Lines of the list drawn before "and N more".
const REACH_SHOWN: usize = 8;

enum Reached {
    Reading {
        slow: bool,
    },
    /// The engine reads nothing for this statement ([`Error::Unsupported`]):
    /// nothing is shown.
    Unread,
    Read(Vec<Dependent>),
    Failed(String),
}

struct ReachView {
    state: Reached,
    empty: String,
    waits: bool,
    _tasks: [Task<()>; 2],
}

struct ConfirmView {
    confirmation: Confirmation,
    /// The list of what else it reaches, when the caller asked for one.
    reach: Option<ReachView>,
    name: Option<Entity<InputState>>,
    list: Option<Entity<TextareaState>>,
    /// The environment's name when it asks twice: the first click on the
    /// action arms it and the second runs it.
    twice: Option<String>,
    /// The environment's name when its rules ask (every run, or a write
    /// twice): the title names it and the connection.
    asked_by: Option<String>,
    armed: bool,
    /// `run` was called: the confirmation was not declined.
    confirmed: bool,
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
    open_or_decline(confirmation, window, cx, run, |_| {});
}

/// As [`open`], and calls `declined` when the confirmation goes away without
/// running (Cancel, Escape, a click beside it): a run on several connections
/// skips that target (J14).
pub fn open_or_decline(
    confirmation: Confirmation,
    window: &mut Window,
    cx: &mut App,
    run: impl Fn(String, &mut Window, &mut App) + 'static,
    declined: impl FnOnce(&mut App) + 'static,
) {
    show(confirmation, None, window, cx, run, declined);
}

/// As [`open`], with the list of what else the statement reaches under its
/// explanation (C11).
pub fn open_with_reach(
    confirmation: Confirmation,
    reach: Reach,
    window: &mut Window,
    cx: &mut App,
    run: impl Fn(String, &mut Window, &mut App) + 'static,
) {
    show(confirmation, Some(reach), window, cx, run, |_| {});
}

fn show(
    confirmation: Confirmation,
    reach: Option<Reach>,
    window: &mut Window,
    cx: &mut App,
    run: impl Fn(String, &mut Window, &mut App) + 'static,
    declined: impl FnOnce(&mut App) + 'static,
) {
    let view = cx.new(|cx| {
        cx.on_release(|view: &mut ConfirmView, cx| {
            if !view.confirmed {
                cx.defer(declined);
            }
        })
        .detach();
        let mut view = ConfirmView::new(confirmation, Rc::new(run), window, cx);
        view.reach = reach.map(|reach| ReachView::start(reach, cx));
        view
    });
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
            reach: None,
            name,
            list,
            twice,
            asked_by,
            armed: false,
            confirmed: false,
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

    /// Whether the button waits for the list of what the statement
    /// destroys.
    fn held(&self) -> bool {
        self.reach.as_ref().is_some_and(|reach| {
            reach.waits && matches!(reach.state, Reached::Reading { slow: false })
        })
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(sql) = self.statement(cx) else {
            return;
        };
        if self.held() {
            return;
        }
        if self.twice.is_some() && !self.armed {
            self.armed = true;
            cx.notify();
            return;
        }
        self.confirmed = true;
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
            .children(self.reach.as_ref().and_then(|reach| reach.render(cx)))
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
                            .disabled(statement.is_err() || self.held())
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    ),
            )
    }
}

impl ReachView {
    /// Starts the read, and the clock after which the button stops waiting.
    fn start(reach: Reach, cx: &mut Context<ConfirmView>) -> Self {
        let Reach {
            reading,
            empty,
            waits,
        } = reach;
        let read = cx.spawn(async move |this, cx| {
            let outcome = reading.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(reach) = &mut this.reach {
                    reach.state = match outcome {
                        Ok(found) => Reached::Read(found),
                        Err(Error::Unsupported(_)) => Reached::Unread,
                        Err(error) => Reached::Failed(describe(&error).to_string()),
                    };
                }
                cx.notify();
            });
        });
        let clock = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REACH_WAIT).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(Reached::Reading { slow }) =
                    this.reach.as_mut().map(|reach| &mut reach.state)
                {
                    *slow = true;
                    cx.notify();
                }
            });
        });
        Self {
            state: Reached::Reading { slow: false },
            empty,
            waits,
            _tasks: [read, clock],
        }
    }

    /// The panel, or nothing while a list the button does not wait for is
    /// read (most come in a few milliseconds: no line that would vanish).
    fn render(&self, cx: &App) -> Option<gpui_kit::AnyElement> {
        let theme = cx.theme();
        let (lines, copied) = match &self.state {
            Reached::Unread => return None,
            Reached::Reading { .. } if !self.waits => return None,
            Reached::Reading { slow: false } => {
                (vec![Line::Note("Reading what depends on it…".into())], None)
            }
            Reached::Reading { slow: true } => (
                vec![Line::Warning(format!(
                    "Still reading what depends on it after {} s. You can go on without the list.",
                    REACH_WAIT.as_secs()
                ))],
                None,
            ),
            Reached::Failed(error) => (
                vec![
                    Line::Warning(
                        "Kerno could not read what depends on it, so nothing is listed here."
                            .into(),
                    ),
                    Line::Note(error.clone()),
                ],
                None,
            ),
            Reached::Read(found) if found.is_empty() => {
                (vec![Line::Note(self.empty.clone())], None)
            }
            Reached::Read(found) => (summary(found, REACH_SHOWN), Some(listed(found))),
        };
        let label = lines.iter().map(Line::text).collect::<Vec<_>>().join("\n");
        Some(
            v_flex()
                .id("sql-reach")
                .test_support()
                .aria_label(SharedString::from(label))
                .gap_0p5()
                .text_sm()
                .children(lines.into_iter().map(|line| {
                    match line {
                        Line::Heading(text) => div()
                            .pt_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(text)),
                        Line::Item(text) => div()
                            .pl_3()
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .child(SharedString::from(text)),
                        Line::More(text) | Line::Note(text) => div()
                            .pl_3()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(text)),
                        Line::Warning(text) => div()
                            .text_color(theme.warning)
                            .child(SharedString::from(text)),
                    }
                }))
                .when_some(copied, |this, copied| {
                    this.child(
                        h_flex().pt_1().child(
                            Button::new("sql-reach-copy")
                                .ghost()
                                .label("Copy the list")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                                }),
                        ),
                    )
                })
                .into_any_element(),
        )
    }
}

/// A line of the panel.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    Heading(String),
    Item(String),
    More(String),
    Note(String),
    Warning(String),
}

impl Line {
    fn text(&self) -> &str {
        match self {
            Line::Heading(text)
            | Line::Item(text)
            | Line::More(text)
            | Line::Note(text)
            | Line::Warning(text) => text,
        }
    }
}

/// What the dialog says above the objects of one tie.
fn heading(tie: Tie) -> &'static str {
    match tie {
        Tie::Dropped => "Also dropped",
        Tie::Emptied => "Also emptied",
        Tie::Blocks => "The server refuses the drop while these depend on it",
        Tie::Breaks => "These stay but stop working",
        Tie::Mentions => "These mention its name (a text search: Kerno did not parse them)",
    }
}

fn plural(kind: &str, count: usize) -> String {
    if count == 1 {
        return format!("1 {kind}");
    }
    let many = if kind.ends_with('y') && !kind.ends_with("ey") {
        format!("{}ies", &kind[..kind.len() - 1])
    } else if kind.ends_with('x') || kind.ends_with('s') || kind.ends_with("ch") {
        format!("{kind}es")
    } else {
        format!("{kind}s")
    };
    format!("{count} {many}")
}

/// The objects grouped by tie, each group under its heading with a count per
/// kind ("Also dropped: 4 views, 1 foreign key"), at most `shown` objects
/// drawn in all, the rest of a group said as "and N more".
fn summary(found: &[Dependent], shown: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut left = shown;
    let mut ties: Vec<Tie> = found.iter().map(|d| d.tie).collect();
    ties.sort();
    ties.dedup();
    for tie in ties {
        let group: Vec<&Dependent> = found.iter().filter(|d| d.tie == tie).collect();
        let mut kinds: Vec<(&str, usize)> = Vec::new();
        for dependent in &group {
            match kinds.iter_mut().find(|(kind, _)| *kind == dependent.kind) {
                Some((_, count)) => *count += 1,
                None => kinds.push((&dependent.kind, 1)),
            }
        }
        kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let counts: Vec<String> = kinds
            .iter()
            .map(|(kind, count)| plural(kind, *count))
            .collect();
        lines.push(Line::Heading(format!(
            "{}: {}",
            heading(tie),
            counts.join(", ")
        )));
        let drawn = group.len().min(left);
        lines.extend(
            group[..drawn]
                .iter()
                .map(|d| Line::Item(format!("{} {}", d.kind, d.name))),
        );
        if drawn < group.len() {
            lines.push(Line::More(format!("and {} more", group.len() - drawn)));
        }
        left -= drawn;
    }
    lines
}

/// Every object, as "Copy the list" copies it.
fn listed(found: &[Dependent]) -> String {
    summary(found, usize::MAX)
        .iter()
        .map(|line| match line {
            Line::Item(text) => format!("  {text}"),
            line => line.text().to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependent(tie: Tie, kind: &str, name: &str) -> Dependent {
        Dependent {
            kind: kind.into(),
            name: name.into(),
            tie,
        }
    }

    /// C11: a long list is cut after its first lines with "and N more", and its heading counts
    /// every object by kind; the copy holds them all.
    #[test]
    fn a_long_list_is_cut_and_counted_by_kind() {
        let mut found: Vec<Dependent> = (0..40)
            .map(|i| dependent(Tie::Dropped, "view", &format!("s.v{i}")))
            .collect();
        found.push(dependent(Tie::Dropped, "foreign key", "fk on s.t"));
        found.push(dependent(Tie::Dropped, "policy", "p on s.t"));
        found.push(dependent(Tie::Dropped, "policy", "q on s.t"));
        let lines = summary(&found, 12);
        assert_eq!(
            lines[0],
            Line::Heading("Also dropped: 40 views, 2 policies, 1 foreign key".into())
        );
        assert_eq!(lines[1], Line::Item("view s.v0".into()));
        assert_eq!(lines.len(), 1 + 12 + 1);
        assert_eq!(lines[13], Line::More("and 31 more".into()));
        let copied = listed(&found);
        assert_eq!(copied.lines().count(), 1 + 43);
        assert!(copied.contains("  policy q on s.t"), "{copied}");
    }

    /// C11: MySQL's lines of different meaning come under their own headings, in a fixed order.
    #[test]
    fn each_tie_has_its_heading() {
        let found = [
            dependent(Tie::Breaks, "view", "kerno.v"),
            dependent(Tie::Blocks, "foreign key", "fk on kerno.child"),
        ];
        let lines = summary(&found, 12);
        assert_eq!(
            lines,
            [
                Line::Heading(
                    "The server refuses the drop while these depend on it: 1 foreign key".into()
                ),
                Line::Item("foreign key fk on kerno.child".into()),
                Line::Heading("These stay but stop working: 1 view".into()),
                Line::Item("view kerno.v".into()),
            ]
        );
        assert_eq!(plural("index", 2), "2 indexes");
        assert_eq!(plural("default value", 3), "3 default values");
    }
}
