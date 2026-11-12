//! Kerno: a fast, native database client.

mod actions;
mod bridge;
mod connections;
mod data;
mod engine_logo;
mod library;
mod logging;
mod navigator;
mod query;
mod settings;
mod sidebar;
mod sql_confirm;
mod state;
mod tabs;
mod theme;
#[cfg(test)]
mod ui_tests;
mod workspace;

use gpui_kit::component::{TitleBar, WindowExt as _};
use gpui_kit::{
    App, AppContext as _, Bounds, Focusable as _, Menu, MenuItem, WindowBounds, WindowOptions, px,
    size,
};

use crate::actions::{
    About, CancelQuery, CloseTab, ExplainAnalyze, ExplainStatement, ExportConnections,
    FindInEditor, ImportConnections, NewConnection, NewQueryTab, OpenCommandPalette, OpenSettings,
    OpenSqlFile, Quit, ReplaceInEditor, RunAll, RunStatement, SaveFile, SaveFileAs, SaveQuery,
    ShowHistory, ShowSavedQueries, ToggleComment, ToggleSidebar, UseDarkTheme, UseLightTheme,
    UseSystemTheme,
};
use crate::connections::manager::{Connections, Drivers};
use crate::state::{AppState, Startup};
use crate::theme::Appearance;
use crate::workspace::Workspace;

fn main() {
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

    let keymap_problems = actions::bind_keys(&startup.paths.keymap(), cx);
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
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
        // The user's own known_hosts is read, never written: keys Kerno
        // learns go to its own file.
        let user_known_hosts = std::env::var_os("HOME")
            .map(|home| std::path::PathBuf::from(home).join(".ssh/known_hosts"));
        let drivers = Drivers::default()
            .with(kerno_postgres::PostgresDriver)
            .with(kerno_mysql::MysqlDriver);
        let connections = cx.new(|_| Connections::new(connections, drivers, user_known_hosts));
        let workspace = cx.new(|cx| Workspace::new(connections, tabs, active_tab, window, cx));
        workspace.focus_handle(cx).focus(window, cx);
        for problem in keymap_problems {
            window.push_notification(format!("{}: {problem}", keymap_file.display()), cx);
        }
        workspace
    })?;
    cx.activate(true);
    Ok(())
}

/// The native menu bar.
fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Kerno".into(),
            items: vec![
                MenuItem::action("About Kerno", About),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Kerno", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Query Tab", NewQueryTab),
                MenuItem::action("New Connection…", NewConnection),
                MenuItem::separator(),
                MenuItem::action("Open SQL File…", OpenSqlFile),
                MenuItem::action("Save", SaveFile),
                MenuItem::action("Save As…", SaveFileAs),
                MenuItem::separator(),
                MenuItem::action("Import Connections…", ImportConnections),
                MenuItem::action("Export Connections…", ExportConnections),
                MenuItem::separator(),
                MenuItem::action("Close Tab", CloseTab),
            ],
            disabled: false,
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Find…", FindInEditor),
                MenuItem::action("Replace…", ReplaceInEditor),
                MenuItem::separator(),
                MenuItem::action("Toggle Line Comment", ToggleComment),
            ],
            disabled: false,
        },
        Menu {
            name: "Query".into(),
            items: vec![
                MenuItem::action("Run Statement", RunStatement),
                MenuItem::action("Run All Statements", RunAll),
                MenuItem::separator(),
                MenuItem::action("Explain Statement", ExplainStatement),
                MenuItem::action("Explain Analyze", ExplainAnalyze),
                MenuItem::separator(),
                MenuItem::action("Cancel Running Statement", CancelQuery),
                MenuItem::separator(),
                MenuItem::action("Query History", ShowHistory),
                MenuItem::action("Saved Queries", ShowSavedQueries),
                MenuItem::action("Save Query…", SaveQuery),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Command Palette…", OpenCommandPalette),
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::separator(),
                MenuItem::action("Follow System Appearance", UseSystemTheme),
                MenuItem::action("Light Appearance", UseLightTheme),
                MenuItem::action("Dark Appearance", UseDarkTheme),
            ],
            disabled: false,
        },
    ]
}
