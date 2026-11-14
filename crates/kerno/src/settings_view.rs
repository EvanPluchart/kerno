//! The settings (Cmd+,): appearance, fonts, editor, grid, confirmations,
//! tools, and the keymap file. Each choice applies at once and is stored.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, TestSupportExt as _, Window, div, px,
};

use crate::connections::manager::Connections;
use crate::environments_view::EnvironmentsView;
use crate::local_clients_view::LocalClientsView;
use crate::settings::{
    CODE_FONT_SIZES, PAGE_SIZES, Settings, TabLayout, TableDoubleClick, UI_FONT_SIZES,
};
use crate::state::AppState;
use crate::theme::{self, Appearance};
use crate::workspace::Workspace;
use crate::{actions, bridge};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Appearance,
    Fonts,
    Editor,
    Grid,
    Confirmations,
    Tools,
    Environments,
    Updates,
    Ai,
}

pub(crate) const SECTIONS: [(Section, &str); 9] = [
    (Section::Appearance, "Appearance"),
    (Section::Fonts, "Fonts"),
    (Section::Editor, "Editor"),
    (Section::Grid, "Grid"),
    (Section::Confirmations, "Confirmations"),
    (Section::Tools, "Local clients"),
    (Section::Environments, "Environments"),
    (Section::Updates, "Updates"),
    (Section::Ai, "AI"),
];

/// One text size the settings hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontSize {
    Ui,
    Editor,
    Grid,
}

impl FontSize {
    fn id(self) -> &'static str {
        match self {
            Self::Ui => "ui-font",
            Self::Editor => "editor-font",
            Self::Grid => "grid-font",
        }
    }

    fn get(self, settings: &Settings) -> f32 {
        match self {
            Self::Ui => settings.ui_font_size,
            Self::Editor => settings.editor_font_size,
            Self::Grid => settings.grid_font_size,
        }
    }

    /// Changes the size by `step` points within its range, applies and
    /// stores it.
    fn change(self, step: f32, window: &mut Window, cx: &mut App) {
        AppState::update_settings(cx, |settings| {
            let range = if self == Self::Ui {
                UI_FONT_SIZES
            } else {
                CODE_FONT_SIZES
            };
            let size = (self.get(settings) + step).clamp(*range.start(), *range.end());
            match self {
                Self::Ui => settings.ui_font_size = size,
                Self::Editor => settings.editor_font_size = size,
                Self::Grid => settings.grid_font_size = size,
            }
        });
        theme::apply_fonts(cx);
        window.refresh();
    }

    /// Gives the size its default back.
    fn reset(self, window: &mut Window, cx: &mut App) {
        let default = self.get(&Settings::default());
        let step = default - self.get(&AppState::get(cx).settings);
        self.change(step, window, cx);
    }
}

/// The text size of the SQL editors and the grids together: the zoom
/// commands (⌘+ / ⌘- / ⌘0 over an editor or a grid, ⌘-wheel, DBeaver's
/// `resultset.zoomIn` / `zoomOut`) move both by `step` points, each within
/// its range; `None` gives both their defaults back.
pub fn zoom_grid(step: Option<f32>, window: &mut Window, cx: &mut App) {
    for size in [FontSize::Editor, FontSize::Grid] {
        match step {
            Some(step) => size.change(step, window, cx),
            None => size.reset(window, cx),
        }
    }
}

