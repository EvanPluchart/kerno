//! The settings (Cmd+,): appearance, fonts, editor, grid, confirmations,
//! tools, environments, updates, AI and the keyboard shortcuts. Each choice
//! applies at once and is stored.

use gpui_kit::assets::IconName;
use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::RadioGroup;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window, div,
    px,
};

use crate::connections::manager::Connections;
use crate::environments_view::EnvironmentsView;
use crate::local_clients_view::LocalClientsView;
use crate::settings::{
    CODE_FONT_SIZES, PAGE_SIZES, Settings, TabLayout, TableDoubleClick, UI_FONT_SIZES,
};
use crate::shortcuts_view::ShortcutsView;
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
    Shortcuts,
}

/// Shortcuts comes last: the sections before it keep their places (K20).
pub(crate) const SECTIONS: [(Section, &str); 10] = [
    (Section::Appearance, "Appearance"),
    (Section::Fonts, "Fonts"),
    (Section::Editor, "Editor"),
    (Section::Grid, "Grid"),
    (Section::Confirmations, "Confirmations"),
    (Section::Tools, "Local clients"),
    (Section::Environments, "Environments"),
    (Section::Updates, "Updates"),
    (Section::Ai, "AI"),
    (Section::Shortcuts, "Shortcuts"),
];

/// What a row of the settings draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Theme,
    DoubleClick,
    TabLayout,
    UiFont,
    EditorFont,
    GridFont,
    TabSize,
    SoftWrap,
    LineNumbers,
    AutoCompletion,
    TabAccepts,
    Ligatures,
    Vim,
    Formatter,
    FormatOnSave,
    PageSize,
    FetchOnScroll,
    NullText,
    TimeZone,
    StackResults,
    ConfirmDestructive,
    ConfirmCloseTab,
    LocalClients,
    Environments,
    Updates,
    Ai,
    Shortcuts,
}

/// One setting, declared once: the view draws it from here and the search
/// reads the same words (C5).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub kind: Kind,
    pub section: Section,
    /// The id of its control; `override-<key>` is its override line.
    pub key: &'static str,
    pub label: &'static str,
    /// The muted line under it, if any.
    pub description: &'static str,
    /// Other words a user would type to find it.
    pub keywords: &'static [&'static str],
}

const FONT_WORDS: &[&str] = &["font", "font size", "text size", "zoom", "typeface"];

