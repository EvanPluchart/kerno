//! The settings (Cmd+,): appearance, fonts, editor, grid, confirmations,
//! tools, environments, updates, AI, passwords and the keyboard shortcuts. Each choice
//! applies at once and is stored.

use gpui_kit::assets::IconName;
use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, Context, Entity, FocusHandle,
    Focusable as _, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, TestSupportExt as _, Window, div, ease_out_quint, px,
};

use crate::connections::manager::Connections;
use crate::environments_view::EnvironmentsView;
use crate::local_clients_view::LocalClientsView;
use crate::settings::{CODE_FONT_SIZES, Settings, TabLayout, TableDoubleClick, UI_FONT_SIZES};
use crate::settings_ui::{self, OptionCards};
use crate::shortcuts_view::ShortcutsView;
use crate::state::AppState;
use crate::theme::{self, Appearance};
use crate::workspace::Workspace;
use crate::{actions, bridge};
use kerno_core::secrets::StoreChoice;

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
    Passwords,
    Shortcuts,
}

/// Shortcuts comes last: the sections before it keep their places (K20).
pub(crate) const SECTIONS: [(Section, &str); 11] = [
    (Section::Appearance, "Appearance"),
    (Section::Fonts, "Fonts"),
    (Section::Editor, "Editor"),
    (Section::Grid, "Grid"),
    (Section::Confirmations, "Confirmations"),
    (Section::Tools, "Local clients"),
    (Section::Environments, "Environments"),
    (Section::Updates, "Updates"),
    (Section::Ai, "AI"),
    (Section::Passwords, "Passwords"),
    (Section::Shortcuts, "Shortcuts"),
];

impl Section {
    /// Its glyph in the side list.
    fn icon(self) -> IconName {
        match self {
            Self::Appearance => IconName::Palette,
            Self::Fonts => IconName::ALargeSmall,
            Self::Editor => IconName::SquareTerminal,
            Self::Grid => IconName::Table,
            Self::Confirmations => IconName::ShieldCheck,
            Self::Tools => IconName::Wrench,
            Self::Environments => IconName::Layers,
            Self::Updates => IconName::RefreshCw,
            Self::Ai => IconName::Bot,
            Self::Passwords => IconName::Lock,
            Self::Shortcuts => IconName::Keyboard,
        }
    }

    /// The line under the page's name (not searched: the rows are).
    fn intro(self) -> &'static str {
        match self {
            Self::Appearance => "How Kerno looks, and what the tree and the tabs do.",
            Self::Fonts => "The text sizes of the interface, the SQL editor and the grids.",
            Self::Editor => "How the SQL editor indents, completes and formats.",
            Self::Grid => "How results and table data are fetched and shown.",
            Self::Confirmations => "When Kerno asks before it runs or closes something.",
            Self::Updates => "Whether Kerno looks for a newer version, and how.",
            Self::Passwords => "Where the passwords and passphrases you save are kept.",
            // A view of its own: the row's line says what it holds.
            Self::Tools | Self::Environments => ROWS
                .iter()
                .find(|row| row.section == self)
                .map_or("", |row| row.description),
            // Their views open with what they are for.
            Self::Ai | Self::Shortcuts => "",
        }
    }

    /// Whether its page is a view of its own.
    fn is_view(self) -> bool {
        matches!(
            self,
            Self::Tools | Self::Environments | Self::Ai | Self::Shortcuts
        )
    }
}

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
    SecretStore,
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
        label: "Rows fetched at a time",
        description: "From 100 to 10,000. The next query result and the next table opened use it; \
             a table's footer changes it for that tab (Fetch size).",
        keywords: &["page size", "fetch", "limit", "paging"],
    },
    Row {
        kind: Kind::FetchOnScroll,
        section: Section::Grid,
        key: "settings-fetch-on-scroll",
        label: "Fetch the next rows when scrolling reaches the end of a result",
        description: "Off, a result or a table fetches more only from Fetch more and Fetch all: \
             for a slow link.",
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
             without following the answer; when a newer version is found, it also reads that \
             release's notes from GitHub. Nothing about you is sent. Help ▸ Check for Updates… asks \
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
    // K33 (D-106): the owner's request of 2026-11-21.
    Row {
        kind: Kind::SecretStore,
        section: Section::Passwords,
        key: "settings-secret-store",
        label: "Keep saved passwords in",
        description: SECRET_STORE_DESCRIPTION,
        keywords: &[
            "password",
            "passwords",
            "passphrase",
            "secret",
            "secrets",
            "keychain",
            "secret service",
            "gnome keyring",
            "kwallet",
            "encrypted",
            "prompt",
            "security",
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
        // `secrets::app_store`: the variable gives one store alone.
        Kind::SecretStore => "KERNO_SECRET_STORE wins: when it is set (memory, file or keychain), \
             that store is used alone and nothing moves from the other one."
            .to_string(),
        _ => return None,
    })
}

