//! Kerno: a fast, native database client.

mod actions;
mod admin;
mod ai_settings_view;
mod ai_view;
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
mod erd_tab;
mod export_dialog;
mod extensions_tab;
mod fdw_dialog;
mod folder_dialog;
mod import_dialog;
mod library;
mod local_clients_view;
mod logging;
mod maintenance_dialog;
#[cfg(test)]
mod menu_palette;
mod multi_run;
mod navigator;
mod open_url;
mod portability;
mod query;
mod quick_open;
mod settings;
mod settings_ui;
mod settings_view;
mod shortcuts_view;
mod sidebar;
mod sql_confirm;
mod start_failure;
mod state;
mod structure;
mod tabs;
mod tasks_view;
mod theme;
mod transfer_dialog;
#[cfg(test)]
mod ui_tests;
mod update;
mod workspace;

use std::io::Write as _;
use std::os::unix::net::UnixListener;

use gpui_kit::component::{TitleBar, WindowExt as _};
use gpui_kit::{App, AppContext as _, Focusable as _, Menu, MenuItem, WindowOptions, px, size};
use kerno_core::config::{Secret, parse_connection_url};
use kerno_core::instance::{self, Invocation};
use kerno_core::storage::AppPaths;

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

/// An error notification (C14). One line: as the kit draws it. More (the
/// server's message, detail and hint after Kerno's own words): the first line
/// as its title, the rest below in the selectable text a connection failure
/// uses, to copy a hint; it stays until closed, the hint often being the fix.
pub(crate) fn error_notification(text: String) -> gpui_kit::component::notification::Notification {
    use gpui_kit::IntoElement as _;
    use gpui_kit::component::notification::{Notification, NotificationType};
    use std::sync::atomic::{AtomicUsize, Ordering};
    // The kit's notifications have no label to read their text by.
    #[cfg(test)]
    ERRORS_SHOWN.with(|shown| shown.borrow_mut().push(text.clone()));
    let Some((first, rest)) = text.split_once('\n') else {
        return Notification::error(gpui_kit::SharedString::from(text));
    };
    // The text's element id, unique among the notifications on screen.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let id = format!("error-note-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let rest = rest.to_string();
    Notification::new()
        .with_type(NotificationType::Error)
        .title(gpui_kit::SharedString::from(first.to_string()))
        .content(move |_, _, cx| {
            crate::connections::dialog::raw_message(&id, &rest, cx).into_any_element()
        })
        .autohide(false)
}

#[cfg(test)]
thread_local! {
    /// The texts of [`error_notification`]s made on this thread (a test's).
    pub(crate) static ERRORS_SHOWN: std::cell::RefCell<Vec<String>> = Default::default();
}

/// An error's text with Kerno's advice after its first line (the message),
/// before the server's detail and hint (C14).
pub(crate) fn advice_after(error: &str, advice: &str) -> String {
    match error.split_once('\n') {
        Some((message, more)) => format!("{message} {advice}\n{more}"),
        None => format!("{error} {advice}"),
    }
}

/// What `kerno --help` prints.
const USAGE: &str = "\
Kerno, a fast, native database client.

Usage: kerno [URL]
       kerno --version
       kerno --help

URL opens a connection: postgres://, postgresql://, mysql:// or mariadb://,
for example `kerno postgres://ada@localhost:5432/shop`. A saved connection to
the same engine, host, port, user and database is used when there is one;
otherwise the connection opens unsaved, and its menu offers \"Save
connection…\". When Kerno is already running, the URL opens in its window.

Leave the password out of the URL: other programs of your user can read a
command line (`ps`). Kerno asks for it when the server wants one.
";