/// Every row of the settings, section by section, in the order drawn.
pub(crate) static ROWS: &[Row] = &[
    Row {
        kind: Kind::Theme,
        section: Section::Appearance,
        key: "appearance",
        label: "Theme",
        description: "",
        keywords: &[
            "appearance",
            "dark",
            "light",
            "system",
            "colour",
            "color",
            "mode",
        ],
    },
    Row {
        kind: Kind::DoubleClick,
        section: Section::Appearance,
        key: "table-double-click",
        label: "Double click on a table or view in the tree",
        description: "Enter on the selected table or view does the same.",
        keywords: &[
            "enter",
            "open",
            "data",
            "structure",
            "navigator",
            "schema tree",
        ],
    },
    Row {
        kind: Kind::TabLayout,
        section: Section::Appearance,
        key: "settings-tab-layout",
        label: "Open tabs",
        description: "",
        keywords: &[
            "tab", "tabs", "rows", "side", "vertical", "overflow", "layout",
        ],
    },
    Row {
        kind: Kind::UiFont,
        section: Section::Fonts,
        key: "ui-font",
        label: "Interface",
        description: "",
        keywords: FONT_WORDS,
    },
    Row {
        kind: Kind::EditorFont,
        section: Section::Fonts,
        key: "editor-font",
        label: "SQL editor",
        description: "",
        keywords: FONT_WORDS,
    },
    Row {
        kind: Kind::GridFont,
        section: Section::Fonts,
        key: "grid-font",
        label: "Result and table grids",
        description: "The faces are the embedded Inter and JetBrains Mono. Over an editor or a grid, \
             Cmd+= and Cmd+- (or Cmd with the wheel) change both sizes, Cmd+0 resets them.",
        keywords: FONT_WORDS,
    },
    Row {
        kind: Kind::TabSize,
        section: Section::Editor,
        key: "tab-size",
        label: "Tab key inserts",
        description: "",
        keywords: &["tab", "indent", "indentation", "spaces", "tab size"],
    },
    Row {
        kind: Kind::SoftWrap,
        section: Section::Editor,
        key: "settings-soft-wrap",
        label: "Wrap long lines",
        description: "",
        keywords: &["soft wrap", "word wrap", "lines"],
    },
    Row {
        kind: Kind::LineNumbers,
        section: Section::Editor,
        key: "settings-line-numbers",
        label: "Show line numbers",
        description: "",
        keywords: &["gutter"],
    },
    Row {
        kind: Kind::AutoCompletion,
        section: Section::Editor,
        key: "settings-auto-completion",
        label: "Open the completion list while typing (Ctrl+Space always opens it)",
        description: "",
        keywords: &["autocomplete", "suggestions", "intellisense"],
    },
    Row {
        kind: Kind::TabAccepts,
        section: Section::Editor,
        key: "settings-tab-accepts-completion",
        label: "Tab accepts the highlighted completion, as Enter does",
        description: "",
        keywords: &["autocomplete", "accept"],
    },
    Row {
        kind: Kind::Ligatures,
        section: Section::Editor,
        key: "settings-ligatures",
        label: "Draw ligatures (>= as one sign)",
        description: "",
        keywords: &["font"],
    },
    Row {
        kind: Kind::Vim,
        section: Section::Editor,
        key: "settings-vim-mode",
        label: "Vim mode: Normal, Insert and Visual modes in the query editor",
        description: "",
        keywords: &["vi", "modal", "keys", "keyboard"],
    },
    Row {
        kind: Kind::Formatter,
        section: Section::Editor,
        key: "formatter",
        label: "Formatter",
        description: "",
        keywords: &[
            "format",
            "keyword case",
            "uppercase",
            "lowercase",
            "indent",
            "indentation",
            "comma",
            "line width",
            "beautify",
            "pretty",
        ],
    },
    Row {
        kind: Kind::FormatOnSave,
        section: Section::Editor,
        key: "settings-format-on-save",
        label: "Format .sql files when saving them (a script that does not lay out cleanly is saved as it is)",
        description: "",
        keywords: &["formatter", "on save"],
    },
    Row {
        kind: Kind::PageSize,
        section: Section::Grid,
        key: "settings-page-size",
        label: "Rows fetched per page",
        description: "From 100 to 10,000. The next query result and the next table opened use it; \
             a table already open keeps its size until it is opened again.",
        keywords: &["page size", "fetch", "limit", "paging"],
    },
    Row {
        kind: Kind::FetchOnScroll,
        section: Section::Grid,
        key: "settings-fetch-on-scroll",
        label: "Fetch the next rows when scrolling reaches the end of a result",
        description: "Off, a result fetches more only from Fetch more and Fetch all: for a slow \
             link. A table's data keeps its pages.",
        keywords: &[
            "load more",
            "auto fetch",
            "autofetch",
            "next page",
            "infinite scroll",
            "segment",
        ],
    },
    Row {
        kind: Kind::NullText,
        section: Section::Grid,
        key: "settings-null-text",
        label: "NULL is shown as",
        description: "",
        keywords: &["empty", "display"],
    },
    Row {
        kind: Kind::TimeZone,
        section: Section::Grid,
        key: "settings-time-zone",
        label: "Show the time zone of timestamps that have one",
        description: "",
        keywords: &["timezone", "offset", "date", "utc"],
    },
    Row {
        kind: Kind::StackResults,
        section: Section::Grid,
        key: "settings-stack-results",
        label: "Show the results of a script one under the other in one panel",
        description: "",
        keywords: &[
            "stack",
            "stacked",
            "multiple results",
            "script",
            "result tabs",
        ],
    },
    Row {
        kind: Kind::ConfirmDestructive,
        section: Section::Confirmations,
        key: "settings-confirm-destructive",
        label: "Ask before running a statement typed in the editor that drops, \
             truncates, or changes rows without a WHERE",
        description: "Statements run from a query tab: one statement, a selection, a script, \
             EXPLAIN ANALYZE, a run on several connections. A drop or a truncate from the tree, \
             and the changes applied from a grid or a table form, always show their SQL first.",
        keywords: &[
            "confirm",
            "confirmation",
            "ask",
            "warning",
            "delete",
            "drop",
            "truncate",
            "update",
            "dangerous",
            "safety",
            "production",
        ],
    },
    Row {
        kind: Kind::ConfirmCloseTab,
        section: Section::Confirmations,
        key: "confirm-close-tab",
        label: "Ask before closing a tab with pending changes, unsaved text or a changed table form",
        description: "",
        keywords: &[
            "confirm",
            "close",
            "unsaved",
            "pending",
            "ask",
            "transaction",
        ],
    },
    Row {
        kind: Kind::LocalClients,
        section: Section::Tools,
        key: "local-clients",
        label: "Local clients",
        description: "The pg_dump, pg_restore, psql, mysqldump and mysql that backup, restore and \
             Execute script run, and the default one per engine.",
        keywords: &[
            "pg_dump",
            "pg_dumpall",
            "pg_restore",
            "psql",
            "mysqldump",
            "mysql",
            "mariadb",
            "client",
            "tools",
            "backup",
            "restore",
            "binary",
            "path",
            "homebrew",
            "postgres.app",
        ],
    },
    Row {
        kind: Kind::Environments,
        section: Section::Environments,
        key: "environments",
        label: "Environments",
        description: "Local, staging, production and your own: the colour of their connections \
             and their rules.",
        // The rules' own labels are searched too (`environments_view::RULE_LABELS`).
        keywords: &[
            "production",
            "staging",
            "local",
            "colour",
            "color",
            "tint",
            "rules",
            "autocommit",
            "readonly",
            "confirm",
            "twice",
        ],
    },
    Row {
        kind: Kind::Updates,
        section: Section::Updates,
        key: "settings-check-updates",
        label: "Check for updates",
        description: "At launch and every 6 hours, Kerno asks \
             https://github.com/EvanPluchart/kerno/releases/latest which version is the newest, \
             without following the answer; nothing else is sent. Help ▸ Check for Updates… asks \
             even when this is off.",
        keywords: &[
            "update", "version", "release", "github", "upgrade", "network",
        ],
    },
    Row {
        kind: Kind::Ai,
        section: Section::Ai,
        key: "settings-ai",
        label: "AI assistant",
        description: "Off until you turn it on: a model you choose writes SQL from a sentence and \
             explains a query, a plan or an error. Endpoints found on this computer, imported \
             from omp or pi, or typed; you see what is sent.",
        keywords: &[
            "ai",
            "assistant",
            "llm",
            "ollama",
            "lm studio",
            "llama.cpp",
            "vllm",
            "litellm",
            "jan",
            "omp",
            "openai",
            "model",
            "models",
            "endpoint",
            "endpoints",
            "detect",
            "import",
            "api key",
            "reasoning",
        ],
    },
    // K20: the owner's request of 2026-11-20.
    Row {
        kind: Kind::Shortcuts,
        section: Section::Shortcuts,
        key: "shortcuts",
        label: "Keyboard shortcuts",
        description: "Every command and its shortcuts: change, add or remove one, or go back to \
             the defaults. They are stored in the keymap file.",
        keywords: &[
            "keys",
            "keymap",
            "keybindings",
            "bindings",
            "hotkeys",
            "shortcut",
            "accelerators",
            "keystroke",
        ],
    },
];

