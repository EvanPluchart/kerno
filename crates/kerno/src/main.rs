//! Kerno: a fast, native database client.

mod actions;
mod admin;
mod app_icon;
mod backup_dialog;
mod bench;
mod brand;
mod bridge;
mod connections;
mod data;
mod database_dialog;
mod engine_logo;
mod environments_view;
mod export_dialog;
mod fdw_dialog;
mod folder_dialog;
mod import_dialog;
mod library;
mod local_clients_view;
mod logging;
mod maintenance_dialog;
mod navigator;
mod query;
mod quick_open;
mod settings;
mod settings_view;
mod sidebar;
mod sql_confirm;
mod state;
mod structure;
mod tabs;
mod tasks_view;
mod theme;
#[cfg(test)]
mod ui_tests;
mod update;
mod workspace;

use gpui_kit::component::{TitleBar, WindowExt as _};
use gpui_kit::{
    App, AppContext as _, Bounds, Focusable as _, Menu, MenuItem, WindowBounds, WindowOptions, px,
    size,
};

use crate::actions::{
    About, BackupDatabase, CancelQuery, CloseTab, ExplainAnalyze, ExplainStatement,
    ExportConnections, ExportData, FindInEditor, FormatSql, ImportConnections, ImportCsv,
    NewConnection, NewQueryTab, OpenCommandPalette, OpenSettings, OpenSqlFile, Quit,
    ReplaceInEditor, RestoreDatabase, RunAll, RunStatement, SaveFile, SaveFileAs, SaveQuery,
    ShowHistory, ShowSavedQueries, ToggleComment, ToggleSidebar, UseDarkTheme, UseLightTheme,
    UseSystemTheme,
};
use crate::connections::manager::{Connections, Drivers};
use crate::state::{AppState, Startup};
use crate::theme::Appearance;
use crate::workspace::Workspace;

fn main() {
    // Run by `ssh` to ask a question for an OpenSSH tunnel: answer, end.
    if let Some(code) = kerno_core::openssh::answer_askpass() {
        std::process::exit(code);
    }
    bench::init();
    // Kerno's own files are read on another thread while the platform
    // starts up, so the first frame has the saved tabs and the right theme
    // without the UI thread ever touching the disk.
    let startup = std::thread::spawn(Startup::load);

    gpui_kit::application()
        .with_assets(engine_logo::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let started = startup
                .join()
                .map_err(|_| anyhow::anyhow!("reading Kerno's data panicked"))
                .and_then(|startup| startup)
                .and_then(|startup| start(startup, cx));
            if let Err(error) = started {
                eprintln!("Kerno could not start: {error:#}");
                cx.quit();
            }
        });
}

