//! User settings, stored as one value in Kerno's storage.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use kerno_core::config::FolderLook;
use kerno_core::environment::{self, EnvironmentDef};
use kerno_core::sql::FormatOptions;
use kerno_core::storage::Storage;
use kerno_core::tools::ClientFamily;
use serde::{Deserialize, Serialize};

use crate::theme::Appearance;

const KEY: &str = "settings";

/// What a double click (or Enter) on a table or view in the schema tree
/// opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TableDoubleClick {
    /// Its rows.
    #[default]
    Data,
    /// Its columns, keys, indexes and DDL.
    Structure,
    /// Its columns and indexes, in the tree.
    Expand,
}

impl TableDoubleClick {
    pub const ALL: [TableDoubleClick; 3] = [Self::Data, Self::Structure, Self::Expand];

    pub fn label(self) -> &'static str {
        match self {
            Self::Data => "Open its data",
            Self::Structure => "Open its structure",
            Self::Expand => "Expand it in the tree",
        }
    }
}

/// Everything the settings view edits. Unknown or missing fields fall back
/// to their defaults, so settings survive upgrades in both directions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Light, dark, or follow the system.
    pub appearance: Appearance,
    /// Rows fetched per page of a result.
    pub page_size: u32,
    /// A result fetches its next page when scrolling reaches its last
    /// fetched rows (DBeaver's `resultset.autofetch.next.segment`, K21).
    pub fetch_on_scroll: bool,
    /// Ask before running a destructive statement typed in the editor.
    pub confirm_destructive: bool,
    /// Whether the sidebar is shown.
    pub sidebar_visible: bool,
    /// What a double click on a table or view in the tree opens.
    pub table_double_click: TableDoubleClick,
    /// Connection folders made by the user, kept even when empty (a folder
    /// that holds connections also lives in their `group`).
    pub connection_folders: BTreeSet<String>,
    /// Connection folders shown closed in the sidebar.
    pub collapsed_folders: BTreeSet<String>,
    /// How folders are drawn (colour, glyph, picture), by path; a folder
    /// without an entry is plain.
    pub folder_looks: BTreeMap<String, FolderLook>,
    /// Ask before closing a tab that holds pending changes, unsaved text or
    /// a changed table form. An open transaction always asks.
    pub confirm_close_tab: bool,
    /// Before local clients: one binary per tool name. Read once, its
    /// folders become [`Settings::client_folders`], then it is empty.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_paths: BTreeMap<String, PathBuf>,
    /// Client folders the user added (Settings ▸ Local clients), looked at
    /// before the usual install places.
    pub client_folders: Vec<PathBuf>,
    /// The client each engine family uses unless a connection names its
    /// own; the newest found when absent.
    pub default_clients: BTreeMap<ClientFamily, PathBuf>,
    /// Text size of the interface, in points.
    pub ui_font_size: f32,
    /// Text size of the SQL editors.
    pub editor_font_size: f32,
    /// Text size of the result and table grids.
    pub grid_font_size: f32,
    /// Spaces a Tab key inserts in the SQL editors: 2 or 4.
    pub tab_size: u8,
    /// Long lines wrap in the SQL editors.
    pub soft_wrap: bool,
    /// Line numbers beside the SQL editors.
    pub line_numbers: bool,
    /// The completion list opens by itself while typing (it always opens on
    /// demand).
    pub auto_completion: bool,
    /// Tab accepts the highlighted completion, as Enter does (DBeaver
    /// #2316); off, Tab indents.
    pub tab_accepts_completion: bool,
    /// How "Format SQL" lays the text out.
    pub format: FormatOptions,
    /// Saving a `.sql` file formats its text first, when it lays out
    /// cleanly.
    pub format_on_save: bool,
    /// Ligatures in the SQL editors (`>=` drawn as one sign).
    pub ligatures: bool,
    /// Vim mode in the SQL editors (J19): off until the user turns it on.
    pub vim_mode: bool,
    /// What a NULL cell shows.
    pub null_text: String,
    /// Timestamps with a time zone show their offset.
    pub show_time_zone: bool,
    /// The schema tree selects the table of the tab being activated.
    pub link_with_editor: bool,
    /// The footer of a grid shows the figures of the selected cells.
    pub selection_stats: bool,
    /// The administration's sessions list leaves out idle (PostgreSQL) and
    /// sleeping (MySQL, MariaDB) sessions.
    pub hide_idle_sessions: bool,
    /// The environments connections can be in: the three built-ins, edited
    /// or not, then the user's own.
    pub environments: Vec<EnvironmentDef>,
    /// Ask GitHub for a newer release at launch and every 6 hours
    /// (CLAUDE.md §3 item 12); "Check for updates…" asks anyway.
    pub check_updates: bool,
    /// The release "Skip this version" chose: not offered by the checks.
    pub skipped_update: Option<String>,
    /// The main toolbar is shown under the title bar.
    pub toolbar_visible: bool,
    /// The main toolbar's items the user hid (its right-click menu), by id.
    pub toolbar_hidden: BTreeSet<String>,
    /// The `.sql` files opened or saved last, the latest first (the main
    /// toolbar's New SQL editor lists them).
    pub recent_scripts: Vec<PathBuf>,
    /// The connection each recent script last ran on, by path: proposed
    /// first when it is opened again, bound only where it binds silently
    /// (C6).
    pub script_connections: BTreeMap<PathBuf, String>,
    /// How the open tabs are laid out (DBeaver #7114, #23248).
    pub tab_layout: TabLayout,
    /// A multi-statement run shows its results one under the other in one
    /// panel, instead of one result tab each (DBeaver #6861).
    pub stack_results: bool,
    /// The AI assistant (J17): off until the user turns it on; its key is in
    /// the secret store, never here.
    pub ai: kerno_core::ai::AiSettings,
    /// The value panel shows the selected cell beside every grid (K17).
    pub value_panel: bool,
    /// Its width, in points.
    pub value_panel_width: f32,
}