impl Row {
    /// Every word the search looks at: the section's name, the label, the
    /// description and the keywords (and an environment's rules).
    fn words(&self) -> Vec<String> {
        let section = SECTIONS
            .iter()
            .find(|(section, _)| *section == self.section)
            .map_or("", |(_, name)| name);
        let rules = match self.kind {
            Kind::Environments => crate::environments_view::RULE_LABELS.as_slice(),
            _ => &[],
        };
        [section, self.label, self.description]
            .into_iter()
            .chain(self.keywords.iter().copied())
            .chain(rules.iter().copied())
            .flat_map(words)
            .collect()
    }

    /// Whether every word typed starts a word of the row.
    fn matches(&self, terms: &[String]) -> bool {
        let words = self.words();
        terms
            .iter()
            .all(|term| words.iter().any(|word| word.starts_with(term.as_str())))
    }
}

/// The lowercase words of `text`: letters, digits, `_` and `.` (`pg_dump`,
/// `postgres.app`).
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .map(|word| word.trim_matches('.').to_lowercase())
        .filter(|word| !word.is_empty())
        .collect()
}

/// The rows that match `query`, grouped by section in the settings' order;
/// `None` when nothing is typed (every row shows, section by section).
pub(crate) fn search(query: &str) -> Option<Vec<(Section, Vec<&'static Row>)>> {
    let terms = words(query);
    if terms.is_empty() {
        return None;
    }
    Some(
        SECTIONS
            .iter()
            .map(|(section, _)| {
                let rows = ROWS
                    .iter()
                    .filter(|row| row.section == *section && row.matches(&terms))
                    .collect::<Vec<_>>();
                (*section, rows)
            })
            .filter(|(_, rows)| !rows.is_empty())
            .collect(),
    )
}