/// Zooms the SQL editors and the grids (as [`zoom_grid`]) with the wheel
/// while Cmd (Ctrl elsewhere) is held over the element it is a child of,
/// which must be `relative`: before the editor or the grid scrolls. A
/// trackpad's small deltas add up to one step per 40 pixels.
pub fn zoom_wheel() -> impl IntoElement {
    use gpui_kit::{HitboxBehavior, ScrollDelta, ScrollEvent, canvas};
    thread_local! {
        static PIXELS: std::cell::Cell<f32> = const { std::cell::Cell::new(0.) };
    }
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        |_, hitbox, window, _| {
            window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                if !phase.capture() || !event.modifiers.secondary(cx) || !hitbox.is_hovered(window)
                {
                    return;
                }
                cx.stop_propagation();
                let steps = match event.delta {
                    ScrollDelta::Lines(lines) if lines.y == 0. => 0.,
                    ScrollDelta::Lines(lines) => lines.y.signum(),
                    ScrollDelta::Pixels(pixels) => PIXELS.with(|sum| {
                        let total = sum.get() + pixels.y.as_f32();
                        let steps = (total / 40.).trunc();
                        sum.set(total - steps * 40.);
                        steps
                    }),
                };
                if steps != 0. {
                    zoom_grid(Some(steps), window, cx);
                }
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// Opens the keymap file, written with an example (every command and its
/// default keys, nothing changed) when it does not exist yet, then shows it
/// in the file manager.
pub fn open_keymap_file(window: &mut Window, cx: &mut App) {
    let path = AppState::get(cx).paths.keymap();
    let writing = bridge::spawn_blocking(cx, move || write_keymap_example(&path).map(|_| path));
    window
        .spawn(cx, async move |cx| {
            let written = writing.await;
            let _ = cx.update(|window, cx| match written {
                Ok(path) => cx.reveal_path(&path),
                Err(error) => window.push_notification(
                    Notification::error(SharedString::from(format!(
                        "The keymap file could not be written: {error}"
                    ))),
                    cx,
                ),
            });
        })
        .detach();
}

/// Writes the keymap example at `path` unless a keymap file is there.
pub fn write_keymap_example(path: &std::path::Path) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, actions::keymap_example())
}

/// The settings view, shown in a dialog.
pub struct SettingsView {
    section: Section,
    page_size: Entity<InputState>,
    null_text: Entity<InputState>,
    local_clients: Entity<LocalClientsView>,
    environments: Entity<EnvironmentsView>,
    ai: Entity<crate::ai_view::AiSettingsView>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    /// Opens the settings in a dialog.
    pub fn open(connections: Entity<Connections>, window: &mut Window, cx: &mut App) {
        let view = cx.new(|cx| Self::new(connections, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            dialog
                .title("Settings")
                .w(px(720.))
                // Enter in a field does not close the settings.
                .on_ok(|_, _, _| false)
                .content(move |content, _, _| content.child(view.clone()))
        });
    }

