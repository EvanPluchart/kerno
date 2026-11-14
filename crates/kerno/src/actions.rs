//! Every command of the app, in one table: its action, its name in the
//! command palette and menus, and its default shortcut. The palette, the
//! key bindings and the keymap override file are all derived from it, so a
//! command cannot exist without being reachable from the keyboard.

use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use gpui_kit::base::input::ShowCompletions;
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
        /// Runs the statement at the cursor and keeps the earlier results.
        RunStatementInNewTab,
        /// Runs the statements from the one at the cursor to the end.
        RunFromCursor,
        /// Moves the cursor to the start of the next statement.
        NextStatement,
        /// Moves the cursor to the start of the previous statement.
        PreviousStatement,
        /// Counts the rows the statement at the cursor would return.
        CountStatementRows,
        /// Pins the result on screen, or unpins it.
        TogglePinnedResult,
        /// Closes the result tab on screen.
        CloseResultTab,
        /// Hides the results of the active query tab, or shows them.
        ToggleResultsPanel,
        /// Shows only the results of the active query tab, or both again.
        MaximizeResultsPanel,
        /// Moves the keyboard between the editor and the results.
        SwitchResultsFocus,
        /// Closes the tabs to the left of the active one.
        CloseTabsToTheLeft,
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
        /// Connects the connection selected in the sidebar when it has the
        /// keyboard, else the active tab's.
        Connect,
        /// Disconnects the connection of the active tab.
        DisconnectTabConnection,
        /// Disconnects and connects again the connection of the active tab.
        ReconnectTabConnection,
        /// Disconnects every open connection.
        DisconnectAll,
        /// Disconnects every open connection but the active tab's (or the
        /// one selected in the sidebar).
        DisconnectOtherConnections,
        /// Makes the active tab's connection (or the one selected in the
        /// sidebar) read-only, or lets it write again.
        ToggleReadOnly,
        /// Opens the dialog of the connection of the active tab.
        EditTabConnection,
        /// Opens the administration of the active tab's connection, or of
        /// the one selected in the sidebar.
        OpenAdmin,
        /// The same administration, on its Locks sub-tab.
        OpenLockManager,
        /// Runs "Read data in SQL console" on the selected tree node.
        ReadObjectInConsole,
        /// Writes the call of the selected routine into a query tab.
        GenerateRoutineCall,
        /// Runs the selected routine, asking for its parameters.
        ExecuteRoutine,
        /// Opens the form of the selected table on a new column.
        AddColumnToObject,
        /// Backs up the selected database, or the selected table alone.
        BackupObject,
        /// Writes a backup of a database with the engine's dump tool.
        BackupDatabase,
        /// Restores a backup file into a database with the engine's client tool.
        RestoreDatabase,
        /// Backs up a whole PostgreSQL server with `pg_dumpall`.
        GlobalBackup,
        /// Runs a SQL file with the engine's client tool.
        ExecuteScript,
        /// The foreign data wrappers of a PostgreSQL database.
        ForeignDataWrappers,
        /// A tool's wizard whose aim is a saved task.
        CreateTask,
        /// Opens the Tasks view.
        ShowTasks,
        /// Opens the maintenance tools of the selected tree node.
        MaintenanceTools,
        /// Lists the statements run in the active tab's open transaction.
        ShowTransactionLog,
        /// Hides the object selected in the schema tree (its kind's filter).
        HideObjectInTree,
        /// Shows only the object selected in the schema tree, of its kind.
        ShowOnlyObjectInTree,
        /// Edits the filter of the kind of the object selected in the tree.
        ConfigureObjectFilter,
        /// Turns the filter of the selected object's kind on or off.
        ToggleObjectFilter,
        /// Removes the filter of the selected object's kind.
        ClearObjectFilter,
        /// Activates the next tab.
        NextTab,
        /// Activates the previous tab.
        PreviousTab,
        /// Shows or hides the sidebar.
        ToggleSidebar,
        /// Shows or hides the main toolbar.
        ToggleMainToolbar,
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
        /// Asks GitHub whether a newer release is out.
        CheckForUpdates,
        /// Quits Kerno.
        Quit,
        /// Lists the tables and views of the active connection, to open one.
        QuickOpen,
        /// Puts the keyboard in the schema tree's filter.
        FocusNavigatorFilter,
        /// Saves a copy of the connection selected in the sidebar.
        DuplicateConnection,
        /// Deletes the connection selected in the sidebar, after asking.
        DeleteConnection,
        /// Shows the next result tab of the active query tab.
        NextResultTab,
        /// Shows the previous result tab of the active query tab.
        PreviousResultTab,
        /// Fetches the next page of the result on screen.
        FetchMoreRows,
        /// Fetches every remaining row of the result on screen.
        FetchAllRows,
        /// Copies the selected cells as CSV.
        CopyAsCsv,
        /// Copies the selected cells as JSON.
        CopyAsJson,
        /// Copies the selected cells as a Markdown table.
        CopyAsMarkdown,
        /// Copies the selected cells as SQL INSERT statements.
        CopyAsSqlInsert,
        /// Shows the selected cell's whole value.
        ViewCellValue,
        /// Wraps long lines in the SQL editors, or stops.
        ToggleSoftWrap,
        /// Opens the rows of the table or view named at the editor's cursor.
        OpenTableAtCursor,
        /// Opens the structure of the table or view named at the cursor.
        OpenStructureAtCursor,
        /// Opens the keymap file, made with an example first when missing.
        OpenKeymapFile,
        /// Wraps the selection in a block comment, or unwraps it.
        ToggleBlockComment,
        /// Puts the selection in upper case, strings and quoted names aside.
        UpperCase,
        /// Puts the selection in lower case, strings and quoted names aside.
        LowerCase,
        /// Copies the statement at the cursor, or the selection.
        CopySelectedQuery,
        /// Reads again what completion knows of the tab's database.
        RefreshCompletionMetadata,
        /// Exports the rows of the statement at the cursor to a file.
        ExportFromQuery,
        /// Puts back the text of the active tab's file.
        RevertFile,
        /// Selects the active tab's table in the schema tree, now and as
        /// tabs are activated, or stops.
        LinkWithEditor,
        /// Shows the current row of the grid as a list of its columns, or
        /// the grid again.
        ToggleRecordView,
        /// Selects the first row of the grid.
        FirstRow,
        /// Selects the row above.
        PreviousRow,
        /// Selects the row below.
        NextRow,
        /// Selects the last row fetched.
        LastRow,
        /// Asks for a row number and selects that row.
        GoToRow,
        /// Lists the columns of the grid to select one.
        GoToColumn,
        /// Opens the find bar over the fetched rows of the grid.
        FindInResults,
        /// Moves to the next match of the grid's find bar.
        FindNextInResults,
        /// Moves to the previous match of the grid's find bar.
        FindPreviousInResults,
        /// Forgets the remembered layout of the table's columns.
        ResetTableLayout,
        /// Copies the numbers of the selected rows.
        CopyRowNumbers,
        /// Shows the figures of the selected cells in the grid's footer, or
        /// stops.
        ToggleSelectionStats,
        /// Shows count, sum, average, minimum and maximum of the selection.
        ShowAggregatePanel,
        /// Lists the columns of the result with their types and keys.
        ShowColumnsPanel,
        /// Lists the values of the selected column with their counts, to
        /// filter on some.
        FilterDistinctValues,
        /// Shows every row of the table or result on screen again.
        ClearFilters,
        /// Counts every row of the table or result on screen.
        CalculateRowCount,
        /// Reads the rows on screen again every few seconds, or stops.
        ToggleAutoRefresh,
        /// Selects every occurrence of the word or selection at the cursor.
        SelectAllOccurrences,
        /// Opens every folded region of the editor.
        UnfoldAll,
        /// Makes the text of the SQL editors and the grids larger.
        ZoomIn,
        /// Makes the text of the SQL editors and the grids smaller.
        ZoomOut,
        /// Gives the SQL editors and the grids their usual text size.
        ZoomReset,
        /// Lays the open tabs out on one row that scrolls (J10).
        TabsOnOneRow,
        /// Lays the open tabs out on as many rows as they need (J10).
        TabsOnSeveralRows,
        /// Lists the open tabs at the left of the tab on screen (J10).
        TabsAtTheSide,
        /// Lists every open tab, to pick one (J10).
        ShowAllTabs,
        /// Shows the results of a script one under the other, or in result
        /// tabs again (J11).
        ToggleStackedResults,
    ]
);