/// The line under a setting that something else can override, saying what
/// wins (C5); `None` when nothing does. Read from the code that decides:
/// see the K13 table in docs/plans/M12.md.
pub(crate) fn override_line(kind: Kind, settings: &Settings) -> Option<String> {
    Some(match kind {
        // `QueryTab::execute`: an environment's `confirm_execute` or
        // `confirm_writes_twice` asks whatever `confirm_destructive` says.
        Kind::ConfirmDestructive => {
            let (asking, silent): (Vec<_>, Vec<_>) = settings
                .environments
                .iter()
                .partition(|def| def.rules.confirm_execute || def.rules.confirm_writes_twice);
            let names = |defs: &[&kerno_core::environment::EnvironmentDef]| {
                and_list(defs.iter().map(|def| def.name.as_str()).collect())
            };
            let (verb, whose) = match asking.len() {
                1 => ("wins", "its"),
                _ => ("win", "their"),
            };
            match (asking.is_empty(), silent.is_empty()) {
                (true, _) => {
                    "No environment wins: with this off, these statements run without asking."
                        .to_string()
                }
                (false, true) => format!("{} {verb}: {whose} rules ask anyway.", names(&asking)),
                (false, false) => format!(
                    "{} {verb}: {whose} rules ask anyway. With this off, they run without asking \
                     in {}.",
                    names(&asking),
                    names(&silent),
                ),
            }
        }
        // `tabs::close_question`: an open transaction asks without the setting.
        Kind::ConfirmCloseTab => {
            "An open transaction wins: closing its tab always asks.".to_string()
        }
        // `QueryTab::stacked`: `unstacked` until the tab's next run.
        Kind::StackResults => {
            "A result's “Open in its own tab” wins for its query tab, until the next run."
                .to_string()
        }
        // `workspace/updates.rs`: the variable stops both checks.
        Kind::Updates => {
            "KERNO_NO_UPDATE_CHECK wins: when it is set, no check runs, not even from the Help menu."
                .to_string()
        }
        // `tools::client_for`: the connection's folder before the default.
        Kind::LocalClients => LOCAL_CLIENT_OVERRIDE.to_string(),
        _ => return None,
    })
}

/// The override line of Settings ▸ Local clients, shown in its view too.
pub(crate) const LOCAL_CLIENT_OVERRIDE: &str =
    "A connection's own client wins: its Advanced tab, or “Local client…” in a wizard.";

