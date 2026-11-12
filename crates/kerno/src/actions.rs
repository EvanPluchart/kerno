//! Every command of the app, in one table: its action, its name in the
//! command palette and menus, and its default shortcut. The palette, the
//! key bindings and the keymap override file are all derived from it, so a
//! command cannot exist without being reachable from the keyboard.

use std::collections::BTreeMap;
use std::path::Path;

use gpui_kit::{Action, App, KeyBinding, Keystroke, OperatingSystem};
use serde::Deserialize;

gpui_kit::actions!(
    kerno,
    [
        /// Opens the command palette.
        OpenCommandPalette,
        /// Opens the dialog to add a connection.
        NewConnection,
        /// Adds the connections of an exported file.
        ImportConnections,
        /// Writes the saved connections, without secrets, to a file.
        ExportConnections,
        /// Runs the statement at the cursor, or the selection.
        RunStatement,
        /// Runs every statement of the active tab.
        RunAll,
        /// Cancels the statement the active tab is running.
        CancelQuery,
        /// Comments the selected lines out, or back in.
        ToggleComment,
        /// Lays out the SQL of the selection, or of the whole editor.
        FormatSql,
        /// Reads the rows of the table on screen again.
        RefreshData,
        /// Adds an empty row to the table on screen.
        AddRow,
        /// Copies the selected rows as new rows.
        DuplicateRows,
        /// Marks the selected rows for deletion.
        DeleteRows,
        /// Sets the selected cells to NULL.
        SetNull,
        /// Gives the selected cells their column's default.
        SetDefault,
        /// Shows the SQL of the pending changes, to apply it.
        ApplyChanges,
        /// Forgets the pending changes.
        DiscardChanges,
        /// Opens the editor's search panel.
        FindInEditor,
        /// Opens the editor's search panel with its replace row.
        ReplaceInEditor,
        /// Shows the plan of the statement at the cursor.
        ExplainStatement,
        /// Runs the statement at the cursor and shows its plan with timings.
        ExplainAnalyze,
        /// Opens a `.sql` file in a tab.
        OpenSqlFile,
        /// Writes the active tab to its file.
        SaveFile,
        /// Writes the active tab to a file chosen now.
        SaveFileAs,
        /// Shows what was run before.
        ShowHistory,
        /// Shows the queries saved under a name.
        ShowSavedQueries,
        /// Saves the text of the active tab under a name.
        SaveQuery,
        /// Opens a new query tab.
        NewQueryTab,
        /// Closes the active tab.
        CloseTab,
        /// Activates the next tab.
        NextTab,
        /// Activates the previous tab.
        PreviousTab,
        /// Shows or hides the sidebar.
        ToggleSidebar,
        /// Opens the settings.
        OpenSettings,
        /// Follows the system's light or dark appearance.
        UseSystemTheme,
        /// Switches to the light theme.
        UseLightTheme,
        /// Switches to the dark theme.
        UseDarkTheme,
        /// Shows the version and licence.
        About,
        /// Quits Kerno.
        Quit,
    ]
);

/// One command of the app.
pub struct Command {
    /// Stable identifier, used in the keymap file.
    pub id: &'static str,
    /// Name shown in the palette and menus.
    pub label: &'static str,
    /// Palette group.
    pub group: &'static str,
    /// Default shortcuts. `secondary` is Cmd on macOS and Ctrl elsewhere.
    pub keys: &'static [&'static str],
    /// Extra words the palette search matches.
    pub keywords: &'static [&'static str],
    /// Builds the action.
    pub action: fn() -> Box<dyn Action>,
}

macro_rules! command {
    ($id:literal, $label:literal, $group:literal, $keys:expr, $keywords:expr, $action:ident) => {
        Command {
            id: $id,
            label: $label,
            group: $group,
            keys: &$keys,
            keywords: &$keywords,
            action: || Box::new($action),
        }
    };
}

