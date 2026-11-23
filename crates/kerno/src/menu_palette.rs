//! C4 (K13): every entry of a context menu and every button of the main toolbar is also a
//! command of the palette (`actions::COMMANDS`), or an exception named here with its reason.
//!
//! The menus built as data are walked (their builders called with facts that show every
//! entry), and each entry is classified by an exhaustive `match` per kind of entry: a new
//! variant does not compile until it is given a command or an exception, and a command named
//! here must be one of `COMMANDS`.
//!
//! Menus built from closures, which a test cannot walk (each entry acts on the thing under the
//! pointer or picks from a list): the ER diagram's table menu (`erd_tab::node_menu`: open data,
//! open structure, copy name of that table) and its background menu (`canvas_menu`: fit, zoom
//! to 100 %, export SVG / PNG / Mermaid, refresh: they act on the diagram on screen and would
//! make sense as commands, left for later); the administration grid's cell menu
//! (`admin::tab::cell_menu`: copy, cancel or terminate that session or the one holding that
//! lock); a column row of the table form (`structure::editor`: move up, move down, remove that
//! column); the Tasks view's "New task" menu (one entry per connection and tool: Create New
//! Task… asks for both); the query tab's connection picker (`query::tab::connection_menu`) and
//! the toolbar's lists (`workspace::toolbar`: the engines of New connection, the recent scripts
//! of New SQL editor, the commit modes and isolation levels, the connection and the database /
//! schema pickers, the items that did not fit, the toolbar's own right-click to hide items); the
//! schema tree's "Cancel the running statement" and "Cancel loading" (`navigator::view::
//! object_menu`, shown only while a statement or a read runs on that row: they stop that row's
//! work, which Cmd+. — the palette's Cancel — also does with the row selected).

use crate::actions::COMMANDS;
use crate::navigator::actions::{Action, FilterAction, Template, ToolEntry, tool_entries};
use crate::query::grid::{
    self, CellFacts, CopyFormat, GridAction, HeaderFacts, Mark, Panel, cell_menu, header_menu,
    row_menu,
};
use crate::sidebar::{self, SidebarAction, Subject};
use crate::tabs::{self, Choices, EditorFacts, ResultFacts, StatusFacts, StatusItem, TabAction};
use crate::tasks_view::TaskAction;
use crate::workspace::toolbar::Item;
use kerno_core::sql::RowSql;
use kerno_core::{TxState, TypeKind};