/// What each place of Settings ▸ Passwords means, in plain words (K33,
/// D-106): what Kerno's file protects and what it does not, and why the
/// keychain asks again.
const SECRET_STORE_DESCRIPTION: &str = if cfg!(target_os = "macos") {
    "Kerno's file: one encrypted file in Kerno's data folder, its key in a second file beside \
     it. No prompt; a copy of the file, a file search or another user of this Mac sees nothing, \
     but a program running under your account could read both files, as with DBeaver or \
     ~/.pgpass. macOS Keychain: guarded by your login, but macOS asks for your password again \
     after every update of Kerno, which is not signed by Apple. A password saved in the other \
     place moves here the first time it is used."
} else if cfg!(windows) {
    "Kerno's file: one encrypted file in Kerno's data folder, its key in a second file beside \
     it. No prompt; a copy of the file, a file search or another user of this computer sees \
     nothing, but a program running under your account could read both files, as with DBeaver \
     or pgpass.conf. Windows Credential Manager: guarded by your Windows login. A password \
     saved in the other place moves here the first time it is used."
} else {
    "Kerno's file: one encrypted file in Kerno's data folder, its key in a second file beside \
     it. No prompt; a copy of the file, a file search or another user of this computer sees \
     nothing, but a program running under your account could read both files, as with DBeaver \
     or ~/.pgpass. Secret Service: GNOME Keyring or KWallet, guarded by your login; without \
     one running, passwords are kept until Kerno quits. A password saved in the other place \
     moves here the first time it is used."
};

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
    /// The items of the side list, one per section.
    nav_focus: Vec<FocusHandle>,
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
                .w(px(settings_ui::DIALOG_WIDTH))
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
                if let Some(size) = crate::settings::page_size(&input.read(cx).value()) {
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
            nav_focus: SECTIONS.iter().map(|_| cx.focus_handle()).collect(),
            _subscriptions: subscriptions,
        }
    }

    /// A row's name, at its left; its control sits at the right.
    fn name(text: &'static str) -> gpui_kit::Div {
        div().flex_1().min_w_0().child(text)
    }

    /// A switch bound to one boolean of the settings, at the right of the
    /// row's name: its id is the row's.
    fn toggle(row: &'static Row, checked: bool, set: fn(&mut Settings, bool)) -> AnyElement {
        h_flex()
            .gap_3()
            .child(Self::name(row.label))
            .child(
                Switch::new(row.key)
                    .accessibility_label(row.label)
                    .checked(checked)
                    .on_click(move |checked, window, cx| {
                        let checked = *checked;
                        AppState::update_settings(cx, |settings| set(settings, checked));
                        window.refresh();
                    }),
            )
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
        ) -> AnyElement {
            h_flex()
                .gap_2()
                .px_3()
                .py_2()
                .child(SettingsView::name(label))
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
                .into_any_element()
        }
        let options = settings.format;
        let theme = cx.theme();
        let sample = format_with(SAMPLE, LexRules::POSTGRES, &options).replace('\t', "    ");
        let rows = vec![
            row(
                "Keywords",
                "format-case",
                &[
                    (KeywordCase::Upper, "UPPER"),
                    (KeywordCase::Lower, "lower"),
                    (KeywordCase::Keep, "As typed"),
                ],
                options.keyword_case,
                |options, case| options.keyword_case = case,
            ),
            row(
                "Indentation",
                "format-indent",
                &[
                    (Indent::Spaces(2), "2 spaces"),
                    (Indent::Spaces(4), "4 spaces"),
                    (Indent::Tab, "Tab"),
                ],
                options.indent,
                |options, indent| options.indent = indent,
            ),
            row(
                "Commas in lists",
                "format-comma",
                &[
                    (Comma::Trailing, "At line end"),
                    (Comma::Leading, "At line start"),
                ],
                options.comma,
                |options, comma| options.comma = comma,
            ),
            row(
                "Short lists on one line",
                "format-width",
                &[
                    (0, "Never"),
                    (80, "Up to 80 columns"),
                    (120, "Up to 120 columns"),
                ],
                options.line_width,
                |options, width| options.line_width = width,
            ),
        ];
        v_flex().gap_2().child(settings_ui::panel(rows, cx)).child(
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
            .child(Self::name(label))
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

    /// Whether a row is drawn under a heading of its own (a group of cards,
    /// the formatter) rather than as one line of a panel.
    fn headed(kind: Kind) -> bool {
        matches!(
            kind,
            Kind::Theme | Kind::DoubleClick | Kind::TabLayout | Kind::Formatter
        )
    }

    /// One row: its control (its name at the left, the control at the
    /// right, or a heading over cards), its description, and the line
    /// saying what overrides it. A section that is a view of its own shows
    /// the view, or, in the search's results, a line with "Open".
    fn render_row(&self, row: &'static Row, searching: bool, cx: &Context<Self>) -> AnyElement {
        let settings = AppState::get(cx).settings.clone();
        let (muted, danger) = (cx.theme().muted_foreground, cx.theme().danger);
        let label = Self::name;
        let headed = |control: AnyElement| control;
        let control = match row.kind {
            Kind::Theme => {
                let current = AppState::get(cx).appearance();
                let cards = OptionCards::new(
                    "appearance",
                    Appearance::ALL.iter().position(|a| *a == current),
                    |index, window, cx| {
                        if let Some(appearance) = Appearance::ALL.get(index) {
                            Workspace::set_appearance(*appearance, window, cx);
                        }
                    },
                );
                headed(
                    Appearance::ALL
                        .into_iter()
                        .fold(cards, |cards, appearance| {
                            cards.option(
                                appearance.label(),
                                settings_ui::theme_preview(appearance, cx),
                            )
                        })
                        .into_any_element(),
                )
            }
            Kind::DoubleClick => headed(
                TableDoubleClick::ALL
                    .into_iter()
                    .fold(
                        OptionCards::new(
                            "table-double-click",
                            TableDoubleClick::ALL
                                .iter()
                                .position(|choice| *choice == settings.table_double_click),
                            |index, window, cx| {
                                if let Some(choice) = TableDoubleClick::ALL.get(index) {
                                    AppState::update_settings(cx, |settings| {
                                        settings.table_double_click = *choice
                                    });
                                    window.refresh();
                                }
                            },
                        ),
                        |cards, choice| {
                            cards.option(
                                choice.label(),
                                settings_ui::double_click_preview(choice, cx),
                            )
                        },
                    )
                    .into_any_element(),
            ),
            // J10 (DBeaver #7114, #23248).
            Kind::TabLayout => headed(
                TabLayout::ALL
                    .into_iter()
                    .fold(
                        OptionCards::new(
                            "settings-tab-layout",
                            TabLayout::ALL
                                .iter()
                                .position(|layout| *layout == settings.tab_layout),
                            |index, window, cx| {
                                if let Some(layout) = TabLayout::ALL.get(index) {
                                    AppState::update_settings(cx, |settings| {
                                        settings.tab_layout = *layout
                                    });
                                    window.refresh();
                                }
                            },
                        ),
                        |cards, layout| {
                            cards.option(layout.label(), settings_ui::tabs_preview(layout, cx))
                        },
                    )
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
            Kind::Updates => {
                let toggle = Self::toggle(row, settings.check_updates, |settings, on| {
                    settings.check_updates = on
                });
                match settings.skipped_update.clone() {
                    // "Skip this version" of the update dialog (D-097): undone here.
                    Some(version) => {
                        let line = format!("Kerno {version} was skipped");
                        v_flex()
                            .gap_2()
                            .child(toggle)
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("settings-skipped-update")
                                            .test_support()
                                            .child(SharedString::from(line.clone()))
                                            .test_text(line),
                                    )
                                    .child(
                                        Button::new("settings-ask-again")
                                            .ghost()
                                            .small()
                                            .label("Ask again")
                                            .on_click(|_, window, cx| {
                                                AppState::update_settings(cx, |settings| {
                                                    settings.skipped_update = None
                                                });
                                                window.refresh();
                                            }),
                                    ),
                            )
                            .into_any_element()
                    }
                    None => toggle,
                }
            }
            // K33: one chip per place, the change in use from the next read.
            Kind::SecretStore => h_flex()
                .gap_2()
                .child(label(row.label))
                .children(
                    StoreChoice::ALL
                        .into_iter()
                        .enumerate()
                        .map(|(index, choice)| {
                            theme::chip(
                                SharedString::from(format!("secret-store-{index}")),
                                settings.secret_store == choice,
                            )
                            .small()
                            .label(choice.label())
                            .on_click(move |_, window, cx| {
                                AppState::set_secret_store(choice, cx);
                                window.refresh();
                            })
                        }),
                )
                .into_any_element(),
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
        let description = (!row.description.is_empty()).then(|| {
            div()
                .text_xs()
                .text_color(if invalid { danger } else { muted })
                .child(row.description)
        });
        // What wins over the setting, under it, marked as a note.
        let overrides = override_line(row.kind, &settings).map(|line| {
            h_flex()
                .gap_1()
                .items_start()
                .text_xs()
                .text_color(muted)
                .child(
                    Icon::new(IconName::Info)
                        .size_3()
                        .mt(px(1.))
                        .flex_none()
                        .text_color(muted),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("override-{}", row.key)))
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .child(SharedString::from(line.clone()))
                        .test_text(line),
                )
        });
        let shell = v_flex()
            .id(SharedString::from(format!("row-{}", row.key)))
            .test_support();
        if Self::headed(row.kind) {
            // A heading, what it is about, then the cards.
            shell
                .gap_2()
                .child(settings_ui::section_title(row.label))
                .children(description)
                .child(control)
                .children(overrides)
                .into_any_element()
        } else {
            // One line of a panel: the name and the control, then the lines
            // under the name.
            shell
                .gap_1()
                .px_3()
                .py_2p5()
                .child(control)
                .children(description)
                .children(overrides)
                .into_any_element()
        }
    }

    /// The rows of one section as they are drawn: the rows under a heading
    /// on their own, the others in panels, one panel for each run of them.
    fn blocks(
        &self,
        rows: impl IntoIterator<Item = &'static Row>,
        searching: bool,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let mut blocks = Vec::new();
        let mut lines = Vec::new();
        for row in rows {
            let element = self.render_row(row, searching, cx);
            let own_view = !searching && row.section.is_view();
            if Self::headed(row.kind) || own_view {
                if !lines.is_empty() {
                    blocks.push(
                        settings_ui::panel(std::mem::take(&mut lines), cx).into_any_element(),
                    );
                }
                blocks.push(element);
            } else {
                lines.push(element);
            }
        }
        if !lines.is_empty() {
            blocks.push(settings_ui::panel(lines, cx).into_any_element());
        }
        blocks
    }

    /// Whether the page size typed is one the settings take.
    fn page_size_valid(&self, cx: &App) -> bool {
        crate::settings::page_size(&self.page_size.read(cx).value()).is_some()
    }

    /// The section on screen, or what the search found in all of them.
    fn render_body(&self, cx: &Context<Self>) -> AnyElement {
        let query = self.search.read(cx).value().to_string();
        match search(&query) {
            None => {
                let section = self.section;
                let (index, name) = SECTIONS
                    .iter()
                    .enumerate()
                    .find(|(_, (each, _))| *each == section)
                    .map(|(index, (_, name))| (index, *name))
                    .unwrap_or_default();
                let rows = ROWS.iter().filter(|row| row.section == section);
                v_flex()
                    .gap_5()
                    .child(settings_ui::page_head(name, section.intro(), cx))
                    .children(self.blocks(rows, false, cx))
                    // A page comes in with a short fade (120 ms), the next
                    // click already landing on it.
                    .with_animation(
                        ("settings-page", index),
                        Animation::new(cx.theme().motion_tokens().duration_fast)
                            .with_easing(ease_out_quint()),
                        |page, delta| page.opacity(0.4 + 0.6 * delta),
                    )
                    .into_any_element()
            }
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
                .gap_5()
                .children(found.into_iter().map(|(section, rows)| {
                    let (index, name) = SECTIONS
                        .iter()
                        .enumerate()
                        .find(|(_, (each, _))| *each == section)
                        .map(|(index, (_, name))| (index, *name))
                        .unwrap_or_default();
                    let muted = cx.theme().muted_foreground;
                    v_flex()
                        .id(("settings-found", index))
                        .test_support()
                        .gap_2()
                        // Where it is: the section's glyph and name.
                        .child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(muted)
                                .child(Icon::new(section.icon()).size_3p5().text_color(muted))
                                .child(name),
                        )
                        .children(self.blocks(rows, true, cx))
                        // And the way to its page, after its rows: Tab from
                        // the search reaches the setting found first.
                        .when(!section.is_view(), |group| {
                            group.child(
                                h_flex().child(
                                    Button::new(("settings-found-open", index))
                                        .ghost()
                                        .xsmall()
                                        .label(SharedString::from(format!("Show the {name} page")))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.show(section, window, cx)
                                        })),
                                ),
                            )
                        })
                }))
                .into_any_element(),
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        // The search's results span sections: none is marked meanwhile.
        let searching = search(&self.search.read(cx).value()).is_some();
        let nav = v_flex()
            .id("settings-nav")
            .test_support()
            .w(px(settings_ui::NAV_WIDTH))
            .flex_none()
            .gap_0p5()
            .pr_3()
            .border_r_1()
            .border_color(cx.theme().border)
            .children(
                SECTIONS
                    .iter()
                    .enumerate()
                    .map(|(index, (section, label))| {
                        let section = *section;
                        settings_ui::nav_item(
                            (index, section.icon(), label),
                            section == self.section && !searching,
                            &self.nav_focus[index],
                            cx.listener(move |this, _, window, cx| this.show(section, window, cx)),
                            window,
                            cx,
                        )
                    }),
            );
        // One size for every section and for the search's results, the
        // longer ones scrolling inside: the dialog (its title, the search,
        // the side list, the footer) stays put from section to section and
        // while rows are filtered.
        let body = div()
            .id("settings-body")
            .test_support()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                v_flex()
                    .id("settings-scroll")
                    .size_full()
                    // Its one part keeps its height: the page scrolls.
                    .child(
                        div()
                            .flex_none()
                            // Room for a focus ring, and for the scrollbar.
                            .p_1()
                            .pr_4()
                            .child(self.render_body(cx)),
                    )
                    .overflow_y_scrollbar(),
            );
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
            .h(settings_ui::height(window))
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
                // The page comes before the side list to the keyboard (Tab
                // from the search reaches the settings shown, or found), and
                // after it on screen.
                h_flex()
                    .flex_row_reverse()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .gap_3()
                    .child(body)
                    .child(nav),
            )
            .child(
                h_flex()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(div().flex_1().text_xs().text_color(muted).child(
                        "Shortcuts are changed in Shortcuts, in the list at the left, or by hand \
                         in the keymap file (read when Kerno starts and when Settings opens).",
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
        // On macOS the keychain's prompt after an update is said too (K33).
        let sections: Vec<Section> = found("update").into_iter().map(|(s, _)| s).collect();
        let mut expected = vec![Section::Confirmations, Section::Updates];
        if cfg!(target_os = "macos") {
            expected.push(Section::Passwords);
        }
        assert_eq!(sections, expected);
        assert_eq!(
            found("keychain"),
            [(Section::Passwords, vec!["Keep saved passwords in"])]
        );
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
                Kind::SecretStore,
            ]
        );
        // One shape: who wins, then what it does.
        for kind in with_line {
            let line = override_line(kind, &settings).unwrap();
            assert!(line.contains(" wins") || line.contains(" win:"), "{line}");
        }
    }
}