/// Every command, in palette order.
pub const COMMANDS: &[Command] = &[
    command!(
        "new_connection",
        "New Connection",
        "Connections",
        ["secondary-n"],
        [
            "add",
            "server",
            "database",
            "postgresql",
            "mysql",
            "mariadb"
        ],
        NewConnection
    ),
    command!(
        "import_connections",
        "Import Connections…",
        "Connections",
        [],
        ["json", "file"],
        ImportConnections
    ),
    command!(
        "export_connections",
        "Export Connections…",
        "Connections",
        [],
        ["json", "file", "backup", "share"],
        ExportConnections
    ),
    command!(
        "new_query_tab",
        "New Query Tab",
        "Tabs",
        ["secondary-t"],
        ["sql", "editor"],
        NewQueryTab
    ),
    command!(
        "open_sql_file",
        "Open SQL File…",
        "Files",
        ["secondary-o"],
        ["load", "script"],
        OpenSqlFile
    ),
    command!(
        "save_file",
        "Save",
        "Files",
        ["secondary-s"],
        ["write", "sql", "file"],
        SaveFile
    ),
    command!(
        "save_file_as",
        "Save As…",
        "Files",
        ["secondary-shift-s"],
        ["write", "sql", "file"],
        SaveFileAs
    ),
    command!(
        "show_history",
        "Query History",
        "Query",
        ["secondary-y"],
        ["past", "recent", "ran"],
        ShowHistory
    ),
    command!(
        "show_saved_queries",
        "Saved Queries",
        "Query",
        [],
        ["snippets", "favourites", "library"],
        ShowSavedQueries
    ),
    command!(
        "save_query",
        "Save Query…",
        "Query",
        [],
        ["name", "snippet", "favourite", "keep"],
        SaveQuery
    ),
    command!(
        "run_statement",
        "Run Statement",
        "Query",
        ["secondary-enter"],
        ["execute", "selection", "sql"],
        RunStatement
    ),
    command!(
        "run_all",
        "Run All Statements",
        "Query",
        ["secondary-shift-enter"],
        ["execute", "script", "sql"],
        RunAll
    ),
    command!(
        "cancel_query",
        "Cancel Running Statement",
        "Query",
        ["secondary-."],
        ["stop", "abort", "kill"],
        CancelQuery
    ),
    command!(
        "refresh_data",
        "Refresh Rows",
        "Data",
        ["secondary-r"],
        ["reload", "read again"],
        RefreshData
    ),
    command!(
        "add_row",
        "Add Row",
        "Data",
        [],
        ["insert", "new row"],
        AddRow
    ),
    command!(
        "duplicate_rows",
        "Duplicate Selected Rows",
        "Data",
        [],
        ["copy row", "clone"],
        DuplicateRows
    ),
    command!(
        "delete_rows",
        "Delete Selected Rows",
        "Data",
        [],
        ["remove row"],
        DeleteRows
    ),
    command!(
        "set_null",
        "Set Selected Cells to NULL",
        "Data",
        [],
        ["null", "empty"],
        SetNull
    ),
    command!(
        "set_default",
        "Set Selected Cells to Default",
        "Data",
        [],
        ["default value"],
        SetDefault
    ),
    command!(
        "apply_changes",
        "Review and Apply Pending Changes",
        "Data",
        [],
        ["commit", "save rows", "review sql"],
        ApplyChanges
    ),
    command!(
        "discard_changes",
        "Discard Pending Changes",
        "Data",
        [],
        ["revert", "undo edits"],
        DiscardChanges
    ),
    // The editor binds its own search keys (Cmd+F, Cmd+Shift+F) while it has
    // the focus; these make the panel reachable from the palette and the menu.
    command!(
        "find_in_editor",
        "Find in Editor",
        "Editor",
        [],
        ["search", "find text"],
        FindInEditor
    ),
    command!(
        "replace_in_editor",
        "Replace in Editor",
        "Editor",
        [],
        ["search and replace", "substitute"],
        ReplaceInEditor
    ),
    command!(
        "toggle_comment",
        "Toggle Line Comment",
        "Editor",
        ["secondary-/"],
        ["comment out", "uncomment", "--"],
        ToggleComment
    ),
    command!(
        "format_sql",
        "Format SQL",
        "Editor",
        ["secondary-shift-i"],
        ["beautify", "pretty", "indent"],
        FormatSql
    ),
    command!(
        "explain_statement",
        "Explain Statement",
        "Query",
        ["secondary-e"],
        ["plan", "query plan"],
        ExplainStatement
    ),
    command!(
        "explain_analyze",
        "Explain Analyze (Runs the Statement)",
        "Query",
        ["secondary-shift-e"],
        ["plan", "timing", "slow"],
        ExplainAnalyze
    ),
    command!(
        "close_tab",
        "Close Tab",
        "Tabs",
        ["secondary-w"],
        [],
        CloseTab
    ),
    command!("next_tab", "Next Tab", "Tabs", ["ctrl-tab"], [], NextTab),
    command!(
        "previous_tab",
        "Previous Tab",
        "Tabs",
        ["ctrl-shift-tab"],
        [],
        PreviousTab
    ),
    command!(
        "toggle_sidebar",
        "Toggle Sidebar",
        "View",
        ["secondary-b"],
        ["connections", "navigator"],
        ToggleSidebar
    ),
    command!(
        "use_system_theme",
        "Theme: Follow System",
        "View",
        [],
        ["appearance", "auto"],
        UseSystemTheme
    ),
    command!(
        "use_light_theme",
        "Theme: Light",
        "View",
        [],
        ["appearance"],
        UseLightTheme
    ),
    command!(
        "use_dark_theme",
        "Theme: Dark",
        "View",
        [],
        ["appearance"],
        UseDarkTheme
    ),
    command!(
        "open_command_palette",
        "Command Palette",
        "Kerno",
        ["secondary-k"],
        ["commands", "actions"],
        OpenCommandPalette
    ),
    command!(
        "open_settings",
        "Settings",
        "Kerno",
        ["secondary-,"],
        ["preferences", "options"],
        OpenSettings
    ),
    command!(
        "about",
        "About Kerno",
        "Kerno",
        [],
        ["version", "licence", "license"],
        About
    ),
    command!(
        "quit",
        "Quit Kerno",
        "Kerno",
        ["secondary-q"],
        ["exit"],
        Quit
    ),
];

