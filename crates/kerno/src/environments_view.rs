//! Settings → Environments: the environments connections can be in, as
//! DBeaver's connection types page (`PrefPageConnectionTypes`, read for its
//! behaviour): the list, Add / Duplicate / Delete, and the selected one's
//! name, colours, tint and rules. Every change applies at once to the open
//! windows and is stored.

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, TestSupportExt as _, Window, div, px, rgb,
};
use kerno_core::environment::{self, EnvironmentDef, EnvironmentRules, Tint};

use crate::connections::manager::Connections;
use crate::state::AppState;
use crate::theme;

/// The idle period a rule starts with when it is turned on, in seconds.
const IDLE_DEFAULT: u32 = 600;

const AUTO_COMMIT: &str = "Auto-commit by default (otherwise query tabs start in manual commit)";
const CONFIRM_EXECUTE: &str = "Confirm every statement run from an editor";
const CONFIRM_EDITS: &str =
    "Confirm data edits (always: changes are shown as SQL before they apply)";
const WRITES_TWICE: &str = "Ask a second time before any statement that writes";
const READ_ONLY: &str = "New connections start read-only";
const CLOSE_IDLE: &str = "Roll back a transaction left idle for";

/// The rules' labels, as drawn: Settings' search finds the environments
/// by them too.
pub(crate) const RULE_LABELS: [&str; 6] = [
    AUTO_COMMIT,
    CONFIRM_EXECUTE,
    CONFIRM_EDITS,
    WRITES_TWICE,
    READ_ONLY,
    CLOSE_IDLE,
];

/// What wins over a rule, or what a rule wins over (C5), under it: from
/// `ConnectionConfig::starts_in_manual_commit`, `QueryTab::execute` and
/// `sql_confirm` (the second question), and the connection dialog (the
/// read-only rule presets a new connection's switch).
const AUTO_COMMIT_WINS: &str =
    "A connection's own commit mode wins over this default (its Advanced tab).";
const CONFIRMATIONS_LOSE: &str =
    "This rule wins over Settings ▸ Confirmations: it asks even when that is off.";
const READ_ONLY_WINS: &str =
    "A connection's Read-only switch wins: this rule only presets it on new ones.";

/// One of the colours of an environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Light,
    Dark,
}

impl Mode {
    fn key(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn field(self, def: &mut EnvironmentDef) -> &mut Option<String> {
        match self {
            Self::Light => &mut def.color_light,
            Self::Dark => &mut def.color_dark,
        }
    }
}

pub struct EnvironmentsView {
    connections: Entity<Connections>,
    selected: usize,
    name: Entity<InputState>,
    color_light: Entity<InputState>,
    color_dark: Entity<InputState>,
    idle_minutes: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl EnvironmentsView {
    pub fn new(
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let name = input("A name", window, cx);
        let color_light = input("#RRGGBB", window, cx);
        let color_dark = input("#RRGGBB", window, cx);
        let idle_minutes = input("10", window, cx);
        let on_change = |input: &Entity<InputState>,
                         cx: &mut Context<Self>,
                         apply: fn(&mut EnvironmentDef, &str)| {
            cx.subscribe(input, move |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.change(cx, |def| apply(def, &text));
                }
            })
        };
        let subscriptions = vec![
            on_change(&name, cx, |def, text| {
                // An environment is never nameless: a blank box keeps the old.
                if !text.trim().is_empty() {
                    def.name = text.trim().to_string();
                }
            }),
            on_change(&color_light, cx, |def, text| {
                set_color(def, Mode::Light, text)
            }),
            on_change(&color_dark, cx, |def, text| {
                set_color(def, Mode::Dark, text)
            }),
            on_change(&idle_minutes, cx, |def, text| {
                if def.rules.close_idle_transactions.is_some()
                    && let Ok(minutes) = text.trim().parse::<u32>()
                    && minutes > 0
                {
                    def.rules.close_idle_transactions = Some(minutes.saturating_mul(60));
                }
            }),
        ];
        let mut view = Self {
            connections,
            selected: 0,
            name,
            color_light,
            color_dark,
            idle_minutes,
            _subscriptions: subscriptions,
        };
        view.select(0, window, cx);
        view
    }

    fn environments(cx: &App) -> &[EnvironmentDef] {
        &AppState::get(cx).settings.environments
    }