/// "A", "A and B", "A, B and C".
fn and_list(names: Vec<&str>) -> String {
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

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
    /// The search at the top (C5): rows whose words start with what is
    /// typed, from every section.
    search: Entity<InputState>,
    page_size: Entity<InputState>,
    null_text: Entity<InputState>,
    local_clients: Entity<LocalClientsView>,
    environments: Entity<EnvironmentsView>,
    ai: Entity<crate::ai_settings_view::AiSettingsView>,
    shortcuts: Entity<ShortcutsView>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    /// Opens the settings in a dialog, on `section`, the keyboard in the
    /// search.
    pub fn open(
        connections: Entity<Connections>,
        section: Section,
        window: &mut Window,
        cx: &mut App,
    ) {
        let view = cx.new(|cx| Self::new(connections, window, cx));
        view.update(cx, |view, _| view.section = section);
        let search = view.read(cx).search.clone();
        window.defer(cx, move |window, cx| {
            search.read(cx).focus_handle(cx).focus(window, cx)
        });
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
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Find a setting"));
        let page_size = text(settings.page_size.to_string(), "1000", window, cx);
        let null_text = text(settings.null_text.clone(), "NULL", window, cx);
        let subscriptions = vec![
            // Escape clears what is typed; on an empty field it closes the
            // settings, as before.
            cx.subscribe(&search, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.stop_recording(cx);
                    input.update(cx, |input, _| {
                        let typed = !input.value().is_empty();
                        input.set_clean_on_escape(typed)
                    });
                    cx.notify();
                }
            }),
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
        let ai = cx.new(|cx| crate::ai_settings_view::AiSettingsView::new(window, cx));
        let shortcuts = cx.new(|cx| ShortcutsView::new(window, cx));
        Self {
            section: Section::Appearance,
            search,
            page_size,
            null_text,
            local_clients,
            environments,
            ai,
            shortcuts,
            _subscriptions: subscriptions,
        }
    }

    fn heading(text: &'static str) -> impl IntoElement {
        div().pt_2().font_weight(FontWeight::MEDIUM).child(text)
    }

    /// A checkbox bound to one boolean of the settings: its id and label
    /// are the row's.
    fn toggle(row: &'static Row, checked: bool, set: fn(&mut Settings, bool)) -> AnyElement {
        Checkbox::new(row.key)
            .label(row.label)
            .checked(checked)
            .on_click(move |checked, window, cx| {
                let checked = *checked;
                AppState::update_settings(cx, |settings| set(settings, checked));
                window.refresh();
            })
            .into_any_element()
    }

    /// Settings ▸ Editor ▸ Formatter: the options of "Format SQL" and a
    /// sample laid out with them.
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
                &[
                    (Comma::Trailing, "At line end"),
                    (Comma::Leading, "At line start"),
                ],
                options.comma,
                |options, comma| options.comma = comma,
            ))
            .child(row(
                "Short lists on one line",
                "format-width",
                &[
                    (0, "Never"),
                    (80, "Up to 80 columns"),
                    (120, "Up to 120 columns"),
                ],
                options.line_width,
                |options, width| options.line_width = width,
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

    /// Empties the search, every row back in its section.
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| {
            search.set_value("", window, cx);
            // Escape on the empty search closes the settings again.
            search.set_clean_on_escape(false);
        });
        cx.notify();
    }

    /// Gives the keyboard to the search (⌘F in the settings).
    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.read(cx).focus_handle(cx).focus(window, cx);
    }

    /// A shortcut being recorded stops when the page leaves the screen: no
    /// keystroke is taken with nothing on screen saying why.
    fn stop_recording(&self, cx: &mut Context<Self>) {
        self.shortcuts
            .update(cx, |shortcuts, cx| shortcuts.stop_recording(cx));
    }

    /// Shows `section`, the search emptied.
    fn show(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_recording(cx);
        self.section = section;
        self.clear_search(window, cx);
    }

    /// One row: its control (a heading first over a list of choices), its
    /// description, and the line saying what overrides it. A section that
    /// is a view of its own shows the view, or, in the search's results,
    /// a line with "Open".
    fn render_row(&self, row: &'static Row, searching: bool, cx: &Context<Self>) -> AnyElement {
        let settings = AppState::get(cx).settings.clone();
        let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
        let label = |text: &'static str| div().w(px(180.)).flex_none().child(text);
        let headed = |control: AnyElement| {
            v_flex()
                .gap_2()
                .child(Self::heading(row.label))
                .child(control)
                .into_any_element()
        };
        let control = match row.kind {
            Kind::Theme => {
                let current = AppState::get(cx).appearance();
                headed(
                    RadioGroup::new("appearance")
                        .children(Appearance::ALL.map(Appearance::label))
                        .selected_index(Appearance::ALL.iter().position(|a| *a == current))
                        .on_change(|index, window, cx| {
                            if let Some(appearance) = Appearance::ALL.get(*index) {
                                Workspace::set_appearance(*appearance, window, cx);
                            }
                        })
                        .into_any_element(),
                )
            }
            Kind::DoubleClick => headed(
                RadioGroup::new("table-double-click")
                    .children(TableDoubleClick::ALL.map(TableDoubleClick::label))
                    .selected_index(
                        TableDoubleClick::ALL
                            .iter()
                            .position(|choice| *choice == settings.table_double_click),
                    )
                    .on_change(|index, _, cx| {
                        if let Some(choice) = TableDoubleClick::ALL.get(*index) {
                            AppState::update_settings(cx, |settings| {
                                settings.table_double_click = *choice
                            });
                        }
                    })
                    .into_any_element(),
            ),
            // J10 (DBeaver #7114, #23248).
            Kind::TabLayout => headed(
                RadioGroup::new("settings-tab-layout")
                    .children(TabLayout::ALL.map(TabLayout::label))
                    .selected_index(
                        TabLayout::ALL
                            .iter()
                            .position(|layout| *layout == settings.tab_layout),
                    )
                    .on_change(|index, window, cx| {
                        if let Some(layout) = TabLayout::ALL.get(*index) {
                            AppState::update_settings(cx, |settings| settings.tab_layout = *layout);
                            window.refresh();
                        }
                    })
                    .into_any_element(),
            ),
            Kind::UiFont => Self::font_row(FontSize::Ui, row.label, cx).into_any_element(),
            Kind::EditorFont => Self::font_row(FontSize::Editor, row.label, cx).into_any_element(),
            Kind::GridFont => Self::font_row(FontSize::Grid, row.label, cx).into_any_element(),
            Kind::TabSize => h_flex()
                .gap_2()
                .child(label(row.label))
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
                }))
                .into_any_element(),
            Kind::SoftWrap => Self::toggle(row, settings.soft_wrap, |settings, on| {
                settings.soft_wrap = on
            }),
            Kind::LineNumbers => Self::toggle(row, settings.line_numbers, |settings, on| {
                settings.line_numbers = on
            }),
            Kind::AutoCompletion => Self::toggle(row, settings.auto_completion, |settings, on| {
                settings.auto_completion = on
            }),
            Kind::TabAccepts => {
                Self::toggle(row, settings.tab_accepts_completion, |settings, on| {
                    settings.tab_accepts_completion = on
                })
            }
            Kind::Ligatures => Self::toggle(row, settings.ligatures, |settings, on| {
                settings.ligatures = on
            }),
            // J19 (DBeaver #37897, Beekeeper #317).
            Kind::Vim => Self::toggle(row, settings.vim_mode, |settings, on| {
                settings.vim_mode = on
            }),
            Kind::Formatter => headed(Self::formatter(&settings, cx).into_any_element()),
            Kind::FormatOnSave => Self::toggle(row, settings.format_on_save, |settings, on| {
                settings.format_on_save = on
            }),
            Kind::PageSize => h_flex()
                .gap_2()
                .child(label(row.label))
                .child(
                    div()
                        .id(row.key)
                        .test_support()
                        .w(px(120.))
                        .child(Input::new(&self.page_size).small()),
                )
                .into_any_element(),
            Kind::FetchOnScroll => Self::toggle(row, settings.fetch_on_scroll, |settings, on| {
                settings.fetch_on_scroll = on
            }),
            Kind::NullText => h_flex()
                .gap_2()
                .child(label(row.label))
                .child(
                    div()
                        .id(row.key)
                        .test_support()
                        .w(px(120.))
                        .child(Input::new(&self.null_text).small()),
                )
                .into_any_element(),
            Kind::TimeZone => Self::toggle(row, settings.show_time_zone, |settings, on| {
                settings.show_time_zone = on
            }),
            // J11 (DBeaver #6861).
            Kind::StackResults => Self::toggle(row, settings.stack_results, |settings, on| {
                settings.stack_results = on
            }),
            Kind::ConfirmDestructive => {
                Self::toggle(row, settings.confirm_destructive, |settings, on| {
                    settings.confirm_destructive = on
                })
            }
            Kind::ConfirmCloseTab => {
                Self::toggle(row, settings.confirm_close_tab, |settings, on| {
                    settings.confirm_close_tab = on
                })
            }
            Kind::Updates => Self::toggle(row, settings.check_updates, |settings, on| {
                settings.check_updates = on
            }),
            Kind::LocalClients | Kind::Environments | Kind::Ai | Kind::Shortcuts if searching => {
                let section = row.section;
                let index = SECTIONS
                    .iter()
                    .position(|(each, _)| *each == section)
                    .unwrap_or_default();
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::MEDIUM)
                            .child(row.label),
                    )
                    .child(
                        Button::new(("settings-open", index))
                            .outline()
                            .small()
                            .label("Open")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show(section, window, cx)
                            })),
                    )
                    .into_any_element()
            }
            // A view of its own, which says what it needs to.
            Kind::LocalClients => return self.local_clients.clone().into_any_element(),
            Kind::Environments => return self.environments.clone().into_any_element(),
            Kind::Ai => return self.ai.clone().into_any_element(),
            Kind::Shortcuts => return self.shortcuts.clone().into_any_element(),
        };
        let invalid = row.kind == Kind::PageSize && !self.page_size_valid(cx);
        v_flex()
            .id(SharedString::from(format!("row-{}", row.key)))
            .test_support()
            .gap_1()
            .child(control)
            .when(!row.description.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(if invalid { danger } else { muted })
                        .child(row.description),
                )
            })
            .children(override_line(row.kind, &settings).map(|line| {
                div()
                    .id(SharedString::from(format!("override-{}", row.key)))
                    .test_support()
                    .text_xs()
                    .text_color(muted)
                    .child(SharedString::from(line.clone()))
                    .test_text(line)
            }))
            .into_any_element()
    }

    /// Whether the page size typed is one the settings take.
    fn page_size_valid(&self, cx: &App) -> bool {
        self.page_size
            .read(cx)
            .value()
            .trim()
            .replace([',', '_', ' '], "")
            .parse::<u32>()
            .is_ok_and(|size| PAGE_SIZES.contains(&size))
    }

    /// The section on screen, or what the search found in all of them.
    fn render_body(&self, cx: &Context<Self>) -> AnyElement {
        let query = self.search.read(cx).value().to_string();
        match search(&query) {
            None => v_flex()
                .gap_3()
                .children(
                    ROWS.iter()
                        .filter(|row| row.section == self.section)
                        .map(|row| self.render_row(row, false, cx)),
                )
                .into_any_element(),
            Some(found) if found.is_empty() => {
                use gpui_kit::component::empty::{
                    Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle,
                };
                div()
                    .id("settings-no-match")
                    .test_support()
                    .size_full()
                    .flex()
                    .child(
                        Empty::new()
                            .header(
                                EmptyHeader::new()
                                    .title(EmptyTitle::new().child(SharedString::from(format!(
                                        "No setting matches “{}”",
                                        query.trim()
                                    ))))
                                    .description(EmptyDescription::new().child(
                                        "Try another word, such as “font”, “commit” or \
                                         “confirm”, or clear the search to see every section.",
                                    )),
                            )
                            .content(
                                EmptyContent::new().child(
                                    Button::new("settings-search-clear")
                                        .outline()
                                        .small()
                                        .label("Clear search")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.clear_search(window, cx);
                                            this.focus_search(window, cx);
                                        })),
                                ),
                            ),
                    )
                    .into_any_element()
            }
            Some(found) => v_flex()
                .gap_3()
                .children(found.into_iter().map(|(section, rows)| {
                    let (index, name) = SECTIONS
                        .iter()
                        .enumerate()
                        .find(|(_, (each, _))| *each == section)
                        .map(|(index, (_, name))| (index, *name))
                        .unwrap_or_default();
                    v_flex()
                        .id(("settings-found", index))
                        .test_support()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(cx.theme().muted_foreground)
                                .child(name),
                        )
                        .children(rows.into_iter().map(|row| self.render_row(row, true, cx)))
                }))
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
            .key_context("Settings")
            .on_action(
                cx.listener(|this, _: &actions::SearchSettings, window, cx| {
                    this.focus_search(window, cx)
                }),
            )
            // The theme switch works here too, the Theme row showing it.
            .on_action(|_: &actions::ToggleTheme, window, cx| Workspace::toggle_theme(window, cx))
            .gap_3()
            .text_sm()
            .child(
                div().id("settings-search").test_support().child(
                    Input::new(&self.search)
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).size_3p5().text_color(muted)),
                ),
            )
            .child(
                TabBar::new("settings-sections")
                    .underline()
                    .selected_index(selected.unwrap_or_default())
                    .on_click(cx.listener(|this, index: &usize, window, cx| {
                        if let Some((section, _)) = SECTIONS.get(*index) {
                            this.show(*section, window, cx);
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
            // One size for every section and for the search's results, the
            // longer ones scrolling inside: the dialog (and its title) stays
            // put from section to section and while rows are filtered.
            .child(
                div()
                    .id("settings-body")
                    .test_support()
                    .h(px(400.))
                    .overflow_y_scroll()
                    .child(self.render_body(cx)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().text_xs().text_color(muted).child(
                        "Shortcuts are changed in Shortcuts, above, or by hand in the \
                                 keymap file (read when Kerno starts and when Settings opens).",
                    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::environment::{PRODUCTION, STAGING};

    /// The labels of the rows `query` finds, by section.
    fn found(query: &str) -> Vec<(Section, Vec<&'static str>)> {
        search(query)
            .expect("a search")
            .into_iter()
            .map(|(section, rows)| (section, rows.iter().map(|row| row.label).collect()))
            .collect()
    }

    /// C5: a row is found by its label, its description or a keyword, by
    /// the start of its words; its section shows only with a row in it.
    #[test]
    fn settings_are_found_by_label_description_and_keywords() {
        // Label.
        assert_eq!(found("wrap"), [(Section::Editor, vec!["Wrap long lines"])]);
        // Description.
        assert_eq!(
            found("jetbrains"),
            [(Section::Fonts, vec!["Result and table grids"])]
        );
        // Keywords, the words a user would type.
        assert_eq!(found("dark"), [(Section::Appearance, vec!["Theme"])]);
        assert_eq!(
            found("Font size"),
            [(
                Section::Fonts,
                vec!["Interface", "SQL editor", "Result and table grids"]
            )]
        );
        assert_eq!(
            found("vim"),
            [(
                Section::Editor,
                vec!["Vim mode: Normal, Insert and Visual modes in the query editor"]
            )]
        );
        assert_eq!(found("ai"), [(Section::Ai, vec!["AI assistant"])]);
        assert_eq!(
            found("autocommit"),
            [(Section::Environments, vec!["Environments"])]
        );
        // An environment's rules are found by their own labels.
        assert_eq!(
            found("idle"),
            [(Section::Environments, vec!["Environments"])]
        );
        assert_eq!(found("pg_dump"), [(Section::Tools, vec!["Local clients"])]);
        // Several words: every one starts a word of the row.
        assert_eq!(
            found("confirm close"),
            [(
                Section::Confirmations,
                vec![
                    "Ask before closing a tab with pending changes, unsaved text or a changed table form"
                ]
            )]
        );
        // A section's name finds its rows.
        assert_eq!(found("updates").len(), 1);
        // "update" is also the statement the confirmation is about.
        let sections: Vec<Section> = found("update").into_iter().map(|(s, _)| s).collect();
        assert_eq!(sections, [Section::Confirmations, Section::Updates]);
        // Words start a word: "rap" is inside "wrap", not its start.
        assert_eq!(search("rap"), Some(Vec::new()));
    }

    #[test]
    fn nothing_typed_shows_everything_and_nothing_found_is_said() {
        assert!(search("").is_none() && search("   ").is_none());
        // Only signs: nothing to look for, everything shows.
        assert!(search("…").is_none());
        assert_eq!(search("zzzz"), Some(Vec::new()));
    }

    /// One declaration per row: every row is found by its own label, every
    /// section has a row, and the element ids are unique.
    #[test]
    fn every_row_is_declared_once_and_found_by_its_label() {
        for row in ROWS {
            let found = search(row.label).expect("a search");
            assert!(
                found
                    .iter()
                    .any(|(_, rows)| rows.iter().any(|each| each.key == row.key)),
                "{}",
                row.label
            );
        }
        for (section, name) in SECTIONS {
            assert!(ROWS.iter().any(|row| row.section == section), "{name}");
        }
        let mut keys: Vec<&str> = ROWS.iter().map(|row| row.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ROWS.len());
    }

    /// C5: the line under "Ask before running…" names the environments that
    /// ask whatever it says and those it silences, from the rules as they
    /// stand (`QueryTab::execute`).
    #[test]
    fn the_destructive_confirmation_says_which_environments_win() {
        let line = |settings: &Settings| override_line(Kind::ConfirmDestructive, settings).unwrap();
        let mut settings = Settings::default();
        assert_eq!(
            line(&settings),
            "Production wins: its rules ask anyway. With this off, they run without asking in \
             Local and Staging."
        );
        let staging = settings
            .environments
            .iter()
            .position(|def| def.id == STAGING)
            .unwrap();
        settings.environments[staging].rules.confirm_execute = true;
        assert_eq!(
            line(&settings),
            "Staging and Production win: their rules ask anyway. With this off, they run \
             without asking in Local."
        );
        // Either of production's two rules is enough to ask.
        let production = settings
            .environments
            .iter()
            .position(|def| def.id == PRODUCTION)
            .unwrap();
        settings.environments[staging].rules.confirm_execute = false;
        settings.environments[production].rules.confirm_execute = false;
        assert!(line(&settings).starts_with("Production wins"));
        settings.environments[production].rules.confirm_writes_twice = false;
        assert_eq!(
            line(&settings),
            "No environment wins: with this off, these statements run without asking."
        );
        for def in &mut settings.environments {
            def.rules.confirm_writes_twice = true;
        }
        assert_eq!(
            line(&settings),
            "Local, Staging and Production win: their rules ask anyway."
        );
    }

    /// Only the settings something else can override carry a line.
    #[test]
    fn only_what_can_be_overridden_has_a_line() {
        let settings = Settings::default();
        let with_line: Vec<Kind> = ROWS
            .iter()
            .map(|row| row.kind)
            .filter(|kind| override_line(*kind, &settings).is_some())
            .collect();
        assert_eq!(
            with_line,
            [
                Kind::StackResults,
                Kind::ConfirmDestructive,
                Kind::ConfirmCloseTab,
                Kind::LocalClients,
                Kind::Updates,
            ]
        );
        // One shape: who wins, then what it does.
        for kind in with_line {
            let line = override_line(kind, &settings).unwrap();
            assert!(line.contains(" wins") || line.contains(" win:"), "{line}");
        }
    }
}