/// The system `secondary-` resolves against: Cmd on macOS, Ctrl elsewhere.
const HOST_OS: OperatingSystem = if cfg!(target_os = "macos") {
    OperatingSystem::Mac
} else {
    OperatingSystem::Linux
};

/// The user's keymap file: `{"bindings": {"<command id>": <keys>}}`, where
/// keys are a shortcut, a list of shortcuts, or `null` to leave the command
/// without one.
#[derive(Debug, Default, Deserialize)]
struct KeymapFile {
    #[serde(default)]
    bindings: BTreeMap<String, Option<KeysEntry>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum KeysEntry {
    One(String),
    Many(Vec<String>),
}

/// The shortcuts in effect: defaults, replaced per command by the user's
/// overrides. Returns `(shortcut, command id)` pairs and what was wrong
/// with the overrides, if anything.
fn effective_bindings(overrides: &str) -> (Vec<(String, &'static str)>, Vec<String>) {
    let mut problems = Vec::new();
    let file = if overrides.trim().is_empty() {
        KeymapFile::default()
    } else {
        serde_json::from_str::<KeymapFile>(overrides).unwrap_or_else(|error| {
            problems.push(format!("the keymap file is not valid JSON: {error}"));
            KeymapFile::default()
        })
    };
    for id in file.bindings.keys() {
        if !COMMANDS.iter().any(|command| command.id == id) {
            problems.push(format!("unknown command \"{id}\""));
        }
    }

    let mut bindings = Vec::new();
    for command in COMMANDS {
        let keys: Vec<String> = match file.bindings.get(command.id) {
            None => command.keys.iter().map(|key| key.to_string()).collect(),
            Some(None) => Vec::new(),
            Some(Some(KeysEntry::One(key))) => vec![key.clone()],
            Some(Some(KeysEntry::Many(keys))) => keys.clone(),
        };
        for key in keys {
            // A shortcut may be a sequence of keystrokes separated by spaces.
            let valid = !key.trim().is_empty() && key.split_whitespace().all(is_shortcut);
            if valid {
                bindings.push((key, command.id));
            } else {
                problems.push(format!(
                    "\"{key}\" is not a shortcut (command \"{}\")",
                    command.id
                ));
            }
        }
    }

    // The same shortcut on two commands: the later one would silently win.
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for (key, id) in &bindings {
        if let Some(other) = seen.insert(key.as_str(), id)
            && other != *id
        {
            problems.push(format!(
                "\"{key}\" is bound to both \"{other}\" and \"{id}\""
            ));
        }
    }
    (bindings, problems)
}

/// Whether `stroke` is a keystroke Kerno will bind: modifiers followed by
/// one character or a named key. GPUI's parser accepts any word as a key
/// name, so a typo such as `secondary-entr` would otherwise be bound to a
/// key that does not exist, silently.
fn is_shortcut(stroke: &str) -> bool {
    const NAMED: &[&str] = &[
        "enter",
        "tab",
        "escape",
        "space",
        "backspace",
        "delete",
        "insert",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "pageup",
        "pagedown",
    ];
    let Ok(parsed) = Keystroke::parse_with_os(stroke, HOST_OS) else {
        return false;
    };
    let key = parsed.key.as_str();
    let function_key = key
        .strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=24).contains(&number));
    key.chars().count() == 1 || NAMED.contains(&key) || function_key
}

