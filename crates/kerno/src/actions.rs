//! Every command of the app, in one table: its action, its name in the
//! command palette and menus, and its default shortcut. The palette, the
//! key bindings and the keymap override file are all derived from it, so a
//! command cannot exist without being reachable from the keyboard.

use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use gpui_kit::base::input::ShowCompletions;
use gpui_kit::{
    Action, App, KeyBinding, KeyBindingContextPredicate, KeyBindingMetaIndex, Keystroke,
    OperatingSystem,
};
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
        /// Adds the connections, folders and scripts of a DBeaver export.
        ImportFromDbeaver,
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
        /// Opens a `.sql` file in a tab on the connection selected in the sidebar (K38).
        OpenSqlFileOnConnection,
        /// Lists the SQL scripts of the connection selected in the sidebar, to open one (K38).
        ConnectionScripts,
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
        /// Opens the ER diagram of the selected schema or table, or of the
        /// active structure tab's table.
        ViewDiagram,
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
        /// Copies tables with their data to another connection (J13).
        CopyTables,
        /// Compares two schemas and opens the script between them (K30).
        CompareSchemas,
        /// Writes a schema's structure as a document (K31).
        ExportSchema,
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
        /// Opens the settings with the keyboard in their search (C5).
        SearchSettings,
        /// Copies the name of the selected cell's column.
        CopyColumnName,
        /// Drops the pending changes of the selected rows.
        RevertSelectedRows,
        /// Shows the hidden columns of the grid on screen.
        ShowAllColumns,
        /// Fits every column of the grid on screen to its values.
        FitAllColumnWidths,
        /// Writes a SELECT by key for each selected row in a query tab.
        GenerateSelectByKey,
        /// Writes one SELECT … IN for the selected rows in a query tab.
        GenerateSelectIn,
        /// Writes an INSERT for each selected row in a query tab.
        GenerateRowInsert,
        /// Writes an UPDATE by key for each selected row in a query tab.
        GenerateRowUpdate,
        /// Writes a DELETE by key for each selected row in a query tab.
        GenerateRowDelete,
        /// Opens the dialog to add a connection in the folder selected in the sidebar.
        NewConnectionInFolder,
        /// Makes a folder in the folder selected in the sidebar.
        NewSubfolder,
        /// Disconnects every open connection of the folder selected in the sidebar.
        DisconnectFolder,
        /// Opens the rows of the table or view selected in the schema tree.
        OpenObjectData,
        /// Opens the structure of the object selected in the schema tree.
        OpenObjectStructure,
        /// Shows the DDL of the object selected in the schema tree.
        ShowObjectDdl,
        /// Opens the source of the view or routine selected in the schema tree, to change it.
        EditObjectSource,
        /// Writes a SELECT of the table selected in the schema tree in a new query tab.
        GenerateSelect,
        /// Writes an INSERT into the table selected in the schema tree in a new query tab.
        GenerateInsert,
        /// Writes an UPDATE of the table selected in the schema tree in a new query tab.
        GenerateUpdate,
        /// Writes a DELETE from the table selected in the schema tree in a new query tab.
        GenerateDelete,
        /// Drops the object selected in the schema tree and what depends on it (PostgreSQL).
        DropObjectCascade,
        /// Writes a CREATE TABLE in the schema selected in the schema tree.
        CreateTable,
        /// Writes a CREATE VIEW in the schema selected in the schema tree.
        CreateView,
        /// Creates a schema in the database selected in the schema tree.
        CreateSchema,
        /// Creates a database on the server of the node selected in the schema tree.
        CreateDatabase,
        /// Writes a CREATE FUNCTION in the schema selected in the schema tree.
        CreateFunction,
        /// Writes a CREATE PROCEDURE in the schema selected in the schema tree.
        CreateProcedure,
        /// Writes a CREATE SEQUENCE in the schema selected in the schema tree.
        CreateSequence,
        /// Writes a CREATE TRIGGER on the table or schema selected in the schema tree.
        CreateTrigger,
        /// Writes a CREATE EVENT in the database selected in the schema tree (MySQL).
        CreateEvent,
        /// Creates an enum type in the schema selected in the schema tree.
        CreateEnum,
        /// Adds a value to the enum selected in the schema tree.
        AddEnumValue,
        /// Renames the enum value selected in the schema tree.
        RenameEnumValue,
        /// Opens the extensions manager of the database selected in the schema
        /// tree, or of the active tab (K27).
        ManageExtensions,
        /// Writes the rows of the table selected in the schema tree to a file.
        ExportObject,
        /// Imports a CSV file into the table selected in the schema tree.
        ImportCsvIntoObject,
        /// Renames the column selected in the schema tree.
        RenameColumn,
        /// Drops the column selected in the schema tree.
        DropColumn,
        /// Selects the table a foreign key selected in the schema tree points to.
        RevealReferencedTable,
        /// Closes the result tabs but the one on screen and the pinned ones.
        CloseOtherResultTabs,
        /// Closes the unpinned result tabs left of the one on screen.
        CloseResultTabsToTheLeft,
        /// Closes the unpinned result tabs right of the one on screen.
        CloseResultTabsToTheRight,
        /// Closes every unpinned result tab.
        CloseAllResultTabs,
        /// Unpins every result tab of the active query tab.
        UnpinAllResultTabs,
        /// Renames the result tab on screen.
        RenameResultTab,
        /// Copies the statement of the result tab on screen.
        CopyResultStatement,
        /// Applies the source of the view or routine being edited, after showing it.
        ApplySource,
        /// Shows the messages of the active query tab's last run.
        ShowMessages,
        /// Saves a copy of the connection copied last.
        PasteConnection,
        /// Copies the URL of the connection selected in the sidebar, without its password.
        CopyConnectionUrl,
        /// Copies the host of the connection selected in the sidebar.
        CopyConnectionHost,
        /// Follows the system's light or dark appearance.
        UseSystemTheme,
        /// Switches to the light theme.
        UseLightTheme,
        /// Switches to the dark theme.
        UseDarkTheme,
        /// Switches to the opposite of the theme on screen, light or dark (K20).
        ToggleTheme,
        /// Opens Settings ▸ Shortcuts (K20).
        OpenKeyboardShortcuts,
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
        /// Fetches the next rows of the result or the table on screen.
        FetchMoreRows,
        /// Fetches every remaining row of the result or the table on screen.
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
        /// Runs the active tab's script on connections the user picks (J14).
        RunScriptOnConnections,
        /// Shows the last run on several connections again.
        ShowMultiRun,
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
        /// Opens the rows the foreign key of the selected cell points to.
        FollowForeignKey,
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
        /// Shows the selected cell's value beside the grid, or hides it (K17).
        ToggleValuePanel,
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
        /// Moves the active tab into a new group at the right of its own (K19).
        SplitRight,
        /// Moves the active tab into a new group below its own (K19).
        SplitDown,
        /// Moves the active tab into the next group (the other one of two);
        /// with one group, splits it to the right (K19).
        MoveTabToNextGroup,
        /// Moves the active tab into the group before its own (K19).
        MoveTabToPreviousGroup,
        /// Gives the keyboard to the next group of tabs (K19).
        FocusNextGroup,
        /// Gives the keyboard to the group of tabs before (K19).
        FocusPreviousGroup,
        /// Shows the results of a script one under the other, or in result
        /// tabs again (J11).
        ToggleStackedResults,
        /// Turns vim mode in the SQL editors on or off (J19).
        ToggleVimMode,
        /// Asks the AI assistant for SQL written from a sentence (J17).
        AiWriteSql,
        /// Asks the AI assistant to explain the statement at the cursor.
        AiExplainQuery,
        /// Asks the AI assistant to explain the plan on screen.
        AiExplainPlan,
        /// Asks the AI assistant to explain the error on screen.
        AiExplainError,
        /// Shows or hides the AI assistant's panel (K51).
        ToggleAiPanel,
        /// Clears the active query tab's AI conversation (K51).
        AiNewConversation,
        /// Asks the AI assistant about the active tab's table, else the
        /// table selected in the tree (K53).
        AiAskAboutTable,
        /// Asks the AI assistant to explain that table (K53).
        AiExplainTable,
        /// Asks the AI assistant for a query on that table (K53).
        AiWriteQueryOnTable,
        /// Asks the AI assistant for test data for that table (K53).
        AiGenerateTestData,
        /// Asks the AI assistant to explain the selected columns (K53).
        AiExplainColumns,
        /// Asks the AI assistant to explain the result on screen (K53).
        AiExplainResult,
        /// Asks the AI assistant about the selected cells (K53).
        AiAskAboutSelection,
        /// Asks the AI assistant at the editor's cursor; its proposal comes as a diff (K52).
        AiAskHere,
        /// Asks the AI assistant to fix the failed statement on screen, as a diff (K52).
        AiFixError,
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
        ["json", "file", "environments", "tasks", "settings"],
        ImportConnections
    ),
    command!(
        "import_from_dbeaver",
        "Import from DBeaver…",
        "Connections",
        [],
        [
            "dbeaver",
            "dbp",
            "project",
            "data-sources",
            "migrate",
            "scripts"
        ],
        ImportFromDbeaver
    ),
    command!(
        "export_connections",
        "Export Connections…",
        "Connections",
        [],
        ["json", "file", "backup", "share", "environments", "tasks"],
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
    // DataGrip's Synchronize (⌘⌥Y, Ctrl+Alt+Y): DBeaver has no key for it (K20).
    command!(
        "refresh_schema_tree",
        "Refresh Schema Tree",
        "Connections",
        ["secondary-alt-y"],
        ["reload", "navigator", "objects"],
        RefreshSchemaTree
    ),
    // VS Code's ⌘← (Ctrl+Left elsewhere) on its lists: a key of the tree, so
    // the text fields keep it for the start of the line (K44).
    command!(
        "collapse_schema_tree",
        "Collapse Schema Tree",
        "Navigator",
        ["secondary-left"],
        ["close", "navigator", "objects"],
        CollapseSchemaTree,
        "Navigator"
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
        "new_connection_in_folder",
        "New Connection in Selected Folder…",
        "Connections",
        [],
        ["add", "group", "directory"],
        NewConnectionInFolder
    ),
    command!(
        "new_subfolder",
        "New Folder in Selected Folder…",
        "Connections",
        [],
        ["group", "directory", "organize"],
        NewSubfolder
    ),
    command!(
        "disconnect_folder",
        "Disconnect Selected Folder",
        "Connections",
        [],
        ["close", "group", "directory"],
        DisconnectFolder
    ),
    command!(
        "paste_connection",
        "Paste Copied Connection",
        "Connections",
        [],
        ["duplicate", "clone", "copy"],
        PasteConnection
    ),
    command!(
        "copy_connection_url",
        "Copy URL of Selected Connection",
        "Connections",
        [],
        ["address", "link", "clipboard"],
        CopyConnectionUrl
    ),
    command!(
        "copy_connection_host",
        "Copy Host of Selected Connection",
        "Connections",
        [],
        ["server", "address", "clipboard"],
        CopyConnectionHost
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
        "open_sql_file_on_connection",
        "Open SQL File on Selected Connection…",
        "Files",
        [],
        ["load", "script", "here"],
        OpenSqlFileOnConnection
    ),
    command!(
        "connection_scripts",
        "SQL Scripts of Selected Connection…",
        "Files",
        [],
        ["recent", "files", "saved queries", "open", "script"],
        ConnectionScripts
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
        "close_other_result_tabs",
        "Close Other Result Tabs",
        "Query",
        [],
        ["result tab"],
        CloseOtherResultTabs
    ),
    command!(
        "close_result_tabs_to_the_left",
        "Close Result Tabs to the Left",
        "Query",
        [],
        ["result tab"],
        CloseResultTabsToTheLeft
    ),
    command!(
        "close_result_tabs_to_the_right",
        "Close Result Tabs to the Right",
        "Query",
        [],
        ["result tab"],
        CloseResultTabsToTheRight
    ),
    command!(
        "close_all_result_tabs",
        "Close All Result Tabs",
        "Query",
        [],
        ["result tab"],
        CloseAllResultTabs
    ),
    command!(
        "unpin_all_result_tabs",
        "Unpin All Result Tabs",
        "Query",
        [],
        ["result tab", "pinned"],
        UnpinAllResultTabs
    ),
    command!(
        "rename_result_tab",
        "Rename Result Tab…",
        "Query",
        [],
        ["result tab", "title"],
        RenameResultTab
    ),
    command!(
        "copy_result_statement",
        "Copy Statement of Result Tab",
        "Query",
        [],
        ["result tab", "sql", "query"],
        CopyResultStatement
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
    // report as `+`): a query result's next rows, or a table's (K36).
    command!(
        "fetch_more_rows",
        "Fetch More Rows",
        "Query",
        ["ctrl-alt-n"],
        ["next rows", "load more", "results", "table data"],
        FetchMoreRows
    ),
    command!(
        "fetch_all_rows",
        "Fetch All Rows",
        "Query",
        ["ctrl-+"],
        ["every row", "load all", "results", "table data"],
        FetchAllRows
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
    // DataGrip's Set NULL (⌘⌥N); DBeaver has none (K20).
    command!(
        "set_null",
        "Set Selected Cells to NULL",
        "Data",
        ["cmd-alt-n"],
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
    // What a foreign key's arrow and ⌘-click do (K49, D-118): ⌘↓ (Ctrl+Down
    // elsewhere) opens what is selected, as in the Finder. Not DBeaver's
    // Alt+Space: ⌥Space types a character in a cell being edited.
    command!(
        "follow_foreign_key",
        "Open the Rows a Foreign Key Points To",
        "Data",
        ["secondary-down"],
        ["navigate link", "reference", "jump", "foreign key", "go to"],
        FollowForeignKey,
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
        "copy_column_name",
        "Copy Column Name",
        "Data",
        [],
        ["header", "clipboard"],
        CopyColumnName
    ),
    // DataGrip's Revert Selected (⌘⌥Z) (K20).
    command!(
        "revert_selected_rows",
        "Revert Selected Rows",
        "Data",
        ["cmd-alt-z"],
        ["undo", "pending", "discard"],
        RevertSelectedRows
    ),
    command!(
        "show_all_columns",
        "Show All Columns",
        "Data",
        [],
        ["hidden", "unhide"],
        ShowAllColumns
    ),
    command!(
        "fit_all_column_widths",
        "Fit All Column Widths",
        "Data",
        [],
        ["resize", "autosize", "width"],
        FitAllColumnWidths
    ),
    command!(
        "generate_select_by_key",
        "Generate SELECT by Key for Selected Rows",
        "Data",
        [],
        ["sql", "rows"],
        GenerateSelectByKey
    ),
    command!(
        "generate_select_in",
        "Generate SELECT … IN for Selected Rows",
        "Data",
        [],
        ["sql", "rows"],
        GenerateSelectIn
    ),
    command!(
        "generate_row_insert",
        "Generate INSERT for Selected Rows",
        "Data",
        [],
        ["sql", "rows"],
        GenerateRowInsert
    ),
    command!(
        "generate_row_update",
        "Generate UPDATE for Selected Rows",
        "Data",
        [],
        ["sql", "rows"],
        GenerateRowUpdate
    ),
    command!(
        "generate_row_delete",
        "Generate DELETE for Selected Rows",
        "Data",
        [],
        ["sql", "rows"],
        GenerateRowDelete
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
    // F7 is DBeaver's key for its panels (`resultset.grid.togglePreview`).
    // On a Mac keyboard F7 is a media key unless Fn is held: Ctrl+Alt+V is
    // the key that works as is, and the one menus and tooltips show.
    command!(
        "toggle_value_panel",
        "Toggle Value Panel",
        "Data",
        ["f7", "ctrl-alt-v"],
        [
            "value", "viewer", "json", "preview", "cell", "panels", "editor"
        ],
        ToggleValuePanel
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
    // K19: the tab area split in groups (VS Code's and DBeaver's split
    // editors). The keys are letters with ⌘⌥⇧ (Ctrl+Alt+Shift on Linux):
    // L right, I down (taller than wide), M move, N and P next and
    // previous group.
    command!(
        "split_right",
        "Split Right",
        "Tabs",
        ["secondary-alt-shift-l"],
        ["side by side", "split view", "group", "pane", "editor"],
        SplitRight
    ),
    command!(
        "split_down",
        "Split Down",
        "Tabs",
        ["secondary-alt-shift-i"],
        ["below", "split view", "group", "pane", "editor"],
        SplitDown
    ),
    command!(
        "move_tab_to_next_group",
        "Move Tab to the Next Group",
        "Tabs",
        ["secondary-alt-shift-m"],
        ["other group", "split view", "pane"],
        MoveTabToNextGroup
    ),
    command!(
        "move_tab_to_previous_group",
        "Move Tab to the Previous Group",
        "Tabs",
        [],
        ["other group", "split view", "pane"],
        MoveTabToPreviousGroup
    ),
    command!(
        "focus_next_group",
        "Focus Next Group",
        "Tabs",
        ["secondary-alt-shift-n"],
        ["split view", "pane", "switch group"],
        FocusNextGroup
    ),
    command!(
        "focus_previous_group",
        "Focus Previous Group",
        "Tabs",
        ["secondary-alt-shift-p"],
        ["split view", "pane", "switch group"],
        FocusPreviousGroup
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
    command!(
        "apply_source",
        "Apply Source…",
        "Editor",
        [],
        ["view", "routine", "function", "procedure", "definition"],
        ApplySource
    ),
    // DBeaver's Ctrl+Alt+Shift+W (`word.wrap`).
    // J19 (DBeaver #37897, #8219; Beekeeper #317): off by default.
    command!(
        "toggle_vim_mode",
        "Toggle Vim Mode",
        "Editor",
        [],
        ["vim", "modal", "normal mode", "insert mode", "keybindings"],
        ToggleVimMode
    ),
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
        "run_script_on_connections",
        "Run Script on Connections…",
        "Query",
        [],
        ["several", "multiple", "servers", "connections", "execute"],
        RunScriptOnConnections
    ),
    command!(
        "show_multi_run",
        "Show Last Run on Connections",
        "Query",
        [],
        ["several", "multiple", "summary", "servers"],
        ShowMultiRun
    ),
    // DBeaver's `ui.editors.sql.show.output` (Ctrl+Shift+O) (K20).
    command!(
        "show_messages",
        "Show Messages of the Last Run",
        "Query",
        ["ctrl-shift-o"],
        ["notices", "warnings", "server", "output"],
        ShowMessages
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
    // J17 (DBeaver #36951, #20072): off until turned on in Settings → AI.
    command!(
        "ai_write_sql",
        "AI: Write SQL…",
        "AI assistant",
        [],
        ["assistant", "llm", "generate", "natural language", "ollama"],
        AiWriteSql
    ),
    command!(
        "ai_explain_query",
        "AI: Explain Query",
        "AI assistant",
        [],
        ["assistant", "llm", "describe", "ollama"],
        AiExplainQuery
    ),
    command!(
        "ai_explain_plan",
        "AI: Explain Plan",
        "AI assistant",
        [],
        ["assistant", "llm", "explain", "slow", "ollama"],
        AiExplainPlan
    ),
    command!(
        "ai_explain_error",
        "AI: Explain Error",
        "AI assistant",
        [],
        ["assistant", "llm", "fix", "ollama"],
        AiExplainError
    ),
    // K51: the conversation panel at the right of the window.
    command!(
        "toggle_ai_panel",
        "AI: Show or Hide the Assistant",
        "AI assistant",
        ["secondary-shift-j"],
        [
            "assistant",
            "llm",
            "chat",
            "conversation",
            "panel",
            "ollama"
        ],
        ToggleAiPanel
    ),
    command!(
        "ai_new_conversation",
        "AI: New Conversation",
        "AI assistant",
        [],
        ["assistant", "llm", "chat", "clear", "reset"],
        AiNewConversation
    ),
    // K53: what the AI ▸ menus ask, on the active tab's table or result.
    command!(
        "ai_ask_about_table",
        "AI: Ask About This Table…",
        "AI assistant",
        [],
        ["assistant", "llm", "chat", "question", "table", "view"],
        AiAskAboutTable
    ),
    command!(
        "ai_explain_table",
        "AI: Explain This Table",
        "AI assistant",
        [],
        ["assistant", "llm", "describe", "table", "relations", "keys"],
        AiExplainTable
    ),
    command!(
        "ai_write_query_on_table",
        "AI: Write a Query on This Table…",
        "AI assistant",
        [],
        ["assistant", "llm", "sql", "select", "table"],
        AiWriteQueryOnTable
    ),
    command!(
        "ai_generate_test_data",
        "AI: Generate Test Data…",
        "AI assistant",
        [],
        [
            "assistant",
            "llm",
            "insert",
            "sample",
            "fake",
            "rows",
            "seed"
        ],
        AiGenerateTestData
    ),
    command!(
        "ai_explain_columns",
        "AI: Explain These Columns",
        "AI assistant",
        [],
        ["assistant", "llm", "describe", "column", "fields"],
        AiExplainColumns
    ),
    command!(
        "ai_explain_result",
        "AI: Explain This Result",
        "AI assistant",
        [],
        ["assistant", "llm", "describe", "result", "rows", "grid"],
        AiExplainResult
    ),
    command!(
        "ai_ask_about_selection",
        "AI: Ask About the Selection…",
        "AI assistant",
        [],
        [
            "assistant",
            "llm",
            "question",
            "cells",
            "selected",
            "values"
        ],
        AiAskAboutSelection
    ),
    // K52: a field at the cursor, the proposal as a diff to accept or reject.
    command!(
        "ai_ask_here",
        "AI: Ask Here…",
        "AI assistant",
        ["secondary-shift-h"],
        [
            "assistant",
            "llm",
            "inline",
            "edit",
            "change",
            "diff",
            "ollama"
        ],
        AiAskHere
    ),
    command!(
        "ai_fix_error",
        "AI: Fix Error",
        "AI assistant",
        [],
        ["assistant", "llm", "fix", "correct", "diff", "ollama"],
        AiFixError
    ),
    command!(
        "close_tab",
        "Close Tab",
        "Tabs",
        ["secondary-w"],
        [],
        CloseTab
    ),
    // Safari's Close Other Tabs (⌥⌘W) (K20).
    command!(
        "close_other_tabs",
        "Close Other Tabs",
        "Tabs",
        ["secondary-alt-w"],
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
    // Browsers' Reopen Closed Tab (⌘⇧T); Ctrl+Shift+T is the query tab's
    // Maximize Results Panel, so not `secondary` (K20).
    command!(
        "reopen_closed_tab",
        "Reopen Closed Tab",
        "Tabs",
        ["cmd-shift-t"],
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
        // DBeaver's other keys, Ctrl+Alt+Shift+K and R: digits sit behind
        // Shift on an AZERTY keyboard (K20). The last key is the one menus
        // and tooltips show: Ctrl+4 stays it.
        ["ctrl-alt-shift-k", "ctrl-4"],
        ["end", "save"],
        CommitTransaction
    ),
    command!(
        "rollback_transaction",
        "Roll Back Transaction",
        "Query",
        ["ctrl-alt-shift-r", "ctrl-8"],
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
    // DBeaver's `ui.dashboard.open` (Ctrl+Alt+Shift+B) (K20).
    command!(
        "open_admin",
        "Server Administration",
        "Server",
        ["ctrl-alt-shift-b"],
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
    // DBeaver's data transfer, database to database (DBeaver #17767).
    command!(
        "copy_tables",
        "Copy Tables to Another Connection…",
        "Server",
        [],
        [
            "data transfer",
            "migrate",
            "copy data",
            "move tables",
            "export to database"
        ],
        CopyTables
    ),
    command!(
        "compare_schemas",
        "Compare Schemas…",
        "Server",
        [],
        [
            "schema compare",
            "diff",
            "migration script",
            "synchronize",
            "compare databases"
        ],
        CompareSchemas
    ),
    command!(
        "export_schema",
        "Export Schema…",
        "Server",
        [],
        [
            "schema document",
            "data dictionary",
            "dbml",
            "markdown",
            "html",
            "documentation",
            "dbdiagram"
        ],
        ExportSchema
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
    // DBeaver's "View Diagram" (`ui.editors.erd`), J18.
    // DataGrip's Show Diagram (⌥⇧⌘U) for the key (K20).
    command!(
        "view_diagram",
        "View ER Diagram",
        "Navigator",
        ["cmd-alt-shift-u"],
        [
            "erd",
            "entity relationship",
            "schema",
            "foreign keys",
            "mermaid"
        ],
        ViewDiagram
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
    command!(
        "open_object_data",
        "Open Data of Selected Object",
        "Navigator",
        [],
        ["rows", "table", "view", "browse"],
        OpenObjectData
    ),
    command!(
        "open_object_structure",
        "Open Structure of Selected Object",
        "Navigator",
        [],
        ["columns", "indexes", "keys", "constraints"],
        OpenObjectStructure
    ),
    command!(
        "show_object_ddl",
        "Show DDL of Selected Object",
        "Navigator",
        [],
        ["create", "definition", "script"],
        ShowObjectDdl
    ),
    command!(
        "edit_object_source",
        "Edit Source of Selected View or Routine",
        "Navigator",
        [],
        ["definition", "function", "procedure", "body"],
        EditObjectSource
    ),
    command!(
        "generate_select",
        "Generate SELECT for Selected Table",
        "Navigator",
        [],
        ["sql", "template", "query"],
        GenerateSelect
    ),
    command!(
        "generate_insert",
        "Generate INSERT for Selected Table",
        "Navigator",
        [],
        ["sql", "template"],
        GenerateInsert
    ),
    command!(
        "generate_update",
        "Generate UPDATE for Selected Table",
        "Navigator",
        [],
        ["sql", "template"],
        GenerateUpdate
    ),
    command!(
        "generate_delete",
        "Generate DELETE for Selected Table",
        "Navigator",
        [],
        ["sql", "template"],
        GenerateDelete
    ),
    command!(
        "drop_object_cascade",
        "Drop Selected Object With Cascade…",
        "Navigator",
        [],
        ["delete", "remove", "dependent"],
        DropObjectCascade
    ),
    command!(
        "create_table",
        "Create Table…",
        "Navigator",
        [],
        ["new", "schema"],
        CreateTable
    ),
    command!(
        "create_view",
        "Create View…",
        "Navigator",
        [],
        ["new", "schema"],
        CreateView
    ),
    command!(
        "create_schema",
        "Create Schema…",
        "Navigator",
        [],
        ["new", "namespace"],
        CreateSchema
    ),
    command!(
        "create_database",
        "Create Database…",
        "Navigator",
        [],
        ["new", "catalog"],
        CreateDatabase
    ),
    command!(
        "create_function",
        "Create Function…",
        "Navigator",
        [],
        ["new", "routine"],
        CreateFunction
    ),
    command!(
        "create_procedure",
        "Create Procedure…",
        "Navigator",
        [],
        ["new", "routine"],
        CreateProcedure
    ),
    command!(
        "create_sequence",
        "Create Sequence…",
        "Navigator",
        [],
        ["new", "serial", "identity"],
        CreateSequence
    ),
    command!(
        "create_trigger",
        "Create Trigger…",
        "Navigator",
        [],
        ["new"],
        CreateTrigger
    ),
    command!(
        "create_event",
        "Create Event…",
        "Navigator",
        [],
        ["new", "scheduler", "mysql"],
        CreateEvent
    ),
    command!(
        "create_enum",
        "Create Enum…",
        "Navigator",
        [],
        ["new", "type"],
        CreateEnum
    ),
    command!(
        "add_enum_value",
        "Add Value to Selected Enum…",
        "Navigator",
        [],
        ["type", "label"],
        AddEnumValue
    ),
    command!(
        "rename_enum_value",
        "Rename Selected Enum Value…",
        "Navigator",
        [],
        ["type", "label"],
        RenameEnumValue
    ),
    command!(
        // K27: the id of the old "Install Selected Extension…", kept so that a
        // keymap override naming it still binds.
        "install_extension",
        "Manage Extensions…",
        "Navigator",
        [],
        [
            "install extension",
            "create extension",
            "update extension",
            "drop extension",
            "postgresql"
        ],
        ManageExtensions
    ),
    command!(
        "export_object",
        "Export Selected Table…",
        "Navigator",
        [],
        ["csv", "json", "xlsx", "sql", "file"],
        ExportObject
    ),
    command!(
        "import_csv_into_object",
        "Import CSV Into Selected Table…",
        "Navigator",
        [],
        ["file", "load", "rows"],
        ImportCsvIntoObject
    ),
    command!(
        "rename_column",
        "Rename Selected Column…",
        "Navigator",
        [],
        ["alter", "name"],
        RenameColumn
    ),
    command!(
        "drop_column",
        "Drop Selected Column…",
        "Navigator",
        [],
        ["alter", "remove", "delete"],
        DropColumn
    ),
    command!(
        "reveal_referenced_table",
        "Show Referenced Table in Tree",
        "Navigator",
        [],
        ["foreign key", "navigate"],
        RevealReferencedTable
    ),
    // Safari's and Chrome's ⌥⌘→, Linux browsers' Ctrl+PageDown (K20), before
    // Ctrl+Tab, which menus show (the last key is the one shown).
    command!(
        "next_tab",
        "Next Tab",
        "Tabs",
        ["cmd-alt-right", "ctrl-pagedown", "ctrl-tab"],
        [],
        NextTab
    ),
    command!(
        "previous_tab",
        "Previous Tab",
        "Tabs",
        ["cmd-alt-left", "ctrl-pageup", "ctrl-shift-tab"],
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
    // The owner's request of 2026-11-20 (K20): ⌘⇧D / Ctrl+Shift+D, free in
    // Kerno, the kit's inputs and macOS; D for dark.
    command!(
        "toggle_theme",
        "Theme: Switch Light and Dark",
        "View",
        ["secondary-shift-d"],
        [
            "appearance",
            "toggle",
            "dark mode",
            "light mode",
            "night",
            "day"
        ],
        ToggleTheme
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
    // ⌘F while the settings have the keyboard; from the palette or the menu
    // bar, it opens them there.
    command!(
        "search_settings",
        "Find a Setting…",
        "Kerno",
        ["secondary-f"],
        ["preferences", "options", "search", "settings"],
        SearchSettings,
        "Settings"
    ),
    command!(
        "open_keyboard_shortcuts",
        "Keyboard Shortcuts…",
        "Kerno",
        [],
        [
            "shortcuts",
            "keys",
            "bindings",
            "keyboard",
            "keymap",
            "hotkeys"
        ],
        OpenKeyboardShortcuts
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
pub(crate) const HOST_OS: OperatingSystem = if cfg!(target_os = "macos") {
    OperatingSystem::Mac
} else {
    OperatingSystem::Linux
};

/// Marks every binding Kerno adds, so a new keymap replaces them while the
/// kit's own (text inputs, lists, dialogs) stay where they are. The kit's
/// `NoAction` bindings carry no meta (0) and out-rank any meta, as they did
/// when Kerno's bindings had none (D-089).
const KERNO_BINDING: KeyBindingMetaIndex = KeyBindingMetaIndex(0x4b45);

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

/// The entries of a keymap file, per command id: its shortcuts, `None` for
/// `null` (no shortcut).
pub type Overrides = BTreeMap<String, Option<Vec<String>>>;

/// The entries of the keymap file `text` (empty: none); an error when it is
/// not a keymap file.
pub fn overrides(text: &str) -> Result<Overrides, String> {
    if text.trim().is_empty() {
        return Ok(Overrides::new());
    }
    let file = serde_json::from_str::<KeymapFile>(text)
        .map_err(|error| format!("the keymap file is not valid JSON: {error}"))?;
    Ok(file
        .bindings
        .into_iter()
        .map(|(id, keys)| {
            let keys = keys.map(|keys| match keys {
                KeysEntry::One(key) => vec![key],
                KeysEntry::Many(keys) => keys,
            });
            (id, keys)
        })
        .collect())
}

/// The shortcuts of `command`: its entry in `overrides`, else its defaults.
pub fn keys_in_effect(command: &Command, overrides: &Overrides) -> Vec<String> {
    match overrides.get(command.id) {
        None => command.keys.iter().map(|key| key.to_string()).collect(),
        Some(keys) => keys.clone().unwrap_or_default(),
    }
}

/// The shortcuts in effect: defaults, replaced per command by the user's
/// overrides. Returns `(shortcut, command id)` pairs and what was wrong
/// with the overrides, if anything.
fn effective_bindings(text: &str) -> (Vec<(String, &'static str)>, Vec<String>) {
    let mut problems = Vec::new();
    let overrides = overrides(text).unwrap_or_else(|problem| {
        problems.push(problem);
        Overrides::new()
    });
    for id in overrides.keys() {
        if !COMMANDS.iter().any(|command| command.id == id) {
            problems.push(format!("unknown command \"{id}\""));
        }
    }

    let mut bindings: Vec<(String, &'static str)> = Vec::new();
    for command in COMMANDS {
        for key in keys_in_effect(command, &overrides) {
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

    // One keystroke for two commands that can both answer it: one of them
    // would silently win.
    for (index, (key, id)) in bindings.iter().enumerate() {
        if let Some((_, other)) = bindings[..index].iter().find(|(other_key, other)| {
            other != id
                && contexts_overlap(context_of(id), context_of(other))
                && same_keys(key, other_key, HOST_OS)
        }) {
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

/// Key contexts drawn inside another: a results grid (`DataTable`,
/// `query/grid_view.rs`) and its find bar (`GridFind`, `query/find.rs`) sit
/// in a query tab (`QueryTab`), the find bar beside the grid.
const NESTED: &[(&str, &str)] = &[
    ("QueryTab", "DataTable"),
    ("QueryTab", "GridFind"),
    ("DataTable", "GridFind"),
];

/// Whether the keys of two contexts can both apply where the keyboard is: a
/// key with no context works everywhere; `QueryTab > mode == editor` is in
/// `QueryTab`; the schema tree, the settings and a grid are apart.
pub(crate) fn contexts_overlap(a: Option<&str>, b: Option<&str>) -> bool {
    fn root(context: &str) -> &str {
        context.split_whitespace().next().unwrap_or(context)
    }
    match (a.map(root), b.map(root)) {
        (None, _) | (_, None) => true,
        (Some(a), Some(b)) => {
            a == b || NESTED.iter().any(|pair| *pair == (a, b) || *pair == (b, a))
        }
    }
}

/// Whether two shortcuts are the same keystrokes on `os` (`secondary-k` is
/// `cmd-k` on macOS, `ctrl-k` elsewhere).
pub(crate) fn same_keys(a: &str, b: &str, os: OperatingSystem) -> bool {
    let resolved = |key: &str| -> Option<Vec<String>> {
        key.split_whitespace()
            .map(|stroke| {
                Keystroke::parse_with_os(stroke, os)
                    .ok()
                    .map(|stroke| stroke.unparse(os))
            })
            .collect()
    };
    matches!((resolved(a), resolved(b)), (Some(a), Some(b)) if a == b)
}

/// The commands that already answer `key` where `id` would: the same
/// keystrokes on `os`, in key contexts that can both apply. Two commands of
/// disjoint contexts (F4 in the tree and in the editor) may share a key.
pub fn conflicts(
    id: &str,
    key: &str,
    overrides: &Overrides,
    os: OperatingSystem,
) -> Vec<&'static Command> {
    let context = context_of(id);
    COMMANDS
        .iter()
        .filter(|command| command.id != id && contexts_overlap(context, command.context))
        .filter(|command| {
            keys_in_effect(command, overrides)
                .iter()
                .any(|other| same_keys(other, key, os))
        })
        .collect()
}

/// Whether `stroke` is a keystroke Kerno will bind: modifiers followed by
/// one character or a named key. GPUI's parser accepts any word as a key
/// name, so a typo such as `secondary-entr` would otherwise be bound to a
/// key that does not exist, silently.
fn is_shortcut(stroke: &str) -> bool {
    let Ok(parsed) = Keystroke::parse_with_os(stroke, HOST_OS) else {
        return false;
    };
    let key = parsed.key.as_str();
    key.chars().count() == 1 || NAMED_KEYS.contains(&key) || is_function_key(key)
}

/// The keys bound by name besides F1 to F24.
const NAMED_KEYS: &[&str] = &[
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

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=24).contains(&number))
}

/// The modifiers that make a shortcut of a key that types, said on the page.
pub const RECORD_RULE: &str = if cfg!(target_os = "macos") {
    "A shortcut needs ⌘ or ⌃ with a letter, a digit or a sign (⌥ or ⇧ alone would type a \
     character); ⌥ is enough with an arrow, Enter, Tab or another named key; F1 to F24 work \
     alone. A key the text fields use (⌘C, ⌘Z, ⌥←…) is kept for them. Escape cancels."
} else {
    "A shortcut needs Ctrl or Super with a letter, a digit or a sign (Alt or Shift alone would \
     type a character); Alt is enough with an arrow, Enter, Tab or another named key; F1 to F24 \
     work alone. A key the text fields use (Ctrl+C, Ctrl+Z, Ctrl+Left…) is kept for them. Escape \
     cancels."
};

/// The kit's own binding of `key` (a text field's, the editor's, a list's or
/// a dialog's), by its action's name. A command bound everywhere would take
/// that key from them: GPUI ranks a binding with no key context with the
/// deepest context, and Kerno binds after the kit, so its key wins in every
/// field (D-089).
pub fn kit_binding_of(key: &str, cx: &App) -> Option<String> {
    let strokes = key
        .split_whitespace()
        .map(|stroke| Keystroke::parse_with_os(stroke, HOST_OS))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    keymap
        .bindings()
        .filter(|binding| binding.meta() != Some(KERNO_BINDING))
        .filter(|binding| !binding.action().as_any().is::<gpui_kit::NoAction>())
        .find(|binding| binding.match_keystrokes(&strokes) == Some(false))
        .map(|binding| {
            let name = binding.action().name();
            name.rsplit("::").next().unwrap_or(name).to_string()
        })
}

/// The kit's binding of `key` that a command with no key context must leave
/// alone; `None` when the kit has none, or when a default with no context
/// already wins over it (⌘↵ runs the statement, ⌘. cancels it: the fields
/// lost those keys already, so another command may take them).
pub fn kept_for_the_kit(key: &str, cx: &App) -> Option<String> {
    let taken = COMMANDS
        .iter()
        .filter(|command| command.context.is_none())
        .flat_map(|command| command.keys)
        .any(|default| same_keys(default, key, HOST_OS));
    if taken {
        return None;
    }
    kit_binding_of(key, cx)
}

/// The keymap file's spelling of a keystroke pressed on `os`, which reads
/// back as the same keystroke: Cmd on macOS and Ctrl elsewhere are written
/// `secondary-`, as the defaults are. Fn is left out: macOS sets it on the
/// arrows and the F keys, which the defaults bind without it.
pub fn file_form(stroke: &Keystroke, os: OperatingSystem) -> String {
    let modifiers = stroke.modifiers;
    // `secondary-` first, as the defaults write it.
    let (secondary, other, other_name) = if os == OperatingSystem::Mac {
        (modifiers.platform, modifiers.control, "ctrl-")
    } else {
        (modifiers.control, modifiers.platform, "cmd-")
    };
    let mut form = String::new();
    for (on, name) in [
        (secondary, "secondary-"),
        (other, other_name),
        (modifiers.alt, "alt-"),
        (modifiers.shift, "shift-"),
    ] {
        if on {
            form.push_str(name);
        }
    }
    form.push_str(&stroke.key);
    form
}

/// The shortcut a keystroke pressed on the page records for a command of
/// `context` (`None`: everywhere), in the file's spelling; why not, when it
/// would eat typing: a key that types text needs Cmd or Ctrl; a named key
/// (Enter, an arrow) needs a modifier unless the command's keys work only in
/// a part of the window that takes no text; F keys always work.
pub fn recorded_key(
    stroke: &Keystroke,
    context: Option<&str>,
    os: OperatingSystem,
) -> Result<String, String> {
    let modifiers = stroke.modifiers;
    let key = stroke.key.as_str();
    let shown = shown(&file_form(stroke, os));
    let primary = modifiers.platform || modifiers.control;
    if is_function_key(key) {
        // Fine alone.
    } else if NAMED_KEYS.contains(&key) {
        if context.is_none() && !(primary || modifiers.alt) {
            return Err(format!(
                "{shown} alone moves or edits text in the editor and the fields: hold a modifier with it."
            ));
        }
    } else if key.chars().count() == 1 {
        if !primary {
            return Err(format!(
                "{shown} types a character: hold {} with it.",
                if os == OperatingSystem::Mac {
                    "⌘ or ⌃"
                } else {
                    "Ctrl or Super"
                }
            ));
        }
    } else {
        return Err(format!("{shown} is not a key Kerno can bind."));
    }
    Ok(file_form(stroke, os))
}

/// A shortcut as the tooltips and the palette write it: ⌘⇧K on macOS,
/// Ctrl+Shift+K elsewhere; a sequence with spaces.
pub fn shown(key: &str) -> String {
    key.split_whitespace()
        .map(|stroke| match Keystroke::parse_with_os(stroke, HOST_OS) {
            Ok(stroke) => gpui_kit::component::kbd::Kbd::format(&stroke),
            Err(_) => stroke.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The lowercase words of `text`: runs of letters and digits.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// The words of a search for a shortcut: ⌘, Cmd, Command, Super and Win
/// are `cmd`; ⌃, Ctrl and Control `ctrl`; ⌥, Alt, Opt and Option `alt`; ⇧
/// `shift`; `secondary` is the system's; `+` and `-` separate.
pub fn query_words(query: &str, os: OperatingSystem) -> Vec<String> {
    let mut spaced = String::new();
    for c in query.chars() {
        match c {
            '⌘' => spaced.push_str(" cmd "),
            '⌃' => spaced.push_str(" ctrl "),
            '⌥' => spaced.push_str(" alt "),
            '⇧' => spaced.push_str(" shift "),
            '+' | '-' => spaced.push(' '),
            c => spaced.push(c),
        }
    }
    let secondary = if os == OperatingSystem::Mac {
        "cmd"
    } else {
        "ctrl"
    };
    spaced
        .split_whitespace()
        .map(|word| {
            let word = word.to_lowercase();
            match word.as_str() {
                "command" | "super" | "win" => "cmd".to_string(),
                "control" => "ctrl".to_string(),
                "option" | "opt" => "alt".to_string(),
                "secondary" => secondary.to_string(),
                _ => word,
            }
        })
        .collect()
}

/// The words a shortcut is found by: its modifiers (`ctrl`, `alt`, `shift`,
/// `cmd`) and its key, keystroke after keystroke.
fn key_words(key: &str, os: OperatingSystem) -> Vec<String> {
    let mut found = Vec::new();
    for stroke in key.split_whitespace() {
        let Ok(stroke) = Keystroke::parse_with_os(stroke, os) else {
            continue;
        };
        let modifiers = stroke.modifiers;
        for (on, name) in [
            (modifiers.control, "ctrl"),
            (modifiers.alt, "alt"),
            (modifiers.shift, "shift"),
            (modifiers.platform, "cmd"),
        ] {
            if on {
                found.push(name.to_string());
            }
        }
        found.push(stroke.key.to_lowercase());
    }
    found
}

/// Whether Settings ▸ Shortcuts lists `command` (with `keys` in effect) for
/// `query`: every word typed starts a word of its name, group, keywords or
/// id; or every word is in one of its shortcuts (`cmd k`, `⌘K`); or both,
/// the longer words naming it and the others in one shortcut
/// (`palette cmd`). A lone letter or a modifier is taken for a key there.
pub fn command_matches(
    command: &Command,
    keys: &[String],
    query: &str,
    os: OperatingSystem,
) -> bool {
    let terms = query_words(query, os);
    let names: Vec<String> = [command.label, command.group, command.id]
        .into_iter()
        .chain(command.keywords.iter().copied())
        .flat_map(words)
        .collect();
    let names_it = |term: &String| names.iter().any(|name| name.starts_with(term.as_str()));
    let in_one_key = |rest: &[&String]| {
        keys.iter().any(|key| {
            let found = key_words(key, os);
            rest.iter().all(|term| found.contains(term))
        })
    };
    if terms.iter().all(names_it) {
        return true;
    }
    let all: Vec<&String> = terms.iter().collect();
    if in_one_key(&all) {
        return true;
    }
    let keyish = |term: &String| {
        term.chars().count() == 1 || ["cmd", "ctrl", "alt", "shift"].contains(&term.as_str())
    };
    let rest: Vec<&String> = terms
        .iter()
        .filter(|term| keyish(term) || !names_it(term))
        .collect();
    in_one_key(&rest)
}

/// One change Settings ▸ Shortcuts makes to the keymap file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapChange {
    /// These shortcuts for the command; its entry goes when they are its
    /// defaults.
    Set(&'static str, Vec<String>),
    /// The command back to its default shortcuts: its entry goes.
    Reset(&'static str),
    /// Every command back to its defaults: `bindings` emptied.
    ResetAll,
}

/// The keymap file `text` with `change` made, everything else in it kept as
/// it was (other entries, `about`, keys Kerno does not know, their order);
/// no file (`None`) starts from the example. Refused when `text` is not a
/// keymap file: a file Kerno cannot read is never overwritten.
pub fn changed_keymap(text: Option<&str>, change: &KeymapChange) -> Result<String, String> {
    let base = match text {
        Some(text) if !text.trim().is_empty() => text.to_string(),
        _ => keymap_example(),
    };
    overrides(&base)?;
    let mut root: serde_json::Value = serde_json::from_str(&base)
        .map_err(|error| format!("the keymap file is not valid JSON: {error}"))?;
    let file = root
        .as_object_mut()
        .ok_or("the keymap file is not a JSON object")?;
    let bindings = file
        .entry("bindings")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("the keymap file's \"bindings\" is not an object")?;
    match change {
        KeymapChange::Set(id, keys) => {
            let defaults = COMMANDS
                .iter()
                .find(|command| command.id == *id)
                .map(|command| command.keys);
            if defaults.is_some_and(|defaults| *keys == defaults) {
                bindings.shift_remove(*id);
            } else {
                let value = match keys.as_slice() {
                    [] => serde_json::Value::Null,
                    [key] => serde_json::Value::String(key.clone()),
                    keys => serde_json::json!(keys),
                };
                bindings.insert(id.to_string(), value);
            }
        }
        KeymapChange::Reset(id) => {
            bindings.shift_remove(*id);
        }
        KeymapChange::ResetAll => bindings.clear(),
    }
    serde_json::to_string_pretty(&root)
        .map(|text| text + "\n")
        .map_err(|error| error.to_string())
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
            "Example: \"new_query_tab\": \"secondary-alt-shift-t\", \"close_tab\": [\"secondary-w\", \"ctrl-f4\"], \"toggle_sidebar\": null",
            "Settings ▸ Shortcuts changes this file and applies at once; a change typed here applies when Kerno starts again or when Settings is opened.",
            "The commands and their default shortcuts are listed below."
        ],
        "bindings": {},
        "commands": commands,
    });
    serde_json::to_string_pretty(&example).unwrap_or_default() + "\n"
}

/// Binds every command's shortcut, applying the user's keymap file when it
/// exists. Returns what was wrong with that file, for the app to show.
pub fn bind_keys(keymap_file: &Path, cx: &mut App) -> Vec<String> {
    match std::fs::read_to_string(keymap_file) {
        Ok(text) => apply_keymap(&text, cx),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => apply_keymap("", cx),
        Err(error) => {
            apply_keymap("", cx);
            vec![format!("could not read {}: {error}", keymap_file.display())]
        }
    }
}

/// Replaces Kerno's key bindings with those of the keymap file's `text`, at
/// once and as often as needed: the kit's bindings stay, in their place, and
/// the menu bar is built again so its shortcut column follows. Returns what
/// was wrong with `text` (its valid entries apply, defaults elsewhere).
pub fn apply_keymap(text: &str, cx: &mut App) -> Vec<String> {
    let (bindings, problems) = effective_bindings(text);
    let kept: Vec<KeyBinding> = {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        keymap
            .bindings()
            .filter(|binding| binding.meta() != Some(KERNO_BINDING))
            .cloned()
            .collect()
    };
    cx.clear_key_bindings();
    cx.bind_keys(kept);
    bind(&bindings, cx);
    cx.set_menus(crate::menus());
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
            Ok(binding) => key_bindings.push(binding.with_meta(KERNO_BINDING)),
            // Already validated; a failure here is a shortcut this keyboard
            // layout cannot produce.
            Err(error) => tracing::warn!(%error, key, command = id, "shortcut not bound"),
        }
    }
    // Cmd+Enter in Open Table opens the structure (`quick_open.rs`): a key of
    // that dialog, as Enter and the arrows are the list's own.
    key_bindings.push(
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-enter"
            } else {
                "ctrl-enter"
            },
            gpui_kit::base::actions::Confirm { secondary: true },
            Some("QuickOpen"),
        )
        .with_meta(KERNO_BINDING),
    );
    cx.bind_keys(key_bindings);
}

/// The defaults K19 added: the split tab area's.
#[cfg(test)]
pub(crate) const K19_DEFAULTS: &[(&str, &str)] = &[
    ("split_right", "secondary-alt-shift-l"),
    ("split_down", "secondary-alt-shift-i"),
    ("move_tab_to_next_group", "secondary-alt-shift-m"),
    ("focus_next_group", "secondary-alt-shift-n"),
    ("focus_previous_group", "secondary-alt-shift-p"),
];

/// The defaults K20 added (DBeaver, DataGrip, macOS and browser
/// conventions; the source is named at each command).
#[cfg(test)]
pub(crate) const K20_DEFAULTS: &[(&str, &str)] = &[
    ("toggle_theme", "secondary-shift-d"),
    ("refresh_schema_tree", "secondary-alt-y"),
    ("set_null", "cmd-alt-n"),
    ("revert_selected_rows", "cmd-alt-z"),
    ("show_messages", "ctrl-shift-o"),
    ("close_other_tabs", "secondary-alt-w"),
    ("reopen_closed_tab", "cmd-shift-t"),
    ("open_admin", "ctrl-alt-shift-b"),
    ("view_diagram", "cmd-alt-shift-u"),
    ("commit_transaction", "ctrl-alt-shift-k"),
    ("rollback_transaction", "ctrl-alt-shift-r"),
    ("next_tab", "cmd-alt-right"),
    ("next_tab", "ctrl-pagedown"),
    ("previous_tab", "cmd-alt-left"),
    ("previous_tab", "ctrl-pageup"),
];

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
                "new_query_tab": "secondary-alt-shift-t",
                "close_tab": ["secondary-w", "ctrl-f4"],
                "toggle_sidebar": null,
                "about": "f1"
            }}"#,
        );
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(
            keys_of(&bindings, "new_query_tab"),
            ["secondary-alt-shift-t"]
        );
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
            ("commit_transaction", &["ctrl-alt-shift-k", "ctrl-4"][..]),
            ("rollback_transaction", &["ctrl-alt-shift-r", "ctrl-8"]),
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

    /// K20: the new defaults are bound, and none takes a key of the kit's
    /// text inputs and editor (`gpui-base`), of the system (macOS, the
    /// Linux desktops), of vim mode (Ctrl+R and plain characters), or one
    /// behind Shift or Option on an AZERTY keyboard (digits and signs).
    #[test]
    fn the_new_defaults_are_bound_and_take_no_reserved_key() {
        let (bindings, problems) = effective_bindings("");
        assert_eq!(problems, Vec::<String>::new());
        // K19's split keys follow the same rule.
        let defaults = || K20_DEFAULTS.iter().chain(K19_DEFAULTS);
        for (id, key) in defaults() {
            assert!(
                keys_of(&bindings, id).iter().any(|each| each == key),
                "{id} {key}"
            );
        }
        const RESERVED: &[&str] = &[
            // The kit's inputs and editor.
            "cmd-shift-z",
            "cmd-shift-f",
            "cmd-shift-up",
            "cmd-shift-down",
            "cmd-alt-i",
            "cmd-alt-up",
            "cmd-alt-down",
            "ctrl-cmd-space",
            "shift-cmd-right",
            "shift-cmd-left",
            "ctrl-shift-i",
            "ctrl-shift-e",
            "ctrl-shift-a",
            "ctrl-alt-up",
            "ctrl-alt-down",
            "ctrl-o",
            "ctrl-g",
            "ctrl-h",
            "ctrl-e",
            "ctrl-a",
            "ctrl-f",
            "ctrl-y",
            "ctrl-z",
            "alt-left",
            "alt-right",
            "cmd-left",
            "cmd-right",
            "ctrl-home",
            "ctrl-end",
            // macOS.
            "cmd-h",
            "cmd-m",
            "cmd-q",
            "cmd-tab",
            "cmd-space",
            "cmd-alt-d",
            "cmd-alt-h",
            "cmd-alt-m",
            "cmd-alt-escape",
            "cmd-shift-q",
            "ctrl-cmd-q",
            "ctrl-cmd-f",
            "cmd-shift-3",
            "cmd-shift-4",
            "cmd-shift-5",
            "cmd-shift-/",
            "ctrl-up",
            "ctrl-down",
            "ctrl-left",
            "ctrl-right",
            "ctrl-f2",
            "ctrl-f3",
            "ctrl-f4",
            "ctrl-f5",
            "ctrl-f6",
            "ctrl-f7",
            "ctrl-f8",
            // Linux desktops.
            "ctrl-alt-t",
            "ctrl-alt-l",
            "ctrl-alt-d",
            "ctrl-alt-delete",
            "ctrl-alt-left",
            "ctrl-alt-right",
            "alt-tab",
            "alt-f4",
            // Vim mode.
            "ctrl-r",
        ];
        for os in [OperatingSystem::Mac, OperatingSystem::Linux] {
            for (id, key) in defaults() {
                for reserved in RESERVED {
                    assert!(
                        !same_keys(key, reserved, os),
                        "{os:?}: {id} takes {reserved}"
                    );
                }
                let stroke = Keystroke::parse_with_os(key, os).unwrap();
                let modifiers = stroke.modifiers;
                assert!(
                    modifiers.platform || modifiers.control,
                    "{id}: {key} needs Cmd or Ctrl"
                );
                assert!(
                    stroke.key.chars().all(|c| c.is_ascii_lowercase())
                        && (stroke.key.len() == 1 || NAMED_KEYS.contains(&stroke.key.as_str())),
                    "{id}: {key} is not a letter or a named key"
                );
            }
        }
    }

    /// K20: under the conflict rule of Settings ▸ Shortcuts, no two default
    /// keys answer the same keystroke where both apply, on either system.
    #[test]
    fn no_two_defaults_conflict_on_either_system() {
        let (bindings, _) = effective_bindings("");
        for os in [OperatingSystem::Mac, OperatingSystem::Linux] {
            for (index, (key, id)) in bindings.iter().enumerate() {
                for (other_key, other) in &bindings[..index] {
                    assert!(
                        other == id
                            || !contexts_overlap(context_of(id), context_of(other))
                            || !same_keys(key, other_key, os),
                        "{os:?}: {key} of {id} and {other_key} of {other}"
                    );
                }
            }
        }
    }

    /// K20: a conflict is a keystroke two commands would both answer: same
    /// keystroke on the system, contexts that can both apply. F4 of the tree
    /// and F4 of the editor are not one.
    #[test]
    fn conflicts_follow_the_keystroke_and_the_contexts() {
        let none = Overrides::new();
        let ids = |found: Vec<&'static Command>| -> Vec<&str> {
            found.into_iter().map(|command| command.id).collect()
        };
        let mac = OperatingSystem::Mac;
        // A global key and another global command.
        assert_eq!(
            ids(conflicts("about", "secondary-b", &none, mac)),
            ["toggle_sidebar"]
        );
        // Spelt differently, same keystroke on macOS only.
        assert_eq!(
            ids(conflicts("about", "cmd-b", &none, mac)),
            ["toggle_sidebar"]
        );
        assert!(conflicts("about", "cmd-b", &none, OperatingSystem::Linux).is_empty());
        // The command's own key is not a conflict.
        assert!(conflicts("toggle_sidebar", "secondary-b", &none, mac).is_empty());
        // Disjoint contexts: the tree's F4 is not the editor's.
        assert!(conflicts("edit_object", "f4", &none, mac).is_empty());
        // A global key would be shadowed in the tree, the list and the editor.
        assert_eq!(ids(conflicts("about", "f4", &none, mac)).len(), 3);
        assert_eq!(
            ids(conflicts("rename_object", "f4", &none, mac)),
            ["edit_object"]
        );
        // A grid of a query tab answers the query tab's keys too.
        assert_eq!(
            ids(conflicts("set_default", "ctrl-shift-p", &none, mac)),
            ["toggle_pinned_result"]
        );
        // Not between the settings and a grid.
        assert!(conflicts("search_settings", "secondary-z", &none, mac).is_empty());
        // The user's entries count: a key moved away is free, one added taken.
        let mut moved = Overrides::new();
        moved.insert("toggle_sidebar".into(), Some(vec!["f9".into()]));
        assert!(conflicts("about", "secondary-b", &moved, mac).is_empty());
        assert_eq!(
            ids(conflicts("about", "f9", &moved, mac)),
            ["toggle_sidebar"]
        );
        assert!(contexts_overlap(
            Some("QueryTab > mode == editor"),
            Some("QueryTab")
        ));
        assert!(!contexts_overlap(Some("Navigator"), Some("Settings")));
        assert!(contexts_overlap(None, Some("Navigator")));
    }

    /// K20: what is recorded is written so that it reads back as the same
    /// keystroke, Cmd (macOS) or Ctrl (elsewhere) as `secondary-`, Fn left
    /// out.
    #[test]
    fn a_recorded_keystroke_round_trips_through_the_file() {
        for os in [OperatingSystem::Mac, OperatingSystem::Linux] {
            for typed in [
                "cmd-shift-k",
                "ctrl-k",
                "ctrl-alt-shift-b",
                "cmd-alt-left",
                "f5",
                "shift-f4",
                "ctrl-cmd-t",
                "cmd--",
                "alt-enter",
                "ctrl-pagedown",
            ] {
                let stroke = Keystroke::parse_with_os(typed, os).unwrap();
                let written = file_form(&stroke, os);
                let read = Keystroke::parse_with_os(&written, os).unwrap();
                assert_eq!(
                    read.unparse(os),
                    stroke.unparse(os),
                    "{os:?} {typed} → {written}"
                );
            }
        }
        let mac = |key| Keystroke::parse_with_os(key, OperatingSystem::Mac).unwrap();
        assert_eq!(
            file_form(&mac("cmd-shift-k"), OperatingSystem::Mac),
            "secondary-shift-k"
        );
        assert_eq!(file_form(&mac("ctrl-k"), OperatingSystem::Mac), "ctrl-k");
        let linux = Keystroke::parse_with_os("ctrl-shift-k", OperatingSystem::Linux).unwrap();
        assert_eq!(
            file_form(&linux, OperatingSystem::Linux),
            "secondary-shift-k"
        );
        let mut fn_arrow = mac("cmd-up");
        fn_arrow.modifiers.function = true;
        assert_eq!(file_form(&fn_arrow, OperatingSystem::Mac), "secondary-up");
    }

    /// K20: a key that types needs Cmd or Ctrl; a named key a modifier
    /// unless the command's keys work only outside text; F keys alone.
    #[test]
    fn only_keys_that_do_not_eat_typing_are_recorded() {
        let os = OperatingSystem::Mac;
        let record = |key: &str, context| {
            recorded_key(&Keystroke::parse_with_os(key, os).unwrap(), context, os)
        };
        assert_eq!(record("cmd-shift-k", None), Ok("secondary-shift-k".into()));
        assert_eq!(record("ctrl-k", None), Ok("ctrl-k".into()));
        assert_eq!(record("f6", None), Ok("f6".into()));
        assert_eq!(record("alt-up", None), Ok("alt-up".into()));
        assert_eq!(record("enter", Some("DataTable")), Ok("enter".into()));
        for typing in ["k", "shift-k", "alt-k", "alt-shift-k", "1", "shift-1"] {
            let refused = record(typing, None).unwrap_err();
            assert!(refused.contains("types a character"), "{typing}: {refused}");
            // Not in a grid either: its letters start an edit.
            assert!(record(typing, Some("DataTable")).is_err(), "{typing}");
        }
        for named in ["enter", "backspace", "up", "shift-tab"] {
            assert!(record(named, None).is_err(), "{named}");
        }
    }

    /// K20: the page's changes keep everything else in the file: the other
    /// entries, keys Kerno does not know, their order; defaults written
    /// remove the entry; a file that is not a keymap is never rewritten.
    #[test]
    fn changing_the_keymap_keeps_the_rest_of_the_file() {
        let text = r#"{
  "mine": {"note": "kept"},
  "bindings": {
    "about": "f1",
    "toggle_sidebar": null
  },
  "zzz": [1, 2]
}"#;
        let set = changed_keymap(
            Some(text),
            &KeymapChange::Set("quit", vec!["secondary-shift-q".into(), "f12".into()]),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&set).unwrap();
        assert_eq!(value["mine"]["note"], "kept");
        assert_eq!(value["zzz"], serde_json::json!([1, 2]));
        assert_eq!(value["bindings"]["about"], "f1");
        assert!(value["bindings"]["toggle_sidebar"].is_null());
        assert_eq!(
            value["bindings"]["quit"],
            serde_json::json!(["secondary-shift-q", "f12"])
        );
        let order: Vec<&String> = value.as_object().unwrap().keys().collect();
        assert_eq!(order, ["mine", "bindings", "zzz"]);
        assert_eq!(
            overrides(&set).unwrap()["quit"],
            Some(vec!["secondary-shift-q".to_string(), "f12".to_string()])
        );
        // One key is written as a string, none as null.
        let one =
            changed_keymap(Some(&set), &KeymapChange::Set("about", vec!["f2".into()])).unwrap();
        assert!(one.contains("\"about\": \"f2\""), "{one}");
        let unbound = changed_keymap(Some(text), &KeymapChange::Set("quit", vec![])).unwrap();
        assert_eq!(overrides(&unbound).unwrap()["quit"], None);
        // No shortcut for a command that has none by default: no entry.
        let none = changed_keymap(Some(&set), &KeymapChange::Set("about", vec![])).unwrap();
        assert!(!overrides(&none).unwrap().contains_key("about"));
        // Its defaults again: no entry, the command is not marked changed.
        let back = changed_keymap(
            Some(&set),
            &KeymapChange::Set("quit", vec!["secondary-q".into()]),
        )
        .unwrap();
        assert!(!overrides(&back).unwrap().contains_key("quit"));
        let reset = changed_keymap(Some(&set), &KeymapChange::Reset("about")).unwrap();
        assert!(!overrides(&reset).unwrap().contains_key("about"));
        assert!(overrides(&reset).unwrap().contains_key("quit"));
        let all = changed_keymap(Some(&set), &KeymapChange::ResetAll).unwrap();
        assert!(overrides(&all).unwrap().is_empty());
        assert!(all.contains("\"mine\""));
        // No file yet: the example, with the change.
        let fresh = changed_keymap(None, &KeymapChange::Set("about", vec!["f1".into()])).unwrap();
        assert!(fresh.contains("\"about\"") && fresh.contains("\"commands\""));
        assert_eq!(overrides(&fresh).unwrap().len(), 1);
        // Not a keymap: refused.
        for bad in ["{ not json", r#"{"bindings": 3}"#, "[1]"] {
            assert!(
                changed_keymap(Some(bad), &KeymapChange::ResetAll).is_err(),
                "{bad}"
            );
        }
    }

    /// K20: the page's filter finds a command by its name, group, keywords
    /// and by its shortcut, typed as words or as symbols.
    #[test]
    fn commands_are_found_by_name_and_by_shortcut() {
        let os = OperatingSystem::Mac;
        let found = |query: &str| -> Vec<&'static str> {
            COMMANDS
                .iter()
                .filter(|command| {
                    let keys: Vec<String> = command.keys.iter().map(|k| k.to_string()).collect();
                    command_matches(command, &keys, query, os)
                })
                .map(|command| command.id)
                .collect()
        };
        assert!(found("theme switch").contains(&"toggle_theme"));
        assert!(found("dark mode").contains(&"toggle_theme"));
        for query in ["cmd k", "⌘K", "Command+K", "secondary-k", "CMD K"] {
            assert_eq!(found(query), ["open_command_palette"], "{query}");
        }
        assert_eq!(found("⌘⇧D"), ["toggle_theme"]);
        // Ctrl is not Cmd on macOS: Ctrl+Alt+Shift+K commits.
        assert_eq!(found("ctrl k"), ["commit_transaction"]);
        // A word of the name and the key together.
        assert_eq!(found("palette cmd"), ["open_command_palette"]);
        assert!(found("zzz").is_empty());
        assert_eq!(found("").len(), COMMANDS.len());
        assert_eq!(
            query_words("⌃⌥⇧⌘F5", os),
            ["ctrl", "alt", "shift", "cmd", "f5"]
        );
        assert_eq!(
            query_words("secondary-k", OperatingSystem::Linux),
            ["ctrl", "k"]
        );
    }
}
