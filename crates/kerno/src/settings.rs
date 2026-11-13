//! User settings, stored as one value in Kerno's storage.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use kerno_core::storage::Storage;
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
    /// Ask before closing a tab that holds pending changes, unsaved text or
    /// a changed table form. An open transaction always asks.
    pub confirm_close_tab: bool,
    /// The client tool binaries chosen by the user for backup and restore,
    /// by tool name (`pg_dump`, `psql`, `pg_restore`, `mysqldump`, `mysql`);
    /// the others are looked for where they are usually installed.
    pub tool_paths: BTreeMap<String, PathBuf>,
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
    /// Ligatures in the SQL editors (`>=` drawn as one sign).
    pub ligatures: bool,
    /// What a NULL cell shows.
    pub null_text: String,
    /// Timestamps with a time zone show their offset.
    pub show_time_zone: bool,
    /// The schema tree selects the table of the tab being activated.
    pub link_with_editor: bool,
}

/// The sizes the settings allow, in points.
pub const UI_FONT_SIZES: std::ops::RangeInclusive<f32> = 12.0..=18.0;
pub const CODE_FONT_SIZES: std::ops::RangeInclusive<f32> = 11.0..=20.0;
/// The sizes a page of rows may have.
pub const PAGE_SIZES: std::ops::RangeInclusive<u32> = 100..=10_000;

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            page_size: 1_000,
            confirm_destructive: true,
            sidebar_visible: true,
            table_double_click: TableDoubleClick::Data,
            connection_folders: BTreeSet::new(),
            collapsed_folders: BTreeSet::new(),
            confirm_close_tab: true,
            tool_paths: BTreeMap::new(),
            ui_font_size: 13.0,
            editor_font_size: 12.5,
            grid_font_size: 11.5,
            tab_size: 2,
            soft_wrap: false,
            line_numbers: true,
            auto_completion: true,
            ligatures: false,
            null_text: "NULL".to_string(),
            show_time_zone: true,
            link_with_editor: false,
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
        if !matches!(self.tab_size, 2 | 4) {
            self.tab_size = default.tab_size;
        }
        // NULL never looks like an empty string.
        if self.null_text.trim().is_empty() {
            self.null_text = default.null_text;
        }
        self
    }

    /// Stores the settings.
    pub fn save(&self, storage: &Storage) -> kerno_core::Result<()> {
        storage.set_setting(KEY, self)
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
            confirm_destructive: false,
            sidebar_visible: false,
            table_double_click: TableDoubleClick::Structure,
            connection_folders: ["Work".to_string(), "Work/Clients".to_string()].into(),
            collapsed_folders: ["Work/Clients".to_string()].into(),
            confirm_close_tab: false,
            tool_paths: [("pg_dump".to_string(), PathBuf::from("/opt/pg/bin/pg_dump"))].into(),
            ui_font_size: 15.0,
            editor_font_size: 14.0,
            grid_font_size: 13.0,
            tab_size: 4,
            soft_wrap: true,
            line_numbers: false,
            auto_completion: false,
            ligatures: true,
            null_text: "∅".to_string(),
            show_time_zone: false,
            link_with_editor: true,
        };
        changed.save(&storage).unwrap();
        assert_eq!(Settings::load(&storage), changed);
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
        // Settings stored before the double-click choice existed open data.
        assert_eq!(loaded.table_double_click, TableDoubleClick::Data);
        // And ask before a tab with pending work is closed.
        assert!(loaded.confirm_close_tab);
        // The settings view's values came later: their defaults.
        assert_eq!(loaded.grid_font_size, 11.5);
        assert_eq!(loaded.null_text, "NULL");
        assert!(loaded.line_numbers && loaded.auto_completion && !loaded.ligatures);
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
