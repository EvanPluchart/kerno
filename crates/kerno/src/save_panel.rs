//! Save panels that open where the last file of their kind went (K56): each
//! kind of save remembers its folder with the app's other small memories
//! (the settings); a folder that is gone falls back to the home folder.

use std::path::PathBuf;

use gpui_kit::{App, Task};

use crate::settings::Settings;
use crate::state::AppState;

/// What a save panel saves: each remembers its own folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    /// A table's rows or a result, exported.
    Export,
    /// A backup (its folder is the backup's own memory, `backup_folder`).
    Backup,
    /// A schema's document.
    SchemaDocument,
    /// An ER diagram.
    Diagram,
    /// The connections' export.
    Connections,
    /// A query tab's text, saved as a `.sql` file.
    SqlFile,
    /// A new SQLite database.
    DatabaseFile,
}

impl SaveKind {
    fn key(self) -> &'static str {
        match self {
            Self::Export => "export",
            Self::Backup => "backup",
            Self::SchemaDocument => "schema-document",
            Self::Diagram => "diagram",
            Self::Connections => "connections",
            Self::SqlFile => "sql-file",
            Self::DatabaseFile => "database-file",
        }
    }

    /// The folder this kind of save last used.
    fn remembered(self, settings: &Settings) -> Option<PathBuf> {
        match self {
            Self::Backup => settings.backup_folder.clone(),
            _ => settings.save_folders.get(self.key()).cloned(),
        }
    }

    fn remember(self, folder: PathBuf, settings: &mut Settings) {
        match self {
            Self::Backup => settings.backup_folder = Some(folder),
            _ => {
                settings.save_folders.insert(self.key().to_string(), folder);
            }
        }
    }
}

/// Opens the save panel on `name`: in `folder` when given (the file in use's
/// own), else in the folder this kind of save last used, else in the home
/// folder, the first of them that is there. The folder of the path chosen
/// is remembered for the next one. `None` when the panel was cancelled.
pub fn ask(
    kind: SaveKind,
    folder: Option<PathBuf>,
    name: Option<String>,
    cx: &mut App,
) -> Task<Option<PathBuf>> {
    let remembered = kind.remembered(&AppState::get(cx).settings);
    cx.spawn(async move |cx| {
        // Looked for off the UI thread: a folder on a disk that sleeps.
        let start = cx
            .background_executor()
            .spawn(async move { start_folder([folder, remembered]) })
            .await;
        let picking = cx.update(|cx| cx.prompt_for_new_path(&start, name.as_deref()));
        let path = picking.await.ok()?.ok()??;
        if let Some(folder) = path.parent().map(PathBuf::from) {
            cx.update(|cx| {
                AppState::update_settings(cx, |settings| kind.remember(folder, settings))
            });
        }
        Some(path)
    })
}

/// The first of `folders` that is a folder there now, else the home folder.
fn start_folder(folders: [Option<PathBuf>; 2]) -> PathBuf {
    folders
        .into_iter()
        .flatten()
        .find(|folder| folder.is_absolute() && folder.is_dir())
        .or_else(kerno_core::home::dir)
        .unwrap_or_default()
}