/// A command run from the menu bar by its id: the menu item of a command
/// whose keys work only in one part of the window (the tree, the connection
/// list, a grid) carries this instead of the command's action, so the menu
/// does not take those keys (Backspace, Enter, Escape, Cmd+C) everywhere.
#[derive(Clone, PartialEq, Eq, Deserialize, gpui_kit::Action)]
#[action(namespace = kerno, no_json)]
pub struct RunCommand {
    pub id: String,
}

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
    command!(
        "focus_navigator_filter",
        "Filter the Schema Tree",
        "Connections",
        ["secondary-alt-f"],
        ["search", "find", "navigator", "objects"],
        FocusNavigatorFilter
    ),
    // DBeaver's Open Database Object (Ctrl+Shift+D), as editors' Quick Open.
    command!(
        "quick_open",
        "Open Table…",
        "Connections",
        ["secondary-p"],
        ["quick open", "go to", "find table", "view", "jump"],
        QuickOpen
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
    // DBeaver's Ctrl+] makes a script file and Ctrl+Alt+Enter a console
    // (`core.sql.editor.console`); a query tab is a console until saved, so
    // both keys open one bound to the node's database and schema.
    command!(
        "new_query_on_object",
        "New Query on Selected Object",
        "Navigator",
        ["secondary-]", "secondary-alt-enter"],
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
        "duplicate_connection",
        "Duplicate Selected Connection",
        "Connections",
        [],
        ["copy", "clone"],
        DuplicateConnection,
        "ConnectionList"
    ),
    command!(
        "delete_connection",
        "Delete Selected Connection…",
        "Connections",
        ["backspace", "delete"],
        ["remove"],
        DeleteConnection,
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
    // DBeaver's keys of the SQL editor (`ui.editors.sql` plugin.xml): those
    // of `sql.script.focused` work while the editor has the focus, those of
    // `sql.script` anywhere in the query tab. Ctrl is Ctrl on macOS too.
    // On a Mac, Option with a letter types a character (Option+P is π)
    // instead of reaching the app, so each Alt key comes after a Cmd+Option
    // one, which types nothing; Cmd+Option+Up/Down add cursors in the
    // editor, so the statements move with Cmd+Option+] and [.
    command!(
        "run_statement_new_tab",
        "Run Statement in New Result Tab",
        "Query",
        ["secondary-\\"],
        ["execute", "keep results", "another tab"],
        RunStatementInNewTab,
        "QueryTab > mode == editor"
    ),
    command!(
        "run_from_cursor",
        "Run Script From the Cursor",
        "Query",
        ["cmd-alt-enter", "alt-p"],
        ["execute", "rest", "position"],
        RunFromCursor,
        "QueryTab > mode == editor"
    ),
    command!(
        "next_statement",
        "Go to Next Statement",
        "Query",
        ["cmd-alt-]", "alt-down"],
        ["query", "move", "jump"],
        NextStatement,
        "QueryTab > mode == editor"
    ),
    command!(
        "previous_statement",
        "Go to Previous Statement",
        "Query",
        ["cmd-alt-[", "alt-up"],
        ["query", "move", "jump"],
        PreviousStatement,
        "QueryTab > mode == editor"
    ),
    command!(
        "count_statement_rows",
        "Count Rows of Statement",
        "Query",
        [],
        ["count", "select count", "how many"],
        CountStatementRows
    ),
    command!(
        "toggle_pinned_result",
        "Pin or Unpin Result Tab",
        "Query",
        ["ctrl-shift-p"],
        ["keep", "result tab"],
        TogglePinnedResult,
        "QueryTab"
    ),
    // DBeaver's Ctrl+Shift+\: keyboards report a shifted symbol as the
    // symbol itself, `|` on a US layout.
    command!(
        "close_result_tab",
        "Close Result Tab",
        "Query",
        ["ctrl-|"],
        ["result tab"],
        CloseResultTab,
        "QueryTab"
    ),
    command!(
        "next_result_tab",
        "Next Result Tab",
        "Query",
        [],
        ["results", "messages"],
        NextResultTab
    ),
    command!(
        "previous_result_tab",
        "Previous Result Tab",
        "Query",
        [],
        ["results", "messages"],
        PreviousResultTab
    ),
    // DBeaver's fetch keys (Ctrl+Alt+N, Ctrl+Shift+=, which keyboards
    // report as `+`).
    command!(
        "fetch_more_rows",
        "Fetch More Rows",
        "Query",
        ["ctrl-alt-n"],
        ["next page", "load more", "results"],
        FetchMoreRows,
        "QueryTab"
    ),
    command!(
        "fetch_all_rows",
        "Fetch All Rows",
        "Query",
        ["ctrl-+"],
        ["every row", "load all", "results"],
        FetchAllRows,
        "QueryTab"
    ),
    // Completion while the list is closed: DBeaver's Ctrl+Space, which
    // macOS may keep for the input sources, and Cmd+I as other Mac editors.
    command!(
        "show_completions",
        "Show Completions",
        "Editor",
        ["ctrl-space", "secondary-i"],
        ["content assist", "autocomplete", "suggest", "proposals"],
        ShowCompletions,
        "QueryTab > mode == editor"
    ),
    command!(
        "open_table_at_cursor",
        "Open Rows of the Table at the Cursor",
        "Editor",
        ["f4"],
        ["data", "declaration", "go to"],
        OpenTableAtCursor,
        "QueryTab > mode == editor"
    ),
    command!(
        "open_structure_at_cursor",
        "Open Structure of the Table at the Cursor",
        "Editor",
        ["shift-f4"],
        ["columns", "ddl", "declaration"],
        OpenStructureAtCursor,
        "QueryTab > mode == editor"
    ),
    // DBeaver toggles the panel with Ctrl+T, which is New Query Tab on Linux.
    command!(
        "toggle_results_panel",
        "Toggle Results Panel",
        "View",
        ["cmd-alt-r", "alt-shift-t"],
        ["hide", "show", "results"],
        ToggleResultsPanel,
        "QueryTab"
    ),
    command!(
        "maximize_results_panel",
        "Maximize Results Panel",
        "View",
        ["ctrl-shift-t"],
        ["results", "full", "restore"],
        MaximizeResultsPanel,
        "QueryTab"
    ),
    command!(
        "switch_results_focus",
        "Focus Results / Editor",
        "View",
        ["cmd-alt-t", "alt-t"],
        ["switch panel", "grid", "keyboard"],
        SwitchResultsFocus,
        "QueryTab"
    ),
    command!(
        "refresh_data",
        "Refresh Rows",
        "Data",
        ["secondary-r"],
        ["reload", "read again"],
        RefreshData
    ),
    // DBeaver's key (`core.resultset.filterMenu.distinct`), in a grid.
    command!(
        "filter_distinct_values",
        "Filter by Distinct Values…",
        "Data",
        ["ctrl-f11"],
        ["distinct", "values", "checklist", "in", "filter"],
        FilterDistinctValues,
        "DataTable"
    ),
    command!(
        "clear_filters",
        "Clear Filters",
        "Data",
        [],
        ["reset", "filter", "every row", "where"],
        ClearFilters
    ),
    command!(
        "calculate_row_count",
        "Calculate Total Row Count",
        "Data",
        [],
        ["count", "how many", "rows", "total", "count exactly"],
        CalculateRowCount
    ),
    command!(
        "toggle_auto_refresh",
        "Toggle Auto-Refresh",
        "Data",
        [],
        ["refresh", "every", "seconds", "timer", "reload"],
        ToggleAutoRefresh
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
        "copy_as_csv",
        "Copy Selection as CSV",
        "Data",
        [],
        ["clipboard", "comma"],
        CopyAsCsv
    ),
    command!(
        "copy_as_json",
        "Copy Selection as JSON",
        "Data",
        [],
        ["clipboard", "objects"],
        CopyAsJson
    ),
    command!(
        "copy_as_markdown",
        "Copy Selection as Markdown",
        "Data",
        [],
        ["clipboard", "table"],
        CopyAsMarkdown
    ),
    command!(
        "copy_as_sql_insert",
        "Copy Selection as SQL INSERT",
        "Data",
        [],
        ["clipboard", "statements"],
        CopyAsSqlInsert
    ),
    command!(
        "view_cell_value",
        "View or Edit Value of the Selected Cell",
        "Data",
        ["shift-enter"],
        [
            "viewer",
            "value panel",
            "edit",
            "json",
            "text",
            "hex",
            "long"
        ],
        ViewCellValue,
        "DataTable"
    ),
    // DBeaver's keys of the result viewer (`ui.editors.data` plugin.xml):
    // Tab toggles the record mode, Ctrl+Alt+arrows move between rows,
    // Ctrl+G and Ctrl+Shift+G go to a row or a column.
    command!(
        "toggle_record_view",
        "Toggle Record View",
        "Data",
        ["tab"],
        ["record mode", "row as list", "transpose", "layout"],
        ToggleRecordView,
        "DataTable"
    ),
    command!(
        "first_row",
        "Go to First Row",
        "Data",
        ["ctrl-alt-shift-left"],
        ["navigate", "top"],
        FirstRow,
        "DataTable"
    ),
    command!(
        "previous_row",
        "Go to Previous Row",
        "Data",
        ["ctrl-alt-left"],
        ["navigate", "up"],
        PreviousRow,
        "DataTable"
    ),
    command!(
        "next_row",
        "Go to Next Row",
        "Data",
        ["ctrl-alt-right"],
        ["navigate", "down"],
        NextRow,
        "DataTable"
    ),
    command!(
        "last_row",
        "Go to Last Row",
        "Data",
        ["ctrl-alt-shift-right"],
        ["navigate", "bottom"],
        LastRow,
        "DataTable"
    ),
    command!(
        "go_to_row",
        "Go to Row…",
        "Data",
        ["ctrl-g"],
        ["jump", "row number", "navigate"],
        GoToRow,
        "DataTable"
    ),
    command!(
        "go_to_column",
        "Go to Column…",
        "Data",
        ["ctrl-shift-g"],
        ["jump", "find column", "navigate"],
        GoToColumn,
        "DataTable"
    ),
    // J6 (DBeaver #38133): Cmd+F finds in the grid that has the focus; the
    // editor keeps its own Cmd+F. Next and previous while the find bar has
    // the keyboard; `cmd-` because Ctrl+G goes to a row on Linux.
    command!(
        "find_in_results",
        "Find in Results…",
        "Data",
        ["secondary-f"],
        ["search", "quick filter", "matching rows", "grid"],
        FindInResults,
        "DataTable"
    ),
    command!(
        "find_next_in_results",
        "Find Next in Results",
        "Data",
        ["cmd-g"],
        ["search", "next match"],
        FindNextInResults,
        "GridFind"
    ),
    command!(
        "find_previous_in_results",
        "Find Previous in Results",
        "Data",
        ["cmd-shift-g"],
        ["search", "previous match"],
        FindPreviousInResults,
        "GridFind"
    ),
    command!(
        "reset_table_layout",
        "Reset Table Layout",
        "Data",
        [],
        ["columns", "widths", "hidden", "order", "forget", "default"],
        ResetTableLayout
    ),
    command!(
        "copy_row_numbers",
        "Copy Row Numbers",
        "Data",
        [],
        ["clipboard", "row names"],
        CopyRowNumbers
    ),
    command!(
        "toggle_selection_stats",
        "Toggle Selection Statistics",
        "Data",
        [],
        ["count", "sum", "average", "status", "layout"],
        ToggleSelectionStats
    ),
    command!(
        "show_aggregate_panel",
        "Show Aggregate Panel",
        "Data",
        [],
        [
            "count", "distinct", "sum", "average", "min", "max", "panels"
        ],
        ShowAggregatePanel
    ),
    command!(
        "show_columns_panel",
        "Show Columns Panel",
        "Data",
        [],
        ["metadata", "types", "keys", "nullable", "panels"],
        ShowColumnsPanel
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
    // DBeaver's `comment.multi` (Ctrl+Shift+/) and Eclipse's To Upper /
    // Lower Case (Ctrl+Shift+X / Y).
    command!(
        "toggle_block_comment",
        "Toggle Block Comment",
        "Editor",
        ["secondary-shift-/"],
        ["comment out", "uncomment", "/*", "multi-line"],
        ToggleBlockComment
    ),
    command!(
        "upper_case",
        "Upper Case",
        "Editor",
        ["secondary-shift-x"],
        ["capitals", "uppercase", "format"],
        UpperCase
    ),
    command!(
        "lower_case",
        "Lower Case",
        "Editor",
        ["secondary-shift-y"],
        ["small letters", "lowercase", "format"],
        LowerCase
    ),
    // DBeaver's ⌥J (`select.all.occurrences`), on VS Code's key (CLAUDE.md §4 J7).
    command!(
        "select_all_occurrences",
        "Select All Occurrences",
        "Editor",
        ["secondary-shift-l"],
        [
            "multi-cursor",
            "same word",
            "matches",
            "rename",
            "highlight"
        ],
        SelectAllOccurrences,
        "QueryTab > mode == editor"
    ),
    // The kit folds from the gutter only (#17240); opening every fold is reachable.
    command!(
        "unfold_all",
        "Unfold All",
        "Editor",
        [],
        ["folding", "expand", "collapse", "regions"],
        UnfoldAll
    ),
    // ⌘+ / ⌘- / ⌘0 from anywhere: the editors and the grids together (#17817).
    command!(
        "zoom_in",
        "Zoom In (Editor and Grids)",
        "View",
        ["secondary-="],
        ["font size", "bigger", "larger text"],
        ZoomIn
    ),
    command!(
        "zoom_out",
        "Zoom Out (Editor and Grids)",
        "View",
        ["secondary--"],
        ["font size", "smaller", "smaller text"],
        ZoomOut
    ),
    command!(
        "zoom_reset",
        "Reset Zoom (Editor and Grids)",
        "View",
        ["secondary-0"],
        ["font size", "usual size", "reset"],
        ZoomReset
    ),
    // J10: the tabs on one row, on several, or listed at the side
    // (DBeaver #7114, #23248), and the list of every open tab.
    command!(
        "tabs_on_one_row",
        "Tabs on One Row",
        "View",
        [],
        ["tab layout", "strip", "single row"],
        TabsOnOneRow
    ),
    command!(
        "tabs_on_several_rows",
        "Tabs on Several Rows",
        "View",
        [],
        ["tab layout", "wrap", "multi-row", "multiple rows"],
        TabsOnSeveralRows
    ),
    command!(
        "tabs_at_the_side",
        "Tabs Listed at the Side",
        "View",
        [],
        ["tab layout", "vertical tabs", "list", "sidebar"],
        TabsAtTheSide
    ),
    command!(
        "show_all_tabs",
        "Show All Open Tabs…",
        "View",
        [],
        ["tabs", "overflow", "switch", "list", "editors"],
        ShowAllTabs
    ),
    // J11: the result sets of a script in one panel (DBeaver #6861).
    command!(
        "toggle_stacked_results",
        "Stack Results in One Panel",
        "View",
        [],
        ["results", "single panel", "one tab", "multiple result sets"],
        ToggleStackedResults
    ),
    // DBeaver's `copy.query`.
    command!(
        "copy_selected_query",
        "Copy Statement at Cursor",
        "Editor",
        [],
        ["copy query", "clipboard", "statement"],
        CopySelectedQuery
    ),
    // DBeaver's `refresh.current.schema` and `refresh.all.schemas`: Kerno's
    // catalog holds every schema, so one command reads them all again.
    command!(
        "refresh_completion_metadata",
        "Refresh Completion Metadata",
        "Editor",
        [],
        [
            "reload",
            "autocomplete",
            "schema",
            "tables",
            "content assist"
        ],
        RefreshCompletionMetadata
    ),
    command!(
        "revert_file",
        "Revert to Saved File",
        "Editor",
        [],
        ["reload", "discard", "undo changes", "file"],
        RevertFile
    ),
    // DBeaver's Ctrl+Alt+Shift+W (`word.wrap`).
    command!(
        "toggle_soft_wrap",
        "Toggle Soft Wrap",
        "Editor",
        ["ctrl-alt-shift-w"],
        ["word wrap", "long lines"],
        ToggleSoftWrap
    ),
    // DBeaver's `export.data` from the editor: the rows the statement
    // returns, run again for the export.
    command!(
        "export_from_query",
        "Export From Query…",
        "Query",
        [],
        ["csv", "json", "file", "statement", "dump"],
        ExportFromQuery
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
        "close_tabs_to_the_left",
        "Close Tabs to the Left",
        "Tabs",
        [],
        [],
        CloseTabsToTheLeft
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
    // DBeaver's Ctrl+4 and Ctrl+8 (`core.commit`, `core.rollback`; Ctrl on
    // macOS too).
    command!(
        "commit_transaction",
        "Commit Transaction",
        "Query",
        ["ctrl-4"],
        ["end", "save"],
        CommitTransaction
    ),
    command!(
        "rollback_transaction",
        "Roll Back Transaction",
        "Query",
        ["ctrl-8"],
        ["rollback", "undo", "abort"],
        RollbackTransaction
    ),
    command!(
        "toggle_commit_mode",
        "Switch Between Auto-Commit and Manual Commit",
        "Query",
        ["ctrl-alt-a"],
        ["transaction", "autocommit"],
        ToggleCommitMode
    ),
    // The main toolbar's Connect: the connection selected in the sidebar
    // when it has the keyboard, else the active tab's.
    command!(
        "connect",
        "Connect",
        "Connections",
        [],
        ["open", "login", "server"],
        Connect
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
        "disconnect_all",
        "Disconnect All Connections",
        "Connections",
        [],
        ["close", "every"],
        DisconnectAll
    ),
    command!(
        "disconnect_other_connections",
        "Disconnect Other Connections",
        "Connections",
        [],
        ["close", "others", "keep"],
        DisconnectOtherConnections
    ),
    command!(
        "toggle_read_only",
        "Toggle Read-Only Connection",
        "Connections",
        [],
        ["readonly", "protect", "writes", "safe"],
        ToggleReadOnly
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
    // DBeaver's Lock Manager (`postgresql.lock.manager`).
    command!(
        "lock_manager",
        "Lock Manager",
        "Server",
        [],
        [
            "locks",
            "blocking",
            "waiting",
            "deadlock",
            "blocked sessions"
        ],
        OpenLockManager
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
    command!(
        "global_backup",
        "Global Backup…",
        "Server",
        [],
        [
            "pg_dumpall",
            "roles",
            "tablespaces",
            "whole server",
            "backup all"
        ],
        GlobalBackup
    ),
    command!(
        "execute_script",
        "Execute Script…",
        "Server",
        [],
        ["psql", "mysql", "run file", "sql file", "script"],
        ExecuteScript
    ),
    command!(
        "foreign_data_wrappers",
        "Foreign Data Wrappers…",
        "Server",
        [],
        [
            "fdw",
            "postgres_fdw",
            "foreign server",
            "user mapping",
            "import foreign schema"
        ],
        ForeignDataWrappers
    ),
    // DBeaver's Database ▸ Tasks (`DatabaseTasksView`); no scheduler.
    command!(
        "create_task",
        "Create New Task…",
        "Server",
        [],
        ["task", "save settings", "backup task", "saved task"],
        CreateTask
    ),
    command!(
        "show_tasks",
        "Tasks",
        "Server",
        [],
        ["tasks", "saved tasks", "run again", "task log"],
        ShowTasks
    ),
    command!(
        "maintenance_tools",
        "Maintenance Tools…",
        "Navigator",
        [],
        [
            "vacuum", "analyze", "refresh", "truncate", "trigger", "check", "optimize", "repair"
        ],
        MaintenanceTools
    ),
    // DBeaver's transaction monitor (`core.txn.log`).
    command!(
        "transaction_log",
        "Transaction Log",
        "Query",
        [],
        ["statements", "pending", "transaction monitor", "commit"],
        ShowTransactionLog
    ),
    // The entries of the tree's menus, on its selected node. "Edit column…"
    // is F4 on a column (`edit_object`).
    command!(
        "read_object_in_console",
        "Read Data in SQL Console",
        "Navigator",
        [],
        ["select", "rows", "table", "console"],
        ReadObjectInConsole
    ),
    command!(
        "generate_routine_call",
        "Generate CALL for Selected Routine",
        "Navigator",
        [],
        ["function", "procedure", "call", "sql"],
        GenerateRoutineCall
    ),
    command!(
        "execute_routine",
        "Execute Selected Routine…",
        "Navigator",
        [],
        ["function", "procedure", "call", "run"],
        ExecuteRoutine
    ),
    command!(
        "add_column_to_object",
        "Add Column to Selected Table…",
        "Navigator",
        [],
        ["alter", "new column", "table form"],
        AddColumnToObject
    ),
    command!(
        "backup_object",
        "Back Up Selected Object…",
        "Navigator",
        [],
        [
            "backup",
            "dump",
            "pg_dump",
            "mysqldump",
            "table",
            "database"
        ],
        BackupObject
    ),
    // DBeaver's Filter ▸ of the navigator (`core.object.filter.*`).
    command!(
        "hide_object_in_tree",
        "Filter: Hide Selected Object",
        "Navigator",
        [],
        ["object filter", "exclude", "schema tree"],
        HideObjectInTree
    ),
    command!(
        "show_only_object_in_tree",
        "Filter: Show Only Selected Object",
        "Navigator",
        [],
        ["object filter", "include", "schema tree"],
        ShowOnlyObjectInTree
    ),
    command!(
        "configure_object_filter",
        "Filter: Configure…",
        "Navigator",
        [],
        ["object filter", "mask", "include", "exclude", "schema tree"],
        ConfigureObjectFilter
    ),
    command!(
        "toggle_object_filter",
        "Filter: Turn On or Off",
        "Navigator",
        [],
        ["object filter", "enable", "disable", "schema tree"],
        ToggleObjectFilter
    ),
    command!(
        "clear_object_filter",
        "Filter: Clear",
        "Navigator",
        [],
        ["object filter", "remove", "reset", "schema tree"],
        ClearObjectFilter
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
        "toggle_main_toolbar",
        "Show Main Toolbar",
        "View",
        [],
        ["toolbar", "hide", "buttons"],
        ToggleMainToolbar
    ),
    // DBeaver's `core.navigator.linkeditor` (Ctrl+Shift+,).
    command!(
        "link_with_editor",
        "Link Schema Tree With Editor",
        "View",
        ["secondary-shift-,"],
        ["reveal", "sync", "navigator", "select table", "follow"],
        LinkWithEditor
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
        "open_keymap_file",
        "Open Keymap File",
        "Kerno",
        [],
        ["shortcuts", "keys", "bindings", "keyboard"],
        OpenKeymapFile
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
        "check_for_updates",
        "Check for Updates…",
        "Kerno",
        [],
        ["update", "upgrade", "release", "version", "new"],
        CheckForUpdates
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

/// What "Open Keymap File" writes when there is no keymap file yet: valid
/// JSON whose `about` and `commands` are ignored, with no binding changed,
/// and every command id with its name and default shortcuts to copy from.
pub fn keymap_example() -> String {
    let commands: serde_json::Map<String, serde_json::Value> = COMMANDS
        .iter()
        .map(|command| {
            let keys = if command.keys.is_empty() {
                "no shortcut".to_string()
            } else {
                command.keys.join(", ")
            };
            let text = format!("{} ({keys})", command.label);
            (command.id.to_string(), serde_json::Value::String(text))
        })
        .collect();
    let example = serde_json::json!({
        "about": [
            "Kerno's keymap. Each entry of \"bindings\" replaces the shortcuts of one command:",
            "one shortcut, a list of shortcuts, or null to leave the command without one.",
            "\"secondary\" is Cmd on macOS and Ctrl elsewhere; a sequence is keys separated by spaces.",
            "Example: \"new_query_tab\": \"secondary-shift-t\", \"close_tab\": [\"secondary-w\", \"ctrl-f4\"], \"toggle_sidebar\": null",
            "The commands and their default shortcuts are listed below. Changes apply when Kerno starts again."
        ],
        "bindings": {},
        "commands": commands,
    });
    serde_json::to_string_pretty(&example).unwrap_or_default() + "\n"
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
    // Cmd+Enter in Open Table opens the structure (`quick_open.rs`): a key of
    // that dialog, as Enter and the arrows are the list's own.
    key_bindings.push(KeyBinding::new(
        if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        },
        gpui_kit::base::actions::Confirm { secondary: true },
        Some("QuickOpen"),
    ));
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
    fn the_query_tab_keys_shadow_no_global_key_on_either_system() {
        // A key of the query tab would win over a global one while the tab
        // has the focus: none may be the same keystroke, on either system.
        let (bindings, _) = effective_bindings("");
        for os in [OperatingSystem::Mac, OperatingSystem::Linux] {
            let mut seen = BTreeMap::new();
            for (key, id) in &bindings {
                if context_of(id).is_some_and(|context| !context.starts_with("QueryTab")) {
                    continue;
                }
                let stroke = Keystroke::parse_with_os(key, os).unwrap().unparse(os);
                if let Some(other) = seen.insert(stroke.clone(), *id)
                    && other != *id
                {
                    panic!("{os:?}: {stroke} is bound to both {other} and {id}");
                }
            }
        }
        assert_eq!(
            keys_of(&bindings, "run_statement_new_tab"),
            ["secondary-\\"]
        );
        assert_eq!(keys_of(&bindings, "toggle_pinned_result"), ["ctrl-shift-p"]);
        assert!(KeyBindingContextPredicate::parse("QueryTab > mode == editor").is_ok());
    }

    #[test]
    fn every_alt_key_has_one_a_mac_keyboard_reaches() {
        // Option with a letter types a character on a Mac (Option+P is π),
        // so a command bound to Alt alone has a Cmd or Ctrl key too.
        let (bindings, _) = effective_bindings("");
        let mac = |key: &str| Keystroke::parse_with_os(key, OperatingSystem::Mac).unwrap();
        let alt_only = |key: &str| {
            let modifiers = mac(key).modifiers;
            modifiers.alt && !modifiers.platform && !modifiers.control
        };
        for command in COMMANDS {
            let keys = keys_of(&bindings, command.id);
            if keys.iter().any(|key| alt_only(key)) {
                assert!(
                    keys.iter().any(|key| {
                        let modifiers = mac(key).modifiers;
                        modifiers.platform || modifiers.control
                    }),
                    "{}: {keys:?}",
                    command.id
                );
            }
        }
        // DBeaver's key stays, second.
        assert_eq!(
            keys_of(&bindings, "run_from_cursor"),
            ["cmd-alt-enter", "alt-p"]
        );
        assert_eq!(
            keys_of(&bindings, "next_statement"),
            ["cmd-alt-]", "alt-down"]
        );
        assert_eq!(
            keys_of(&bindings, "previous_statement"),
            ["cmd-alt-[", "alt-up"]
        );
        assert_eq!(
            keys_of(&bindings, "toggle_results_panel"),
            ["cmd-alt-r", "alt-shift-t"]
        );
        assert_eq!(
            keys_of(&bindings, "switch_results_focus"),
            ["cmd-alt-t", "alt-t"]
        );
        // The editor's own Mac keys (gpui-base `input/base/state.rs`) are
        // not taken by the query tab's: they would stop working in it.
        for editor in [
            "cmd-alt-up",
            "cmd-alt-down",
            "cmd-up",
            "cmd-down",
            "cmd-[",
            "cmd-]",
            "alt-left",
            "alt-right",
            "cmd-left",
            "cmd-right",
        ] {
            let unparse = |key: &str| mac(key).unparse(OperatingSystem::Mac);
            for (key, id) in &bindings {
                if context_of(id).is_some_and(|context| context.starts_with("QueryTab")) {
                    assert_ne!(unparse(key), unparse(editor), "{id} takes the editor's key");
                }
            }
        }
    }

    #[test]
    fn every_command_can_be_rebound_or_unbound() {
        for command in COMMANDS {
            let id = command.id;
            let (bindings, problems) =
                effective_bindings(&format!(r#"{{"bindings": {{"{id}": null}}}}"#));
            assert_eq!(problems, Vec::<String>::new(), "{id}");
            assert_eq!(keys_of(&bindings, id), Vec::<String>::new(), "{id}");
            let (bindings, problems) = effective_bindings(&format!(
                r#"{{"bindings": {{"{id}": ["ctrl-alt-shift-f12", "f13"]}}}}"#
            ));
            assert_eq!(problems, Vec::<String>::new(), "{id}");
            assert_eq!(
                keys_of(&bindings, id),
                ["ctrl-alt-shift-f12", "f13"],
                "{id}"
            );
            // The others keep their defaults.
            assert_eq!(
                bindings.len(),
                effective_bindings("").0.len() + 2 - command.keys.len()
            );
        }
    }

    #[test]
    fn the_keymap_example_changes_nothing_and_lists_every_command() {
        let example = keymap_example();
        let (bindings, problems) = effective_bindings(&example);
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(bindings, effective_bindings("").0);
        for command in COMMANDS {
            assert!(
                example.contains(&format!("\"{}\"", command.id)),
                "{}",
                command.id
            );
        }
        assert!(example.contains("\"secondary-k\"") || example.contains("secondary-k"));
    }

    #[test]
    fn the_new_commands_have_their_keys() {
        let (bindings, _) = effective_bindings("");
        for (id, keys) in [
            ("commit_transaction", &["ctrl-4"][..]),
            ("rollback_transaction", &["ctrl-8"]),
            ("toggle_commit_mode", &["ctrl-alt-a"]),
            ("quick_open", &["secondary-p"]),
            ("focus_navigator_filter", &["secondary-alt-f"]),
            ("toggle_soft_wrap", &["ctrl-alt-shift-w"]),
            ("show_completions", &["ctrl-space", "secondary-i"]),
            ("fetch_more_rows", &["ctrl-alt-n"]),
            ("fetch_all_rows", &["ctrl-+"]),
            ("zoom_in", &["secondary-="]),
            ("zoom_out", &["secondary--"]),
            ("zoom_reset", &["secondary-0"]),
            ("toggle_block_comment", &["secondary-shift-/"]),
            ("upper_case", &["secondary-shift-x"]),
            ("lower_case", &["secondary-shift-y"]),
            ("link_with_editor", &["secondary-shift-,"]),
        ] {
            assert_eq!(keys_of(&bindings, id), keys, "{id}");
        }
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