fn start(startup: Startup, cx: &mut App) -> anyhow::Result<()> {
    logging::init(&startup.paths.logs(), cx)?;

    bridge::init(cx)?;
    // KERNO_APPEARANCE=light|dark pins the theme for one run; screenshots
    // of the real app use it.
    let pinned = match std::env::var("KERNO_APPEARANCE").as_deref() {
        Ok("light") => Some(Appearance::Light),
        Ok("dark") => Some(Appearance::Dark),
        _ => None,
    };
    AppState::init(&startup, pinned, cx);
    theme::init(AppState::get(cx).appearance(), cx)?;

    #[cfg(target_os = "macos")]
    app_icon::set_dock_icon();
    let keymap_problems = actions::bind_keys(&startup.paths.keymap(), cx);
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    // The one request Kerno makes by itself (CLAUDE.md §1), unless an off
    // switch is set: where this copy lives is looked at off the UI thread.
    let exe = std::env::current_exe().ok();
    let updates = bridge::spawn_blocking(cx, move || {
        let updates = update::for_this_run(|name| std::env::var_os(name).is_some(), exe);
        if let Some(updates) = &updates {
            update::remove_leftovers(&updates.target);
        }
        updates
    });
    cx.spawn(async move |cx| {
        if let Some(updates) = updates.await {
            cx.update(|cx| cx.set_global(updates));
        }
    })
    .detach();
    cx.set_menus(menus());
    // One window: closing it ends the app.
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();

    let mut window_size = size(px(1280.), px(800.));
    if let Some(display) = cx.primary_display() {
        let available = display.bounds().size;
        window_size.width = window_size.width.min(available.width * 0.9);
        window_size.height = window_size.height.min(available.height * 0.9);
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            window_size,
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(440.))),
        // Linux: the desktop entry's `StartupWMClass` matches it, so the
        // window takes the installed icon; X11 also gets the icon itself.
        app_id: Some("kerno".into()),
        #[cfg(target_os = "linux")]
        icon: app_icon::window_icon(),
        ..TitleBar::window_options()
    };
    let Startup {
        connections,
        tabs,
        active_tab,
        ..
    } = startup;
    let keymap_file = AppState::get(cx).paths.keymap();
    gpui_kit::open_window(options, cx, move |window, cx| {
        window.set_window_title("Kerno");
        // The built-in client reads the user's own known_hosts, never writes
        // it (OpenSSH connections use OpenSSH's, see D-056): keys Kerno
        // learns go to its own file.
        let user_known_hosts = std::env::var_os("HOME")
            .map(|home| std::path::PathBuf::from(home).join(".ssh/known_hosts"));
        let drivers = Drivers::default()
            .with(kerno_postgres::PostgresDriver)
            .with(kerno_mysql::MysqlDriver);
        // The system's OpenSSH (J2); `KERNO_SSH_CONFIG` names another file.
        let openssh = kerno_core::openssh::OpenSshEnv::detect(
            std::env::var_os("PATH").as_deref(),
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .as_deref(),
            std::env::var_os("KERNO_SSH_CONFIG").map(std::path::PathBuf::from),
            std::env::current_exe().unwrap_or_else(|_| "kerno".into()),
        );
        let connections = cx.new(|_| {
            let mut connections = Connections::new(connections, drivers, user_known_hosts);
            connections.set_openssh(openssh);
            connections
        });
        let workspace =
            cx.new(|cx| Workspace::new(connections.clone(), tabs, active_tab, window, cx));
        bench::begin(&workspace, &connections, window, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.start_update_checks(window, cx)
        });
        workspace.focus_handle(cx).focus(window, cx);
        for problem in keymap_problems {
            window.push_notification(format!("{}: {problem}", keymap_file.display()), cx);
        }
        workspace
    })?;
    cx.activate(true);
    Ok(())
}

/// A menu item running the command `id`, named as in the palette. The
/// commands whose keys work only in the tree, the connection list or a grid
/// go through `RunCommand`, so the menu does not take those keys
/// (Backspace, Enter, Escape, Cmd+C) everywhere; the others show their keys.
fn command(id: &str) -> MenuItem {
    let found = actions::COMMANDS
        .iter()
        .find(|command| command.id == id)
        .unwrap_or_else(|| panic!("no command {id}"));
    let local = matches!(
        found.context,
        Some("Navigator" | "ConnectionList" | "DataTable")
    );
    MenuItem::Action {
        name: found.label.into(),
        action: if local {
            Box::new(actions::RunCommand { id: id.to_string() })
        } else {
            (found.action)()
        },
        os_action: None,
        checked: false,
        disabled: false,
    }
}

fn menu(name: &str, items: Vec<MenuItem>) -> Menu {
    Menu {
        name: name.to_string().into(),
        items,
        disabled: false,
    }
}