/// How the open tabs are laid out above (or beside) the tab on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TabLayout {
    /// One row that scrolls sideways.
    #[default]
    SingleRow,
    /// As many rows as the tabs need, up to four, then it scrolls.
    Rows,
    /// A list at the left of the tab on screen.
    Vertical,
}

impl TabLayout {
    pub const ALL: [TabLayout; 3] = [Self::SingleRow, Self::Rows, Self::Vertical];

    pub fn label(self) -> &'static str {
        match self {
            Self::SingleRow => "One row",
            Self::Rows => "Several rows",
            Self::Vertical => "A list at the side",
        }
    }
}

/// How many `.sql` files the main toolbar lists.
pub const RECENT_SCRIPTS: usize = 10;

/// The sizes the settings allow, in points.
pub const UI_FONT_SIZES: std::ops::RangeInclusive<f32> = 12.0..=18.0;
pub const CODE_FONT_SIZES: std::ops::RangeInclusive<f32> = 11.0..=20.0;
/// The sizes a page of rows may have.
pub const PAGE_SIZES: std::ops::RangeInclusive<u32> = 100..=10_000;
/// The widths the value panel may have, in points.
pub const VALUE_PANEL_WIDTHS: std::ops::RangeInclusive<f32> = 240.0..=900.0;

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            page_size: 1_000,
            fetch_on_scroll: true,
            confirm_destructive: true,
            sidebar_visible: true,
            table_double_click: TableDoubleClick::Data,
            connection_folders: BTreeSet::new(),
            collapsed_folders: BTreeSet::new(),
            folder_looks: BTreeMap::new(),
            confirm_close_tab: true,
            tool_paths: BTreeMap::new(),
            client_folders: Vec::new(),
            default_clients: BTreeMap::new(),
            ui_font_size: 13.0,
            editor_font_size: 12.5,
            grid_font_size: 11.5,
            tab_size: 2,
            soft_wrap: false,
            line_numbers: true,
            auto_completion: true,
            tab_accepts_completion: true,
            format: FormatOptions::default(),
            format_on_save: false,
            ligatures: false,
            vim_mode: false,
            null_text: "NULL".to_string(),
            show_time_zone: true,
            link_with_editor: false,
            selection_stats: true,
            hide_idle_sessions: false,
            environments: environment::builtin_environments(),
            check_updates: true,
            skipped_update: None,
            toolbar_visible: true,
            toolbar_hidden: BTreeSet::new(),
            recent_scripts: Vec::new(),
            script_connections: BTreeMap::new(),
            tab_layout: TabLayout::SingleRow,
            stack_results: false,
            ai: kerno_core::ai::AiSettings::default(),
            value_panel: false,
            value_panel_width: 380.0,
        }
    }
}