/// Binds every command's shortcut, applying the user's keymap file when it
/// exists. Returns what was wrong with that file, for the app to show.
pub fn bind_keys(keymap_file: &Path, cx: &mut App) -> Vec<String> {
    let overrides = match std::fs::read_to_string(keymap_file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            let problem = format!("could not read {}: {error}", keymap_file.display());
            bind(&effective_bindings("").0, cx);
            return vec![problem];
        }
    };
    let (bindings, problems) = effective_bindings(&overrides);
    bind(&bindings, cx);
    problems
}

fn bind(bindings: &[(String, &'static str)], cx: &mut App) {
    let mut key_bindings = Vec::with_capacity(bindings.len());
    for (key, id) in bindings {
        let Some(command) = COMMANDS.iter().find(|command| command.id == *id) else {
            continue;
        };
        match KeyBinding::load(key, (command.action)(), None, false, None, cx) {
            Ok(binding) => key_bindings.push(binding),
            // Already validated; a failure here is a shortcut this keyboard
            // layout cannot produce.
            Err(error) => tracing::warn!(%error, key, command = id, "shortcut not bound"),
        }
    }
    cx.bind_keys(key_bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(bindings: &[(String, &'static str)], id: &str) -> Vec<String> {
        bindings
            .iter()
            .filter(|(_, command)| *command == id)
            .map(|(key, _)| key.clone())
            .collect()
    }

    #[test]
    fn command_ids_are_unique_and_shortcuts_valid() {
        let mut ids: Vec<_> = COMMANDS.iter().map(|command| command.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), COMMANDS.len());

        let (bindings, problems) = effective_bindings("");
        assert_eq!(problems, Vec::<String>::new());
        // The shortcuts the spec names.
        assert_eq!(keys_of(&bindings, "open_command_palette"), ["secondary-k"]);
        assert_eq!(keys_of(&bindings, "new_query_tab"), ["secondary-t"]);
        assert_eq!(keys_of(&bindings, "close_tab"), ["secondary-w"]);
    }

    #[test]
    fn overrides_replace_defaults_per_command() {
        let (bindings, problems) = effective_bindings(
            r#"{"bindings": {
                "new_query_tab": "secondary-shift-t",
                "close_tab": ["secondary-w", "ctrl-f4"],
                "toggle_sidebar": null,
                "about": "f1"
            }}"#,
        );
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(keys_of(&bindings, "new_query_tab"), ["secondary-shift-t"]);
        assert_eq!(keys_of(&bindings, "close_tab"), ["secondary-w", "ctrl-f4"]);
        assert_eq!(keys_of(&bindings, "toggle_sidebar"), Vec::<String>::new());
        assert_eq!(keys_of(&bindings, "about"), ["f1"]);
        // Untouched commands keep their defaults.
        assert_eq!(keys_of(&bindings, "open_command_palette"), ["secondary-k"]);
    }

    #[test]
    fn problems_in_the_keymap_file_are_reported_not_fatal() {
        let (bindings, problems) = effective_bindings("{ not json");
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("not valid JSON"), "{problems:?}");
        assert_eq!(
            keys_of(&bindings, "quit"),
            ["secondary-q"],
            "defaults still apply"
        );

        let (bindings, problems) = effective_bindings(
            r#"{"bindings": {"no_such_command": "f2", "quit": "not a key!!", "about": "secondary-k"}}"#,
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("unknown command \"no_such_command\"")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("\"not a key!!\"")),
            "{problems:?}"
        );
        assert!(
            problems
                .iter()
                .any(|p| p.contains("\"secondary-k\" is bound to both")),
            "{problems:?}"
        );
        assert_eq!(keys_of(&bindings, "quit"), Vec::<String>::new());
    }

    #[test]
    fn shortcuts_need_a_real_key() {
        for good in [
            "a",
            "secondary-k",
            "ctrl-shift-tab",
            "f5",
            "cmd-,",
            "alt-enter",
            "shift-f12",
            "cmd-.",
        ] {
            assert!(is_shortcut(good), "{good}");
        }
        for bad in [
            "secondary-entr",
            "not",
            "key!!",
            "ctrl-",
            "f25",
            "cmd-cmd",
            "",
        ] {
            assert!(!is_shortcut(bad), "{bad}");
        }
    }
}