/// The native menu bar: every command of `actions::COMMANDS` is in it.
fn menus() -> Vec<Menu> {
    let separator = MenuItem::separator;
    vec![
        menu(
            "Kerno",
            vec![
                MenuItem::action("About Kerno", About),
                separator(),
                MenuItem::action("Settings…", OpenSettings),
                command("open_keymap_file"),
                separator(),
                MenuItem::action("Quit Kerno", Quit),
            ],
        ),
        menu(
            "File",
            vec![
                MenuItem::action("New Query Tab", NewQueryTab),
                MenuItem::action("New Connection…", NewConnection),
                separator(),
                MenuItem::action("Open SQL File…", OpenSqlFile),
                MenuItem::action("Save", SaveFile),
                MenuItem::action("Save As…", SaveFileAs),
                command("revert_file"),
                command("copy_tab_file_path"),
                command("reveal_tab_file"),
                separator(),
                MenuItem::action("Export…", ExportData),
                MenuItem::action("Import CSV…", ImportCsv),
                MenuItem::action("Back Up Database…", BackupDatabase),
                MenuItem::action("Restore Database…", RestoreDatabase),
                separator(),
                MenuItem::action("Import Connections…", ImportConnections),
                MenuItem::action("Export Connections…", ExportConnections),
                separator(),
                MenuItem::action("Close Tab", CloseTab),
            ],
        ),
        menu(
            "Edit",
            vec![
                MenuItem::action("Find…", FindInEditor),
                MenuItem::action("Replace…", ReplaceInEditor),
                separator(),
                MenuItem::action("Toggle Line Comment", ToggleComment),
                command("toggle_block_comment"),
                MenuItem::action("Format SQL", FormatSql),
                command("upper_case"),
                command("lower_case"),
                command("show_completions"),
                command("toggle_soft_wrap"),
                command("select_all_occurrences"),
                command("unfold_all"),
                separator(),
                command("open_table_at_cursor"),
                command("open_structure_at_cursor"),
                command("copy_selected_query"),
                command("refresh_completion_metadata"),
            ],
        ),
        menu(
            "View",
            vec![
                MenuItem::action("Command Palette…", OpenCommandPalette),
                command("quick_open"),
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                command("toggle_main_toolbar"),
                command("focus_navigator_filter"),
                command("link_with_editor"),
                separator(),
                MenuItem::action("Follow System Appearance", UseSystemTheme),
                MenuItem::action("Light Appearance", UseLightTheme),
                MenuItem::action("Dark Appearance", UseDarkTheme),
                separator(),
                command("zoom_in"),
                command("zoom_out"),
                command("zoom_reset"),
            ],
        ),
        menu(
            "Query",
            vec![
                MenuItem::action("Run Statement", RunStatement),
                MenuItem::action(
                    "Run Statement in New Result Tab",
                    actions::RunStatementInNewTab,
                ),
                MenuItem::action("Run Script From the Cursor", actions::RunFromCursor),
                MenuItem::action("Run All Statements", RunAll),
                MenuItem::action("Count Rows of Statement", actions::CountStatementRows),
                command("export_from_query"),
                separator(),
                MenuItem::action("Explain Statement", ExplainStatement),
                MenuItem::action("Explain Analyze", ExplainAnalyze),
                separator(),
                MenuItem::action("Go to Next Statement", actions::NextStatement),
                MenuItem::action("Go to Previous Statement", actions::PreviousStatement),
                separator(),
                MenuItem::action("Toggle Results Panel", actions::ToggleResultsPanel),
                MenuItem::action("Maximize Results Panel", actions::MaximizeResultsPanel),
                MenuItem::action("Focus Results / Editor", actions::SwitchResultsFocus),
                command("next_result_tab"),
                command("previous_result_tab"),
                command("toggle_pinned_result"),
                command("close_result_tab"),
                command("fetch_more_rows"),
                command("fetch_all_rows"),
                separator(),
                MenuItem::action("Cancel Running Statement", CancelQuery),
                separator(),
                MenuItem::action("Query History", ShowHistory),
                MenuItem::action("Saved Queries", ShowSavedQueries),
                MenuItem::action("Save Query…", SaveQuery),
            ],
        ),
        menu(
            "Data",
            vec![
                command("refresh_data"),
                command("toggle_auto_refresh"),
                command("calculate_row_count"),
                command("filter_distinct_values"),
                command("clear_filters"),
                separator(),
                command("add_row"),
                command("duplicate_rows"),
                command("delete_rows"),
                command("set_null"),
                command("set_default"),
                command("copy_from_row_above"),
                command("copy_from_row_below"),
                command("revert_cells"),
                command("undo_change"),
                separator(),
                command("apply_changes"),
                command("discard_changes"),
                separator(),
                command("copy_with_column_names"),
                command("copy_as_csv"),
                command("copy_as_json"),
                command("copy_as_markdown"),
                command("copy_as_sql_insert"),
                command("view_cell_value"),
                command("copy_row_numbers"),
                separator(),
                command("toggle_record_view"),
                command("first_row"),
                command("previous_row"),
                command("next_row"),
                command("last_row"),
                command("go_to_row"),
                command("go_to_column"),
                separator(),
                command("find_in_results"),
                command("find_next_in_results"),
                command("find_previous_in_results"),
                separator(),
                command("toggle_selection_stats"),
                command("show_aggregate_panel"),
                command("show_columns_panel"),
                command("reset_table_layout"),
            ],
        ),
        // The connection of the active tab, as DBeaver's Database menu.
        menu(
            "Database",
            vec![
                command("connect"),
                MenuItem::action("Disconnect", actions::DisconnectTabConnection),
                MenuItem::action("Reconnect", actions::ReconnectTabConnection),
                MenuItem::action("Disconnect Others", actions::DisconnectOtherConnections),
                MenuItem::action("Disconnect All", actions::DisconnectAll),
                command("edit_tab_connection"),
                separator(),
                MenuItem::action("Read-Only Connection", actions::ToggleReadOnly),
                separator(),
                MenuItem::action("Commit Transaction", actions::CommitTransaction),
                MenuItem::action("Roll Back Transaction", actions::RollbackTransaction),
                MenuItem::action(
                    "Switch Between Auto-Commit and Manual Commit",
                    actions::ToggleCommitMode,
                ),
                command("transaction_log"),
                separator(),
                command("open_admin"),
                command("lock_manager"),
                command("maintenance_tools"),
                MenuItem::submenu(menu(
                    "Tools",
                    vec![
                        MenuItem::action("Back Up Database…", BackupDatabase),
                        command("global_backup"),
                        MenuItem::action("Restore Database…", RestoreDatabase),
                        command("execute_script"),
                        separator(),
                        command("foreign_data_wrappers"),
                        separator(),
                        command("create_task"),
                        command("show_tasks"),
                    ],
                )),
                separator(),
                MenuItem::submenu(menu(
                    "Connections",
                    vec![
                        command("open_connection"),
                        command("edit_connection"),
                        command("duplicate_connection"),
                        command("delete_connection"),
                        separator(),
                        command("new_connection_folder"),
                        command("refresh_connections"),
                        command("collapse_connection_folders"),
                    ],
                )),
                MenuItem::submenu(menu(
                    "Schema Tree",
                    vec![
                        command("refresh_schema_tree"),
                        command("collapse_schema_tree"),
                        separator(),
                        command("edit_object"),
                        command("rename_object"),
                        command("drop_object"),
                        command("refresh_object"),
                        command("copy_object_name"),
                        command("copy_qualified_name"),
                        command("new_query_on_object"),
                        command("set_default_schema"),
                        command("read_object_in_console"),
                        command("generate_routine_call"),
                        command("execute_routine"),
                        command("add_column_to_object"),
                        command("backup_object"),
                        separator(),
                        command("hide_object_in_tree"),
                        command("show_only_object_in_tree"),
                        command("configure_object_filter"),
                        command("toggle_object_filter"),
                        command("clear_object_filter"),
                    ],
                )),
            ],
        ),
        menu(
            "Window",
            vec![
                command("next_tab"),
                command("previous_tab"),
                separator(),
                command("rename_tab"),
                command("duplicate_tab"),
                command("reopen_closed_tab"),
                separator(),
                command("close_other_tabs"),
                command("close_tabs_to_the_left"),
                command("close_tabs_to_the_right"),
                command("close_all_tabs"),
            ],
        ),
        menu(
            "Help",
            vec![
                command("open_command_palette"),
                command("open_keymap_file"),
                separator(),
                command("check_for_updates"),
                command("about"),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids of the commands the menu items run, through the submenus,
    /// and whether each is run by its own action (its keys shown).
    fn ids(items: &[MenuItem], found: &mut Vec<(String, bool)>) {
        for item in items {
            match item {
                MenuItem::Action { action, .. } => {
                    match action.as_any().downcast_ref::<actions::RunCommand>() {
                        Some(run) => found.push((run.id.clone(), false)),
                        None => found.extend(
                            actions::COMMANDS
                                .iter()
                                .filter(|command| (command.action)().name() == action.name())
                                .map(|command| (command.id.to_string(), true)),
                        ),
                    }
                }
                MenuItem::Submenu(menu) => ids(&menu.items, found),
                _ => {}
            }
        }
    }

    #[test]
    fn every_command_is_in_a_menu() {
        let mut found = Vec::new();
        for menu in menus() {
            ids(&menu.items, &mut found);
        }
        let missing: Vec<&str> = actions::COMMANDS
            .iter()
            .map(|command| command.id)
            .filter(|id| !found.iter().any(|(found, _)| found == id))
            .collect();
        assert_eq!(missing, Vec::<&str>::new());
        // Backspace drops in the tree only: the menu does not take it.
        assert!(found.contains(&("drop_object".to_string(), false)));
        assert!(found.contains(&("revert_cells".to_string(), false)));
        assert!(found.contains(&("commit_transaction".to_string(), true)));
        assert!(found.contains(&("quick_open".to_string(), true)));
    }
}