// The order matters: `ssh` runs Kerno with its prompt as the argument, so
// the askpass answer comes before the arguments are read; `--version` and
// `--help` exit before anything needs a display, a GPU, the keychain, the
// data directory or the network (Linux packages are checked that way in
// containers); bench modes start after them; a URL goes to the running
// instance before this one reads its data.
fn main() {
    // Run by `ssh` to ask a question for an OpenSSH tunnel: answer, end.
    if let Some(code) = kerno_core::openssh::answer_askpass() {
        std::process::exit(code);
    }
    let url = match instance::parse_args(std::env::args().skip(1)) {
        Invocation::Run => None,
        Invocation::Open(url) => Some(url),
        Invocation::Version => {
            let _ = writeln!(std::io::stdout(), "kerno {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Invocation::Help => {
            let _ = write!(std::io::stdout(), "{USAGE}");
            return;
        }
        Invocation::Invalid(message) => {
            eprintln!("kerno: {message}\nTry `kerno --help`.");
            std::process::exit(2);
        }
    };
    bench::init();
    // The only place that looks the data directory up: everything below is
    // handed these paths.
    let paths = AppPaths::locate();
    // One running instance: it opens the URL and comes to the front.
    if let Some(url) = &url
        && let Ok(paths) = &paths
        && instance::send(&instance::socket_path(&paths.data_dir), url.expose()).is_ok()
    {
        return;
    }
    // The log first (K8): it needs only the paths, so every start failure
    // below is in it. Without it Kerno still runs, and says why on stderr.
    let log = paths
        .as_ref()
        .ok()
        .and_then(|paths| match logging::start(&paths.logs()) {
            Ok(guard) => Some(guard),
            Err(error) => {
                eprintln!("kerno: no log this run: {error:#}");
                None
            }
        });
    // Kerno's own files are read on another thread while the platform
    // starts up, so the first frame has the saved tabs and the right theme
    // without the UI thread ever touching the disk. The instance socket
    // lives in the data directory in use (a `KERNO_DATA_DIR` run has its
    // own).
    let paths = paths.map_err(|error| start_failure::NoDataDir(error.to_string()));
    let loading = paths
        .clone()
        .map(|paths| std::thread::spawn(move || load(&paths)));

    gpui_kit::application()
        .with_assets(engine_logo::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            if let Some(log) = log {
                logging::keep(log, cx);
            }
            // One window at a time (the main one, or the start failure's):
            // closing the last one ends the app.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let loaded = loading.map_err(anyhow::Error::new).and_then(|loading| {
                loading
                    .join()
                    .map_err(|_| anyhow::anyhow!("reading Kerno's data panicked"))?
            });
            // A start that fails shows why, and offers what helps (K8).
            let paths = paths.ok();
            match loaded {
                Ok((startup, listener)) => {
                    if let Err(error) = start(startup, listener, url, cx) {
                        start_failure::open(&error, paths, None, cx);
                    }
                }
                Err(error) => start_failure::open(&error, paths, url, cx),
            }
        });
}

/// The instance socket: `Some` when this instance holds it.
type Listener = std::io::Result<Option<UnixListener>>;
/// What [`load`] reads before the first frame.
type Loaded = (Startup, Listener);

/// Opens Kerno's data under `paths` and takes the instance socket; off the
/// UI thread.
fn load(paths: &AppPaths) -> anyhow::Result<Loaded> {
    Startup::load(paths).map(|startup| {
        let socket = instance::socket_path(&startup.paths.data_dir);
        let listener = instance::listen(&socket);
        // What a run that crashed left of its unsaved connections (J4);
        // only by the instance that holds the socket: a second one
        // beside it must not touch the first one's.
        if matches!(listener, Ok(Some(_)))
            && let Err(error) = startup.storage.forget_unknown_connections()
        {
            tracing::warn!(%error, "could not forget the unsaved connections");
        }
        (startup, listener)
    })
}