    /// The environment being edited.
    fn current(&self, cx: &App) -> Option<EnvironmentDef> {
        Self::environments(cx).get(self.selected).cloned()
    }

    /// Changes the environment being edited, stores it, and repaints every
    /// window (the chrome of its connections follows).
    fn change(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut EnvironmentDef)) {
        let index = self.selected;
        AppState::update_settings(cx, |settings| {
            if let Some(def) = settings.environments.get_mut(index) {
                change(def);
            }
        });
        cx.refresh_windows();
        cx.notify();
    }

    /// Shows environment `index` in the form.
    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = index.min(Self::environments(cx).len().saturating_sub(1));
        let Some(def) = self.current(cx) else {
            return;
        };
        let minutes = def
            .rules
            .close_idle_transactions
            .unwrap_or(IDLE_DEFAULT)
            .div_ceil(60)
            .to_string();
        for (input, value) in [
            (&self.name, def.name.clone()),
            (
                &self.color_light,
                def.color_light.clone().unwrap_or_default(),
            ),
            (&self.color_dark, def.color_dark.clone().unwrap_or_default()),
            (&self.idle_minutes, minutes),
        ] {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        cx.notify();
    }

    /// Adds an environment (a copy of the selected one when `duplicate`),
    /// stores it and selects it.
    fn add(&mut self, duplicate: bool, window: &mut Window, cx: &mut Context<Self>) {
        let environments = Self::environments(cx);
        let new = match self.current(cx).filter(|_| duplicate) {
            Some(source) => {
                let name = format!("{} copy", source.name);
                EnvironmentDef {
                    id: environment::unused_id(environments, &name),
                    name,
                    ..source
                }
            }
            None => {
                // Kerno's blue tag, so it differs from the built-ins.
                let (_, light, dark) = theme::TAG_COLORS[5];
                EnvironmentDef {
                    id: environment::unused_id(environments, "New environment"),
                    name: "New environment".into(),
                    color_light: Some(format!("#{light:06X}")),
                    color_dark: Some(format!("#{dark:06X}")),
                    tint: Tint::Soft,
                    rules: EnvironmentRules::default(),
                }
            }
        };
        AppState::update_settings(cx, |settings| settings.environments.push(new));
        let last = Self::environments(cx).len() - 1;
        self.select(last, window, cx);
        cx.refresh_windows();
    }

    /// How many saved connections are in environment `id`.
    fn users(&self, id: &str, cx: &App) -> usize {
        self.connections
            .read(cx)
            .saved()
            .iter()
            .filter(|saved| saved.config.environment == id)
            .count()
    }

    /// Deletes the selected environment, unless it is built in or used.
    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(def) = self.current(cx) else {
            return;
        };
        if def.deletion_refused(self.users(&def.id, cx)).is_some() {
            return;
        }
        AppState::update_settings(cx, |settings| {
            settings.environments.retain(|kept| kept.id != def.id)
        });
        self.select(self.selected, window, cx);
        cx.refresh_windows();
    }

    fn render_list(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let dark = theme.is_dark();
        let (muted, accent, border) = (theme.muted_foreground, theme.accent, theme.border);
        let refused = self
            .current(cx)
            .and_then(|def| def.deletion_refused(self.users(&def.id, cx)));
        v_flex()
            .w(px(190.))
            .flex_none()
            .gap_2()
            .child(
                v_flex()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(border)
                    .py_1()
                    .children(
                        Self::environments(cx)
                            .iter()
                            .enumerate()
                            .map(|(index, def)| {
                                h_flex()
                                    .id(("env-row", index))
                                    .test_support()
                                    .px_2()
                                    .py_1()
                                    .gap_2()
                                    .cursor_pointer()
                                    .when(index == self.selected, |row| row.bg(accent))
                                    .hover(|row| row.bg(accent))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.select(index, window, cx)
                                    }))
                                    .child(swatch(def.color(dark), border))
                                    .child(
                                        div()
                                            .flex_1()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(SharedString::from(def.name.clone())),
                                    )
                                    .when(def.is_builtin(), |row| {
                                        row.child(
                                            div()
                                                .id(("env-builtin", index))
                                                .test_support()
                                                .text_xs()
                                                .text_color(muted)
                                                .child("built-in"),
                                        )
                                    })
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("env-add")
                            .outline()
                            .small()
                            .label("Add")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add(false, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("env-duplicate")
                            .outline()
                            .small()
                            .label("Duplicate")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add(true, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("env-delete")
                            .outline()
                            .small()
                            .label("Delete")
                            .disabled(refused.is_some())
                            .on_click(cx.listener(|this, _, window, cx| this.delete(window, cx))),
                    ),
            )
            .children(refused.map(|reason| {
                div()
                    .id("env-delete-reason")
                    .test_support()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(reason))
            }))
    }

    /// The colour row of one theme: no colour, the connection palette, and
    /// a field for any `#RRGGBB`.
    fn render_colors(&self, mode: Mode, def: &EnvironmentDef, cx: &Context<Self>) -> AnyElement {
        let input = match mode {
            Mode::Light => self.color_light.clone(),
            Mode::Dark => self.color_dark.clone(),
        };
        let current = def.color(mode == Mode::Dark);
        let typed = input.read(cx).value().trim().to_string();
        let invalid = !typed.is_empty() && environment::parse_color(&typed).is_none();
        let (ring, border, danger) = (cx.theme().ring, cx.theme().border, cx.theme().danger);
        let choices = std::iter::once(None).chain(
            theme::TAG_COLORS
                .iter()
                .map(|(_, light, dark)| Some(if mode == Mode::Dark { *dark } else { *light })),
        );
        h_flex()
            .gap_1()
            .children(choices.enumerate().map(|(index, color)| {
                let selected = current == color;
                div()
                    .id(SharedString::from(format!("env-{}-{index}", mode.key())))
                    .test_support()
                    .size(px(18.))
                    .rounded_full()
                    .border_2()
                    .border_color(if selected { ring } else { border })
                    .when_some(color, |this, color| this.bg(rgb(color)))
                    .cursor_pointer()
                    .on_click(cx.listener({
                        let input = input.clone();
                        move |this, _, window, cx| {
                            let hex = color.map(|color| format!("#{color:06X}"));
                            input.update(cx, |input, cx| {
                                input.set_value(hex.clone().unwrap_or_default(), window, cx)
                            });
                            this.change(cx, |def| *mode.field(def) = hex);
                        }
                    }))
            }))
            .child(
                div()
                    .id(SharedString::from(format!("env-color-{}", mode.key())))
                    .test_support()
                    .w(px(96.))
                    .child(Input::new(&input).small()),
            )
            .when(invalid, |this| {
                this.child(div().text_xs().text_color(danger).child("Not #RRGGBB"))
            })
            .into_any_element()
    }

    fn render_form(&self, def: &EnvironmentDef, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let label = |text: &'static str| div().w(px(90.)).flex_none().child(text);
        let rule = |id: &'static str,
                    text: &'static str,
                    checked: bool,
                    set: fn(&mut EnvironmentRules, bool),
                    cx: &mut Context<Self>| {
            let view = cx.entity();
            Checkbox::new(id)
                .label(text)
                .checked(checked)
                .on_click(move |checked, _, cx| {
                    let checked = *checked;
                    view.update(cx, |this, cx| {
                        this.change(cx, |def| set(&mut def.rules, checked))
                    });
                })
        };
        let idle = def.rules.close_idle_transactions.is_some();
        // The line under a rule saying what wins (C5).
        let wins = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .test_support()
                // Tight under its rule, which it is about.
                .mt(px(-6.))
                .text_xs()
                .text_color(muted)
                .child(text)
                .test_text(text)
        };
        let tints = Tint::ALL
            .iter()
            .enumerate()
            .map(|(index, tint)| {
                let tint = *tint;
                theme::chip(("env-tint", index), def.tint == tint)
                    .small()
                    .label(tint.label())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.change(cx, |def| def.tint = tint)),
                    )
            })
            .collect::<Vec<_>>();
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .child(
                h_flex().gap_2().child(label("Name")).child(
                    div()
                        .id("env-name")
                        .test_support()
                        .flex_1()
                        .child(Input::new(&self.name).small()),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(label("Light theme"))
                    .child(self.render_colors(Mode::Light, def, cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(label("Dark theme"))
                    .child(self.render_colors(Mode::Dark, def, cx)),
            )
            .child(h_flex().gap_2().child(label("Tint")).children(tints))
            .child(div().text_xs().text_color(muted).child(
                "How much of a connection's chrome takes the colour: its tabs, the frame of \
                 the editor and the grid, the status bar.",
            ))
            .child(crate::settings_ui::section_title("Rules").pt_1())
            .child(rule(
                "env-auto-commit",
                AUTO_COMMIT,
                def.rules.auto_commit,
                |rules, on| rules.auto_commit = on,
                cx,
            ))
            .child(wins("override-env-auto-commit", AUTO_COMMIT_WINS))
            .child(rule(
                "env-confirm-execute",
                CONFIRM_EXECUTE,
                def.rules.confirm_execute,
                |rules, on| rules.confirm_execute = on,
                cx,
            ))
            .child(wins("override-env-confirm-execute", CONFIRMATIONS_LOSE))
            .child(
                // Kerno always shows pending changes as SQL before applying
                // them (CLAUDE.md §1): the rule is on and cannot be turned off.
                Checkbox::new("env-confirm-edits")
                    .label(CONFIRM_EDITS)
                    .checked(true)
                    .disabled(true),
            )
            .child(rule(
                "env-confirm-writes-twice",
                WRITES_TWICE,
                def.rules.confirm_writes_twice,
                |rules, on| rules.confirm_writes_twice = on,
                cx,
            ))
            .child(wins(
                "override-env-confirm-writes-twice",
                CONFIRMATIONS_LOSE,
            ))
            .child(rule(
                "env-read-only",
                READ_ONLY,
                def.rules.read_only,
                |rules, on| rules.read_only = on,
                cx,
            ))
            .child(wins("override-env-read-only", READ_ONLY_WINS))
            .child(
                h_flex()
                    .gap_2()
                    .child({
                        // Turned on, the rule takes the minutes in the field.
                        let (view, minutes) = (cx.entity(), self.idle_minutes.clone());
                        Checkbox::new("env-close-idle")
                            .label(CLOSE_IDLE)
                            .checked(idle)
                            .on_click(move |on, _, cx| {
                                let seconds = minutes
                                    .read(cx)
                                    .value()
                                    .trim()
                                    .parse::<u32>()
                                    .ok()
                                    .filter(|minutes| *minutes > 0)
                                    .map_or(IDLE_DEFAULT, |minutes| minutes.saturating_mul(60));
                                let period = on.then_some(seconds);
                                view.update(cx, |this, cx| {
                                    this.change(cx, |def| {
                                        def.rules.close_idle_transactions = period
                                    })
                                });
                            })
                    })
                    .child(
                        div()
                            .id("env-idle-minutes")
                            .test_support()
                            .w(px(56.))
                            .child(Input::new(&self.idle_minutes).small().disabled(!idle)),
                    )
                    .child("minutes"),
            )
            .child(div().text_xs().text_color(muted).child(
                "In a query tab whose open transaction changed something, in either commit \
                 mode; never while a statement runs.",
            ))
            .into_any_element()
    }
}

/// A colour dot, or an empty ring of `border` when there is no colour.
pub(crate) fn swatch(color: Option<u32>, border: gpui_kit::Hsla) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(10.))
        .rounded_full()
        .map(|this| match color {
            Some(color) => this.bg(rgb(color)),
            None => this.border_1().border_color(border),
        })
}

/// Stores a typed colour: a valid `#RRGGBB` (in capitals), or none when
/// the field is empty; anything else waits until it is valid.
fn set_color(def: &mut EnvironmentDef, mode: Mode, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        *mode.field(def) = None;
    } else if let Some(color) = environment::parse_color(text) {
        *mode.field(def) = Some(format!("#{color:06X}"));
    }
}

impl Render for EnvironmentsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.current(cx);
        h_flex()
            .id("environments")
            .test_support()
            .items_start()
            .gap_3()
            .child(self.render_list(cx))
            .children(current.map(|def| self.render_form(&def, cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_colour_is_kept_only_when_valid() {
        let mut def = environment::builtin_environments().remove(1);
        set_color(&mut def, Mode::Light, " #1d6fb8 ");
        assert_eq!(def.color_light.as_deref(), Some("#1D6FB8"));
        // Half typed: the last valid colour stays.
        set_color(&mut def, Mode::Light, "#1d6");
        assert_eq!(def.color_light.as_deref(), Some("#1D6FB8"));
        set_color(&mut def, Mode::Dark, "");
        assert_eq!(def.color_dark, None);
    }
}