impl Settings {
    /// Reads the stored settings; defaults when none were stored or the
    /// store cannot be read.
    pub fn load(storage: &Storage) -> Self {
        match storage.setting::<Settings>(KEY) {
            Ok(Some(settings)) => Settings::sanitized(settings),
            Ok(None) => Self::default(),
            Err(error) => {
                tracing::warn!(%error, "could not read settings; using defaults");
                Self::default()
            }
        }
    }

    /// The settings with every value inside what the settings view allows
    /// (a hand-edited or damaged store cannot make the text unreadable).
    pub fn sanitized(mut self) -> Self {
        let clamp = |size: f32, range: &std::ops::RangeInclusive<f32>, default: f32| {
            if size.is_finite() {
                size.clamp(*range.start(), *range.end())
            } else {
                default
            }
        };
        let default = Self::default();
        self.ui_font_size = clamp(self.ui_font_size, &UI_FONT_SIZES, default.ui_font_size);
        self.editor_font_size = clamp(
            self.editor_font_size,
            &CODE_FONT_SIZES,
            default.editor_font_size,
        );
        self.grid_font_size = clamp(
            self.grid_font_size,
            &CODE_FONT_SIZES,
            default.grid_font_size,
        );
        self.value_panel_width = clamp(
            self.value_panel_width,
            &VALUE_PANEL_WIDTHS,
            default.value_panel_width,
        );
        self.format = self.format.sanitized();
        if !matches!(self.tab_size, 2 | 4) {
            self.tab_size = default.tab_size;
        }
        // NULL never looks like an empty string.
        if self.null_text.trim().is_empty() {
            self.null_text = default.null_text;
        }
        // The binaries chosen before local clients: their folders.
        for path in std::mem::take(&mut self.tool_paths).into_values() {
            if let Some(dir) = path.parent().map(PathBuf::from)
                && !self.client_folders.contains(&dir)
            {
                self.client_folders.push(dir);
            }
        }
        // The built-ins are never lost, whatever was stored.
        self.environments = environment::with_builtins(std::mem::take(&mut self.environments));
        // The AI address of before K23 (or of an older Kerno since): a saved endpoint.
        self.ai.adopt_legacy();
        self
    }

    /// Stores the settings.
    pub fn save(&self, storage: &Storage) -> kerno_core::Result<()> {
        storage.set_setting(KEY, self)
    }

