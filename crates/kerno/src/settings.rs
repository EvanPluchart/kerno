//! User settings, stored as one value in Kerno's storage.

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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            appearance: Appearance::System,
            page_size: 1_000,
            confirm_destructive: true,
            sidebar_visible: true,
            table_double_click: TableDoubleClick::Data,
        }
    }
}

impl Settings {
    /// Reads the stored settings; defaults when none were stored or the
    /// store cannot be read.
    pub fn load(storage: &Storage) -> Self {
        match storage.setting::<Settings>(KEY) {
            Ok(Some(settings)) => settings,
            Ok(None) => Self::default(),
            Err(error) => {
                tracing::warn!(%error, "could not read settings; using defaults");
                Self::default()
            }
        }
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
    }
}