/// How the palette reaches a menu entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Palette {
    /// The palette's command that does the same, on the same thing.
    Command(&'static str),
    /// It only makes sense on the thing under the pointer, or is one of a list.
    Here(&'static str),
    /// It would make sense in the palette; not a command yet.
    Later(&'static str),
}

use Palette::{Command, Here, Later};

/// The text field's and the grid's own keys: Cmd+Z, Cmd+X, Cmd+C, Cmd+V, Cmd+A.
const EDIT_KEYS: &str = "the system's editing keys, which every text field and grid takes";

/// Where an entry of `tabs`' menus is shown: the same entry means another thing there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabMenu {
    /// A tab, the empty tab bar, the SQL editor, the toolbar's Run list, the status bar.
    Window,
    /// A result tab of a query tab.
    Result,
}

fn tab_entry(menu: TabMenu, action: &TabAction) -> Palette {
    use TabAction as A;
    let result = menu == TabMenu::Result;
    match action {
        A::Close if result => Command("close_result_tab"),
        A::Close => Command("close_tab"),
        A::CloseOthers if result => Command("close_other_result_tabs"),
        A::CloseOthers => Command("close_other_tabs"),
        A::CloseToRight if result => Command("close_result_tabs_to_the_right"),
        A::CloseToRight => Command("close_tabs_to_the_right"),
        A::CloseToLeft => Command("close_result_tabs_to_the_left"),
        A::CloseAll if result => Command("close_all_result_tabs"),
        A::CloseAll => Command("close_all_tabs"),
        A::Rename if result => Command("rename_result_tab"),
        A::Rename => Command("rename_tab"),
        A::Duplicate => Command("duplicate_tab"),
        A::CopyPath => Command("copy_tab_file_path"),
        A::Reveal => Command("reveal_tab_file"),
        A::SplitRight => Command("split_right"),
        A::SplitDown => Command("split_down"),
        A::MoveToOtherGroup | A::MoveToNextGroup => Command("move_tab_to_next_group"),
        A::MoveToPreviousGroup => Command("move_tab_to_previous_group"),
        A::NewQuery => Command("new_query_tab"),
        A::ReopenClosed => Command("reopen_closed_tab"),
        A::ApplySource => Command("apply_source"),
        A::RunStatement => Command("run_statement"),
        A::RunInNewTab => Command("run_statement_new_tab"),
        A::RunFromCursor => Command("run_from_cursor"),
        A::RunScript => Command("run_all"),
        A::RunOnConnections => Command("run_script_on_connections"),
        A::Explain => Command("explain_statement"),
        A::ExplainAnalyze => Command("explain_analyze"),
        A::CountRows => Command("count_statement_rows"),
        A::Format => Command("format_sql"),
        A::NextStatement => Command("next_statement"),
        A::PreviousStatement => Command("previous_statement"),
        A::ToggleResults => Command("toggle_results_panel"),
        A::MaximizeResults => Command("maximize_results_panel"),
        A::SwitchFocus => Command("switch_results_focus"),
        A::Undo | A::Redo | A::Cut | A::Copy | A::Paste | A::SelectAll => Here(EDIT_KEYS),
        A::SelectAllOccurrences => Command("select_all_occurrences"),
        A::ToggleComment => Command("toggle_comment"),
        A::ToggleBlockComment => Command("toggle_block_comment"),
        A::UpperCase => Command("upper_case"),
        A::LowerCase => Command("lower_case"),
        A::ToggleSoftWrap => Command("toggle_soft_wrap"),
        A::UnfoldAll => Command("unfold_all"),
        A::Find => Command("find_in_editor"),
        A::Replace => Command("replace_in_editor"),
        A::Save => Command("save_file"),
        A::SaveAs => Command("save_file_as"),
        A::SaveQuery => Command("save_query"),
        A::RevertFile => Command("revert_file"),
        A::OpenFile => Command("open_sql_file"),
        A::FetchAllRows => Command("fetch_all_rows"),
        A::ExportFromQuery => Command("export_from_query"),
        A::UseConnection(..) => Here(
            "one entry per saved connection, a list; the main toolbar's connection picker lists them",
        ),
        A::UseDatabase(_) | A::UseSchema(_) => {
            Here("one entry per database or schema, a list; the main toolbar's picker lists them")
        }
        A::Disconnect => Command("disconnect_tab_connection"),
        A::Reconnect => Command("reconnect_tab_connection"),
        A::Administration => Command("open_admin"),
        A::EditConnection => Command("edit_tab_connection"),
        A::Commit => Command("commit_transaction"),
        A::Rollback => Command("rollback_transaction"),
        A::TransactionLog => Command("transaction_log"),
        A::ManualCommit(_) => Command("toggle_commit_mode"),
        A::Cancel => Command("cancel_query"),
        A::ReviewChanges => Command("apply_changes"),
        A::DiscardChanges => Command("discard_changes"),
        A::CopyText(_) => Here("copies the text of the status bar item it is on"),
        A::AboutReadOnly | A::AboutMemoryOnly => Here("explains the status bar item it is on"),
        A::ShowMessages => Command("show_messages"),
        A::NewConnection => Command("new_connection"),
        A::DisconnectAll => Command("disconnect_all"),
        A::Pin | A::Unpin => Command("toggle_pinned_result"),
        A::UnpinAll => Command("unpin_all_result_tabs"),
        A::CopyStatement if result => Command("copy_result_statement"),
        A::CopyStatement => Command("copy_selected_query"),
        A::RunAgain => Later(
            "runs an incomplete result's statement again; its menu offers it only on an \
             incomplete result, and a command needs that check first",
        ),
        A::Update | A::OpenReleasePage | A::SkipVersion => Here(
            "answers the release the status bar item offers; Check for Updates… says when one is out",
        ),
        A::TurnOffUpdateChecks => {
            Here("the switch of Settings ▸ Updates, which the settings' search finds")
        }
    }
}

/// An entry of the sidebar's menus, on `subject`. The connection commands act on the active
/// tab's connection, else on the one selected in the sidebar (`connection_target`).
fn sidebar_entry(subject: &Subject, action: &SidebarAction) -> Palette {
    use SidebarAction as A;
    let folder = matches!(subject, Subject::Folder(_));
    match action {
        A::NewConnection => Command("new_connection"),
        A::NewFolder => Command("new_connection_folder"),
        A::ImportConnections => Command("import_connections"),
        A::Paste => Command("paste_connection"),
        A::Refresh if *subject == Subject::List => Command("refresh_connections"),
        A::Refresh => Command("refresh_object"),
        A::CollapseAll => Command("collapse_connection_folders"),
        A::LinkWithEditor(_) => Command("link_with_editor"),
        A::NewConnectionHere => Command("new_connection_in_folder"),
        A::NewSubfolder => Command("new_subfolder"),
        A::EditFolder => Command("edit_object"),
        A::Rename => Command("rename_object"),
        A::Delete if folder => Command("drop_object"),
        A::Delete => Command("delete_connection"),
        A::Connect => Command("open_connection"),
        A::Disconnect if folder => Command("disconnect_folder"),
        A::Disconnect => Command("disconnect_tab_connection"),
        A::Reconnect => Command("reconnect_tab_connection"),
        A::DisconnectOthers => Command("disconnect_other_connections"),
        A::DisconnectAll => Command("disconnect_all"),
        A::ReadOnly(_) => Command("toggle_read_only"),
        A::NewSqlEditor => Command("new_query_on_object"),
        A::Tool(entry) => tool_entry(false, entry),
        A::Edit => Command("edit_connection"),
        A::Duplicate => Command("duplicate_connection"),
        A::MoveTo(_) => Here(
            "one entry per folder, a list; dragging the connection, or its dialog's Folder field, \
             moves it",
        ),
        A::CopyName => Command("copy_object_name"),
        A::CopyUrl => Command("copy_connection_url"),
        A::CopyHost => Command("copy_connection_host"),
        // Editing a connection opened from a URL saves it (`Sidebar::edit`).
        A::SaveConnection => Command("edit_connection"),
        // SQL scripts ▸ (K38): one entry per script, a list; the palette's command lists the
        // same scripts of the selected connection, to pick one.
        A::OpenScript(_) | A::AllScripts => Command("connection_scripts"),
        A::OpenSqlFileHere => Command("open_sql_file_on_connection"),
        A::SavedQueriesHere => Command("show_saved_queries"),
    }
}

/// An entry of Tools ▸: of a node of the tree (`object`), or of a connection.
fn tool_entry(object: bool, entry: &ToolEntry) -> Palette {
    match entry {
        ToolEntry::Backup if object => Command("backup_object"),
        ToolEntry::Backup => Command("backup_database"),
        ToolEntry::GlobalBackup => Command("global_backup"),
        ToolEntry::Restore => Command("restore_database"),
        ToolEntry::ExecuteScript => Command("execute_script"),
        ToolEntry::Maintain(_) => Command("maintenance_tools"),
        ToolEntry::ForeignDataWrappers => Command("foreign_data_wrappers"),
        ToolEntry::Dashboard | ToolEntry::SessionManager => Command("open_admin"),
        ToolEntry::LockManager => Command("lock_manager"),
        ToolEntry::NewTask => Command("create_task"),
        ToolEntry::Tasks => Command("show_tasks"),
        ToolEntry::CopyTables => Command("copy_tables"),
        ToolEntry::Extensions => Command("install_extension"),
        ToolEntry::CompareSchemas => Command("compare_schemas"),
    }
}

/// An entry of a node's menu in the schema tree: they act on the selected node.
fn tree_entry(action: Action) -> Palette {
    match action {
        Action::OpenData => Command("open_object_data"),
        Action::OpenStructure => Command("open_object_structure"),
        Action::NewQueryHere => Command("new_query_on_object"),
        Action::Generate(Template::Select) => Command("generate_select"),
        Action::Generate(Template::Insert) => Command("generate_insert"),
        Action::Generate(Template::Update) => Command("generate_update"),
        Action::Generate(Template::Delete) => Command("generate_delete"),
        Action::ShowDdl => Command("show_object_ddl"),
        Action::CopyName => Command("copy_object_name"),
        Action::CopyQualifiedName => Command("copy_qualified_name"),
        Action::SetDefault => Command("set_default_schema"),
        Action::Rename => Command("rename_object"),
        // Under Tools ▸, which Maintenance Tools… opens on the selected node.
        Action::Truncate | Action::Tools => Command("maintenance_tools"),
        Action::Drop => Command("drop_object"),
        Action::DropCascade => Command("drop_object_cascade"),
        Action::CreateTable => Command("create_table"),
        Action::CreateView => Command("create_view"),
        Action::CreateSchema => Command("create_schema"),
        Action::CreateDatabase => Command("create_database"),
        Action::CreateFunction => Command("create_function"),
        Action::CreateProcedure => Command("create_procedure"),
        Action::CreateSequence => Command("create_sequence"),
        Action::CreateTrigger => Command("create_trigger"),
        Action::CreateEvent => Command("create_event"),
        Action::CreateEnum => Command("create_enum"),
        Action::AddEnumValue => Command("add_enum_value"),
        Action::RenameEnumValue => Command("rename_enum_value"),
        Action::ManageExtensions | Action::ShowInExtensions => Command("install_extension"),
        Action::EditSource => Command("edit_object_source"),
        Action::Export => Command("export_object"),
        Action::ImportCsv => Command("import_csv_into_object"),
        Action::Backup => Command("backup_object"),
        Action::Restore => Command("restore_database"),
        Action::Refresh => Command("refresh_object"),
        Action::AddColumn => Command("add_column_to_object"),
        Action::RenameColumn => Command("rename_column"),
        Action::DropColumn => Command("drop_column"),
        Action::RevealTable => Command("reveal_referenced_table"),
        Action::ReadInConsole => Command("read_object_in_console"),
        Action::GenerateCall => Command("generate_routine_call"),
        Action::ExecuteRoutine => Command("execute_routine"),
        // F4 (`edit_action`): a column's form, a table's Properties.
        Action::EditColumn | Action::Properties => Command("edit_object"),
        Action::ViewDiagram => Command("view_diagram"),
        Action::CompareWith => Command("compare_schemas"),
        Action::ExportSchema => Command("export_schema"),
    }
}

fn filter_entry(action: FilterAction) -> Palette {
    match action {
        FilterAction::Hide => Command("hide_object_in_tree"),
        FilterAction::ShowOnly => Command("show_only_object_in_tree"),
        FilterAction::Configure => Command("configure_object_filter"),
        FilterAction::Toggle => Command("toggle_object_filter"),
        FilterAction::Clear => Command("clear_object_filter"),
    }
}

/// An entry of a grid's menus: the commands act on the selection of the grid on screen.
fn grid_entry(action: &GridAction) -> Palette {
    use GridAction as A;
    let column = "acts on the column under the pointer";
    match action {
        A::Copy | A::CopyAs(CopyFormat::Tsv) | A::Paste => Here(EDIT_KEYS),
        A::CopyAs(CopyFormat::TsvWithHeader) => Command("copy_with_column_names"),
        A::CopyAs(CopyFormat::Csv) => Command("copy_as_csv"),
        A::CopyAs(CopyFormat::Json) => Command("copy_as_json"),
        A::CopyAs(CopyFormat::Markdown) => Command("copy_as_markdown"),
        A::CopyAs(CopyFormat::SqlInsert) => Command("copy_as_sql_insert"),
        A::CopyColumnName => Command("copy_column_name"),
        A::FilterByValue | A::ExcludeValue => Later(
            "filters by the selected cell's value; its menu greys it on a binary or JSON value, \
             and a command needs that check first",
        ),
        A::FilterWith(_) => {
            Here("one entry per operator on the value under the pointer; the filter bar takes any")
        }
        A::FilterColumn => Here(column),
        A::DistinctValues => Command("filter_distinct_values"),
        A::ClearFilters => Command("clear_filters"),
        A::Order(_) => Here(column),
        A::Follow(..) => Here("one entry per foreign key of the row under the pointer"),
        A::ViewValue | A::EditValue => Command("view_cell_value"),
        A::EditCell => {
            Here("edits the cell under the pointer in place; Enter edits the selected one")
        }
        A::SetNull => Command("set_null"),
        A::SetDefault => Command("set_default"),
        A::CopyFromRow(true) => Command("copy_from_row_above"),
        A::CopyFromRow(false) => Command("copy_from_row_below"),
        A::DuplicateRow => Command("duplicate_rows"),
        A::DeleteRow => Command("delete_rows"),
        A::RevertRow => Command("revert_selected_rows"),
        A::Pin(_) | A::HideColumn | A::FitWidth => Here(column),
        A::ShowAll(_) => Command("show_all_columns"),
        A::FitAllWidths => Command("fit_all_column_widths"),
        A::Export => Command("export_data"),
        A::Generate(RowSql::SelectByKey) => Command("generate_select_by_key"),
        A::Generate(RowSql::SelectIn) => Command("generate_select_in"),
        A::Generate(RowSql::Insert) => Command("generate_row_insert"),
        A::Generate(RowSql::Update) => Command("generate_row_update"),
        A::Generate(RowSql::Delete) => Command("generate_row_delete"),
        A::CopyRowNumbers => Command("copy_row_numbers"),
        A::AddRow => Command("add_row"),
        A::Refresh => Command("refresh_data"),
        A::RecordView(_) => Command("toggle_record_view"),
        A::SelectionStats(_) => Command("toggle_selection_stats"),
        A::Zoom(1) => Command("zoom_in"),
        A::Zoom(-1) => Command("zoom_out"),
        A::Zoom(_) => Command("zoom_reset"),
        A::Panel(Panel::Aggregate) => Command("show_aggregate_panel"),
        A::Panel(Panel::Columns) => Command("show_columns_panel"),
        A::GoToRow => Command("go_to_row"),
        A::GoToColumn => Command("go_to_column"),
        A::Find => Command("find_in_results"),
        A::ResetLayout => Command("reset_table_layout"),
    }
}

/// An entry of a task's menu in the Tasks view.
fn task_entry(action: TaskAction) -> Palette {
    match action {
        TaskAction::Run
        | TaskAction::Edit
        | TaskAction::Duplicate
        | TaskAction::Rename
        | TaskAction::Delete
        | TaskAction::ShowLog => Here(
            "acts on the task it is on, in the Tasks view, which has no selection; Tasks opens it",
        ),
    }
}

/// A button of the main toolbar: its own command, or a list.
fn toolbar_item(item: Item) -> Palette {
    match item.command() {
        Some(id) => Command(id),
        None => match item {
            Item::Connection | Item::Target => Here(
                "a list of connections, or of databases and schemas, to pick from; the editor's \
                 menu has the same lists",
            ),
            // Its entries are Tools ▸ of a connection, walked below.
            _ => Here("a list: Tools ▸ of the connection, whose entries are commands"),
        },
    }
}

/// One entry found: the menu, its label, how the palette reaches it.
type Found = (&'static str, String, Palette);

fn walk_tabs(
    menu: &'static str,
    place: TabMenu,
    entries: Vec<tabs::Entry>,
    found: &mut Vec<Found>,
) {
    for entry in entries {
        match entry {
            tabs::Entry::Item(action, _) => {
                found.push((menu, action.label(), tab_entry(place, &action)))
            }
            tabs::Entry::Submenu(_, items) => walk_tabs(menu, place, items, found),
            tabs::Entry::Separator => {}
        }
    }
}

fn walk_sidebar(subject: &Subject, entries: Vec<sidebar::Entry>, found: &mut Vec<Found>) {
    for entry in entries {
        match entry {
            sidebar::Entry::Item(action, _) => {
                found.push(("sidebar", action.label(), sidebar_entry(subject, &action)))
            }
            sidebar::Entry::Submenu(_, items) => walk_sidebar(subject, items, found),
            sidebar::Entry::Note(_) | sidebar::Entry::Separator => {}
        }
    }
}

fn walk_grid(menu: &'static str, entries: Vec<grid::Entry>, found: &mut Vec<Found>) {
    for entry in entries {
        match entry {
            grid::Entry::Item(action, ..) => {
                found.push((menu, action.label(), grid_entry(&action)))
            }
            grid::Entry::Submenu(_, items) => walk_grid(menu, items, found),
            grid::Entry::Separator => {}
        }
    }
}

/// Every entry of the menus built as data, with facts that show each entry at least once.
fn every_entry() -> Vec<Found> {
    let mut found = Vec::new();
    // Tabs, the tab bar, the editor and its Run list, a result tab, the status bar.
    for kind in [
        tabs::TabKind::Query { file: false },
        tabs::TabKind::Query { file: true },
        tabs::TabKind::Other,
    ] {
        walk_tabs(
            "tab",
            TabMenu::Window,
            tabs::tab_menu(kind, 0, 3),
            &mut found,
        );
    }
    walk_tabs("tab bar", TabMenu::Window, tabs::bar_menu(2, 1), &mut found);
    // A tab's entries about the groups (K19), with two groups and with more.
    for groups in [2, 3] {
        walk_tabs(
            "tab groups",
            TabMenu::Window,
            tabs::group_entries(groups, None),
            &mut found,
        );
    }
    let choices = Choices {
        connections: vec![("c1".into(), "one".into(), true)],
        connection: Some("c1".into()),
        databases: vec!["db".into()],
        database: Some("db".into()),
        schemas: vec!["public".into()],
        schema: Some("public".into()),
    };
    for source in [false, true] {
        let facts = EditorFacts {
            connected: true,
            source,
            file: true,
            more_rows: true,
            selection: true,
            ..EditorFacts::default()
        };
        walk_tabs(
            "editor",
            TabMenu::Window,
            tabs::editor_menu(facts, &choices),
            &mut found,
        );
        walk_tabs(
            "toolbar Run",
            TabMenu::Window,
            tabs::run_menu(facts),
            &mut found,
        );
    }
    for pinned in [false, true] {
        let facts = ResultFacts {
            pinned,
            left: 1,
            right: 1,
            pinned_count: 1,
            statement: true,
            incomplete: true,
            writes: false,
        };
        walk_tabs(
            "result tab",
            TabMenu::Result,
            tabs::result_menu(facts),
            &mut found,
        );
    }
    let items = [
        StatusItem::Summary,
        StatusItem::Connection,
        StatusItem::Database,
        StatusItem::ReadOnly,
        StatusItem::Transaction,
        StatusItem::Running,
        StatusItem::RunTrace,
        StatusItem::Pending,
        StatusItem::Rows,
        StatusItem::Timing,
        StatusItem::Messages,
        StatusItem::MemoryOnly,
        StatusItem::Update,
    ];
    for open_connections in [0, 1] {
        let facts = StatusFacts {
            query: true,
            connected: true,
            databases: vec!["db".into(), "other".into()],
            database: Some("db".into()),
            transaction: TxState::Open,
            manual_commit: open_connections == 0,
            running: false,
            open_connections,
        };
        for item in items {
            walk_tabs(
                "status bar",
                TabMenu::Window,
                tabs::status_menu(item, "1 row", &facts),
                &mut found,
            );
        }
    }
    // The sidebar: its empty area, a folder, a connection with its Tools ▸, an unsaved one.
    for subject in [
        Subject::List,
        Subject::Folder("Work".into()),
        Subject::Connection("c1".into()),
    ] {
        for status in [
            crate::connections::manager::Status::Disconnected,
            crate::connections::manager::Status::Connected {
                version: String::new(),
            },
        ] {
            let entries = sidebar::menu(&subject, &status, &["Work".into()], None, 1, true, true);
            let entries = sidebar::with_tools(
                entries,
                tool_entries(None, kerno_core::Engine::Postgres, &[]),
            );
            // SQL scripts ▸ with a saved query in a folder, a file, and "All scripts…".
            let saved: Vec<kerno_core::storage::SavedQuery> = (1..=sidebar::MENU_SAVED_QUERIES + 1)
                .map(|id| kerno_core::storage::SavedQuery {
                    id: id as i64,
                    connection_id: Some("c1".into()),
                    folder: "reports".into(),
                    name: format!("q{id}"),
                    sql: "select 1".into(),
                })
                .collect();
            let entries =
                sidebar::with_scripts(entries, &saved, &[std::path::PathBuf::from("/s/a.sql")]);
            walk_sidebar(&subject, entries, &mut found);
        }
    }
    walk_sidebar(
        &Subject::Connection("c1".into()),
        vec![sidebar::Entry::Item(SidebarAction::SaveConnection, None)],
        &mut found,
    );
    // Tools ▸ of a connection (the toolbar's Tools) on both dialects.
    for engine in [kerno_core::Engine::Postgres, kerno_core::Engine::MySql] {
        for (entry, _) in tool_entries(None, engine, &[]).concat() {
            found.push(("toolbar Tools", entry.label(), tool_entry(false, &entry)));
        }
    }
    // The grids: a table's cell, row number and header, and a query result's.
    for data in [true, false] {
        let column = HeaderFacts {
            data,
            sort: Some(false),
            loading: false,
            pinned: false,
            hidden: 1,
            filtered: true,
            filters: true,
        };
        let cell = CellFacts {
            data,
            editable: !data,
            read_only: None,
            kind: TypeKind::Text,
            null: false,
            computed: false,
            nullable: true,
            row: Mark::Edited,
            many: false,
            clipboard: true,
            column,
            related: vec!["orders".into()],
            export: None,
            keyless: false,
            record: false,
            stats: false,
        };
        walk_grid("grid cell", cell_menu(&cell), &mut found);
        walk_grid("grid row", row_menu(&cell), &mut found);
        walk_grid("grid header", header_menu(&column), &mut found);
        walk_grid(
            "grid header",
            header_menu(&HeaderFacts {
                pinned: true,
                ..column
            }),
            &mut found,
        );
    }
    for action in TaskAction::ALL {
        found.push(("task", action.label().to_string(), task_entry(action)));
    }
    for item in Item::all() {
        found.push(("toolbar", item.name().to_string(), toolbar_item(item)));
    }
    found
}

/// C4: every entry of the menus built as data, and every toolbar button, is a command of the
/// palette, or an exception named with its reason: one only makes sense on the thing under
/// the pointer (or is one of a list); one left for later says why.
#[test]
fn every_menu_entry_and_toolbar_button_is_in_the_palette() {
    let found = every_entry();
    assert!(
        found.len() > 200,
        "the walk shows every menu: {}",
        found.len()
    );
    let ids: Vec<&str> = COMMANDS.iter().map(|command| command.id).collect();
    let missing: Vec<String> = found
        .iter()
        .filter_map(|(menu, label, palette)| match palette {
            Command(id) if !ids.contains(id) => Some(format!("{menu} ▸ {label}: no command {id}")),
            _ => None,
        })
        .collect();
    assert_eq!(missing, Vec::<String>::new());
    // Every exception says why.
    for (menu, label, palette) in &found {
        if let Here(why) | Later(why) = palette {
            assert!(why.len() > 20, "{menu} ▸ {label}: {why}");
        }
    }
    // What is left for later is known: one more fails here until it is named.
    let mut later: Vec<String> = found
        .iter()
        .filter(|(.., palette)| matches!(palette, Later(_)))
        .map(|(menu, label, _)| format!("{menu} ▸ {label}"))
        .collect();
    later.sort();
    later.dedup();
    assert_eq!(
        later,
        [
            "grid cell ▸ Exclude this value",
            "grid cell ▸ Filter by this value",
            "result tab ▸ Run again",
        ]
    );
}

/// The navigator's menus are filled by the window from these lists: every node action, every
/// filter entry and every Tools ▸ entry of a node reach a command (the `match` is the walk:
/// each variant is there or this does not compile).
#[test]
fn every_entry_of_the_schema_tree_is_in_the_palette() {
    use kerno_core::maintenance::Maintenance;
    let ids: Vec<&str> = COMMANDS.iter().map(|command| command.id).collect();
    let actions = [
        Action::OpenData,
        Action::OpenStructure,
        Action::NewQueryHere,
        Action::Generate(Template::Select),
        Action::Generate(Template::Insert),
        Action::Generate(Template::Update),
        Action::Generate(Template::Delete),
        Action::ShowDdl,
        Action::CopyName,
        Action::CopyQualifiedName,
        Action::SetDefault,
        Action::Rename,
        Action::Truncate,
        Action::Drop,
        Action::CreateTable,
        Action::CreateView,
        Action::CreateSchema,
        Action::CreateDatabase,
        Action::EditSource,
        Action::ManageExtensions,
        Action::ShowInExtensions,
        Action::CreateEnum,
        Action::AddEnumValue,
        Action::RenameEnumValue,
        Action::Export,
        Action::ImportCsv,
        Action::Backup,
        Action::Restore,
        Action::Refresh,
        Action::DropCascade,
        Action::CreateFunction,
        Action::CreateProcedure,
        Action::CreateSequence,
        Action::CreateTrigger,
        Action::CreateEvent,
        Action::AddColumn,
        Action::RenameColumn,
        Action::DropColumn,
        Action::RevealTable,
        Action::Tools,
        Action::ReadInConsole,
        Action::GenerateCall,
        Action::ExecuteRoutine,
        Action::EditColumn,
        Action::Properties,
        Action::ViewDiagram,
        Action::CompareWith,
        Action::ExportSchema,
    ];
    let mut palettes: Vec<(String, Palette)> = actions
        .iter()
        .map(|action| (action.label().to_string(), tree_entry(*action)))
        .collect();
    palettes.extend(
        [
            FilterAction::Hide,
            FilterAction::ShowOnly,
            FilterAction::Configure,
            FilterAction::Toggle,
            FilterAction::Clear,
        ]
        .map(|action| (action.label().to_string(), filter_entry(action))),
    );
    palettes.extend(
        [
            ToolEntry::Backup,
            ToolEntry::GlobalBackup,
            ToolEntry::Restore,
            ToolEntry::ExecuteScript,
            ToolEntry::Maintain(Maintenance::Analyze),
            ToolEntry::ForeignDataWrappers,
            ToolEntry::Dashboard,
            ToolEntry::SessionManager,
            ToolEntry::LockManager,
            ToolEntry::NewTask,
            ToolEntry::Tasks,
            ToolEntry::CopyTables,
            ToolEntry::Extensions,
            ToolEntry::CompareSchemas,
        ]
        .map(|entry| (entry.label(), tool_entry(true, &entry))),
    );
    for (label, palette) in palettes {
        match palette {
            Command(id) => assert!(ids.contains(&id), "{label}: no command {id}"),
            other => panic!("{label}: a node's entry acts on the selected node: {other:?}"),
        }
    }
}

/// The AI entries of the editor's menu dispatch the palette's own actions.
#[test]
fn the_ai_entries_of_the_editor_menu_are_commands() {
    let names: Vec<&str> = COMMANDS
        .iter()
        .map(|command| (command.action)().name())
        .collect();
    for (label, action, _) in crate::ai_view::menu_entries(true) {
        assert!(names.contains(&action().name()), "{label}");
    }
}