fn start(
    startup: Startup,
    listener: Listener,
    url: Option<Secret>,
    cx: &mut App,
) -> anyhow::Result<()> {
    // Without the socket this window still works; later URLs open a window
    // of their own.
    let listener = listener.unwrap_or_else(|error| {
        tracing::warn!(%error, "the instance socket could not be opened");
        None
    });

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
    // Where Settings → AI's Import and Detect look, on click only (K23).
    cx.set_global(ai_settings_view::AiSources::of_this_computer());
    // The menu bar was set by `bind_keys`, which builds it again whenever
    // the shortcuts change (K20).

    // Where it was last time (K28), else centred.
    let (window_bounds, display_id) = crate::state::window_bounds(startup.window.as_ref(), cx);
    let options = WindowOptions {
        window_bounds: Some(window_bounds),
        display_id,
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
        unreadable_connections,
        tabs,
        active_tab,
        tab_layout,
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
            // Password files and commands (J3): the user's PATH (plus
            // Homebrew's), HOME and PGPASSFILE, read once here.
            connections.set_password_env(Some(kerno_core::password::PasswordEnv::detect(
                std::env::var_os("PATH").as_deref(),
                std::env::var_os("HOME").map(std::path::PathBuf::from),
                std::env::var_os("PGPASSFILE").map(std::path::PathBuf::from),
            )));
            connections
        });
        let workspace = cx.new(|cx| {
            Workspace::new(
                connections.clone(),
                tabs,
                active_tab,
                tab_layout,
                window,
                cx,
            )
        });
        workspace::handle_quit(window.window_handle(), workspace.downgrade(), cx);
        bench::begin(&workspace, &connections, window, cx);
        if let Some(listener) = listener {
            open_url::listen(listener, &workspace, &connections, window, cx);
            // The connections opened from a URL and never saved leave no
            // history or layout behind (J4); only this instance had them.
            cx.on_app_quit(|cx| {
                let storage = AppState::get(cx).storage.clone();
                bridge::spawn_blocking(cx, move || {
                    if let Err(error) = storage.forget_unknown_connections() {
                        tracing::warn!(%error, "could not forget the unsaved connections");
                    }
                })
            })
            .detach();
        }
        if let Some(url) = url.as_ref().and_then(|url| {
            tracing::info!(url = %instance::redact_url(url.expose()), "opening a URL");
            parse_connection_url(url.expose()).ok()
        }) {
            let (workspace, connections) = (workspace.clone(), connections.clone());
            window.defer(cx, move |window, cx| {
                open_url::open(&workspace, &connections, &url, window, cx)
            });
        }
        workspace.update(cx, |workspace, cx| {
            workspace.start_update_checks(window, cx)
        });
        workspace.focus_handle(cx).focus(window, cx);
        for problem in keymap_problems {
            window.push_notification(format!("{}: {problem}", keymap_file.display()), cx);
        }
        // Once per start (K8, C16).
        if let Some(text) = state::unreadable_notice(unreadable_connections) {
            window.push_notification(
                gpui_kit::component::notification::Notification::warning(
                    gpui_kit::SharedString::from(text),
                )
                .autohide(false),
                cx,
            );
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
        Some("Navigator" | "ConnectionList" | "DataTable" | "Settings")
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

/// The native menu bar: every command of `actions::COMMANDS` is in it. Its
/// shortcut column is read from the key bindings when it is set:
/// `actions::apply_keymap` sets it again after each change.
fn menus() -> Vec<Menu> {
    let separator = MenuItem::separator;
    vec![
        menu(
            "Kerno",
            vec![
                MenuItem::action("About Kerno", About),
                separator(),
                MenuItem::action("Settings…", OpenSettings),
                command("search_settings"),
                command("open_keyboard_shortcuts"),
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
                command("apply_source"),
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
                command("toggle_vim_mode"),
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
                command("toggle_theme"),
                separator(),
                command("zoom_in"),
                command("zoom_out"),
                command("zoom_reset"),
                separator(),
                command("tabs_on_one_row"),
                command("tabs_on_several_rows"),
                command("tabs_at_the_side"),
                command("show_all_tabs"),
                command("toggle_stacked_results"),
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
                command("run_script_on_connections"),
                command("show_multi_run"),
                separator(),
                MenuItem::action("Explain Statement", ExplainStatement),
                MenuItem::action("Explain Analyze", ExplainAnalyze),
                // J17: off until turned on in Settings → AI.
                MenuItem::submenu(menu(
                    "AI Assistant",
                    vec![
                        command("ai_write_sql"),
                        command("ai_explain_query"),
                        command("ai_explain_plan"),
                        command("ai_explain_error"),
                    ],
                )),
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
                command("close_other_result_tabs"),
                command("close_result_tabs_to_the_left"),
                command("close_result_tabs_to_the_right"),
                command("close_all_result_tabs"),
                command("unpin_all_result_tabs"),
                command("rename_result_tab"),
                command("copy_result_statement"),
                command("fetch_more_rows"),
                command("fetch_all_rows"),
                separator(),
                MenuItem::action("Cancel Running Statement", CancelQuery),
                command("show_messages"),
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
                command("toggle_value_panel"),
                command("show_aggregate_panel"),
                command("show_columns_panel"),
                command("reset_table_layout"),
                command("copy_column_name"),
                command("revert_selected_rows"),
                command("show_all_columns"),
                command("fit_all_column_widths"),
                command("generate_select_by_key"),
                command("generate_select_in"),
                command("generate_row_insert"),
                command("generate_row_update"),
                command("generate_row_delete"),
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
                        command("copy_tables"),
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
                        command("new_connection_in_folder"),
                        command("new_subfolder"),
                        command("disconnect_folder"),
                        command("paste_connection"),
                        command("copy_connection_url"),
                        command("copy_connection_host"),
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
                        command("view_diagram"),
                        command("generate_routine_call"),
                        command("execute_routine"),
                        command("add_column_to_object"),
                        command("backup_object"),
                        command("open_object_data"),
                        command("open_object_structure"),
                        command("show_object_ddl"),
                        command("edit_object_source"),
                        command("generate_select"),
                        command("generate_insert"),
                        command("generate_update"),
                        command("generate_delete"),
                        command("drop_object_cascade"),
                        command("create_table"),
                        command("create_view"),
                        command("create_schema"),
                        command("create_database"),
                        command("create_function"),
                        command("create_procedure"),
                        command("create_sequence"),
                        command("create_trigger"),
                        command("create_event"),
                        command("create_enum"),
                        command("add_enum_value"),
                        command("rename_enum_value"),
                        command("install_extension"),
                        command("export_object"),
                        command("import_csv_into_object"),
                        command("rename_column"),
                        command("drop_column"),
                        command("reveal_referenced_table"),
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
                command("split_right"),
                command("split_down"),
                command("move_tab_to_next_group"),
                command("move_tab_to_previous_group"),
                command("focus_next_group"),
                command("focus_previous_group"),
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
