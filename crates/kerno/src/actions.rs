//! Every command of the app, in one table: its action, its name in the
//! command palette and menus, and its default shortcut. The palette, the
//! key bindings and the keymap override file are all derived from it, so a
//! command cannot exist without being reachable from the keyboard.

use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use gpui_kit::{Action, App, KeyBinding, KeyBindingContextPredicate, Keystroke, OperatingSystem};
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
        /// Makes a connection folder at the top level.
        NewConnectionFolder,
        /// Reads the saved connections again and checks the open ones.
        RefreshConnections,
        /// Closes every connection folder of the sidebar.
        CollapseConnectionFolders,
        /// Reads the schema tree of the browsed connection again.
        RefreshSchemaTree,
        /// Closes every open node of the schema tree.
        CollapseSchemaTree,
        /// Opens the editor of the object selected in the schema tree.
        EditObject,
        /// Renames the object selected in the schema tree.
        RenameObject,
        /// Drops the object selected in the schema tree.
        DropObject,
        /// Reads the node selected in the schema tree again.
        RefreshObject,
        /// Copies the name of the object selected in the schema tree.
        CopyObjectName,
        /// Copies the qualified name of the object selected in the schema tree.
        CopyQualifiedName,
        /// Opens a query tab on the object selected in the schema tree.
        NewQueryOnObject,
        /// Makes the schema selected in the schema tree the active tab's.
        SetDefaultSchema,
        /// Opens the dialog of the connection selected in the sidebar.
        EditConnection,
        /// Connects the connection selected in the sidebar and browses it.
        OpenConnection,
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
        /// Gives the selected cells the values of the row above.
        CopyFromRowAbove,
        /// Gives the selected cells the values of the row below.
        CopyFromRowBelow,
        /// Drops the pending values of the selected cells.
        RevertCells,
        /// Takes the last pending change back.
        UndoChange,
        /// Copies the selected cells with their column names.
        CopyWithColumnNames,
        /// Writes the rows of the table or result on screen to a file.
        ExportData,
        /// Imports a CSV file into a table.
        ImportCsv,
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
        /// Closes every tab but the active one.
        CloseOtherTabs,
        /// Closes the tabs to the right of the active one.
        CloseTabsToTheRight,
        /// Closes every tab.
        CloseAllTabs,
        /// Renames the active query tab.
        RenameTab,
        /// Opens a copy of the active query tab.
        DuplicateTab,
        /// Opens the query tab closed last again.
        ReopenClosedTab,
        /// Copies the path of the active tab's file.
        CopyTabFilePath,
        /// Shows the active tab's file in the file manager.
        RevealTabFile,
        /// Commits the open transaction of the active tab.
        CommitTransaction,
        /// Rolls back the open transaction of the active tab.
        RollbackTransaction,
        /// Switches the active tab between auto-commit and manual commit.
        ToggleCommitMode,
        /// Disconnects the connection of the active tab.
        DisconnectTabConnection,
        /// Disconnects and connects again the connection of the active tab.
        ReconnectTabConnection,
        /// Opens the dialog of the connection of the active tab.
        EditTabConnection,
        /// Opens the administration of the active tab's connection, or of
        /// the one selected in the sidebar.
        OpenAdmin,
        /// Writes a backup of a database with the engine's dump tool.
        BackupDatabase,
        /// Restores a backup file into a database with the engine's client tool.
        RestoreDatabase,
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
    /// The key context the shortcuts work in (everywhere when `None`):
    /// `Navigator` is the schema tree, `ConnectionList` the sidebar's list.
    pub context: Option<&'static str>,
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
            context: None,
        }
    };
    ($id:literal, $label:literal, $group:literal, $keys:expr, $keywords:expr, $action:ident, $context:literal) => {
        Command {
            context: Some($context),
            ..command!($id, $label, $group, $keys, $keywords, $action)
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
        "new_connection_folder",
        "New Connection Folder…",
        "Connections",
        [],
        ["group", "organize", "directory"],
        NewConnectionFolder
    ),
    command!(
        "refresh_connections",
        "Refresh Connections",
        "Connections",
        [],
        ["reload", "saved", "status"],
        RefreshConnections
    ),
    command!(
        "collapse_connection_folders",
        "Collapse All Connection Folders",
        "Connections",
        [],
        ["close", "folders", "sidebar"],
        CollapseConnectionFolders
    ),
    command!(
        "refresh_schema_tree",
        "Refresh Schema Tree",
        "Connections",
        [],
        ["reload", "navigator", "objects"],
        RefreshSchemaTree
    ),
    command!(
        "collapse_schema_tree",
        "Collapse Schema Tree",
        "Connections",
        [],
        ["close", "navigator", "objects"],
        CollapseSchemaTree
    ),
    // The keys of the schema tree and of the connection list, DBeaver's
    // defaults (Ctrl is Cmd on macOS); they work while that list has the focus.
    command!(
        "edit_object",
        "Edit Selected Object",
        "Navigator",
        ["f4"],
        ["open", "structure", "source", "ddl"],
        EditObject,
        "Navigator"
    ),
    command!(
        "rename_object",
        "Rename Selected Object…",
        "Navigator",
        ["f2"],
        ["name"],
        RenameObject,
        "Navigator"
    ),
    command!(
        "drop_object",
        "Drop Selected Object…",
        "Navigator",
        ["backspace", "delete"],
        ["delete", "remove"],
        DropObject,
        "Navigator"
    ),
    command!(
        "refresh_object",
        "Refresh Selected Object",
        "Navigator",
        ["f5"],
        ["reload"],
        RefreshObject,
        "Navigator"
    ),
    command!(
        "copy_object_name",
        "Copy Object Name",
        "Navigator",
        ["secondary-c"],
        ["clipboard"],
        CopyObjectName,
        "Navigator"
    ),
    command!(
        "copy_qualified_name",
        "Copy Qualified Object Name",
        "Navigator",
        ["secondary-shift-c"],
        ["clipboard", "schema", "full name"],
        CopyQualifiedName,
        "Navigator"
    ),
    command!(
        "new_query_on_object",
        "New Query on Selected Object",
        "Navigator",
        ["secondary-]"],
        ["sql", "console", "script"],
        NewQueryOnObject,
        "Navigator"
    ),
    command!(
        "set_default_schema",
        "Set Selected Schema as Default for This Tab",
        "Navigator",
        ["secondary-shift-a"],
        ["active", "database", "search path", "use"],
        SetDefaultSchema,
        "Navigator"
    ),
    command!(
        "edit_connection",
        "Edit Selected Connection…",
        "Connections",
        ["f4"],
        ["settings", "properties"],
        EditConnection,
        "ConnectionList"
    ),
    command!(
        "open_connection",
        "Open Selected Connection",
        "Connections",
        ["enter"],
        ["connect", "browse"],
        OpenConnection,
        "ConnectionList"
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
    // The keys of the grids, DBeaver's defaults (`ui.editors.data`
    // plugin.xml, context `ui.context.resultset.focused`; Ctrl is Ctrl on
    // macOS too), plus keys Mac keyboards have. They work while a grid has
    // the focus (`DataTable`, the table's own context), never in the editor.
    // `cmd-` where Linux's Ctrl would take another command's key.
    command!(
        "add_row",
        "Add Row",
        "Data",
        ["alt-insert", "secondary-shift-n"],
        ["insert", "new row"],
        AddRow,
        "DataTable"
    ),
    command!(
        "duplicate_rows",
        "Duplicate Selected Rows",
        "Data",
        ["ctrl-alt-insert", "cmd-d"],
        ["copy row", "clone"],
        DuplicateRows,
        "DataTable"
    ),
    command!(
        "delete_rows",
        "Delete Selected Rows",
        "Data",
        ["alt-delete", "cmd-backspace"],
        ["remove row"],
        DeleteRows,
        "DataTable"
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
        ["ctrl-backspace"],
        ["default value"],
        SetDefault,
        "DataTable"
    ),
    command!(
        "copy_from_row_above",
        "Copy Values From the Row Above",
        "Data",
        ["ctrl-d"],
        ["fill down", "repeat"],
        CopyFromRowAbove,
        "DataTable"
    ),
    command!(
        "copy_from_row_below",
        "Copy Values From the Row Below",
        "Data",
        ["ctrl-alt-d"],
        ["fill up", "repeat"],
        CopyFromRowBelow,
        "DataTable"
    ),
    command!(
        "revert_cells",
        "Revert Selected Cells",
        "Data",
        ["escape"],
        ["reset", "undo edit"],
        RevertCells,
        "DataTable"
    ),
    command!(
        "undo_change",
        "Undo Last Pending Change",
        "Data",
        ["secondary-z"],
        ["revert", "take back"],
        UndoChange,
        "DataTable"
    ),
    command!(
        "copy_with_column_names",
        "Copy Selection With Column Names",
        "Data",
        ["secondary-shift-c"],
        ["header", "clipboard", "advanced copy"],
        CopyWithColumnNames,
        "DataTable"
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
    command!(
        "export_data",
        "Export…",
        "Data",
        [],
        [
            "csv",
            "json",
            "ndjson",
            "markdown",
            "insert",
            "write rows",
            "dump"
        ],
        ExportData
    ),
    command!(
        "import_csv",
        "Import CSV…",
        "Data",
        [],
        ["load", "csv", "insert rows", "spreadsheet"],
        ImportCsv
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
    command!(
        "close_other_tabs",
        "Close Other Tabs",
        "Tabs",
        [],
        ["keep"],
        CloseOtherTabs
    ),
    command!(
        "close_tabs_to_the_right",
        "Close Tabs to the Right",
        "Tabs",
        [],
        [],
        CloseTabsToTheRight
    ),
    command!(
        "close_all_tabs",
        "Close All Tabs",
        "Tabs",
        ["secondary-shift-w"],
        [],
        CloseAllTabs
    ),
    command!(
        "rename_tab",
        "Rename Tab…",
        "Tabs",
        [],
        ["title", "name"],
        RenameTab
    ),
    command!(
        "duplicate_tab",
        "Duplicate Tab",
        "Tabs",
        [],
        ["copy", "clone"],
        DuplicateTab
    ),
    command!(
        "reopen_closed_tab",
        "Reopen Closed Tab",
        "Tabs",
        [],
        ["undo close", "restore"],
        ReopenClosedTab
    ),
    command!(
        "copy_tab_file_path",
        "Copy File Path",
        "Files",
        [],
        ["clipboard", "location"],
        CopyTabFilePath
    ),
    command!(
        "reveal_tab_file",
        "Show File in Its Folder",
        "Files",
        [],
        ["reveal", "finder", "file manager"],
        RevealTabFile
    ),
    command!(
        "commit_transaction",
        "Commit Transaction",
        "Query",
        [],
        ["end", "save"],
        CommitTransaction
    ),
    command!(
        "rollback_transaction",
        "Roll Back Transaction",
        "Query",
        [],
        ["rollback", "undo", "abort"],
        RollbackTransaction
    ),
    command!(
        "toggle_commit_mode",
        "Switch Between Auto-Commit and Manual Commit",
        "Query",
        [],
        ["transaction", "autocommit"],
        ToggleCommitMode
    ),
    command!(
        "disconnect_tab_connection",
        "Disconnect the Active Tab's Connection",
        "Connections",
        [],
        ["close"],
        DisconnectTabConnection
    ),
    command!(
        "reconnect_tab_connection",
        "Reconnect the Active Tab's Connection",
        "Connections",
        [],
        ["connect again"],
        ReconnectTabConnection
    ),
    command!(
        "edit_tab_connection",
        "Edit the Active Tab's Connection…",
        "Connections",
        [],
        ["settings", "properties"],
        EditTabConnection
    ),
    command!(
        "open_admin",
        "Server Administration",
        "Server",
        [],
        [
            "sessions",
            "processes",
            "kill",
            "terminate",
            "roles",
            "users",
            "grants",
            "sizes",
            "settings",
            "variables"
        ],
        OpenAdmin
    ),
    command!(
        "backup_database",
        "Back Up Database…",
        "Server",
        [],
        ["backup", "dump", "pg_dump", "mysqldump", "save database"],
        BackupDatabase
    ),
    command!(
        "restore_database",
        "Restore Database…",
        "Server",
        [],
        ["restore", "load dump", "pg_restore", "psql", "mysql"],
        RestoreDatabase
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

    // The same shortcut on two commands of one context: the later one would
    // silently win.
    let mut seen: BTreeMap<(Option<&str>, &str), &str> = BTreeMap::new();
    for (key, id) in &bindings {
        if let Some(other) = seen.insert((context_of(id), key.as_str()), id)
            && other != *id
        {
            problems.push(format!(
                "\"{key}\" is bound to both \"{other}\" and \"{id}\""
            ));
        }
    }
    (bindings, problems)
}

/// The key context of a command's shortcuts.
fn context_of(id: &str) -> Option<&'static str> {
    COMMANDS
        .iter()
        .find(|command| command.id == id)
        .and_then(|command| command.context)
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
        let context = command
            .context
            .and_then(|context| KeyBindingContextPredicate::parse(context).ok())
            .map(Rc::new);
        match KeyBinding::load(key, (command.action)(), context, false, None, cx) {
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
    fn a_shortcut_is_unique_within_its_context() {
        let (bindings, problems) = effective_bindings("");
        assert_eq!(problems, Vec::<String>::new());
        // F4 edits the object of the tree and the connection of the list.
        assert_eq!(keys_of(&bindings, "edit_object"), ["f4"]);
        assert_eq!(keys_of(&bindings, "edit_connection"), ["f4"]);
        assert_eq!(context_of("edit_object"), Some("Navigator"));
        assert_eq!(context_of("quit"), None);
        // Two commands of one context cannot share a key.
        let (_, problems) = effective_bindings(r#"{"bindings": {"rename_object": "f4"}}"#);
        assert!(
            problems
                .iter()
                .any(|p| p.contains("\"f4\" is bound to both")),
            "{problems:?}"
        );
    }

    #[test]
    fn the_grid_keys_differ_on_both_systems() {
        // `secondary-` and `cmd-` resolve per system: two strings can be one
        // keystroke (Ctrl+D on Linux), which the check above cannot see.
        let (bindings, _) = effective_bindings("");
        for os in [OperatingSystem::Mac, OperatingSystem::Linux] {
            let mut seen = BTreeMap::new();
            for (key, id) in &bindings {
                if context_of(id) != Some("DataTable") {
                    continue;
                }
                let stroke = Keystroke::parse_with_os(key, os).unwrap().unparse(os);
                if let Some(other) = seen.insert(stroke.clone(), *id) {
                    panic!("{os:?}: {stroke} is bound to both {other} and {id}");
                }
            }
        }
        assert_eq!(
            keys_of(&bindings, "add_row"),
            ["alt-insert", "secondary-shift-n"]
        );
        assert_eq!(keys_of(&bindings, "revert_cells"), ["escape"]);
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