    /// Puts `path` first in the recent scripts, once, keeping the last
    /// [`RECENT_SCRIPTS`]; with `connection`, the one it runs on (`None`
    /// keeps the one remembered).
    pub fn remember_script(&mut self, path: PathBuf, connection: Option<String>) {
        self.recent_scripts.retain(|recent| *recent != path);
        self.recent_scripts.insert(0, path.clone());
        self.recent_scripts.truncate(RECENT_SCRIPTS);
        if let Some(connection) = connection {
            self.script_connections.insert(path, connection);
        }
        let recent = &self.recent_scripts;
        self.script_connections
            .retain(|path, _| recent.contains(path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_default() {
        let storage = Storage::in_memory().unwrap();
        assert_eq!(Settings::load(&storage), Settings::default());
        let changed = Settings {
            appearance: Appearance::Dark,
            page_size: 250,
            fetch_on_scroll: false,
            confirm_destructive: false,
            sidebar_visible: false,
            table_double_click: TableDoubleClick::Structure,
            connection_folders: ["Work".to_string(), "Work/Clients".to_string()].into(),
            collapsed_folders: ["Work/Clients".to_string()].into(),
            folder_looks: [(
                "Work".to_string(),
                FolderLook {
                    color: Some("blue".into()),
                    glyph: Some("briefcase".into()),
                    image: Some("00000000000000aa.png".into()),
                },
            )]
            .into(),
            confirm_close_tab: false,
            tool_paths: BTreeMap::new(),
            client_folders: vec![PathBuf::from("/opt/pg/bin")],
            default_clients: [(ClientFamily::Postgres, PathBuf::from("/opt/pg/bin"))].into(),
            ui_font_size: 15.0,
            editor_font_size: 14.0,
            grid_font_size: 13.0,
            tab_size: 4,
            soft_wrap: true,
            line_numbers: false,
            auto_completion: false,
            tab_accepts_completion: false,
            format: FormatOptions {
                keyword_case: kerno_core::sql::KeywordCase::Lower,
                indent: kerno_core::sql::Indent::Tab,
                comma: kerno_core::sql::Comma::Leading,
                line_width: 100,
            },
            format_on_save: true,
            ligatures: true,
            vim_mode: true,
            null_text: "∅".to_string(),
            show_time_zone: false,
            link_with_editor: true,
            selection_stats: false,
            hide_idle_sessions: true,
            environments: {
                let mut environments = environment::builtin_environments();
                environments[2].rules.confirm_execute = false;
                environments.push(EnvironmentDef {
                    id: "qa".into(),
                    name: "QA".into(),
                    color_light: Some("#1D6FB8".into()),
                    color_dark: Some("#5AA9F0".into()),
                    tint: environment::Tint::Soft,
                    rules: Default::default(),
                });
                environments
            },
            check_updates: false,
            skipped_update: Some("0.2.0".into()),
            toolbar_visible: false,
            toolbar_hidden: ["commit".to_string()].into(),
            recent_scripts: vec![PathBuf::from("/tmp/a.sql")],
            script_connections: [(PathBuf::from("/tmp/a.sql"), "c1".to_string())].into(),
            tab_layout: TabLayout::Vertical,
            stack_results: true,
            ai: kerno_core::ai::AiSettings {
                enabled: true,
                endpoint: "http://localhost:11434/v1".into(),
                model: "llama3.2".into(),
                confirmed: ["https://api.example.com/v1".to_string()].into(),
                endpoints: vec![kerno_core::ai::SavedEndpoint {
                    name: "Ollama on this computer".into(),
                    base: "http://localhost:11434/v1".into(),
                    model: "llama3.2".into(),
                    models: vec![kerno_core::ai::Model {
                        id: "llama3.2".into(),
                        label: Some("Llama 3.2".into()),
                    }],
                }],
            },
            value_panel: true,
            value_panel_width: 512.0,
        };
        changed.save(&storage).unwrap();
        assert_eq!(Settings::load(&storage), changed);
    }

    #[test]
    fn recent_scripts_keep_the_last_ten_latest_first_once_each() {
        let mut settings = Settings::default();
        for n in 0..12 {
            settings.remember_script(PathBuf::from(format!("/s/{n}.sql")), Some(format!("c{n}")));
        }
        settings.remember_script(PathBuf::from("/s/5.sql"), None);
        let names: Vec<String> = settings
            .recent_scripts
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "/s/5.sql",
                "/s/11.sql",
                "/s/10.sql",
                "/s/9.sql",
                "/s/8.sql",
                "/s/7.sql",
                "/s/6.sql",
                "/s/4.sql",
                "/s/3.sql",
                "/s/2.sql"
            ]
        );
        // C6: each keeps the connection it ran on (`None` keeps it); one
        // that left the list leaves its connection behind.
        let ran_on = |path: &str| {
            settings
                .script_connections
                .get(&PathBuf::from(path))
                .cloned()
        };
        assert_eq!(ran_on("/s/5.sql").as_deref(), Some("c5"));
        assert_eq!(ran_on("/s/2.sql").as_deref(), Some("c2"));
        assert_eq!(ran_on("/s/1.sql"), None);
        assert_eq!(settings.script_connections.len(), RECENT_SCRIPTS);
    }

    #[test]
    fn the_tool_binaries_chosen_before_become_client_folders() {
        let storage = Storage::in_memory().unwrap();
        storage
            .set_setting(
                KEY,
                &serde_json::json!({"tool_paths": {
                    "pg_dump": "/opt/pg/bin/pg_dump",
                    "psql": "/opt/pg/bin/psql",
                    "mysqldump": "/opt/my/bin/mysqldump"
                }}),
            )
            .unwrap();
        let loaded = Settings::load(&storage);
        assert!(loaded.tool_paths.is_empty());
        assert_eq!(
            loaded.client_folders,
            [PathBuf::from("/opt/my/bin"), PathBuf::from("/opt/pg/bin")]
        );
        loaded.save(&storage).unwrap();
        let stored: serde_json::Value = storage.setting(KEY).unwrap().unwrap();
        assert!(stored.get("tool_paths").is_none(), "{stored}");
    }

    #[test]
    fn missing_and_unknown_fields_do_not_lose_the_rest() {
        let storage = Storage::in_memory().unwrap();
        storage
            .set_setting(
                KEY,
                &serde_json::json!({"appearance": "light", "from_the_future": 1}),
            )
            .unwrap();
        let loaded = Settings::load(&storage);
        assert_eq!(loaded.appearance, Appearance::Light);
        assert_eq!(loaded.page_size, Settings::default().page_size);
        // A result fetches its next page at the end of a scroll (K21).
        assert!(loaded.fetch_on_scroll);
        // Settings stored before the double-click choice existed open data.
        assert_eq!(loaded.table_double_click, TableDoubleClick::Data);
        // And ask before a tab with pending work is closed.
        assert!(loaded.confirm_close_tab);
        // The main toolbar came later: shown, with every item.
        assert!(loaded.toolbar_visible && loaded.toolbar_hidden.is_empty());
        // The settings view's values came later: their defaults.
        assert_eq!(loaded.grid_font_size, 11.5);
        assert_eq!(loaded.null_text, "NULL");
        assert!(loaded.line_numbers && loaded.auto_completion && !loaded.ligatures);
        // Tab accepts a completion; the formatter keeps its layout and does
        // not run on save.
        assert!(loaded.tab_accepts_completion && !loaded.format_on_save);
        assert_eq!(loaded.format, FormatOptions::default());
        // Settings stored before environments were the user's: the
        // built-ins, so production still asks twice.
        assert_eq!(loaded.environments, environment::builtin_environments());
    }

    #[test]
    fn a_built_in_environment_left_out_of_the_store_comes_back() {
        let storage = Storage::in_memory().unwrap();
        storage
            .set_setting(
                KEY,
                &serde_json::json!({"environments": [
                    {"id": "staging", "name": "Pre-prod", "tint": "strong"},
                    {"id": "qa", "name": "QA"}
                ]}),
            )
            .unwrap();
        let loaded = Settings::load(&storage);
        let ids: Vec<&str> = loaded.environments.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["local", "staging", "production", "qa"]);
        // The user's edit of a built-in is kept.
        assert_eq!(loaded.environments[1].name, "Pre-prod");
        assert!(loaded.environments[2].rules.confirm_writes_twice);
    }

    #[test]
    fn stored_values_outside_the_ranges_are_brought_back() {
        let storage = Storage::in_memory().unwrap();
        storage
            .set_setting(
                KEY,
                &serde_json::json!({"ui_font_size": 90, "grid_font_size": 1, "tab_size": 3, "null_text": " "}),
            )
            .unwrap();
        let loaded = Settings::load(&storage);
        assert_eq!(loaded.ui_font_size, 18.0);
        assert_eq!(loaded.grid_font_size, 11.0);
        assert_eq!(loaded.tab_size, 2);
        assert_eq!(loaded.null_text, "NULL");
    }
}