    fn new(connections: Entity<Connections>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = AppState::get(cx).settings.clone();
        let text = |value: String,
                    placeholder: &'static str,
                    window: &mut Window,
                    cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let page_size = text(settings.page_size.to_string(), "1000", window, cx);
        let null_text = text(settings.null_text.clone(), "NULL", window, cx);
        let subscriptions = vec![
            cx.subscribe(&page_size, |_, input, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let size = input.read(cx).value().trim().replace([',', '_', ' '], "");
                if let Ok(size) = size.parse::<u32>()
                    && PAGE_SIZES.contains(&size)
                {
                    AppState::update_settings(cx, |settings| settings.page_size = size);
                }
                cx.notify();
            }),
            cx.subscribe(&null_text, |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    // NULL never looks like an empty string: a blank box
                    // keeps "NULL".
                    let text = input.read(cx).value().to_string();
                    let text = if text.trim().is_empty() {
                        Settings::default().null_text
                    } else {
                        text
                    };
                    AppState::update_settings(cx, |settings| settings.null_text = text);
                    cx.refresh_windows();
                }
            }),
        ];
        let local_clients = cx.new(LocalClientsView::new);
        let environments = cx.new(|cx| EnvironmentsView::new(connections, window, cx));
        let ai = cx.new(|cx| crate::ai_view::AiSettingsView::new(window, cx));
        Self {
            section: Section::Appearance,
            page_size,
            null_text,
            local_clients,
            environments,
            ai,
            _subscriptions: subscriptions,
        }
    }

    fn heading(text: &'static str) -> impl IntoElement {
        div().pt_2().font_weight(FontWeight::MEDIUM).child(text)
    }

    /// A checkbox bound to one boolean of the settings.
    fn toggle(
        id: &'static str,
        label: &'static str,
        checked: bool,
        set: fn(&mut Settings, bool),
    ) -> Checkbox {
        Checkbox::new(id)
            .label(label)
            .checked(checked)
            .on_click(move |checked, window, cx| {
                let checked = *checked;
                AppState::update_settings(cx, |settings| set(settings, checked));
                window.refresh();
            })
    }

    /// Settings ▸ Editor ▸ Formatter: the options of "Format SQL", format
    /// on save, and a sample laid out with them.
    fn formatter(settings: &Settings, cx: &App) -> impl IntoElement {
        use kerno_core::sql::{Comma, FormatOptions, Indent, KeywordCase, LexRules, format_with};
        const SAMPLE: &str = "select o.id, c.name, sum(o.total) as total from shop.orders o \
            join shop.customers c on c.id = o.customer_id where o.status = 'paid' \
            and o.total > 10 group by o.id, c.name";
        /// A row of chips, one per value of one option.
        fn row<T: Copy + PartialEq + 'static>(
            label: &'static str,
            id: &'static str,
            choices: &[(T, &'static str)],
            current: T,
            set: fn(&mut FormatOptions, T),
        ) -> impl IntoElement {
            h_flex()
                .gap_2()
                .child(div().w(px(180.)).child(label))
                .children(choices.iter().enumerate().map(|(index, &(value, text))| {
                    theme::chip(
                        SharedString::from(format!("{id}-{index}")),
                        value == current,
                    )
                    .small()
                    .label(text)
                    .on_click(move |_, _, cx| {
                        AppState::update_settings(cx, |settings| set(&mut settings.format, value))
                    })
                }))
        }
        let options = settings.format;
        let theme = cx.theme();
        let sample = format_with(SAMPLE, LexRules::POSTGRES, &options).replace('\t', "    ");
        v_flex()
            .gap_2()
            .child(Self::heading("Formatter"))
            .child(row(
                "Keywords",
                "format-case",
                &[
                    (KeywordCase::Upper, "UPPER"),
                    (KeywordCase::Lower, "lower"),
                    (KeywordCase::Keep, "As typed"),
                ],
                options.keyword_case,
                |options, case| options.keyword_case = case,
            ))
            .child(row(
                "Indentation",
                "format-indent",
                &[
                    (Indent::Spaces(2), "2 spaces"),
                    (Indent::Spaces(4), "4 spaces"),
                    (Indent::Tab, "Tab"),
                ],
                options.indent,
                |options, indent| options.indent = indent,
            ))
            .child(row(
                "Commas in lists",
                "format-comma",
                &[(Comma::Trailing, "At line end"), (Comma::Leading, "At line start")],
                options.comma,
                |options, comma| options.comma = comma,
            ))
            .child(row(
                "Short lists on one line",
                "format-width",
                &[(0, "Never"), (80, "Up to 80 columns"), (120, "Up to 120 columns")],
                options.line_width,
                |options, width| options.line_width = width,
            ))
            .child(Self::toggle(
                "settings-format-on-save",
                "Format .sql files when saving them (a script that does not lay out cleanly is saved as it is)",
                settings.format_on_save,
                |settings, on| settings.format_on_save = on,
            ))
            .child(
                div()
                    .id("formatter-sample")
                    .test_support()
                    .p_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .child(SharedString::from(sample)),
            )
    }

    fn font_row(size: FontSize, label: &'static str, cx: &App) -> impl IntoElement {
        let value = size.get(&AppState::get(cx).settings);
        let id = size.id();
        h_flex()
            .gap_2()
            .child(div().w(px(180.)).child(label))
            .child(
                Button::new(SharedString::from(format!("{id}-smaller")))
                    .outline()
                    .small()
                    .label("−")
                    .on_click(move |_, window, cx| size.change(-1.0, window, cx)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("{id}-size")))
                    .test_support()
                    .w(px(48.))
                    .text_center()
                    .child(SharedString::from(format!("{value} pt"))),
            )
            .child(
                Button::new(SharedString::from(format!("{id}-larger")))
                    .outline()
                    .small()
                    .label("+")
                    .on_click(move |_, window, cx| size.change(1.0, window, cx)),
            )
            .child(
                Button::new(SharedString::from(format!("{id}-reset")))
                    .ghost()
                    .small()
                    .label("Reset")
                    .on_click(move |_, window, cx| size.reset(window, cx)),
            )
    }

    fn render_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let settings = AppState::get(cx).settings.clone();
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let note = |text: &'static str| div().text_xs().text_color(muted).child(text);
        match self.section {
            Section::Appearance => {
                let current = AppState::get(cx).appearance();
                let selected = Appearance::ALL.iter().position(|a| *a == current);
                let opens = TableDoubleClick::ALL
                    .iter()
                    .position(|choice| *choice == settings.table_double_click);
                v_flex()
                    .gap_2()
                    .child(
                        RadioGroup::new("appearance")
                            .children(Appearance::ALL.map(Appearance::label))
                            .selected_index(selected)
                            .on_change(|index, window, cx| {
                                if let Some(appearance) = Appearance::ALL.get(*index) {
                                    Workspace::set_appearance(*appearance, window, cx);
                                }
                            }),
                    )
                    .child(Self::heading("Double click on a table or view in the tree"))
                    .child(
                        RadioGroup::new("table-double-click")
                            .children(TableDoubleClick::ALL.map(TableDoubleClick::label))
                            .selected_index(opens)
                            .on_change(|index, _, cx| {
                                if let Some(choice) = TableDoubleClick::ALL.get(*index) {
                                    AppState::update_settings(cx, |settings| {
                                        settings.table_double_click = *choice
                                    });
                                }
                            }),
                    )
                    // J10 (DBeaver #7114, #23248).
                    .child(Self::heading("Open tabs"))
                    .child(
                        RadioGroup::new("settings-tab-layout")
                            .children(TabLayout::ALL.map(TabLayout::label))
                            .selected_index(
                                TabLayout::ALL
                                    .iter()
                                    .position(|layout| *layout == settings.tab_layout),
                            )
                            .on_change(|index, window, cx| {
                                if let Some(layout) = TabLayout::ALL.get(*index) {
                                    AppState::update_settings(cx, |settings| {
                                        settings.tab_layout = *layout
                                    });
                                    window.refresh();
                                }
                            }),
                    )
                    .into_any_element()
            }
            Section::Fonts => v_flex()
                .gap_2()
                .child(Self::font_row(FontSize::Ui, "Interface", cx))
                .child(Self::font_row(FontSize::Editor, "SQL editor", cx))
                .child(Self::font_row(FontSize::Grid, "Result and table grids", cx))
                .child(note(
                    "The faces are the embedded Inter and JetBrains Mono. Over an editor or a grid, \
                     Cmd+= and Cmd+- (or Cmd with the wheel) change both sizes, Cmd+0 resets them.",
                ))
                .into_any_element(),
            Section::Editor => v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().w(px(180.)).child("Tab key inserts"))
                        .children([2u8, 4].map(|size| {
                            theme::chip(
                                SharedString::from(format!("tab-size-{size}")),
                                settings.tab_size == size,
                            )
                            .small()
                            .label(SharedString::from(format!("{size} spaces")))
                            .on_click(move |_, _, cx| {
                                AppState::update_settings(cx, |settings| settings.tab_size = size)
                            })
                        })),
                )
                .child(Self::toggle(
                    "settings-soft-wrap",
                    "Wrap long lines",
                    settings.soft_wrap,
                    |settings, on| settings.soft_wrap = on,
                ))
                .child(Self::toggle(
                    "settings-line-numbers",
                    "Show line numbers",
                    settings.line_numbers,
                    |settings, on| settings.line_numbers = on,
                ))
                .child(Self::toggle(
                    "settings-auto-completion",
                    "Open the completion list while typing (Ctrl+Space always opens it)",
                    settings.auto_completion,
                    |settings, on| settings.auto_completion = on,
                ))
                .child(Self::toggle(
                    "settings-tab-accepts-completion",
                    "Tab accepts the highlighted completion, as Enter does",
                    settings.tab_accepts_completion,
                    |settings, on| settings.tab_accepts_completion = on,
                ))
                .child(Self::toggle(
                    "settings-ligatures",
                    "Draw ligatures (>= as one sign)",
                    settings.ligatures,
                    |settings, on| settings.ligatures = on,
                ))
                // J19 (DBeaver #37897, Beekeeper #317).
                .child(Self::toggle(
                    "settings-vim-mode",
                    "Vim mode: Normal, Insert and Visual modes in the query editor",
                    settings.vim_mode,
                    |settings, on| settings.vim_mode = on,
                ))
                .child(Self::formatter(&settings, cx))
                .into_any_element(),
            Section::Grid => {
                let typed = self
                    .page_size
                    .read(cx)
                    .value()
                    .trim()
                    .replace([',', '_', ' '], "");
                let valid = typed
                    .parse::<u32>()
                    .is_ok_and(|size| PAGE_SIZES.contains(&size));
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().w(px(180.)).child("Rows fetched per page"))
                            .child(
                                div()
                                    .id("settings-page-size")
                                    .test_support()
                                    .w(px(120.))
                                    .child(Input::new(&self.page_size).small()),
                            ),
                    )
                    .child(
                        note("From 100 to 10,000; the next result or page uses it.")
                            .when(!valid, |note| note.text_color(danger)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().w(px(180.)).child("NULL is shown as"))
                            .child(
                                div()
                                    .id("settings-null-text")
                                    .test_support()
                                    .w(px(120.))
                                    .child(Input::new(&self.null_text).small()),
                            ),
                    )
                    .child(Self::toggle(
                        "settings-time-zone",
                        "Show the time zone of timestamps that have one",
                        settings.show_time_zone,
                        |settings, on| settings.show_time_zone = on,
                    ))
                    // J11 (DBeaver #6861).
                    .child(Self::toggle(
                        "settings-stack-results",
                        "Show the results of a script one under the other in one panel",
                        settings.stack_results,
                        |settings, on| settings.stack_results = on,
                    ))
                    .into_any_element()
            }
            Section::Confirmations => v_flex()
                .gap_2()
                .child(Self::toggle(
                    "settings-confirm-destructive",
                    "Ask before running a statement typed in the editor that drops, \
                     truncates, or changes rows without a WHERE",
                    settings.confirm_destructive,
                    |settings, on| settings.confirm_destructive = on,
                ))
                .child(Self::toggle(
                    "confirm-close-tab",
                    "Ask before closing a tab with pending changes, unsaved text or a \
                     changed table form (an open transaction always asks)",
                    settings.confirm_close_tab,
                    |settings, on| settings.confirm_close_tab = on,
                ))
                .child(note(
                    "An environment can ask more (Settings → Environments): production asks \
                     twice before any write, whatever is chosen here.",
                ))
                .into_any_element(),
            Section::Tools => self.local_clients.clone().into_any_element(),
            Section::Environments => self.environments.clone().into_any_element(),
            Section::Ai => self.ai.clone().into_any_element(),
            Section::Updates => v_flex()
                .gap_2()
                .child(Self::toggle(
                    "settings-check-updates",
                    "Check for updates",
                    settings.check_updates,
                    |settings, on| settings.check_updates = on,
                ))
                .child(note(
                    "At launch and every 6 hours, Kerno asks \
                     https://github.com/EvanPluchart/kerno/releases/latest which version is the \
                     newest, without following the answer; nothing else is sent. Help ▸ Check \
                     for Updates… asks even when this is off; KERNO_NO_UPDATE_CHECK turns both \
                     off.",
                ))
                .into_any_element(),
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = SECTIONS
            .iter()
            .position(|(section, _)| *section == self.section);
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id("settings")
            .test_support()
            .gap_3()
            .text_sm()
            .child(
                TabBar::new("settings-sections")
                    .underline()
                    .selected_index(selected.unwrap_or_default())
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        if let Some((section, _)) = SECTIONS.get(*index) {
                            this.section = *section;
                            cx.notify();
                        }
                    }))
                    .children(SECTIONS.iter().enumerate().map(|(index, (_, label))| {
                        Tab::new().child(
                            div()
                                .id(("settings-section", index))
                                .test_support()
                                .child(*label),
                        )
                    })),
            )
            // One size for every section, the longer ones scrolling inside:
            // the dialog (and its title) stays put from section to section.
            .child(
                div()
                    .id("settings-body")
                    .test_support()
                    .h(px(400.))
                    .overflow_y_scroll()
                    .child(self.render_section(cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(muted)
                            .child("Shortcuts are changed in the keymap file; changes apply when Kerno starts again."),
                    )
                    .child(
                        Button::new("open-keymap-file")
                            .ghost()
                            .small()
                            .label("Open keymap file")
                            .on_click(|_, window, cx| open_keymap_file(window, cx)),
                    ),
            )
    }
}
