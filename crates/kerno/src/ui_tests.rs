//! Tests of the app shell through its real views, in a headless window:
//! key presses, clicks and typed text go through GPUI's dispatch, and the
//! assertions read the app's state and storage.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::base::Root;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
use gpui_kit::{
    AppContext as _, Bounds, Entity, Focusable as _, Point, TestAppContext, WindowBounds,
    WindowHandle, WindowOptions, px, size,
};
use kerno_core::config::{Secret, Secrets};
use kerno_core::secrets::MemorySecretStore;
use kerno_core::storage::{AppPaths, Storage};
use kerno_core::{ConnectionConfig, Engine, testing};

use gpui_kit::component::table::{ColumnSort, TableDelegate as _, TableEvent, TableState};
use kerno_core::browse::{Condition, Filter, Operator};
use kerno_core::schema::{RelationKind, TableRef};

use crate::connections::manager::{Connections, Drivers, Status};
use crate::data::tab::DataTab;
use crate::navigator::actions::{Action, Template};
use crate::query::grid::Grid;
use crate::query::tab::Activity;
use crate::settings::Settings;
use crate::sidebar::{Sidebar, SidebarAction, Subject};
use crate::state::AppState;
use crate::structure::editor::TableEditor;
use crate::structure::tab::{DDL_SECTION, List, StructureTab};
use crate::tabs::TabSnapshot;
use crate::tabs::Target;
use crate::theme::{self, Appearance};
use crate::workspace::Workspace;
use crate::{actions, bridge};

struct Fixture {
    window: WindowHandle<Root>,
    workspace: Entity<Workspace>,
    connections: Entity<Connections>,
    storage: Arc<Storage>,
    /// Kerno's data directory for this run (its SSH known-hosts file).
    _data_dir: Arc<tempfile::TempDir>,
}

/// Starts the app's globals on an in-memory store and opens a workspace.
fn open(cx: &mut TestAppContext, storage: Arc<Storage>) -> Fixture {
    // Storage work runs on real threads; let the test clock wait for them.
    cx.executor().allow_parking();
    let data_dir = Arc::new(tempfile::tempdir().unwrap());
    cx.update(|cx| {
        gpui_kit::init(cx);
        bridge::init(cx).unwrap();
        let settings = Settings::load(&storage);
        let appearance = match settings.appearance {
            // The headless platform has no system appearance to follow.
            Appearance::System => Appearance::Light,
            other => other,
        };
        cx.set_global(AppState {
            paths: AppPaths::under(data_dir.path().to_path_buf()),
            storage: storage.clone(),
            secrets: Arc::new(MemorySecretStore::default()),
            settings,
            appearance_override: None,
        });
        theme::init(appearance, cx).unwrap();
        let problems = actions::bind_keys(Path::new("/nonexistent/kerno-tests/keymap.json"), cx);
        assert_eq!(problems, Vec::<String>::new());

        let saved = storage.connections().unwrap();
        let drivers = Drivers::default()
            .with(kerno_postgres::PostgresDriver)
            .with(kerno_mysql::MysqlDriver);
        // No user known_hosts: tests never read the developer's SSH files.
        let connections = cx.new(|_| Connections::new(saved, drivers, None));
        let tabs = storage.tabs().unwrap();
        let active_tab = storage
            .setting(crate::state::ACTIVE_TAB_KEY)
            .unwrap()
            .unwrap_or(0);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(1200.), px(800.)),
            })),
            ..Default::default()
        };
        let manager = connections.clone();
        let (window, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
            let workspace = cx.new(|cx| Workspace::new(connections, tabs, active_tab, window, cx));
            workspace.focus_handle(cx).focus(window, cx);
            workspace
        })
        .unwrap();
        Fixture {
            window: window.downcast::<Root>().unwrap(),
            workspace,
            connections: manager,
            storage,
            _data_dir: data_dir.clone(),
        }
    })
}

fn memory_storage() -> Arc<Storage> {
    Arc::new(Storage::in_memory().unwrap())
}

impl Fixture {
    fn titles(&self, cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|cx| self.workspace.read(cx).tab_titles(cx))
    }

    fn active(&self, cx: &mut TestAppContext) -> Option<usize> {
        cx.update(|cx| self.workspace.read(cx).active_tab_index())
    }

    /// Runs one interaction in the window, then lets its effects (focus
    /// changes, deferred actions, new frames) settle, as happens between
    /// two real input events.
    fn step<R>(
        &self,
        cx: &mut TestAppContext,
        interaction: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R,
    ) -> R {
        let result = cx
            .update_window(self.window.into(), |_, window, cx| {
                window.render_frame(cx);
                interaction(window, cx)
            })
            .unwrap();
        cx.run_until_parked();
        result
    }

    /// Presses keys one after the other.
    fn press(&self, cx: &mut TestAppContext, keys: &[&str]) {
        for key in keys {
            self.step(cx, |window, cx| window.press(key, cx));
        }
    }

    /// Types text into whatever has the focus.
    fn type_text(&self, cx: &mut TestAppContext, text: &str) {
        self.step(cx, |window, cx| window.input(text, cx));
    }

    /// Clicks the element with this id.
    fn click(&self, cx: &mut TestAppContext, id: impl Into<gpui_kit::ElementId>) {
        self.step(cx, |window, cx| window.click(id, cx));
    }

    fn has(&self, cx: &mut TestAppContext, id: impl Into<gpui_kit::ElementId>) -> bool {
        self.step(cx, |window, _| window.try_find(id).is_some())
    }

    /// Lets real threads (network, storage) make progress until `done`.
    fn wait_until(
        &self,
        cx: &mut TestAppContext,
        what: &str,
        mut done: impl FnMut(&Self, &mut TestAppContext) -> bool,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            cx.run_until_parked();
            if done(self, cx) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn status(&self, cx: &mut TestAppContext, index: usize) -> Status {
        cx.update(|cx| {
            let connections = self.connections.read(cx);
            connections.status(&connections.saved()[index].id)
        })
    }

    /// The rows of the schema tree, indented by depth.
    fn tree(&self, cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|cx| self.workspace.read(cx).navigator(cx).read(cx).outline(cx))
    }

    /// The row of the schema tree reached by these labels, from the root.
    fn tree_row(&self, cx: &mut TestAppContext, path: &[&str]) -> usize {
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .navigator(cx)
                .read(cx)
                .row_of(path, cx)
        })
        .unwrap_or_else(|| panic!("no row {path:?} in {:#?}", self.tree(cx)))
    }

    /// Does what a context-menu entry does for a row of the schema tree.
    fn act(&self, cx: &mut TestAppContext, action: Action, path: &[&str]) {
        let row = self.tree_row(cx, path);
        let navigator = cx.update(|cx| self.workspace.read(cx).navigator(cx));
        self.step(cx, |window, cx| {
            navigator.update(cx, |navigator, cx| {
                let id = navigator.id_at(row, cx);
                navigator.perform(action, &id, window, cx);
            })
        });
    }

    /// Why a context-menu entry is disabled on a row, when it is.
    fn why_not(
        &self,
        cx: &mut TestAppContext,
        action: Action,
        path: &[&str],
    ) -> Option<&'static str> {
        let row = self.tree_row(cx, path);
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .navigator(cx)
                .read(cx)
                .why_not(action, row, cx)
        })
    }

    /// Title and text of the active query tab.
    fn active_query(&self, cx: &mut TestAppContext) -> (String, String) {
        cx.update(|cx| {
            let tab = self
                .workspace
                .read(cx)
                .active_query_tab()
                .expect("an open tab")
                .read(cx);
            (tab.title().to_string(), tab.sql(cx))
        })
    }

    /// What the statements of the active tab's last run produced.
    fn outcomes(&self, cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|cx| {
            let workspace = self.workspace.read(cx);
            workspace
                .active_query_tab()
                .map(|tab| tab.read(cx).outcomes(cx))
                .unwrap_or_default()
        })
    }

    /// Columns and first rows of a result of the active tab.
    fn result(
        &self,
        cx: &mut TestAppContext,
        index: usize,
        limit: usize,
    ) -> (Vec<String>, Vec<Vec<Option<String>>>) {
        cx.update(|cx| {
            let workspace = self.workspace.read(cx);
            workspace
                .active_query_tab()
                .expect("an open tab")
                .read(cx)
                .rows_of(index, limit, cx)
        })
    }

    /// Whether the active tab is waiting for the server.
    fn running(&self, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            let workspace = self.workspace.read(cx);
            workspace
                .active_query_tab()
                .is_some_and(|tab| matches!(tab.read(cx).activity(), Activity::Running { .. }))
        })
    }

    /// The active query tab.
    fn tab(&self, cx: &mut TestAppContext) -> Entity<crate::query::tab::QueryTab> {
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .active_query_tab()
                .expect("an open tab")
                .clone()
        })
    }

    /// Opens the data of a table of the first connection.
    fn open_table(&self, cx: &mut TestAppContext, schema: &str, table: &str, kind: RelationKind) {
        let connection = cx.update(|cx| self.connections.read(cx).saved()[0].id.clone());
        let target = Target {
            connection,
            database: None,
            schema: None,
        };
        let table = TableRef::new(schema, table);
        self.step(cx, |window, cx| {
            self.workspace.update(cx, |workspace, cx| {
                workspace.open_data(target, table, kind, Filter::default(), window, cx);
            })
        });
    }

    /// The active tab, which shows a table's data.
    fn data(&self, cx: &mut TestAppContext) -> Entity<DataTab> {
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .active_data_tab()
                .expect("a data tab")
                .clone()
        })
    }

    /// Does something to the active data tab.
    fn on_data(
        &self,
        cx: &mut TestAppContext,
        act: impl FnOnce(&mut DataTab, &mut gpui_kit::Window, &mut gpui_kit::Context<DataTab>),
    ) {
        let tab = self.data(cx);
        self.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| act(tab, window, cx))
        });
    }

    /// Where the active data tab is with its rows ("Rows 1–3 of 3").
    fn data_state(&self, cx: &mut TestAppContext) -> String {
        let tab = self.data(cx);
        cx.update(|cx| tab.read(cx).state(cx))
    }

    /// Waits for the active data tab to show these rows.
    fn wait_for_rows(&self, cx: &mut TestAppContext, state: &str) {
        self.wait_until(cx, state, |app, cx| {
            let now = app.data_state(cx);
            assert!(!now.starts_with("failed"), "{now}");
            now == state
        });
    }

    /// The grid of the active data tab.
    fn data_grid(&self, cx: &mut TestAppContext) -> Entity<TableState<Grid>> {
        let tab = self.data(cx);
        cx.update(|cx| tab.read(cx).grid()).expect("a grid")
    }

    /// What the grid of the active data tab draws.
    fn data_rows(&self, cx: &mut TestAppContext) -> Vec<Vec<String>> {
        let grid = self.data_grid(cx);
        cx.update(|cx| grid.read(cx).delegate().shown())
    }

    /// Rows of the active data tab with changes waiting.
    fn pending(&self, cx: &mut TestAppContext) -> usize {
        let tab = self.data(cx);
        cx.update(|cx| tab.read(cx).pending_count(cx))
    }

    /// Types `text` in a cell of the active data tab, in place of what it
    /// holds, and ends with `key`.
    fn type_in_cell(
        &self,
        cx: &mut TestAppContext,
        row: usize,
        column: usize,
        text: &str,
        key: &str,
    ) {
        self.on_data(cx, |tab, window, cx| tab.edit_cell(row, column, window, cx));
        self.press(cx, &["secondary-a", "backspace"]);
        if !text.is_empty() {
            self.type_text(cx, text);
        }
        self.press(cx, &[key]);
    }

    /// What the status bar says.
    fn status_bar(&self, cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|cx| self.workspace.read(cx).status_text(cx))
    }

    /// Opens a query tab, types `sql` and runs it with `keys`.
    fn run_in_new_tab(&self, cx: &mut TestAppContext, sql: &str, keys: &str) {
        self.press(cx, &["secondary-t"]);
        self.type_text(cx, sql);
        self.press(cx, &[keys]);
    }

    /// Clicks the chevron of a row of the schema tree: opens or closes it.
    /// A row without children has none, and nothing happens.
    fn toggle(&self, cx: &mut TestAppContext, path: &[&str]) {
        let row = self.tree_row(cx, path);
        self.step(cx, |window, cx| {
            let mut tree = window.within("navigator");
            if tree.try_find(("node-chevron", row)).is_some() {
                tree.click(("node-chevron", row), cx)
            }
        });
    }

    /// Clicks a row of the schema tree, once or twice.
    fn click_row(&self, cx: &mut TestAppContext, path: &[&str], double: bool) {
        let row = self.tree_row(cx, path);
        self.step(cx, |window, cx| {
            let mut tree = window.within("navigator");
            if double {
                tree.double_click(("node", row), cx)
            } else {
                tree.click(("node", row), cx)
            }
        });
    }

    /// The row selected in the schema tree.
    fn selected_row(&self, cx: &mut TestAppContext) -> Option<usize> {
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .navigator(cx)
                .read(cx)
                .selected_row(cx)
        })
    }

    /// Opens the context menu of the element with this id with a real
    /// right-click, `offset` from its top-left corner (its centre when `None`).
    fn right_click(
        &self,
        cx: &mut TestAppContext,
        id: impl Into<gpui_kit::ElementId>,
        offset: Option<Point<gpui_kit::Pixels>>,
    ) {
        use gpui_kit::test::ClickOptions;
        let options = ClickOptions::new().with_button(gpui_kit::MouseButton::Right);
        let options = match offset {
            Some(offset) => options.with_offset(offset),
            None => options,
        };
        self.step(cx, |window, cx| window.click_with_options(id, options, cx));
    }

    /// Whether a menu entry (or any element) with this label is shown.
    fn shows_label(&self, cx: &mut TestAppContext, label: &str) -> bool {
        use gpui_kit::test::TestQueryExt as _;
        self.step(cx, |window, _| window.try_find_by_label(label).is_some())
    }

    /// Clicks the element (a menu entry) with this label: a press and a
    /// release where it is drawn.
    fn click_label(&self, cx: &mut TestAppContext, label: &str) {
        use gpui_kit::test::TestQueryExt as _;
        self.step(cx, |window, cx| {
            let centre = window.find_by_label(label).bounds().center();
            window.drag(centre, centre, cx);
        });
    }

    /// Runs a sidebar menu entry, as clicking it does.
    fn sidebar_does(&self, cx: &mut TestAppContext, subject: Subject, action: SidebarAction) {
        let sidebar = cx.update(|cx| self.workspace.read(cx).sidebar());
        self.step(cx, move |window, cx| {
            sidebar.update(cx, |sidebar, cx| {
                sidebar.perform(&subject, action, window, cx)
            })
        });
    }

    /// The stored settings of the first saved connection.
    fn first_stored(&self) -> ConnectionConfig {
        self.storage.connections().unwrap()[0].config.clone()
    }

    /// Waits until the stored tabs satisfy `predicate`.
    async fn wait_for_stored_tabs(
        &self,
        cx: &mut TestAppContext,
        predicate: impl Fn(&[TabSnapshot]) -> bool,
    ) -> Vec<TabSnapshot> {
        let storage = self.storage.clone();
        cx.wait_for(self.window.into(), Duration::from_secs(10), move |_, _| {
            predicate(&storage.tabs::<TabSnapshot>().unwrap())
        })
        .await;
        self.storage.tabs().unwrap()
    }
}

#[gpui_kit::test]
fn tabs_open_switch_and_close_from_the_keyboard(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    assert_eq!(app.titles(cx), Vec::<String>::new());
    assert_eq!(app.active(cx), None);

    app.press(cx, &["secondary-t", "secondary-t", "secondary-t"]);
    assert_eq!(app.titles(cx), ["Query 1", "Query 2", "Query 3"]);
    assert_eq!(app.active(cx), Some(2));

    app.press(cx, &["ctrl-tab"]);
    assert_eq!(app.active(cx), Some(0), "next wraps around");
    app.press(cx, &["ctrl-shift-tab"]);
    assert_eq!(app.active(cx), Some(2), "previous wraps around");
    app.press(cx, &["ctrl-shift-tab"]);
    assert_eq!(app.active(cx), Some(1));

    // Closing the active tab activates the one that takes its place (its
    // right-hand neighbour), as browsers do.
    app.press(cx, &["secondary-w"]);
    assert_eq!(app.titles(cx), ["Query 1", "Query 3"]);
    assert_eq!(app.active(cx), Some(1));

    // Numbers are not reused while a higher one is open.
    app.press(cx, &["secondary-t"]);
    assert_eq!(app.titles(cx), ["Query 1", "Query 3", "Query 4"]);

    app.press(cx, &["secondary-w", "secondary-w", "secondary-w"]);
    assert_eq!(app.titles(cx), Vec::<String>::new());
    // Closing with nothing open does nothing.
    app.press(cx, &["secondary-w"]);
    assert_eq!(app.active(cx), None);
}

#[gpui_kit::test]
fn tabs_close_with_their_button_and_open_from_the_empty_state(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.click(cx, "empty-new-query");
    app.click(cx, "new-tab");
    app.click(cx, "new-tab");
    assert_eq!(app.titles(cx), ["Query 1", "Query 2", "Query 3"]);

    app.click(cx, ("close-tab", 0usize));
    assert_eq!(app.titles(cx), ["Query 2", "Query 3"]);
    assert_eq!(app.active(cx), Some(1), "the active tab stays the same tab");
}

#[gpui_kit::test]
fn command_palette_finds_and_runs_commands(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    // A tab first, so the palette is opened while an editor has the focus.
    app.press(cx, &["secondary-t"]);

    // Open with the shortcut, search by label, confirm with Enter. "close"
    // is not the first command, so this fails if the search is ignored.
    app.press(cx, &["secondary-k"]);
    app.type_text(cx, "close tab");
    app.press(cx, &["enter"]);
    assert_eq!(app.titles(cx), Vec::<String>::new());

    // The palette closed, so the shortcut works again; search by keyword.
    // Tab numbers are never reused within a run, hence "Query 2".
    app.press(cx, &["secondary-k"]);
    app.type_text(cx, "editor");
    app.press(cx, &["enter"]);
    assert_eq!(app.titles(cx), ["Query 2"]);
    let sql = cx.update(|cx| {
        let workspace = app.workspace.read(cx);
        workspace.active_query_tab().map(|tab| tab.read(cx).sql(cx))
    });
    assert_eq!(
        sql.as_deref(),
        Some(""),
        "palette typing must not reach the editor"
    );

    app.press(cx, &["secondary-k"]);
    app.type_text(cx, "theme: dark");
    app.press(cx, &["enter"]);
    cx.update(|cx| {
        assert!(cx.theme().is_dark());
        assert_eq!(AppState::get(cx).settings.appearance, Appearance::Dark);
    });

    // Escape closes the palette without running anything.
    app.press(cx, &["secondary-k"]);
    app.type_text(cx, "close tab");
    // (The first Escape clears the search, the second closes the palette.)
    app.press(cx, &["escape", "escape"]);
    assert_eq!(app.titles(cx), ["Query 2"]);
    // And the keyboard is back in the workspace.
    app.press(cx, &["secondary-t"]);
    assert_eq!(app.titles(cx), ["Query 2", "Query 3"]);
}

#[gpui_kit::test]
async fn unsaved_text_and_tabs_come_back_after_a_restart(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    app.press(cx, &["secondary-t", "secondary-t"]);
    // The new tab's editor has the focus: typing goes into it.
    app.type_text(cx, "select 'not saved anywhere'");

    let stored = app
        .wait_for_stored_tabs(cx, |tabs| {
            matches!(tabs, [_, TabSnapshot::Query { sql, .. }] if sql == "select 'not saved anywhere'")
        })
        .await;
    assert_eq!(
        stored,
        [
            TabSnapshot::Query {
                title: "Query 1".into(),
                sql: String::new(),
                target: None,
                file: None,
                unsaved: false,
                source: None,
            },
            TabSnapshot::Query {
                title: "Query 2".into(),
                sql: "select 'not saved anywhere'".into(),
                target: None,
                file: None,
                unsaved: false,
                source: None,
            },
        ]
    );

    // A second window on the same store plays the part of the next launch.
    let restarted = open(cx, storage);
    assert_eq!(restarted.titles(cx), ["Query 1", "Query 2"]);
    let sql = cx.update(|cx| {
        let workspace = restarted.workspace.read(cx);
        workspace.active_query_tab().map(|tab| tab.read(cx).sql(cx))
    });
    assert_eq!(sql.as_deref(), Some("select 'not saved anywhere'"));
    // New tabs continue the numbering.
    restarted.press(cx, &["secondary-t"]);
    assert_eq!(restarted.titles(cx), ["Query 1", "Query 2", "Query 3"]);

    // Closing a tab is remembered too.
    restarted.press(cx, &["secondary-w", "secondary-w"]);
    restarted
        .wait_for_stored_tabs(cx, |tabs| tabs.len() == 1)
        .await;
}

#[gpui_kit::test]
async fn appearance_and_sidebar_choices_are_applied_and_stored(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    cx.update(|cx| assert!(!cx.theme().is_dark()));
    let sidebar_shown = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, _| window.try_find("sidebar").is_some())
    };
    assert!(sidebar_shown(&app, cx));

    app.press(cx, &["secondary-b"]);
    assert!(!sidebar_shown(&app, cx));

    // Settings dialog: pick Dark.
    app.press(cx, &["secondary-,"]);
    // The radios are numbered inside their group: 0 system, 1 light, 2 dark.
    app.step(cx, |window, cx| {
        window.within("appearance").click(2usize, cx)
    });
    cx.update(|cx| assert!(cx.theme().is_dark()));
    app.step(cx, |window, cx| {
        window.within("appearance").click(1usize, cx)
    });
    cx.update(|cx| assert!(!cx.theme().is_dark()));
    app.step(cx, |window, cx| {
        window.within("appearance").click(2usize, cx)
    });

    let stored = storage.clone();
    cx.wait_for(app.window.into(), Duration::from_secs(10), move |_, _| {
        let settings = Settings::load(&stored);
        settings.appearance == Appearance::Dark && !settings.sidebar_visible
    })
    .await;

    // The next launch starts dark, without the sidebar.
    let restarted = open(cx, storage);
    cx.update(|cx| assert!(cx.theme().is_dark()));
    assert!(!sidebar_shown(&restarted, cx));
}

#[gpui_kit::test]
fn saved_connections_are_listed(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let mut config = ConnectionConfig::new(Engine::Postgres);
    config.name = "Orders (staging)".into();
    storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    assert!(app.step(cx, |window, _| window.find("sidebar").visible()));
}

#[gpui_kit::test]
async fn the_active_tab_is_active_again_after_a_restart(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    app.press(cx, &["secondary-t", "secondary-t", "secondary-t"]);
    // Go back to the first tab; that choice is what must be remembered.
    app.press(cx, &["ctrl-tab"]);
    assert_eq!(app.active(cx), Some(0));
    let stored = storage.clone();
    cx.wait_for(app.window.into(), Duration::from_secs(10), move |_, _| {
        stored.tabs::<TabSnapshot>().unwrap().len() == 3
            && stored
                .setting::<usize>(crate::state::ACTIVE_TAB_KEY)
                .unwrap()
                == Some(0)
    })
    .await;

    let restarted = open(cx, storage.clone());
    assert_eq!(restarted.titles(cx), ["Query 1", "Query 2", "Query 3"]);
    assert_eq!(restarted.active(cx), Some(0));

    // A stored index past the end (tabs were lost) falls back to the last tab.
    storage
        .set_setting(crate::state::ACTIVE_TAB_KEY, &99usize)
        .unwrap();
    let restarted = open(cx, storage);
    assert_eq!(restarted.active(cx), Some(2));
}

// Connections ---------------------------------------------------------------

/// The test PostgreSQL server as a URL, password included.
fn pg_url() -> String {
    format!(
        "postgres://kerno:{}@127.0.0.1:{}/kerno?sslmode=disable",
        testing::DB_PASSWORD,
        testing::port(Engine::Postgres)
    )
}

#[gpui_kit::test]
fn a_connection_is_filled_from_a_url_tested_saved_and_opened(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = open(cx, memory_storage());
    assert!(
        app.has(cx, "empty-new-connection"),
        "the empty sidebar offers to add one"
    );

    app.press(cx, &["secondary-n"]);
    app.click(cx, "conn-url");
    app.type_text(cx, &pg_url());
    app.press(cx, &["enter"]);
    app.click(cx, "conn-name");
    app.type_text(cx, "Shop (local)");

    app.click(cx, "conn-test");
    app.wait_until(cx, "the connection test", |app, cx| {
        assert!(
            !app.has(cx, "conn-test-failed"),
            "the test connection failed"
        );
        app.has(cx, "conn-test-passed")
    });

    app.click(cx, "conn-save-connect");
    assert!(!app.has(cx, "conn-save"), "saving closes the dialog");
    // The connection is saved on a storage thread before it is listed.
    app.wait_until(cx, "the connection", |app, cx| {
        let listed = cx.update(|cx| !app.connections.read(cx).saved().is_empty());
        listed && matches!(app.status(cx, 0), Status::Connected { .. })
    });
    assert!(app.has(cx, ("connection", 0usize)));

    // The settings are stored; the password is in the secret store only.
    let stored = app.storage.connections().unwrap();
    assert_eq!(stored.len(), 1);
    let config = &stored[0].config;
    assert_eq!(
        (
            config.name.as_str(),
            config.host.as_str(),
            config.port,
            config.user.as_str()
        ),
        (
            "Shop (local)",
            "127.0.0.1",
            testing::port(Engine::Postgres),
            "kerno"
        )
    );
    assert!(
        !serde_json::to_string(config)
            .unwrap()
            .contains(testing::DB_PASSWORD)
    );
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let kept = kerno_core::secrets::load_secrets(secrets.as_ref(), &stored[0].id).unwrap();
    assert_eq!(
        kept.password.as_ref().map(|secret| secret.expose()),
        Some(testing::DB_PASSWORD)
    );

    // Its menu closes it again (a double click never does).
    let id = stored[0].id.clone();
    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::Disconnect);
    assert_eq!(app.status(cx, 0), Status::Disconnected);
}

#[gpui_kit::test]
fn a_form_with_errors_is_not_saved(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.click(cx, "empty-new-connection");
    app.click(cx, "conn-port");
    app.type_text(cx, "x");
    app.click(cx, "conn-save");
    assert!(app.has(cx, "conn-field-error"), "the port error is shown");
    assert!(app.has(cx, "conn-save"), "the dialog stays open");
    assert_eq!(app.storage.connections().unwrap().len(), 0);

    app.click(cx, "conn-cancel");
    assert!(!app.has(cx, "conn-save"));
    assert_eq!(app.storage.connections().unwrap().len(), 0);
}

#[gpui_kit::test]
fn a_password_that_is_not_saved_is_asked_for(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let storage = memory_storage();
    let mut config = testing::config(Engine::Postgres);
    config.save_password = false;
    storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);

    // Declining the prompt gives the connection up.
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the password prompt", |app, cx| {
        app.has(cx, "secret-input")
    });
    assert_eq!(app.status(cx, 0), Status::WaitingForInput);
    app.click(cx, "secret-cancel");
    assert!(!app.has(cx, "secret-input"));
    assert_eq!(app.status(cx, 0), Status::Disconnected);

    // The prompt has the focus: type, Enter, connected.
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the password prompt", |app, cx| {
        app.has(cx, "secret-input")
    });
    app.type_text(cx, testing::DB_PASSWORD);
    app.press(cx, &["enter"]);
    assert!(!app.has(cx, "secret-input"));
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    // A password given at the prompt is not saved.
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let id = app.storage.connections().unwrap()[0].id.clone();
    assert!(
        kerno_core::secrets::load_secrets(secrets.as_ref(), &id)
            .unwrap()
            .password
            .is_none()
    );
}

#[gpui_kit::test]
fn deleting_a_connection_asks_first(cx: &mut TestAppContext) {
    let storage = memory_storage();
    storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let app = open(cx, storage);
    let ask = |app: &Fixture, cx: &mut TestAppContext| {
        let connections = app.connections.clone();
        app.step(cx, move |window, cx| {
            let saved = connections.read(cx).saved()[0].clone();
            Sidebar::confirm_delete(connections, &saved, window, cx);
        });
    };

    ask(&app, cx);
    app.click(cx, "cancel");
    assert_eq!(app.storage.connections().unwrap().len(), 1);

    ask(&app, cx);
    app.click(cx, "ok");
    app.wait_until(cx, "the deletion", |app, _| {
        app.storage.connections().unwrap().is_empty()
    });
    assert!(app.has(cx, "empty-new-connection"));
}

#[gpui_kit::test]
fn connections_are_exported_without_secrets_and_imported_back(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let secrets = Secrets {
        password: Some(Secret::new("export-me-not")),
        ..Secrets::default()
    };
    let saving = app.connections.clone();
    cx.update(|cx| {
        saving.update(cx, |connections, cx| {
            connections.save(
                None,
                testing::config(Engine::MySql),
                secrets,
                None,
                false,
                cx,
            );
        })
    });
    app.wait_until(cx, "the save", |app, cx| {
        cx.update(|cx| app.connections.read(cx).saved().len() == 1)
    });

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("connections.json");
    let (workspace, target) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.export_connections_to(target, window, cx)
        });
    });
    app.wait_until(cx, "the export", |_, _| file.exists());
    let exported = std::fs::read_to_string(&file).unwrap();
    assert!(exported.contains("test MySQL"), "{exported}");
    assert!(!exported.contains("export-me-not"));

    let (workspace, source) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.import_connections_from(source, window, cx)
        });
    });
    app.wait_until(cx, "the import", |app, cx| {
        cx.update(|cx| app.connections.read(cx).saved().len() == 2)
    });
    assert_eq!(app.storage.connections().unwrap().len(), 2);

    // A file that is not an export changes nothing.
    std::fs::write(&file, "{}").unwrap();
    let (workspace, source) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.import_connections_from(source, window, cx)
        });
    });
    cx.run_until_parked();
    assert_eq!(app.storage.connections().unwrap().len(), 2);
}

#[gpui_kit::test]
fn a_tunnelled_connection_asks_for_trust_then_for_each_missing_secret(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::ssh_available() {
        return;
    }
    // Through the bastion, with nothing in the secret store.
    let storage = memory_storage();
    let mut config = testing::config(Engine::Postgres);
    config.host = testing::service_name(Engine::Postgres).into();
    config.port = 5432;
    config.ssh = Some(testing::ssh(kerno_core::config::SshAuth::Password));
    storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let row = ("connection", 0usize);

    // The server's key is shown first; declining gives the attempt up.
    app.step(cx, |window, cx| window.double_click(row, cx));
    app.wait_until(cx, "the host key question", |app, cx| app.has(cx, "ok"));
    assert_eq!(app.status(cx, 0), Status::WaitingForInput);
    app.click(cx, "cancel");
    assert_eq!(app.status(cx, 0), Status::Disconnected);

    app.step(cx, |window, cx| window.double_click(row, cx));
    app.wait_until(cx, "the host key question", |app, cx| app.has(cx, "ok"));
    app.click(cx, "ok");

    // Then the SSH password, then the database password.
    for (what, secret) in [
        ("the SSH password prompt", testing::SSH_PASSWORD),
        ("the database password prompt", testing::DB_PASSWORD),
    ] {
        app.wait_until(cx, what, |app, cx| {
            assert!(
                !matches!(app.status(cx, 0), Status::Failed { .. }),
                "{:?}",
                app.status(cx, 0)
            );
            app.has(cx, "secret-input")
        });
        app.type_text(cx, secret);
        app.click(cx, "secret-ok");
        assert!(!app.has(cx, "secret-input"), "{what} closes once answered");
    }
    app.wait_until(cx, "the connection", |app, cx| {
        assert!(
            !matches!(app.status(cx, 0), Status::Failed { .. }),
            "{:?}",
            app.status(cx, 0)
        );
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
}

#[gpui_kit::test]
fn the_sidebar_answers_a_right_click_on_each_part(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let mut config = testing::config(Engine::Postgres);
    config.group = Some("Work".into());
    storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);

    // The empty area below the rows, and the header.
    for area in ["connection-list-end", "connections-header"] {
        assert!(!app.shows_label(cx, "New folder…"));
        app.right_click(cx, area, None);
        for label in [
            "New connection…",
            "New folder…",
            "Import connections…",
            "Refresh",
            "Collapse all",
        ] {
            assert!(app.shows_label(cx, label), "{area}: {label}");
        }
        app.press(cx, &["escape"]);
    }

    // A folder.
    assert!(!app.shows_label(cx, "New subfolder…"));
    app.right_click(cx, ("conn-folder", 0usize), None);
    for label in [
        "New connection here…",
        "New subfolder…",
        "Rename…",
        "Delete…",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.press(cx, &["escape"]);

    // A connection: what does not apply to a closed one is disabled.
    assert!(!app.shows_label(cx, "New SQL editor"));
    app.right_click(cx, ("connection", 0usize), None);
    for label in [
        "Connect",
        "Disconnect (not connected)",
        "Reconnect (not connected)",
        "New SQL editor",
        "Edit…",
        "Duplicate",
        "Rename…",
        "Move to folder",
        "Delete…",
        "Refresh (not connected)",
        "Copy",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.press(cx, &["escape"]);
}

#[gpui_kit::test]
fn a_folder_is_made_renamed_filled_by_drag_and_deleted(cx: &mut TestAppContext) {
    let storage = memory_storage();
    storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let app = open(cx, storage);

    // New folder, from the menu of the empty area.
    app.right_click(cx, "connection-list-end", None);
    app.click_label(cx, "New folder…");
    assert!(app.has(cx, "name-input"));
    app.type_text(cx, "Work/x");
    app.press(cx, &["enter"]);
    assert!(app.has(cx, "name-error"), "a name with “/” is refused");
    app.press(cx, &["backspace", "backspace"]);
    app.press(cx, &["enter"]);
    assert!(!app.has(cx, "name-input"));
    assert!(app.has(cx, ("conn-folder", 0usize)));
    let folders = |app: &Fixture, cx: &mut TestAppContext| {
        cx.update(|cx| app.connections.read(cx).folders(cx))
    };
    assert_eq!(folders(&app, cx), ["Work"]);

    // Rename it.
    app.right_click(cx, ("conn-folder", 0usize), None);
    app.click_label(cx, "Rename…");
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "Clients");
    app.click(cx, "name-ok");
    assert_eq!(folders(&app, cx), ["Clients"]);

    // Drag the connection onto it, then back to the top level.
    app.step(cx, |window, cx| {
        window.drag_to(("connection", 0usize), ("conn-folder", 0usize), cx)
    });
    app.wait_until(cx, "the move to be stored", |app, _| {
        app.first_stored().group.as_deref() == Some("Clients")
    });
    app.step(cx, |window, cx| {
        window.drag_to(("connection", 0usize), "connection-list-end", cx)
    });
    app.wait_until(cx, "the move back to be stored", |app, _| {
        app.first_stored().group.is_none()
    });
    app.step(cx, |window, cx| {
        window.drag_to(("connection", 0usize), ("conn-folder", 0usize), cx)
    });
    app.wait_until(cx, "the second move to be stored", |app, _| {
        app.first_stored().group.as_deref() == Some("Clients")
    });

    // A connection made from the folder's menu starts in it.
    app.right_click(cx, ("conn-folder", 0usize), None);
    app.click_label(cx, "New connection here…");
    let folder = app.step(cx, |window, _| {
        window.find("conn-group").value().map(|v| v.to_string())
    });
    assert_eq!(folder.as_deref(), Some("Clients"));
    app.click(cx, "conn-cancel");

    // Deleting a folder that holds a connection asks, and the connection
    // stays, at the top level.
    app.right_click(cx, ("conn-folder", 0usize), None);
    app.click_label(cx, "Delete…");
    app.click(cx, "cancel");
    assert_eq!(folders(&app, cx), ["Clients"]);
    app.right_click(cx, ("conn-folder", 0usize), None);
    app.click_label(cx, "Delete…");
    app.click(cx, "ok");
    assert_eq!(folders(&app, cx), Vec::<String>::new());
    assert!(!app.has(cx, ("conn-folder", 0usize)));
    assert!(app.has(cx, ("connection", 0usize)));
    app.wait_until(cx, "the connection to be back at the top", |app, _| {
        app.first_stored().group.is_none()
    });
}

#[gpui_kit::test]
fn the_copied_connection_url_has_no_password(cx: &mut TestAppContext) {
    const PASSWORD: &str = "kerno-ui-url-secret-7Qz";
    let storage = memory_storage();
    let id = storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let password = Secrets {
        password: Some(Secret::new(PASSWORD)),
        ..Secrets::default()
    };
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &password).unwrap();

    app.sidebar_does(cx, Subject::Connection(id.clone()), SidebarAction::CopyUrl);
    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert_eq!(
        copied,
        format!(
            "postgresql://kerno@127.0.0.1:{}/kerno?sslmode=disable",
            testing::port(Engine::Postgres)
        )
    );
    assert!(!copied.contains(PASSWORD));

    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::CopyName);
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some(app.first_stored().name.as_str()));
}

#[gpui_kit::test]
fn new_sql_editor_connects_first_then_opens_a_tab_on_the_connection(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let storage = memory_storage();
    let id = storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();

    app.right_click(cx, ("connection", 0usize), None);
    app.click_label(cx, "New SQL editor");
    app.wait_until(cx, "the tab", |app, cx| app.titles(cx).len() == 1);
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    let tab = app.tab(cx);
    let target = cx.update(|cx| tab.read(cx).target().cloned());
    assert_eq!(target.map(|target| target.connection), Some(id.clone()));

    // Open already: the tab comes at once. Reconnect leaves it connected.
    app.sidebar_does(
        cx,
        Subject::Connection(id.clone()),
        SidebarAction::NewSqlEditor,
    );
    assert_eq!(app.titles(cx).len(), 2);
    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::Reconnect);
    app.wait_until(cx, "the reconnection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
}

#[gpui_kit::test]
fn a_right_click_below_the_schema_tree_opens_its_menu(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.wait_until(cx, "the databases", |app, cx| !app.tree(cx).is_empty());
    assert!(!app.shows_label(cx, "New query here"));
    let height = app.step(cx, |window, _| {
        window.find("navigator-tree").bounds().size.height
    });
    app.right_click(
        cx,
        "navigator-tree",
        Some(gpui_kit::point(px(40.), height - px(8.))),
    );
    for label in ["Refresh", "Collapse all", "New query here"] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.click_label(cx, "New query here");
    assert_eq!(app.titles(cx).len(), 1);
    let tab = app.tab(cx);
    let target = cx.update(|cx| tab.read(cx).target().cloned());
    assert_eq!(
        target.map(|target| target.connection),
        Some(app.storage.connections().unwrap()[0].id.clone())
    );

    // Its entries are palette commands too.
    app.wait_until(cx, "an open database", |app, cx| {
        app.tree(cx).iter().any(|row| row.starts_with("  "))
    });
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::CollapseSchemaTree), cx)
    });
    let rows = app.tree(cx);
    assert!(rows.iter().all(|row| !row.starts_with(' ')), "{rows:?}");
}

// Schema navigator ------------------------------------------------------------

/// Opens the app on the test server of `engine`, saved with its password,
/// and connects to it.
fn connected(cx: &mut TestAppContext, engine: Engine) -> Fixture {
    connected_as(cx, testing::config(engine))
}

/// The same with these settings.
fn connected_as(cx: &mut TestAppContext, config: ConnectionConfig) -> Fixture {
    let storage = memory_storage();
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the connection", |app, cx| {
        assert!(
            !matches!(app.status(cx, 0), Status::Failed { .. }),
            "{:?}",
            app.status(cx, 0)
        );
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    app
}

fn row(depth: usize, label: &str) -> String {
    format!("{}{label}", "  ".repeat(depth))
}

#[gpui_kit::test]
fn the_schema_tree_opens_level_by_level(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    // The database of the connection opens by itself.
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
    let tree = app.tree(cx);
    let position = |label: &str| {
        tree.iter()
            .position(|line| line == label)
            .unwrap_or_else(|| panic!("no {label:?} in {tree:#?}"))
    };
    assert_eq!(position("kerno"), 0);
    // The user's schemas, then the system's, then the extensions; the
    // other databases stay closed.
    assert!(position("  shop") < position("  pg_catalog"));
    assert!(position("  pg_catalog") < position("  Extensions"));
    assert!(position("  Extensions") < position("kerno_other"));

    app.toggle(cx, &["kerno", "shop"]);
    app.wait_until(cx, "the objects of shop", |app, cx| {
        app.tree(cx).contains(&row(2, "Tables"))
    });
    for folder in [
        "Views",
        "Materialized views",
        "Functions",
        "Procedures",
        "Sequences",
        "Types",
        "Triggers",
    ] {
        assert!(app.tree(cx).contains(&row(2, folder)), "{folder}");
    }
    app.toggle(cx, &["kerno", "shop", "Tables"]);
    assert!(app.tree(cx).contains(&row(3, "customers")));

    app.toggle(cx, &["kerno", "shop", "Tables", "customers"]);
    app.wait_until(cx, "the columns of customers", |app, cx| {
        app.tree(cx).contains(&row(4, "Columns"))
    });
    app.toggle(cx, &["kerno", "shop", "Tables", "customers", "Columns"]);
    app.toggle(cx, &["kerno", "shop", "Tables", "customers", "Indexes"]);
    let tree = app.tree(cx);
    assert!(
        tree.contains(&row(5, "email")) && tree.contains(&row(5, "customers_email_key")),
        "{tree:#?}"
    );

    // Closing a folder hides what is in it.
    app.toggle(cx, &["kerno", "shop", "Tables"]);
    assert!(!app.tree(cx).contains(&row(3, "customers")));

    // Another database is browsed through a session of its own.
    app.toggle(cx, &["kerno_other"]);
    app.wait_until(cx, "the schemas of kerno_other", |app, cx| {
        app.tree(cx)
            .iter()
            .filter(|line| **line == row(1, "public"))
            .count()
            == 2
    });
    app.toggle(cx, &["kerno_other", "public"]);
    app.wait_until(cx, "the objects of kerno_other", |app, cx| {
        app.tree(cx)
            .iter()
            .filter(|line| **line == row(2, "Tables"))
            .count()
            == 2
    });
    app.toggle(cx, &["kerno_other", "public", "Tables"]);
    assert!(app.tree(cx).contains(&row(3, "notes")));
}

#[gpui_kit::test]
fn a_click_selects_a_row_and_the_chevron_or_the_arrows_open_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let tables = ["kerno", "shop", "Tables"];
    let customers = ["kerno", "shop", "Tables", "customers"];
    app.open_path(cx, &customers);
    let columns = || row(4, "Columns");

    // One click selects; it opens nothing.
    app.click_row(cx, &customers, false);
    assert_eq!(app.selected_row(cx), Some(app.tree_row(cx, &customers)));
    assert!(!app.is_open(cx, &customers));
    assert!(!app.tree(cx).contains(&columns()), "{:#?}", app.tree(cx));
    assert_eq!(app.titles(cx), Vec::<String>::new());

    // Right opens the selected row, then goes to its first child; Left
    // goes back to the parent, then closes it.
    app.press(cx, &["right"]);
    app.wait_until(cx, "the folders of customers", |app, cx| {
        app.tree(cx).contains(&columns())
    });
    app.press(cx, &["right"]);
    let first_child = app.tree_row(cx, &["kerno", "shop", "Tables", "customers", "Columns"]);
    assert_eq!(app.selected_row(cx), Some(first_child));
    app.press(cx, &["left"]);
    assert_eq!(app.selected_row(cx), Some(app.tree_row(cx, &customers)));
    app.press(cx, &["left"]);
    assert!(!app.tree(cx).contains(&columns()));
    // Up and Down move the selection.
    app.press(cx, &["down"]);
    assert_eq!(app.selected_row(cx), Some(app.tree_row(cx, &customers) + 1));
    app.press(cx, &["up"]);
    assert_eq!(app.selected_row(cx), Some(app.tree_row(cx, &customers)));

    // The chevron opens and closes.
    app.toggle(cx, &customers);
    assert!(app.tree(cx).contains(&columns()));
    app.toggle(cx, &customers);
    assert!(!app.tree(cx).contains(&columns()));

    // A double click on a folder closes and opens it.
    app.click_row(cx, &tables, true);
    assert!(!app.tree(cx).contains(&row(3, "customers")));
    app.click_row(cx, &tables, true);
    assert!(app.tree(cx).contains(&row(3, "customers")));
    assert_eq!(app.titles(cx), Vec::<String>::new());
}

#[gpui_kit::test]
fn a_double_click_or_enter_opens_what_the_row_is_for(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let table = |name: &'static str| ["kerno", "shop", "Tables", name];
    app.open_path(cx, &table("customers"));

    // A double click opens the rows of a table, and does not open the row.
    app.click_row(cx, &table("customers"), true);
    assert_eq!(app.titles(cx), ["customers"]);
    assert!(cx.update(|cx| app.workspace.read(cx).active_data_tab().is_some()));
    assert!(!app.is_open(cx, &table("customers")));

    // Enter does the same for the selected row.
    app.click_row(cx, &table("orders"), false);
    app.press(cx, &["enter"]);
    assert_eq!(app.titles(cx), ["customers", "orders"]);

    // The setting: structure...
    app.press(cx, &["secondary-,"]);
    app.step(cx, |window, cx| {
        window.within("table-double-click").click(1usize, cx)
    });
    app.press(cx, &["escape"]);
    cx.update(|cx| {
        assert_eq!(
            AppState::get(cx).settings.table_double_click,
            crate::settings::TableDoubleClick::Structure
        )
    });
    app.click_row(cx, &table("products"), true);
    assert_eq!(app.titles(cx).len(), 3);
    assert!(cx.update(|cx| app.workspace.read(cx).active_structure_tab().is_some()));
    // ...or open the row in the tree.
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            settings.table_double_click = crate::settings::TableDoubleClick::Expand
        })
    });
    app.click_row(cx, &table("customers"), true);
    app.wait_until(cx, "the folders of customers", |app, cx| {
        app.tree(cx).contains(&row(4, "Columns"))
    });
    assert_eq!(app.titles(cx).len(), 3);

    // A function opens its source.
    app.open_path(cx, &["kerno", "shop", "Functions"]);
    app.toggle(cx, &["kerno", "shop", "Functions"]);
    app.wait_until(cx, "the functions", |app, cx| {
        app.tree(cx)
            .iter()
            .any(|line| line.starts_with("      customer_lifetime_value"))
    });
    let function = app
        .tree(cx)
        .into_iter()
        .find_map(|line| {
            line.strip_prefix("      ")
                .filter(|label| label.starts_with("customer_lifetime_value"))
                .map(str::to_string)
        })
        .unwrap();
    app.click_row(cx, &["kerno", "shop", "Functions", &function], true);
    app.wait_until(cx, "the function's source", |app, cx| {
        app.titles(cx).len() == 4
    });
    let (_, sql) = app.active_query(cx);
    assert!(sql.contains("customer_lifetime_value"), "{sql}");
    assert!(app.has(cx, "tab-apply-source"));
}

#[gpui_kit::test]
fn a_double_click_on_a_connection_connects_and_never_disconnects(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    // `connected` opens the connection with a double click.
    let app = connected(cx, Engine::Postgres);
    let databases = |app: &Fixture, cx: &mut TestAppContext| {
        app.tree(cx).first().is_some_and(|row| row == "kerno")
    };
    app.wait_until(cx, "the databases", databases);
    for _ in 0..2 {
        app.step(cx, |window, cx| {
            window.double_click(("connection", 0usize), cx)
        });
        assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    }
    assert!(databases(&app, cx), "{:#?}", app.tree(cx));

    // Enter does what a double click does on the selected connection.
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::Disconnect);
    app.wait_until(cx, "the disconnection", |app, cx| {
        matches!(app.status(cx, 0), Status::Disconnected)
    });
    app.click(cx, ("connection", 0usize));
    assert!(matches!(app.status(cx, 0), Status::Disconnected));
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    app.wait_until(cx, "the databases again", databases);

    // F4 edits it.
    assert!(!app.has(cx, "conn-name"));
    app.press(cx, &["f4"]);
    app.wait_until(cx, "the connection dialog", |app, cx| {
        app.has(cx, "conn-name")
    });
    app.press(cx, &["escape"]);
}

#[gpui_kit::test]
fn navigator_keys_run_the_actions_of_the_selected_node(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_keys CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_keys",
            r#"CREATE TABLE kerno_ui_keys."Order Items" (id int PRIMARY KEY)"#,
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let orders = ["kerno", "shop", "Tables", "orders"];
    app.open_path(cx, &orders);
    let clipboard = |cx: &mut TestAppContext| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    };

    // Cmd+C copies the name, Cmd+Shift+C the name with its schema.
    app.click_row(cx, &orders, false);
    app.press(cx, &["secondary-c"]);
    assert_eq!(clipboard(cx), "orders");
    app.press(cx, &["secondary-shift-c"]);
    assert_eq!(clipboard(cx), "shop.orders");

    // F2 asks for the new name; Delete (either key) shows the drop. Both
    // are cancelled: `shop.orders` is fixture data.
    app.press(cx, &["f2"]);
    assert!(app.has(cx, "sql-name"));
    app.click(cx, "sql-cancel");
    for key in ["backspace", "delete"] {
        app.click_row(cx, &orders, false);
        app.press(cx, &[key]);
        assert!(
            app.has(cx, "sql-preview") && !app.has(cx, "sql-name"),
            "{key}"
        );
        // Its button: the dialog is the drop's.
        assert!(app.shows_label(cx, "Drop"), "{key}");
        app.click(cx, "sql-cancel");
        assert!(!app.has(cx, "sql-preview") && !app.shows_label(cx, "Drop"));
    }
    assert!(app.shows(cx, &orders));
    assert_eq!(app.titles(cx), Vec::<String>::new());

    // F4 opens the structure of a table.
    app.click_row(cx, &orders, false);
    app.press(cx, &["f4"]);
    assert_eq!(app.titles(cx), ["orders · structure"]);
    assert!(cx.update(|cx| app.workspace.read(cx).active_structure_tab().is_some()));

    // Cmd+] opens a query tab in the table's schema.
    app.click_row(cx, &orders, false);
    app.press(cx, &["secondary-]"]);
    app.wait_until(cx, "the query tab", |app, cx| app.titles(cx).len() == 2);
    let tab = app.tab(cx);
    let target = cx.update(|cx| tab.read(cx).target().cloned());
    assert_eq!(
        target.and_then(|target| target.schema).as_deref(),
        Some("shop")
    );
    // On a PostgreSQL database, in no schema in particular.
    app.click_row(cx, &["kerno"], false);
    app.press(cx, &["secondary-]"]);
    app.wait_until(cx, "the database's tab", |app, cx| {
        app.titles(cx).len() == 3
    });
    let tab = app.tab(cx);
    let target = cx.update(|cx| tab.read(cx).target().cloned()).unwrap();
    assert_eq!(
        (target.database.as_deref(), target.schema),
        (Some("kerno"), None)
    );

    // A name that needs quotes is copied quoted.
    let mixed = ["kerno", "kerno_ui_keys", "Tables", "Order Items"];
    app.open_path(cx, &mixed);
    app.click_row(cx, &mixed, false);
    app.press(cx, &["secondary-shift-c"]);
    assert_eq!(clipboard(cx), r#"kerno_ui_keys."Order Items""#);

    // F5 reads the selected folder again.
    let tables = ["kerno", "kerno_ui_keys", "Tables"];
    on_server(
        Engine::Postgres,
        "CREATE TABLE kerno_ui_keys.added_later (id int)",
    );
    assert!(!app.tree(cx).contains(&row(3, "added_later")));
    app.click_row(cx, &tables, false);
    app.press(cx, &["f5"]);
    app.wait_until(cx, "the new table", |app, cx| {
        app.tree(cx).contains(&row(3, "added_later"))
    });
}

#[gpui_kit::test]
fn set_as_default_points_the_active_tab_at_the_schema(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_default CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_default",
            "CREATE TABLE kerno_ui_default.only_here AS SELECT 42 AS answer",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let schema = ["kerno", "kerno_ui_default"];
    app.open_path(cx, &schema);
    // Without a query tab there is nothing to point.
    app.click_row(cx, &schema, false);
    assert_eq!(
        app.why_not(cx, Action::SetDefault, &schema),
        Some("no query tab is active")
    );

    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select answer from only_here");
    app.click_row(cx, &schema, false);
    assert_eq!(app.why_not(cx, Action::SetDefault, &schema), None);
    app.press(cx, &["secondary-shift-a"]);
    let tab = app.tab(cx);
    let target = cx.update(|cx| tab.read(cx).target().cloned());
    assert_eq!(
        target.and_then(|target| target.schema).as_deref(),
        Some("kerno_ui_default")
    );
    // The unqualified name is found in that schema.
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the row", |app, cx| {
        let outcomes = app.outcomes(cx);
        assert!(
            outcomes.iter().all(|outcome| !outcome.contains("error")),
            "{outcomes:?}"
        );
        outcomes == ["1 rows"]
    });
    assert_eq!(app.result(cx, 0, 1).1, vec![cells(&["42"])]);
}

#[gpui_kit::test]
fn the_filter_narrows_the_tree_and_clearing_it_restores_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
    app.toggle(cx, &["kerno", "shop"]);
    app.wait_until(cx, "the objects of shop", |app, cx| {
        app.tree(cx).contains(&row(2, "Tables"))
    });

    app.click(cx, "navigator-filter");
    app.type_text(cx, "order");
    // What matches is shown with the way to it, opened; nothing else.
    assert_eq!(
        app.tree(cx),
        [
            row(0, "kerno"),
            row(1, "shop"),
            row(2, "Tables"),
            row(3, "order_items"),
            row(3, "orders"),
            row(2, "Views"),
            row(3, "order_totals"),
            row(2, "Functions"),
            row(3, "log_order_status()"),
            row(2, "Procedures"),
            row(3, "cancel_stale_orders(IN p_older_than interval)"),
            row(2, "Sequences"),
            row(3, "orders_id_seq"),
            row(2, "Types"),
            row(3, "order_status"),
            row(2, "Triggers"),
            row(3, "orders_log_status"),
        ]
    );
    app.press(cx, &["secondary-a", "backspace"]);
    let tree = app.tree(cx);
    assert!(
        tree.contains(&row(1, "analytics")) && tree.contains(&row(2, "Sequences")),
        "{tree:#?}"
    );
}

#[gpui_kit::test]
fn mysql_and_mariadb_list_objects_under_their_databases(cx: &mut TestAppContext) {
    for engine in [Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        app.wait_until(cx, "the objects of kerno", |app, cx| {
            app.tree(cx).contains(&row(1, "Tables"))
        });
        let tree = app.tree(cx);
        assert_eq!(tree[0], "kerno", "{engine:?}");
        for folder in ["Tables", "Views", "Routines", "Triggers", "Events"] {
            assert!(
                tree.contains(&row(1, folder)),
                "{engine:?}: {folder} in {tree:#?}"
            );
        }
        assert!(tree.contains(&row(0, "kerno_analytics")), "{engine:?}");

        app.toggle(cx, &["kerno", "Routines"]);
        let tree = app.tree(cx);
        assert!(
            tree.iter()
                .any(|line| line.starts_with(&row(2, "cancel_stale_orders("))),
            "{engine:?}: {tree:#?}"
        );
        app.toggle(cx, &["kerno", "Events"]);
        assert!(
            app.tree(cx).contains(&row(2, "purge_old_audit_log")),
            "{engine:?}"
        );

        // Another database needs no other session on these engines.
        app.toggle(cx, &["kerno_analytics"]);
        app.wait_until(cx, "the objects of kerno_analytics", |app, cx| {
            app.tree(cx)
                .iter()
                .filter(|line| **line == row(1, "Tables"))
                .count()
                == 2
        });
    }
}

#[gpui_kit::test]
fn the_tree_follows_the_connection(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
    assert!(app.has(cx, "navigator-filter"));

    // Disconnecting empties it and says what to do.
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::Disconnect);
    assert_eq!(app.status(cx, 0), Status::Disconnected);
    assert_eq!(app.tree(cx), Vec::<String>::new());
    assert!(!app.has(cx, "navigator-filter"));

    // Connecting again loads it again.
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the schemas again", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
}

/// Runs SQL on a test server, outside the app. Gives the first cell of the
/// last result, or nothing.
fn on_server(engine: Engine, sql: &str) -> String {
    use kerno_core::Driver as _;
    let sql = sql.to_string();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let params = testing::params(engine);
            let session = match engine {
                Engine::Postgres => kerno_postgres::PostgresDriver.connect(&params).await,
                Engine::MySql | Engine::MariaDb => kerno_mysql::MysqlDriver.connect(&params).await,
            }
            .expect("the test server");
            let collected = kerno_core::exec::collect(session.clone(), sql.clone())
                .await
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            session.close().await;
            collected
                .results
                .last()
                .and_then(|result| result.rows().first().and_then(|row| row.first().cloned()))
                .flatten()
                .unwrap_or_default()
        })
    })
    .join()
    .expect("the statement ran")
}

/// Objects of one test, on a test server, removed when the test ends
/// whether it passed or not.
struct Scratch {
    engine: Engine,
    cleanup: Vec<String>,
}

impl Scratch {
    /// Runs `cleanup` (in case an earlier run was killed), then `setup`.
    fn new(engine: Engine, cleanup: &[&str], setup: &[&str]) -> Self {
        let scratch = Self {
            engine,
            cleanup: cleanup.iter().map(|sql| sql.to_string()).collect(),
        };
        for sql in cleanup.iter().chain(setup) {
            on_server(engine, sql);
        }
        scratch
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for sql in &self.cleanup {
            // Runs on its own thread: a failed test must still clean up.
            on_server(self.engine, sql);
        }
    }
}

#[gpui_kit::test]
fn statements_and_ddl_open_in_a_tab_and_run_nothing(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
    app.toggle(cx, &["kerno", "shop"]);
    app.wait_until(cx, "the objects of shop", |app, cx| {
        app.tree(cx).contains(&row(2, "Tables"))
    });
    app.toggle(cx, &["kerno", "shop", "Tables"]);
    let customers = ["kerno", "shop", "Tables", "customers"];

    app.act(cx, Action::Generate(Template::Select), &customers);
    app.wait_until(cx, "the SELECT", |app, cx| app.titles(cx) == ["customers"]);
    let (_, sql) = app.active_query(cx);
    assert!(
        sql.starts_with("SELECT") && sql.contains("\"email\""),
        "{sql}"
    );
    assert!(
        sql.contains("FROM \"shop\".\"customers\"\nLIMIT 1000;"),
        "{sql}"
    );

    app.act(cx, Action::Generate(Template::Update), &customers);
    app.wait_until(cx, "the UPDATE", |app, cx| app.titles(cx).len() == 2);
    let (_, sql) = app.active_query(cx);
    assert!(sql.contains("WHERE \"id\" = :id"), "{sql}");

    // A table without a key never gets a DELETE that would empty it.
    app.act(
        cx,
        Action::Generate(Template::Delete),
        &["kerno", "shop", "Tables", "audit_log"],
    );
    app.wait_until(cx, "the DELETE", |app, cx| app.titles(cx).len() == 3);
    let (title, sql) = app.active_query(cx);
    assert_eq!(title, "audit_log");
    assert!(sql.contains("-- add a condition"), "{sql}");

    app.act(cx, Action::ShowDdl, &customers);
    app.wait_until(cx, "the DDL", |app, cx| app.titles(cx).len() == 4);
    let (title, sql) = app.active_query(cx);
    assert_eq!(title, "DDL customers");
    assert!(
        sql.contains("CREATE TABLE") && sql.contains("customers_email_key"),
        "{sql}"
    );

    app.act(cx, Action::NewQueryHere, &["kerno", "shop"]);
    app.wait_until(cx, "the query tab", |app, cx| app.titles(cx).len() == 5);
    // An empty tab that runs in that schema of that database.
    assert_eq!(app.active_query(cx), ("shop".to_string(), String::new()));
    let target = cx.update(|cx| {
        let workspace = app.workspace.read(cx);
        workspace
            .active_query_tab()
            .unwrap()
            .read(cx)
            .target()
            .cloned()
    });
    let target = target.expect("the tab is bound");
    assert_eq!(
        (target.database.as_deref(), target.schema.as_deref()),
        (Some("kerno"), Some("shop"))
    );

    // "Create table…" opens the table form.
    app.act(cx, Action::CreateTable, &["kerno", "shop", "Tables"]);
    assert_eq!(app.titles(cx).len(), 6);
    assert_eq!(app.titles(cx)[5], "New table");
    assert!(app.has(cx, "table-editor"));
    // Opening a form is not running anything.
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM pg_tables WHERE tablename = 'new_table'"
        ),
        "0"
    );
}

#[gpui_kit::test]
fn dropping_renaming_and_truncating_show_the_sql_and_ask(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let pg = |sql: &str| on_server(Engine::Postgres, sql);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[
            "DROP SCHEMA IF EXISTS kerno_ui_nav CASCADE",
            "DROP SCHEMA IF EXISTS kerno_ui_nav_made CASCADE",
        ],
        &[
            "CREATE SCHEMA kerno_ui_nav",
            "CREATE TABLE kerno_ui_nav.drop_me (id int PRIMARY KEY)",
            "CREATE TABLE kerno_ui_nav.old_name (id int PRIMARY KEY)",
            "CREATE TABLE kerno_ui_nav.keep_me AS SELECT generate_series(1, 3) AS id",
        ],
    );
    let exists = |table: &str| {
        pg(&format!(
            "SELECT count(*) FROM pg_tables WHERE schemaname = 'kerno_ui_nav' AND tablename = '{table}'"
        )) == "1"
    };
    let app = connected(cx, Engine::Postgres);
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "kerno_ui_nav"))
    });
    app.toggle(cx, &["kerno", "kerno_ui_nav"]);
    app.wait_until(cx, "the scratch objects", |app, cx| {
        app.tree(cx).contains(&row(2, "Tables"))
    });
    app.toggle(cx, &["kerno", "kerno_ui_nav", "Tables"]);
    let table = |name: &'static str| ["kerno", "kerno_ui_nav", "Tables", name];

    // Drop: the statement is shown; cancelling runs nothing.
    app.act(cx, Action::Drop, &table("drop_me"));
    assert!(app.has(cx, "sql-preview") && !app.has(cx, "sql-production"));
    app.click(cx, "sql-cancel");
    assert!(!app.has(cx, "sql-preview"));
    assert!(exists("drop_me"));
    // Enter does not confirm: only the button does.
    app.act(cx, Action::Drop, &table("drop_me"));
    app.press(cx, &["enter"]);
    assert!(app.has(cx, "sql-preview") && exists("drop_me"));
    app.click(cx, "sql-confirm");
    // Once the schema is read again, the table is gone and the folder is
    // still open.
    app.wait_until(cx, "the drop", |app, cx| {
        let tree = app.tree(cx);
        tree.contains(&row(3, "old_name")) && !tree.contains(&row(3, "drop_me"))
    });
    assert!(!exists("drop_me") && exists("old_name"));

    // Rename: the new name is typed, the statement follows it.
    app.act(cx, Action::Rename, &table("old_name"));
    assert!(app.has(cx, "sql-name"));
    // The same name is not a rename: there is nothing to run yet.
    app.click(cx, "sql-confirm");
    assert!(app.has(cx, "sql-name") && exists("old_name"));
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "new name");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the rename", |app, cx| {
        app.tree(cx).contains(&row(3, "new name"))
    });
    assert!(exists("new name") && !exists("old_name"));

    // Truncate.
    assert_eq!(pg("SELECT count(*) FROM kerno_ui_nav.keep_me"), "3");
    app.act(cx, Action::Truncate, &table("keep_me"));
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the truncate", |_, _| {
        pg("SELECT count(*) FROM kerno_ui_nav.keep_me") == "0"
    });

    // Create a schema from the database, then drop it.
    app.act(cx, Action::CreateSchema, &["kerno"]);
    app.type_text(cx, "kerno_ui_nav_made");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the new schema", |app, cx| {
        app.tree(cx).contains(&row(1, "kerno_ui_nav_made"))
    });
    app.act(cx, Action::Drop, &["kerno", "kerno_ui_nav_made"]);
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the schema to go", |app, cx| {
        !app.tree(cx).contains(&row(1, "kerno_ui_nav_made"))
    });
    assert_eq!(
        pg("SELECT count(*) FROM pg_namespace WHERE nspname = 'kerno_ui_nav_made'"),
        "0"
    );

    // The database of the connection cannot be dropped from it.
    assert_eq!(
        app.why_not(cx, Action::Drop, &["kerno"]),
        Some("the connection is using it")
    );
    app.act(cx, Action::Drop, &["kerno"]);
    assert!(!app.has(cx, "sql-preview"));
}

#[gpui_kit::test]
fn production_asks_twice_and_read_only_refuses(cx: &mut TestAppContext) {
    for engine in [Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let server = |sql: &str| on_server(engine, sql);
        let _scratch = Scratch::new(
            engine,
            &["DROP DATABASE IF EXISTS kerno_ui_prod"],
            &[
                "CREATE DATABASE kerno_ui_prod",
                "CREATE TABLE kerno_ui_prod.invoices (id INT PRIMARY KEY)",
            ],
        );
        let exists = || {
            server(
                "SELECT count(*) FROM information_schema.tables \
                 WHERE table_schema = 'kerno_ui_prod' AND table_name = 'invoices'",
            ) == "1"
        };
        let invoices = ["kerno_ui_prod", "Tables", "invoices"];
        let open_scratch = |app: &Fixture, cx: &mut TestAppContext| {
            app.wait_until(cx, "the databases", |app, cx| {
                app.tree(cx).contains(&row(0, "kerno_ui_prod"))
            });
            app.toggle(cx, &["kerno_ui_prod"]);
            app.wait_until(cx, "the scratch objects", |app, cx| {
                app.tree(cx)
                    .iter()
                    .filter(|line| **line == row(1, "Tables"))
                    .count()
                    == 2
            });
            app.toggle(cx, &["kerno_ui_prod", "Tables"]);
        };

        // A read-only connection offers no change at all.
        let mut config = testing::config(engine);
        config.read_only = true;
        let app = connected_as(cx, config);
        open_scratch(&app, cx);
        for action in [
            Action::Drop,
            Action::Truncate,
            Action::Rename,
            Action::CreateDatabase,
        ] {
            assert_eq!(
                app.why_not(cx, action, &invoices),
                Some("read-only connection"),
                "{engine:?} {action:?}"
            );
        }
        app.act(cx, Action::Drop, &invoices);
        assert!(!app.has(cx, "sql-preview"), "{engine:?}");
        assert_eq!(app.why_not(cx, Action::ShowDdl, &invoices), None);

        // Production: the first click only arms the button.
        let mut config = testing::config(engine);
        config.environment = kerno_core::config::Environment::Production;
        let app = connected_as(cx, config);
        open_scratch(&app, cx);
        app.act(cx, Action::Drop, &invoices);
        assert!(app.has(cx, "sql-production"), "{engine:?}");
        app.click(cx, "sql-confirm");
        assert!(
            app.has(cx, "sql-preview") && exists(),
            "{engine:?}: one click must not drop on production"
        );
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the drop", |app, cx| {
            !app.tree(cx).contains(&row(2, "invoices"))
        });
        assert!(!exists(), "{engine:?}");
    }
}

impl Fixture {
    /// Whether the schema tree has a row at this path of labels.
    fn shows(&self, cx: &mut TestAppContext, path: &[&str]) -> bool {
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .navigator(cx)
                .read(cx)
                .row_of(path, cx)
                .is_some()
        })
    }

    /// Whether the row of the tree at this path is open.
    fn is_open(&self, cx: &mut TestAppContext, path: &[&str]) -> bool {
        let row = self.tree_row(cx, path);
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .navigator(cx)
                .read(cx)
                .is_open(row, cx)
        })
    }

    /// Opens the rows of the tree along `path`, waiting for each to load.
    fn open_path(&self, cx: &mut TestAppContext, path: &[&str]) {
        for depth in 1..=path.len() {
            let part = &path[..depth];
            self.wait_until(cx, &format!("{part:?}"), |app, cx| app.shows(cx, part));
            if depth < path.len() && !self.is_open(cx, part) {
                self.toggle(cx, part);
            }
        }
    }

    /// Replaces `from` by `to` in the active query tab's text.
    fn edit_query(&self, cx: &mut TestAppContext, from: &str, to: &str) {
        let (_, sql) = self.active_query(cx);
        assert!(sql.contains(from), "{from:?} not in\n{sql}");
        let tab = self.tab(cx);
        let editor = cx.update(|cx| tab.read(cx).editor().clone());
        let edited = sql.replacen(from, to, 1);
        self.step(cx, |window, cx| {
            editor.update(cx, |editor, cx| editor.replace_all(edited, window, cx))
        });
    }
}

#[gpui_kit::test]
fn editing_a_source_applies_it_once_its_sql_is_confirmed(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let pg = engine == Engine::Postgres;
        let (cleanup, setup) = if pg {
            (
                "DROP SCHEMA IF EXISTS kerno_ui_source CASCADE",
                [
                    "CREATE SCHEMA kerno_ui_source",
                    "CREATE VIEW kerno_ui_source.v AS SELECT 1 AS a",
                    "CREATE FUNCTION kerno_ui_source.f() RETURNS integer LANGUAGE sql AS 'SELECT 1'",
                ],
            )
        } else {
            (
                "DROP DATABASE IF EXISTS kerno_ui_source",
                [
                    "CREATE DATABASE kerno_ui_source",
                    "CREATE VIEW kerno_ui_source.v AS SELECT 1 AS a",
                    "CREATE FUNCTION kerno_ui_source.f() RETURNS INT DETERMINISTIC RETURN 1",
                ],
            )
        };
        let _scratch = Scratch::new(engine, &[cleanup], &setup);
        let server = |sql: &str| on_server(engine, sql);
        let columns = || {
            server(
                "SELECT count(*) FROM information_schema.columns \
                 WHERE table_schema = 'kerno_ui_source' AND table_name = 'v'",
            )
        };
        let root: &[&str] = if pg {
            &["kerno", "kerno_ui_source"]
        } else {
            &["kerno_ui_source"]
        };
        let at = |rest: &[&'static str]| [root, rest].concat();
        let (view_from, view_to, body_from, body_to) = if pg {
            ("1 AS a", "1 AS a, 2 AS b", "SELECT 1", "SELECT 2")
        } else {
            ("1 AS `a`", "1 AS `a`,2 AS `b`", "RETURN 1", "RETURN 2")
        };
        let routines = if pg { "Functions" } else { "Routines" };

        // A read-only connection offers no source to edit.
        let mut config = testing::config(engine);
        config.read_only = true;
        let app = connected_as(cx, config);
        app.open_path(cx, &at(&["Views", "v"]));
        assert_eq!(
            app.why_not(cx, Action::EditSource, &at(&["Views", "v"])),
            Some("read-only connection"),
            "{engine:?}"
        );
        app.act(cx, Action::EditSource, &at(&["Views", "v"]));
        assert_eq!(app.titles(cx), Vec::<String>::new(), "{engine:?}");

        let app = connected(cx, engine);
        // The view's columns are shown: applying the source reads them again.
        app.open_path(cx, &at(&["Views", "v", "Columns", "a"]));

        // The script opens in a tab in source mode; nothing runs.
        app.act(cx, Action::EditSource, &at(&["Views", "v"]));
        app.wait_until(cx, "the source", |app, cx| app.titles(cx) == ["v"]);
        let (_, sql) = app.active_query(cx);
        assert!(sql.starts_with("CREATE OR REPLACE"), "{engine:?}: {sql}");
        assert!(app.has(cx, "tab-apply-source"));
        app.edit_query(cx, view_from, view_to);

        // Applying shows the script; cancelling runs nothing.
        app.click(cx, "tab-apply-source");
        assert!(app.has(cx, "sql-preview"), "{engine:?}");
        app.click(cx, "sql-cancel");
        assert!(!app.has(cx, "sql-preview"));
        assert_eq!(columns(), "1", "{engine:?}");

        app.click(cx, "tab-apply-source");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the apply", |app, cx| {
            !app.running(cx) && !app.outcomes(cx).is_empty()
        });
        assert!(
            app.outcomes(cx)
                .iter()
                .all(|outcome| outcome.starts_with("done")),
            "{engine:?}: {:?}",
            app.outcomes(cx)
        );
        assert_eq!(server("SELECT b FROM kerno_ui_source.v"), "2", "{engine:?}");
        // The tree read the objects again: the view closed, and opens on
        // its new column.
        app.wait_until(cx, "the refresh", |app, cx| {
            !app.shows(cx, &at(&["Views", "v", "Columns"]))
        });
        app.open_path(cx, &at(&["Views", "v", "Columns"]));
        app.toggle(cx, &at(&["Views", "v", "Columns"]));
        app.wait_until(cx, "the new column", |app, cx| {
            app.shows(cx, &at(&["Views", "v", "Columns", "b"]))
        });

        // A function gets a new body.
        app.open_path(cx, &at(&[routines, "f()"]));
        app.act(cx, Action::EditSource, &at(&[routines, "f()"]));
        app.wait_until(cx, "the function's source", |app, cx| {
            app.titles(cx).len() == 2
        });
        app.edit_query(cx, body_from, body_to);
        app.click(cx, "tab-apply-source");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the apply", |app, cx| {
            !app.running(cx) && !app.outcomes(cx).is_empty()
        });
        assert_eq!(server("SELECT kerno_ui_source.f()"), "2", "{engine:?}");
    }
}

#[gpui_kit::test]
fn a_materialized_view_whose_new_source_fails_is_kept(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_matview CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_matview",
            "CREATE MATERIALIZED VIEW kerno_ui_matview.m AS SELECT 1 AS a",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let m = ["kerno", "kerno_ui_matview", "Materialized views", "m"];
    app.open_path(cx, &m);
    app.act(cx, Action::EditSource, &m);
    app.wait_until(cx, "the source", |app, cx| app.titles(cx) == ["m"]);
    app.edit_query(cx, "1 AS a", "1 AS a, no_such_column");
    let (_, edited) = app.active_query(cx);
    app.click(cx, "tab-apply-source");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the apply", |app, cx| {
        !app.running(cx) && !app.outcomes(cx).is_empty()
    });
    // BEGIN and DROP ran; the CREATE failed and the rest did not run.
    assert_eq!(app.outcomes(cx), ["done", "done", "failed [42703]"]);
    // Rolled back: no transaction left open, the view is still there, and
    // the text waits to be fixed.
    app.wait_until(cx, "the rollback", |app, cx| !app.has(cx, "tab-rollback"));
    assert_eq!(
        on_server(Engine::Postgres, "SELECT a FROM kerno_ui_matview.m"),
        "1"
    );
    assert_eq!(app.active_query(cx).1, edited);
}

#[gpui_kit::test]
fn extensions_are_installed_and_dropped_from_the_tree(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    // Extensions belong to a database: this test has its own, which the
    // app is still connected to when it is dropped.
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP DATABASE IF EXISTS kerno_ui_ext WITH (FORCE)"],
        &["CREATE DATABASE kerno_ui_ext"],
    );
    let app = connected(cx, Engine::Postgres);
    let extensions = ["kerno_ui_ext", "Extensions"];
    app.open_path(cx, &["kerno_ui_ext", "Extensions", "citext"]);
    // Installed extensions come first: a new database has only plpgsql.
    let first = |app: &Fixture, cx: &mut TestAppContext| {
        let tree = app.tree(cx);
        let database = tree
            .iter()
            .position(|line| *line == row(0, "kerno_ui_ext"))?;
        let folder = database
            + tree[database..]
                .iter()
                .position(|line| *line == row(1, "Extensions"))?;
        tree.get(folder + 1).cloned()
    };
    assert_eq!(first(&app, cx), Some(row(2, "plpgsql")));

    app.act(
        cx,
        Action::InstallExtension,
        &["kerno_ui_ext", "Extensions", "citext"],
    );
    assert!(app.has(cx, "sql-preview"));
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the install", |app, cx| {
        first(app, cx) == Some(row(2, "citext"))
    });

    app.act(cx, Action::Drop, &["kerno_ui_ext", "Extensions", "citext"]);
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the drop", |app, cx| {
        first(app, cx) == Some(row(2, "plpgsql"))
    });
    assert!(app.shows(cx, &[&extensions[..], &["citext"]].concat()));
}

#[gpui_kit::test]
fn enums_are_created_and_their_values_added_and_renamed(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_enum CASCADE"],
        &["CREATE SCHEMA kerno_ui_enum"],
    );
    let values = || {
        on_server(
            Engine::Postgres,
            "SELECT enum_range(NULL::kerno_ui_enum.mood)::text",
        )
    };
    let app = connected(cx, Engine::Postgres);
    let types = ["kerno", "kerno_ui_enum", "Types"];
    let mood = ["kerno", "kerno_ui_enum", "Types", "mood"];
    let value = |name: &'static str| [&mood[..], &[name]].concat();
    app.open_path(cx, &types);

    // A name and its values, one per line.
    app.act(cx, Action::CreateEnum, &types);
    app.type_text(cx, "mood");
    app.click(cx, "sql-list");
    app.type_text(cx, "sad");
    app.press(cx, &["enter"]);
    app.type_text(cx, "ok");
    app.click(cx, "sql-confirm");
    // The folder was empty: it is opened once the schema is read again.
    app.wait_until(cx, "the enum", |app, cx| {
        if app.shows(cx, &mood) {
            return true;
        }
        if app.shows(cx, &types) && !app.is_open(cx, &types) {
            app.toggle(cx, &types);
        }
        false
    });
    // The tree lists its values.
    app.open_path(cx, &value("sad"));
    assert!(app.shows(cx, &value("ok")));
    assert_eq!(values(), "{sad,ok}");

    // A value at the end, then one before another.
    app.act(cx, Action::AddEnumValue, &mood);
    app.type_text(cx, "happy");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the new value", |app, cx| {
        app.shows(cx, &value("happy"))
    });
    app.act(cx, Action::AddEnumValue, &value("ok"));
    app.type_text(cx, "meh");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the value before", |app, cx| {
        app.shows(cx, &value("meh"))
    });

    // Renaming starts from the old value.
    app.act(cx, Action::RenameEnumValue, &value("sad"));
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "glum");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the rename", |app, cx| app.shows(cx, &value("glum")));
    assert_eq!(values(), "{glum,meh,ok,happy}");
    let tree = app.tree(cx);
    let start = tree
        .iter()
        .position(|line| *line == row(4, "glum"))
        .unwrap();
    assert_eq!(
        tree[start..start + 4],
        [row(4, "glum"), row(4, "meh"), row(4, "ok"), row(4, "happy")]
    );

    app.act(cx, Action::Drop, &mood);
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the drop", |app, cx| !app.shows(cx, &mood));
}

// Running statements ----------------------------------------------------------

fn cells(row: &[&str]) -> Vec<Option<String>> {
    row.iter().map(|cell| Some(cell.to_string())).collect()
}

#[gpui_kit::test]
fn statements_run_and_their_rows_fill_the_grid(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        let customers = match engine {
            Engine::Postgres => "shop.customers",
            _ => "kerno.customers",
        };
        // Cmd+Enter runs the statement the cursor is in.
        app.run_in_new_tab(
            cx,
            &format!("select id, full_name, phone from {customers} order by id limit 3"),
            "secondary-enter",
        );
        app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["3 rows"]);
        let (columns, rows) = app.result(cx, 0, 10);
        assert_eq!(columns, ["id", "full_name", "phone"], "{engine:?}");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0][0].as_deref(), Some("1"), "{engine:?}");
        assert!(!app.running(cx));
        // The status bar names the tab's connection and database, the rows
        // on screen and how long the statement took.
        let status = cx.update(|cx| app.workspace.read(cx).status_text(cx));
        assert_eq!(
            status[..3],
            [
                format!("test {}", engine.label()),
                "kerno".to_string(),
                "3 rows".to_string()
            ],
            "{engine:?}"
        );
        assert!(status[3].ends_with(" ms"), "{status:?}");

        // Cmd+Shift+Enter runs them all: one result each; a statement
        // without rows says what it did.
        let script = match engine {
            Engine::Postgres => {
                "select 1 as one; select null::text as nothing, '' as blank; set statement_timeout = 0"
            }
            _ => "select 1 as one; select null as nothing, '' as blank; set @kerno_ui = 1",
        };
        app.run_in_new_tab(cx, script, "secondary-shift-enter");
        app.wait_until(cx, "the script", |app, cx| {
            app.outcomes(cx).len() == 3 && !app.running(cx)
        });
        assert_eq!(app.outcomes(cx)[..2], ["1 rows", "1 rows"], "{engine:?}");
        assert!(app.outcomes(cx)[2].starts_with("done"), "{engine:?}");
        assert_eq!(
            app.result(cx, 0, 10),
            (vec!["one".to_string()], vec![cells(&["1"])])
        );
        // NULL is not the empty string.
        assert_eq!(
            app.result(cx, 1, 10).1,
            [vec![None, Some(String::new())]],
            "{engine:?}"
        );

        // A script stops at the first statement that fails.
        app.run_in_new_tab(
            cx,
            "select 1; select * from kerno_no_such_table; select 2",
            "secondary-shift-enter",
        );
        app.wait_until(cx, "the failure", |app, cx| {
            app.outcomes(cx).len() == 2 && !app.running(cx)
        });
        let outcomes = app.outcomes(cx);
        let code = if engine == Engine::Postgres {
            "42P01"
        } else {
            "1146 (42S02)"
        };
        assert_eq!(
            outcomes,
            ["1 rows".to_string(), format!("failed [{code}]")],
            "{engine:?}"
        );
        assert!(
            app.has(cx, "result-error"),
            "the error is the result on screen"
        );
    }
}

#[gpui_kit::test]
fn a_large_result_arrives_a_page_at_a_time(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g, g * 2 as twice from generate_series(1, 2500) g",
        "secondary-enter",
    );
    // The first page is here and the tab is not busy: the rest waits on
    // the server until asked for.
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx) == ["1000 rows, more"]
    });
    assert!(!app.running(cx));
    assert!(app.has(cx, "fetch-more") && app.has(cx, "fetch-all"));

    app.click(cx, "fetch-more");
    app.wait_until(cx, "the second page", |app, cx| {
        app.outcomes(cx) == ["2000 rows, more"]
    });
    app.click(cx, "fetch-all");
    app.wait_until(cx, "the rest", |app, cx| app.outcomes(cx) == ["2500 rows"]);
    assert!(!app.has(cx, "fetch-more") && !app.running(cx));
    assert_eq!(
        app.result(cx, 0, 2).1,
        [cells(&["1", "2"]), cells(&["2", "4"])]
    );

    // In a script, a statement with more than a page stops at its first
    // page so the next one can run.
    app.run_in_new_tab(
        cx,
        "select g from generate_series(1, 2500) g; select 'after' as marker",
        "secondary-shift-enter",
    );
    app.wait_until(cx, "the script", |app, cx| {
        app.outcomes(cx).len() == 2 && !app.running(cx)
    });
    assert_eq!(app.outcomes(cx), ["1000 rows, incomplete", "1 rows"]);
}

#[gpui_kit::test]
fn a_running_statement_is_cancelled_and_the_window_stays_usable(cx: &mut TestAppContext) {
    for (engine, slow) in [
        (Engine::Postgres, "select pg_sleep(60)"),
        (
            Engine::MySql,
            "select count(*) from kerno.orders where sleep(0.05) = 0",
        ),
        (
            Engine::MariaDb,
            "select count(*) from kerno.orders where sleep(0.05) = 0",
        ),
    ] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        app.run_in_new_tab(cx, slow, "secondary-enter");
        app.wait_until(cx, "the statement to start", |app, cx| app.running(cx));
        assert!(app.has(cx, "tab-cancel"), "{engine:?}");

        // While it runs the window answers: another tab opens, the tree is there.
        app.press(cx, &["secondary-t"]);
        assert_eq!(app.titles(cx).len(), 2, "{engine:?}");
        assert!(app.has(cx, "navigator-filter"));
        app.press(cx, &["ctrl-shift-tab"]);
        assert!(app.running(cx), "{engine:?}: still running in its tab");

        let started = std::time::Instant::now();
        app.press(cx, &["secondary-."]);
        app.wait_until(cx, "the cancellation", |app, cx| !app.running(cx));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{engine:?}: {:?}",
            started.elapsed()
        );
        assert_eq!(app.outcomes(cx), ["cancelled"], "{engine:?}");
        assert!(app.has(cx, "result-cancelled"));

        // The session is usable again.
        app.run_in_new_tab(cx, "select 7 as seven", "secondary-enter");
        app.wait_until(cx, "the next statement", |app, cx| {
            app.outcomes(cx) == ["1 rows"]
        });
    }
}

#[gpui_kit::test]
fn a_tab_without_an_open_connection_says_what_to_do(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.run_in_new_tab(cx, "select 1", "secondary-enter");
    app.wait_until(cx, "the refusal", |app, cx| app.outcomes(cx).len() == 1);
    assert_eq!(app.outcomes(cx), ["failed []"]);
    assert!(app.has(cx, "result-error"));
}

#[gpui_kit::test]
fn parameters_are_asked_for_before_a_statement_runs(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    // A named parameter used twice and a numbered one: two values to give.
    app.run_in_new_tab(
        cx,
        "select :who as who, $1::int + 1 as next, :who || '!' as again",
        "secondary-enter",
    );
    assert!(app.has(cx, ("param", 0usize)) && app.has(cx, ("param", 1usize)));
    assert!(!app.has(cx, ("param", 2usize)));
    assert_eq!(app.outcomes(cx), Vec::<String>::new(), "nothing ran yet");
    // The first field has the focus.
    app.type_text(cx, "O'Brien");
    app.click(cx, ("param", 1usize));
    app.type_text(cx, "41");
    app.click(cx, "params-run");
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["1 rows"]);
    // Text became a string, quote and all; the number stayed a number.
    assert_eq!(
        app.result(cx, 0, 1).1,
        [cells(&["O'Brien", "42", "O'Brien!"])]
    );

    // Running again offers what was typed; NULL is a tick away, and Enter runs.
    app.press(cx, &["secondary-enter"]);
    app.click(cx, ("param-null", 0usize));
    app.click(cx, ("param", 1usize));
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the rows", |app, cx| {
        app.outcomes(cx) == ["1 rows"] && app.result(cx, 0, 1).1[0][0].is_none()
    });
    assert_eq!(
        app.result(cx, 0, 1).1,
        [vec![None, Some("42".to_string()), None]]
    );

    // Cancelling the prompt runs nothing.
    app.run_in_new_tab(cx, "select :x", "secondary-enter");
    app.click(cx, "params-cancel");
    assert_eq!(app.outcomes(cx), Vec::<String>::new());

    // MySQL: every `?` is its own value.
    let app = connected(cx, Engine::MySql);
    app.run_in_new_tab(
        cx,
        "select ? as a, ? as b, '?' as not_one",
        "secondary-enter",
    );
    assert!(app.has(cx, ("param", 1usize)) && !app.has(cx, ("param", 2usize)));
    app.type_text(cx, "1.5");
    app.click(cx, ("param", 1usize));
    app.type_text(cx, "two");
    app.click(cx, "params-run");
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert_eq!(app.result(cx, 0, 1).1, [cells(&["1.5", "two", "?"])]);
}

#[gpui_kit::test]
fn manual_commit_keeps_changes_in_a_transaction_until_it_is_ended(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let server = |sql: &str| on_server(engine, sql);
        let (cleanup, setup, table) = match engine {
            Engine::Postgres => (
                "DROP SCHEMA IF EXISTS kerno_ui_tx CASCADE",
                vec![
                    "CREATE SCHEMA kerno_ui_tx",
                    "CREATE TABLE kerno_ui_tx.entries (id int PRIMARY KEY)",
                ],
                "kerno_ui_tx.entries",
            ),
            _ => (
                "DROP DATABASE IF EXISTS kerno_ui_tx",
                vec![
                    "CREATE DATABASE kerno_ui_tx",
                    "CREATE TABLE kerno_ui_tx.entries (id INT PRIMARY KEY)",
                ],
                "kerno_ui_tx.entries",
            ),
        };
        let _scratch = Scratch::new(engine, &[cleanup], &setup);
        let count = || server(&format!("SELECT count(*) FROM {table}"));
        let app = connected(cx, engine);
        let open = |app: &Fixture, cx: &mut TestAppContext| {
            app.status_bar(cx).contains(&"transaction open".to_string())
        };

        // Auto-commit: the row is there for everyone at once.
        app.run_in_new_tab(
            cx,
            &format!("insert into {table} values (1)"),
            "secondary-enter",
        );
        app.wait_until(cx, "the insert", |app, cx| {
            app.outcomes(cx) == ["done, 1 affected"]
        });
        assert_eq!(count(), "1", "{engine:?}");
        assert!(!open(&app, cx) && !app.has(cx, "tab-commit"));

        // Manual commit: the tab opens a transaction with its first statement.
        let tab = app.tab(cx);
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(true, cx)));
        app.press(cx, &["secondary-a"]);
        app.type_text(cx, &format!("insert into {table} values (2)"));
        app.press(cx, &["secondary-enter"]);
        app.wait_until(cx, "the insert", |app, cx| {
            app.outcomes(cx) == ["done, 1 affected"] && !app.running(cx)
        });
        assert!(open(&app, cx), "{engine:?}: {:?}", app.status_bar(cx));
        assert_eq!(count(), "1", "{engine:?}: not committed yet");

        // Rollback undoes it.
        app.click(cx, "tab-rollback");
        app.wait_until(cx, "the rollback", |app, cx| !open(app, cx));
        assert_eq!(count(), "1", "{engine:?}");

        // Commit keeps it.
        app.press(cx, &["secondary-enter"]);
        app.wait_until(cx, "the insert", |app, cx| {
            open(app, cx) && !app.running(cx)
        });
        app.click(cx, "tab-commit");
        app.wait_until(cx, "the commit", |app, cx| !open(app, cx));
        assert_eq!(count(), "2", "{engine:?}");

        if engine == Engine::Postgres {
            // After an error PostgreSQL accepts only a rollback, and the tab says so.
            app.press(cx, &["secondary-a"]);
            app.type_text(cx, "select 1/0");
            app.press(cx, &["secondary-enter"]);
            app.wait_until(cx, "the failure", |app, cx| {
                app.outcomes(cx) == ["failed [22012]"]
            });
            assert!(
                app.status_bar(cx)
                    .contains(&"transaction failed: roll back".to_string())
            );
            app.click(cx, "tab-rollback");
            app.wait_until(cx, "the rollback", |app, cx| !app.has(cx, "tab-rollback"));
        }
    }
}

#[gpui_kit::test]
fn messages_errors_and_history_of_a_run(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    // What the server says on the side is kept apart from the results.
    app.run_in_new_tab(
        cx,
        "do $$ begin raise notice 'hello from the server'; raise warning 'careful'; end $$",
        "secondary-enter",
    );
    app.wait_until(cx, "the block", |app, cx| {
        app.outcomes(cx).len() == 1 && !app.running(cx)
    });
    assert!(app.outcomes(cx)[0].starts_with("done"));
    let tab = app.tab(cx);
    assert_eq!(
        cx.update(|cx| tab.read(cx).messages()),
        ["NOTICE: hello from the server", "WARNING: careful"]
    );
    assert!(app.status_bar(cx).contains(&"2 messages".to_string()));

    // A failed statement is marked where the server says it went wrong,
    // and the mark goes when the statement is run again.
    app.run_in_new_tab(
        cx,
        "select id,\n  nope\nfrom shop.customers",
        "secondary-enter",
    );
    app.wait_until(cx, "the failure", |app, cx| {
        app.outcomes(cx) == ["failed [42703]"]
    });
    let tab = app.tab(cx);
    assert_eq!(cx.update(|cx| tab.read(cx).error_marks(cx)), 1);
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "select 1");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert_eq!(cx.update(|cx| tab.read(cx).error_marks(cx)), 0);

    // Every statement that ran is in the history, with how it ended.
    let storage = app.storage.clone();
    app.wait_until(cx, "the history", |_, _| {
        storage.history(None, "", 10).unwrap().len() == 3
    });
    let history = storage.history(None, "", 10).unwrap();
    let connection = storage.connections().unwrap()[0].id.clone();
    assert!(
        history
            .iter()
            .all(|entry| entry.connection_id == connection)
    );
    let ended: Vec<(&str, kerno_core::storage::HistoryStatus, Option<i64>)> = history
        .iter()
        .rev()
        .map(|entry| {
            (
                entry.sql.lines().next().unwrap_or_default(),
                entry.status,
                entry.row_count,
            )
        })
        .collect();
    use kerno_core::storage::HistoryStatus::{Error, Ok as Fine};
    assert_eq!(ended[1], ("select id,", Error, None));
    assert_eq!(ended[2], ("select 1", Fine, Some(1)));
    assert!(ended[0].0.starts_with("do $$") && ended[0].1 == Fine);
    assert!(history.iter().any(|entry| {
        entry
            .error
            .as_deref()
            .is_some_and(|error| error.contains("nope"))
    }));
}

// The results grid --------------------------------------------------------------

#[gpui_kit::test]
fn the_grid_sorts_copies_and_rearranges_what_it_shows(cx: &mut TestAppContext) {
    use gpui_kit::component::table::{ColumnSort, TableDelegate as _};

    use crate::query::grid::{CopyFormat, Grid};

    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g as n, case when g % 3 = 0 then null else 'v' || g end as label, g % 2 = 0 as even \
         from generate_series(1, 6) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["6 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    let shown = |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).delegate().shown());
    let row = |cells: [&str; 3]| cells.map(str::to_string).to_vec();
    // NULL and booleans are shown for what they are.
    assert_eq!(
        shown(cx)[..3],
        [
            row(["1", "v1", "false"]),
            row(["2", "v2", "true"]),
            row(["3", "NULL", "false"])
        ]
    );

    // Sorting is done here, on what was fetched; numbers as numbers, NULLs last.
    let sort = |cx: &mut TestAppContext, column: usize, order: ColumnSort| {
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| {
                table.delegate_mut().perform_sort(column, order, window, cx)
            })
        });
    };
    sort(cx, 1, ColumnSort::Descending);
    assert_eq!(
        shown(cx)
            .iter()
            .map(|row| row[0].clone())
            .collect::<Vec<_>>(),
        ["6", "5", "4", "3", "2", "1"]
    );
    sort(cx, 2, ColumnSort::Ascending);
    assert_eq!(
        shown(cx)
            .iter()
            .map(|row| row[1].clone())
            .collect::<Vec<_>>(),
        ["v1", "v2", "v4", "v5", "NULL", "NULL"]
    );
    sort(cx, 1, ColumnSort::Descending);

    // A range is the table's selected cell stretched to another one.
    let copy = |cx: &mut TestAppContext, format: Option<CopyFormat>| {
        let copied =
            cx.update(|cx| grid.update(cx, |table, cx| Grid::copy(table, format, None, cx)));
        assert!(copied, "{format:?}");
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    };
    cx.update(|cx| {
        grid.update(cx, |table, cx| {
            table.set_selected_cell(0, 1, cx);
            table.delegate_mut().extend_to(2, 2);
        })
    });
    assert_eq!(copy(cx, Some(CopyFormat::Tsv)), "6\t\n5\tv5\n4\tv4\n");
    assert_eq!(
        copy(cx, Some(CopyFormat::TsvWithHeader)),
        "n\tlabel\n6\t\n5\tv5\n4\tv4\n"
    );
    assert_eq!(
        copy(cx, Some(CopyFormat::Csv)),
        "n,label\r\n6,\r\n5,v5\r\n4,v4\r\n"
    );
    assert_eq!(
        copy(cx, Some(CopyFormat::Json)),
        "[{\"n\":6,\"label\":null},\n{\"n\":5,\"label\":\"v5\"},\n{\"n\":4,\"label\":\"v4\"}]"
    );
    assert_eq!(
        copy(cx, Some(CopyFormat::Markdown)),
        "| n | label |\n| ---: | --- |\n| 6 | *NULL* |\n| 5 | v5 |\n| 4 | v4 |\n"
    );
    assert_eq!(
        copy(cx, Some(CopyFormat::SqlInsert)).lines().next(),
        Some("INSERT INTO \"schema_name\".\"table_name\" (\"n\", \"label\") VALUES (6, NULL);")
    );
    assert_eq!(copy(cx, None), "n\nlabel");
    // One cell is copied as its value, without quoting.
    cx.update(|cx| {
        grid.update(cx, |table, cx| {
            table.delegate_mut().clear_selection();
            table.set_selected_cell(1, 2, cx);
        })
    });
    assert_eq!(copy(cx, Some(CopyFormat::Tsv)), "v5");

    // Columns can be hidden, pinned and shown again.
    cx.update(|cx| {
        grid.update(cx, |table, cx| {
            table.delegate_mut().hide(2);
            table.refresh(cx);
        })
    });
    let columns =
        |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).delegate().shown_columns());
    assert_eq!(columns(cx), ["n", "even"]);
    cx.update(|cx| grid.update(cx, |table, _| table.delegate_mut().toggle_pin(2)));
    assert_eq!(columns(cx), ["even", "n"]);
    assert_eq!(shown(cx)[0], ["true", "6"]);
    cx.update(|cx| grid.update(cx, |table, _| table.delegate_mut().show_all()));
    assert_eq!(columns(cx), ["n", "label", "even"]);

    // Everything, with Cmd+A then Cmd+C in the grid.
    cx.update(|cx| tab.update(cx, |tab, cx| tab.select_all_rows(cx)));
    assert_eq!(copy(cx, Some(CopyFormat::Tsv)).lines().count(), 6);
}

#[gpui_kit::test]
fn a_double_click_on_a_cell_shows_its_whole_value(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select repeat('long text ', 200) as body, '{\"a\": [1, 2]}'::jsonb as doc, '\\x00ff10'::bytea as raw",
        "secondary-enter",
    );
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    // The grid cuts what is long; the viewer does not.
    let shown = cx.update(|cx| grid.read(cx).delegate().shown());
    assert_eq!(shown[0][0].chars().count(), 300);
    assert_eq!(shown[0][2], "\\x00ff10");
    for column in 1..=3 {
        app.step(cx, |_, cx| {
            grid.update(cx, |_, cx| {
                cx.emit(gpui_kit::component::table::TableEvent::DoubleClickedCell(
                    0, column,
                ))
            })
        });
        assert!(app.has(cx, "value-text"), "column {column}");
        app.click(cx, "value-copy");
        let copied = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        match column {
            1 => assert_eq!(copied.len(), 2000),
            2 => assert_eq!(copied, "{\n  \"a\": [\n    1,\n    2\n  ]\n}"),
            _ => assert!(copied.starts_with("00000000  00 ff 10"), "{copied}"),
        }
        app.click(cx, "value-close");
        assert!(!app.has(cx, "value-text"));
    }
}

#[gpui_kit::test]
fn a_right_click_on_a_cell_opens_the_menu_of_its_row(cx: &mut TestAppContext) {
    use gpui_kit::test::{ClickOptions, TestQueryExt as _};
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(cx, "select 1 as a, 'x' as b", "secondary-enter");
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    let menu_has = |app: &Fixture, cx: &mut TestAppContext, label: &str| {
        app.step(cx, |window, _| window.try_find_by_label(label).is_some())
    };
    assert!(!menu_has(&app, cx, "Copy as CSV"));
    // On the first row, under the headers, in the first column of values.
    app.step(cx, |window, cx| {
        window.click_with_options(
            "table-inner",
            ClickOptions::new()
                .with_offset(gpui_kit::point(px(90.), px(44.)))
                .with_button(gpui_kit::MouseButton::Right),
            cx,
        )
    });
    assert!(menu_has(&app, cx, "Copy as CSV"));
    assert!(menu_has(&app, cx, "Copy column names"));
}

// Plans and guards ----------------------------------------------------------------

#[gpui_kit::test]
fn plans_are_shown_as_a_tree_on_every_engine(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        let schema = if engine == Engine::Postgres {
            "shop"
        } else {
            "kerno"
        };
        let sql = format!(
            "select c.full_name, o.id from {schema}.customers c \
             join {schema}.orders o on o.customer_id = c.id where o.status = 'paid'"
        );
        // Cmd+E: the plan, without running the statement.
        app.run_in_new_tab(cx, &sql, "secondary-e");
        app.wait_until(cx, "the plan", |app, cx| {
            app.outcomes(cx).len() == 1 && !app.running(cx)
        });
        let outcome = app.outcomes(cx).remove(0);
        let steps: usize = outcome
            .strip_prefix("plan, ")
            .and_then(|rest| rest.strip_suffix(" steps"))
            .and_then(|steps| steps.parse().ok())
            .unwrap_or_else(|| panic!("{engine:?}: {outcome}"));
        assert!(steps >= 3, "{engine:?}: {outcome}");
        assert!(
            app.has(cx, "plan-tree") && app.has(cx, ("plan-node", steps - 1)),
            "{engine:?}"
        );

        // Cmd+Shift+E: run it and time each step; the raw output stays a click away.
        app.press(cx, &["secondary-shift-e"]);
        app.wait_until(cx, "the analyzed plan", |app, cx| {
            app.outcomes(cx).len() == 1 && !app.running(cx)
        });
        assert!(
            app.outcomes(cx)[0].starts_with("plan, "),
            "{engine:?}: {:?}",
            app.outcomes(cx)
        );
        app.click(cx, "plan-as-raw");
        assert!(
            app.has(cx, "plan-raw") && !app.has(cx, "plan-tree"),
            "{engine:?}"
        );
        app.click(cx, "plan-as-tree");
        assert!(app.has(cx, "plan-tree"), "{engine:?}");
    }
}

#[gpui_kit::test]
fn statements_that_destroy_or_write_are_guarded(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    let pg = |sql: &str| on_server(Engine::Postgres, sql);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_guard CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_guard",
            "CREATE TABLE kerno_ui_guard.items AS SELECT generate_series(1, 3) AS id",
        ],
    );
    let count = || pg("SELECT count(*) FROM kerno_ui_guard.items");

    // A read-only connection runs nothing that writes, EXPLAIN ANALYZE included.
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    app.run_in_new_tab(
        cx,
        "delete from kerno_ui_guard.items where id = 1",
        "secondary-enter",
    );
    assert_eq!(app.outcomes(cx), ["failed []"]);
    assert!(app.has(cx, "result-error") && !app.has(cx, "sql-preview"));
    app.press(cx, &["secondary-shift-e"]);
    assert_eq!(app.outcomes(cx), ["failed []"]);
    assert_eq!(count(), "3");
    // Reading, and a plan that does not run the statement, are fine.
    app.press(cx, &["secondary-e"]);
    app.wait_until(cx, "the plan", |app, cx| {
        app.outcomes(cx).len() == 1 && !app.running(cx)
    });
    assert!(
        app.outcomes(cx)[0].starts_with("plan, "),
        "{:?}",
        app.outcomes(cx)
    );
    app.run_in_new_tab(
        cx,
        "select count(*) from kerno_ui_guard.items",
        "secondary-enter",
    );
    app.wait_until(cx, "the count", |app, cx| app.outcomes(cx) == ["1 rows"]);

    // Elsewhere, what destroys is shown and confirmed; what does not, runs.
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "delete from kerno_ui_guard.items where id = 1",
        "secondary-enter",
    );
    app.wait_until(cx, "the delete", |app, cx| {
        app.outcomes(cx) == ["done, 1 affected"]
    });
    assert_eq!(count(), "2");
    app.run_in_new_tab(
        cx,
        "select 1; delete from kerno_ui_guard.items",
        "secondary-shift-enter",
    );
    assert!(app.has(cx, "sql-preview") && !app.has(cx, "sql-production"));
    assert_eq!(
        app.outcomes(cx),
        Vec::<String>::new(),
        "nothing runs before the answer"
    );
    app.click(cx, "sql-cancel");
    assert_eq!(app.outcomes(cx), Vec::<String>::new());
    assert_eq!(count(), "2");
    app.press(cx, &["secondary-shift-enter"]);
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the script", |app, cx| {
        app.outcomes(cx).len() == 2 && !app.running(cx)
    });
    assert_eq!(app.outcomes(cx), ["1 rows", "done, 2 affected"]);
    assert_eq!(count(), "0");
    // The keyboard still works after the dialog.
    app.press(cx, &["secondary-t"]);
    assert_eq!(app.titles(cx).len(), 3);

    // On production every write is confirmed, twice.
    let _database = Scratch::new(
        Engine::MySql,
        &["DROP DATABASE IF EXISTS kerno_ui_guard"],
        &[
            "CREATE DATABASE kerno_ui_guard",
            "CREATE TABLE kerno_ui_guard.items (id INT PRIMARY KEY)",
        ],
    );
    let mut config = testing::config(Engine::MySql);
    config.environment = kerno_core::config::Environment::Production;
    let app = connected_as(cx, config);
    app.run_in_new_tab(
        cx,
        "insert into kerno_ui_guard.items values (1)",
        "secondary-enter",
    );
    assert!(app.has(cx, "sql-production"));
    app.click(cx, "sql-confirm");
    assert!(
        app.has(cx, "sql-preview"),
        "one click is not enough on production"
    );
    assert_eq!(
        on_server(Engine::MySql, "SELECT count(*) FROM kerno_ui_guard.items"),
        "0"
    );
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the insert", |app, cx| {
        app.outcomes(cx) == ["done, 1 affected"]
    });
    // Reading is never questioned.
    app.run_in_new_tab(
        cx,
        "select count(*) from kerno_ui_guard.items",
        "secondary-enter",
    );
    app.wait_until(cx, "the count", |app, cx| app.outcomes(cx) == ["1 rows"]);
}

// Files, history, saved queries ---------------------------------------------------

#[gpui_kit::test]
async fn sql_files_are_opened_saved_and_marked_when_changed(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.sql");
    std::fs::write(&file, "select 42;\n").unwrap();
    let open_file = |app: &Fixture, cx: &mut TestAppContext, path: &std::path::Path| {
        let (workspace, path) = (app.workspace.clone(), path.to_path_buf());
        app.step(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_sql_file_from(path, window, cx)
            })
        });
    };
    open_file(&app, cx, &file);
    app.wait_until(cx, "the file", |app, cx| app.titles(cx) == ["report.sql"]);
    assert_eq!(app.active_query(cx).1, "select 42;\n");
    let unsaved = |app: &Fixture, cx: &mut TestAppContext| {
        let tab = app.tab(cx);
        cx.update(|cx| tab.read(cx).unsaved())
    };
    assert!(!unsaved(&app, cx));

    // Typing marks the tab; Cmd+S writes the file and clears the mark.
    app.type_text(cx, "-- checked\n");
    assert!(unsaved(&app, cx));
    app.press(cx, &["secondary-s"]);
    app.wait_until(cx, "the save", |app, cx| !unsaved(app, cx));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "-- checked\nselect 42;\n"
    );

    // A file already open is shown, not opened twice.
    app.press(cx, &["secondary-t"]);
    open_file(&app, cx, &file);
    cx.run_until_parked();
    assert_eq!(app.titles(cx), ["report.sql", "Query 1"]);
    assert_eq!(app.active(cx), Some(0));

    // "Save as" moves the tab to the new file.
    let copy = dir.path().join("copy of report.sql");
    let (workspace, target) = (app.workspace.clone(), copy.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.save_active_tab_to(target, window, cx)
        })
    });
    app.wait_until(cx, "the copy", |app, cx| {
        app.titles(cx)[0] == "copy of report.sql"
    });
    assert_eq!(
        std::fs::read_to_string(&copy).unwrap(),
        "-- checked\nselect 42;\n"
    );

    // The tab is the same file after a restart.
    app.wait_for_stored_tabs(cx, |tabs| {
        tabs.first().is_some_and(|tab| {
            matches!(tab, TabSnapshot::Query { file: Some(path), unsaved: false, .. } if *path == copy)
        })
    })
    .await;
    let restarted = open(cx, storage);
    assert_eq!(restarted.titles(cx), ["copy of report.sql", "Query 1"]);

    // A file that cannot be read says so and opens nothing.
    open_file(&restarted, cx, &dir.path().join("missing.sql"));
    cx.run_until_parked();
    assert_eq!(restarted.titles(cx).len(), 2);
}

#[gpui_kit::test]
fn history_and_saved_queries_are_searched_and_reopened(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(cx, "select 'needle' as hay", "secondary-enter");
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["1 rows"]);
    app.run_in_new_tab(cx, "select 2 as other", "secondary-enter");
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["1 rows"]);
    let storage = app.storage.clone();
    app.wait_until(cx, "the history", |_, _| {
        storage.history(None, "", 10).unwrap().len() == 2
    });

    // Save the text of the tab under a name, in a folder.
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::SaveQuery), cx)
    });
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "The other one");
    app.click(cx, "save-query-folder");
    app.type_text(cx, "/samples/");
    app.click(cx, "save-query-confirm");
    app.wait_until(cx, "the saved query", |_, _| {
        storage.saved_queries().unwrap().len() == 1
    });
    let saved = storage.saved_queries().unwrap().remove(0);
    assert_eq!(
        (
            saved.name.as_str(),
            saved.folder.as_str(),
            saved.sql.as_str()
        ),
        ("The other one", "samples", "select 2 as other")
    );

    // Cmd+Y: the history, newest first; the search narrows it; a click
    // opens the statement in a tab on the connection it ran on.
    app.press(cx, &["secondary-y"]);
    app.wait_until(cx, "the history list", |app, cx| {
        app.has(cx, ("history", 1usize))
    });
    app.type_text(cx, "NEEDLE");
    app.wait_until(cx, "the search", |app, cx| {
        !app.has(cx, ("history", 1usize))
    });
    app.click(cx, ("history", 0usize));
    assert!(!app.has(cx, "library-search"), "picking closes the library");
    assert_eq!(
        app.active_query(cx),
        ("History".to_string(), "select 'needle' as hay".to_string())
    );
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["1 rows"]);

    // The saved query opens under its name; deleting it takes two clicks.
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::ShowSavedQueries), cx)
    });
    app.wait_until(cx, "the saved list", |app, cx| {
        app.has(cx, ("saved", 0usize))
    });
    app.click(cx, ("saved", 0usize));
    assert_eq!(
        app.active_query(cx),
        ("The other one".to_string(), "select 2 as other".to_string())
    );
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::ShowSavedQueries), cx)
    });
    app.wait_until(cx, "the saved list", |app, cx| {
        app.has(cx, ("saved-delete", 0usize))
    });
    app.click(cx, ("saved-delete", 0usize));
    assert_eq!(
        storage.saved_queries().unwrap().len(),
        1,
        "one click only asks"
    );
    app.click(cx, ("saved-delete", 0usize));
    app.wait_until(cx, "the deletion", |_, _| {
        storage.saved_queries().unwrap().is_empty()
    });
}

#[gpui_kit::test]
fn the_comment_shortcut_toggles_the_lines_of_the_selection(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 1\nfrom dual");
    // The cursor is on the last line: only that one.
    app.press(cx, &["secondary-/"]);
    assert_eq!(app.active_query(cx).1, "select 1\n-- from dual");
    // Everything selected: both lines, as a block.
    app.press(cx, &["secondary-a", "secondary-/"]);
    assert_eq!(app.active_query(cx).1, "-- select 1\n-- -- from dual");
    app.press(cx, &["secondary-a", "secondary-/"]);
    assert_eq!(app.active_query(cx).1, "select 1\n-- from dual");
    // One undo takes one toggle back.
    app.press(cx, &["secondary-z"]);
    assert_eq!(app.active_query(cx).1, "-- select 1\n-- -- from dual");
}

#[gpui_kit::test]
fn the_editor_searches_and_replaces(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 1");
    let searching = |cx: &mut TestAppContext| {
        let tab = app.tab(cx);
        cx.update(|cx| tab.read(cx).searching(cx))
    };
    assert_eq!(searching(cx), None);
    // The editor's own shortcut, then the commands of the palette and menu.
    app.press(cx, &["cmd-f"]);
    assert_eq!(searching(cx), Some(false));
    app.press(cx, &["escape"]);
    assert_eq!(searching(cx), None);
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::ReplaceInEditor), cx)
    });
    assert_eq!(searching(cx), Some(true));
}

/// Labels and inserted texts completion proposes in the active tab at the
/// `|` of `sql`.
fn proposals(app: &Fixture, cx: &mut TestAppContext, sql: &str) -> Vec<(String, String)> {
    let cursor = sql.find('|').expect("a | marks the cursor");
    let text = sql.replacen('|', "", 1);
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).completions(&text, cursor, cx))
}

fn labels(proposals: Vec<(String, String)>) -> Vec<String> {
    proposals.into_iter().map(|(label, _)| label).collect()
}

/// Opens a query tab and waits for what its completion proposes from.
fn new_tab_with_catalog(app: &Fixture, cx: &mut TestAppContext) {
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    app.wait_until(cx, "the completion catalog", |_, cx| {
        cx.update(|cx| tab.read(cx).catalog_ready())
    });
}

/// Whether the editor's completion list is open, and its labels.
fn completion_menu(app: &Fixture, cx: &mut TestAppContext) -> (bool, Vec<String>) {
    let tab = app.tab(cx);
    cx.update(|cx| {
        let editor = tab.read(cx).editor().read(cx);
        let menu = editor.completion_menu_state();
        let labels = menu.items.iter().map(|item| item.label.clone()).collect();
        (menu.open, labels)
    })
}

#[gpui_kit::test]
fn completion_proposes_tables_columns_and_joins(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    new_tab_with_catalog(&app, cx);
    let tables = labels(proposals(&app, cx, "select * from shop.ord|"));
    assert!(tables.contains(&"orders".to_string()), "{tables:?}");
    let columns = labels(proposals(&app, cx, "select o.| from shop.orders o"));
    assert_eq!(columns[..3], ["id", "customer_id", "status"], "{columns:?}");
    // The join along the foreign key comes first, with its condition.
    let joins = labels(proposals(&app, cx, "select * from shop.orders o join |"));
    assert!(
        joins[0].contains("customers ON ") && joins[0].contains("o.customer_id"),
        "{joins:?}"
    );
    // A word typed in full proposes nothing: Enter starts a new line.
    let finished = proposals(&app, cx, "select * from shop.orders o left join|");
    assert_eq!(finished, [], "{finished:?}");

    if !testing::available(Engine::MySql) {
        return;
    }
    // A table named with a reserved word is written quoted.
    let (drop, create) = scratch_schema(Engine::MySql, "kerno_ui_complete");
    let _scratch = Scratch::new(
        Engine::MySql,
        &[&drop],
        &[&create, "CREATE TABLE kerno_ui_complete.`select` (id INT)"],
    );
    let app = connected(cx, Engine::MySql);
    new_tab_with_catalog(&app, cx);
    let tables = proposals(&app, cx, "select * from kerno_ui_complete.sel|");
    assert_eq!(
        tables.first(),
        Some(&("select".to_string(), "`select`".to_string())),
        "{tables:?}"
    );
}

#[gpui_kit::test]
fn typing_in_the_editor_opens_completion_and_enter_accepts_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    new_tab_with_catalog(&app, cx);
    app.type_text(cx, "select * from shop.cust");
    let (open, items) = completion_menu(&app, cx);
    assert!(open, "{items:?}");
    assert_eq!(items[0], "customers");
    app.press(cx, &["enter"]);
    assert_eq!(app.active_query(cx).1, "select * from shop.customers");
    assert!(!completion_menu(&app, cx).0);
    // A word opens the list again; a space closes it and accepts nothing.
    app.type_text(cx, " w");
    assert!(completion_menu(&app, cx).0);
    app.type_text(cx, " ");
    assert!(!completion_menu(&app, cx).0);
    assert_eq!(app.active_query(cx).1, "select * from shop.customers w ");
    // After a dot come the alias's columns.
    app.type_text(cx, "where w.");
    let (open, items) = completion_menu(&app, cx);
    assert!(open);
    assert_eq!(items[..2], ["id", "email"], "{items:?}");
}

#[gpui_kit::test]
fn the_catalog_follows_ddl(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (drop, create) = scratch_schema(Engine::Postgres, "kerno_ui_c");
    let _scratch = Scratch::new(Engine::Postgres, &[&drop], &[&create]);
    let app = connected(cx, Engine::Postgres);
    new_tab_with_catalog(&app, cx);
    let fresh = |app: &Fixture, cx: &mut TestAppContext| {
        labels(proposals(app, cx, "select * from kerno_ui_c.|")).contains(&"fresh".to_string())
    };
    assert!(!fresh(&app, cx));
    app.type_text(cx, "create table kerno_ui_c.fresh (id int, label text)");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the new table in completion", |app, cx| fresh(app, cx));
    assert_eq!(app.outcomes(cx), ["done"]);
}

#[gpui_kit::test]
fn format_sql_rewrites_the_selection_or_everything_as_one_undo_step(cx: &mut TestAppContext) {
    use kerno_core::sql::{LexRules, format_sql};
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    let first = "select 1;\n";
    let second = "select a, b from t where x = 1 and y = 2";
    app.type_text(cx, &format!("{first}{second}"));
    // The selection: the second line only, without the newline the
    // formatter ends with.
    app.press(cx, &["shift-home", "secondary-shift-i"]);
    let laid_out = format_sql(second, LexRules::POSTGRES);
    let selection_done = format!("{first}{}", laid_out.trim_end_matches('\n'));
    assert_ne!(laid_out.trim_end(), second);
    assert_eq!(app.active_query(cx).1, selection_done);
    let tab = app.tab(cx);
    let cursor = cx.update(|cx| tab.read(cx).editor().read(cx).cursor());
    assert_eq!(cursor, first.len());
    // No selection: everything, from the menu's command.
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(crate::actions::FormatSql), cx)
    });
    let everything = format_sql(&selection_done, LexRules::POSTGRES);
    assert_ne!(everything, selection_done);
    assert_eq!(app.active_query(cx).1, everything);
    // Already laid out: nothing changes and there is no step to undo.
    app.press(cx, &["secondary-shift-i"]);
    assert_eq!(app.active_query(cx).1, everything);
    app.press(cx, &["secondary-z"]);
    assert_eq!(app.active_query(cx).1, selection_done);
}

// Table data --------------------------------------------------------------------

/// A scratch table with these columns after an auto-numbered `id`, on
/// any engine.
fn scratch_table(engine: Engine, name: &str, columns: &str) -> String {
    let id = match engine {
        Engine::Postgres => "integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY",
        Engine::MySql | Engine::MariaDb => "INT AUTO_INCREMENT PRIMARY KEY",
    };
    format!("CREATE TABLE {name} (id {id}, {columns})")
}

/// What removes and creates a scratch schema (a database on MySQL). Each
/// test has its own: they run side by side.
fn scratch_schema(engine: Engine, name: &str) -> (String, String) {
    match engine {
        Engine::Postgres => (
            format!("DROP SCHEMA IF EXISTS {name} CASCADE"),
            format!("CREATE SCHEMA {name}"),
        ),
        Engine::MySql | Engine::MariaDb => (
            format!("DROP DATABASE IF EXISTS {name}"),
            format!("CREATE DATABASE {name}"),
        ),
    }
}

const ENGINES: [Engine; 3] = [Engine::Postgres, Engine::MySql, Engine::MariaDb];

#[gpui_kit::test]
fn a_table_is_read_in_pages_sorted_and_filtered_by_the_server(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_pages");
        let day = match engine {
            Engine::Postgres => "DATE '2020-01-01' + n",
            Engine::MySql | Engine::MariaDb => "DATE_ADD('2020-01-01', INTERVAL n DAY)",
        };
        let digits = "(SELECT 0 d UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 \
                      UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 \
                      UNION ALL SELECT 8 UNION ALL SELECT 9)";
        // 250 rows: item 1 to item 250.
        let fill = format!(
            "INSERT INTO kerno_ui_pages.items (name, qty, born) \
             SELECT CONCAT('item ', n), MOD(n, 7), {day} FROM \
             (SELECT a.d + b.d * 10 + c.d * 100 + 1 AS n FROM {digits} a, {digits} b, {digits} c) numbers \
             WHERE n <= 250 ORDER BY n"
        );
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &scratch_table(
                    engine,
                    "kerno_ui_pages.items",
                    "name VARCHAR(50) NOT NULL, qty INT, born DATE",
                ),
                &fill,
            ],
        );
        let app = connected(cx, engine);
        cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 100));
        app.open_table(cx, "kerno_ui_pages", "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–100 of 250");
        assert_eq!(app.titles(cx), ["items"]);
        assert_eq!(app.data_rows(cx)[0][..3], ["1", "item 1", "1"]);
        // Opening it again shows the same tab.
        app.open_table(cx, "kerno_ui_pages", "items", RelationKind::Table);
        assert_eq!(app.titles(cx), ["items"]);

        // Pages.
        app.click(cx, "data-next");
        app.wait_for_rows(cx, "Rows 101–200 of 250");
        assert_eq!(app.data_rows(cx)[0][0], "101");
        app.click(cx, "data-next");
        app.wait_for_rows(cx, "Rows 201–250 of 250");
        // There is no page after the last.
        app.on_data(cx, |tab, window, cx| tab.turn_page(1, window, cx));
        cx.run_until_parked();
        assert_eq!(app.data_state(cx), "Rows 201–250 of 250");
        app.click(cx, "data-previous");
        app.wait_for_rows(cx, "Rows 101–200 of 250");

        // A click on a header sorts on the server, from the first page.
        let grid = app.data_grid(cx);
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| {
                table
                    .delegate_mut()
                    .perform_sort(3, ColumnSort::Descending, window, cx)
            })
        });
        app.wait_until(cx, "the sorted page", |app, cx| {
            let tab = app.data(cx);
            cx.update(|cx| tab.read(cx).sort() == Some(("qty", true)))
                && app.data_state(cx) == "Rows 1–100 of 250"
        });
        // The largest quantity first, and the key settles ties.
        assert_eq!(app.data_rows(cx)[0][..3], ["6", "item 6", "6"]);
        assert_eq!(app.data_rows(cx)[1][..3], ["13", "item 13", "6"]);

        // The filter bar: a condition on the first column.
        app.click(cx, "filter-add");
        app.type_text(cx, "42");
        app.press(cx, &["enter"]);
        app.wait_for_rows(cx, "Rows 1–1 of 1");
        assert_eq!(app.data_rows(cx)[0][..2], ["42", "item 42"]);
        // Conditions and SQL together.
        app.on_data(cx, |tab, window, cx| {
            tab.set_filter(
                Filter {
                    conditions: vec![Condition {
                        column: "name".into(),
                        operator: Operator::Contains,
                        value: "item 24".into(),
                    }],
                    raw: "qty < 3 -- few".into(),
                },
                window,
                cx,
            )
        });
        app.wait_for_rows(cx, "Rows 1–4 of 4");
        let ids: Vec<String> = app
            .data_rows(cx)
            .into_iter()
            .map(|row| row[0].clone())
            .collect();
        // Sorted on the quantity, descending, then on the key.
        assert_eq!(ids, ["240", "247", "246", "245"]);

        // Nothing matches: the grid says so.
        app.on_data(cx, |tab, window, cx| {
            let nothing = Filter {
                conditions: vec![Condition::equals("id", "-1")],
                raw: String::new(),
            };
            tab.set_filter(nothing, window, cx)
        });
        app.wait_for_rows(cx, "No rows");
        assert!(app.has(cx, "data-empty"));

        // A filter the server refuses is reported, and can be taken away.
        app.on_data(cx, |tab, window, cx| {
            let broken = Filter {
                conditions: Vec::new(),
                raw: "nope nope".into(),
            };
            tab.set_filter(broken, window, cx)
        });
        app.wait_until(cx, "the error", |app, cx| {
            app.data_state(cx).starts_with("failed")
        });
        assert!(app.has(cx, "data-error"));
        app.click(cx, "filter-clear");
        app.wait_for_rows(cx, "Rows 1–100 of 250");

        // Every operator is valid SQL on every kind of column.
        for (column, value) in [("id", "5"), ("name", "item 5"), ("born", "2020-01-06")] {
            for operator in Operator::ALL {
                app.on_data(cx, |tab, window, cx| {
                    let one = Filter {
                        conditions: vec![Condition {
                            column: column.into(),
                            operator,
                            value: value.into(),
                        }],
                        raw: String::new(),
                    };
                    tab.set_filter(one, window, cx)
                });
                app.wait_until(cx, "the filtered rows", |app, cx| {
                    let state = app.data_state(cx);
                    assert!(
                        !state.starts_with("failed"),
                        "{engine:?} {column} {operator:?}: {state}"
                    );
                    state != "loading"
                });
                // These three keep the one row whose value this is.
                if matches!(operator, Operator::Equals | Operator::In | Operator::Like) {
                    assert_eq!(
                        app.data_rows(cx)[0][0],
                        "5",
                        "{engine:?} {column} {operator:?}"
                    );
                }
            }
        }
    }
}

#[gpui_kit::test]
fn cells_are_edited_reviewed_and_applied_in_one_transaction(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let server = |sql: &str| on_server(engine, sql);
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_edits");
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &scratch_table(
                    engine,
                    "kerno_ui_edits.people",
                    "name VARCHAR(50) NOT NULL, qty INT, note VARCHAR(20) DEFAULT 'n/a'",
                ),
                "INSERT INTO kerno_ui_edits.people (name, qty, note) \
                 VALUES ('ann', 1, 'x'), ('bob', 2, NULL), ('cy', 3, 'z')",
            ],
        );
        let count = |condition: &str| {
            server(&format!(
                "SELECT count(*) FROM kerno_ui_edits.people WHERE {condition}"
            ))
        };
        let app = connected(cx, engine);
        app.open_table(cx, "kerno_ui_edits", "people", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–3 of 3");
        assert!(!app.has(cx, "data-pending") && !app.has(cx, "data-read-only"));
        let select_row = |app: &Fixture, cx: &mut TestAppContext, row: usize| {
            let grid = app.data_grid(cx);
            app.step(cx, |_, cx| {
                grid.update(cx, |table, cx| table.set_selected_row(row, cx))
            });
        };
        let select_cell = |app: &Fixture, cx: &mut TestAppContext, row: usize, column: usize| {
            let grid = app.data_grid(cx);
            app.step(cx, |_, cx| {
                grid.update(cx, |table, cx| table.set_selected_cell(row, column, cx))
            });
        };

        // Typing in a cell changes nothing on the server: the change waits.
        app.type_in_cell(cx, 0, 2, "anna", "enter");
        assert_eq!(app.pending(cx), 1);
        assert_eq!(app.data_rows(cx)[0][1], "anna");
        // The tab is marked while changes wait.
        let labels = |app: &Fixture, cx: &mut TestAppContext| {
            cx.update(|cx| app.workspace.read(cx).tab_labels(cx))
        };
        assert_eq!(labels(&app, cx), ["people •"]);
        assert!(app.status_bar(cx).contains(&"1 pending change".to_string()));
        assert!(app.has(cx, "data-pending"));
        assert_eq!(count("name = 'ann'"), "1");
        // Escape drops what is being typed.
        app.type_in_cell(cx, 1, 2, "zzz", "escape");
        assert_eq!(
            (app.pending(cx), app.data_rows(cx)[1][1].as_str()),
            (1, "bob")
        );
        // Typing the same value back is no change.
        app.type_in_cell(cx, 1, 2, "bob", "enter");
        assert_eq!(app.pending(cx), 1);
        // An emptied number is NULL; emptied text is the empty string.
        app.type_in_cell(cx, 0, 3, "", "enter");
        app.type_in_cell(cx, 2, 4, "", "enter");
        assert_eq!(app.data_rows(cx)[0][2], "NULL");
        assert_eq!(app.data_rows(cx)[2][3], "");
        assert_eq!(app.pending(cx), 2);
        // A deleted row, a new row, a copied row.
        select_row(&app, cx, 1);
        app.click(cx, "data-delete");
        app.click(cx, "data-add-row");
        app.type_in_cell(cx, 3, 2, "dee", "enter");
        select_row(&app, cx, 0);
        app.click(cx, "data-duplicate");
        assert_eq!(app.pending(cx), 5);
        assert_eq!(app.data_rows(cx)[4][1..], ["anna", "NULL", "x"]);

        // The review shows the statements; cancelling runs none.
        let tab = app.data(cx);
        let (_, statements) = cx.update(|cx| tab.read(cx).pending_sql(cx)).unwrap();
        let verbs: Vec<&str> = statements
            .iter()
            .map(|statement| statement.split(' ').next().unwrap())
            .collect();
        assert_eq!(verbs, ["DELETE", "UPDATE", "UPDATE", "INSERT", "INSERT"]);
        if engine == Engine::Postgres {
            assert_eq!(
                statements[1],
                "UPDATE \"kerno_ui_edits\".\"people\" SET \"name\" = 'anna', \"qty\" = NULL WHERE \"id\" = 1"
            );
        }
        app.click(cx, "data-review");
        assert!(app.has(cx, "sql-preview"));
        app.click(cx, "sql-cancel");
        assert_eq!(
            (app.pending(cx), count("true = true")),
            (5, "3".to_string())
        );
        // Applying asks the same way, then writes everything.
        app.click(cx, "data-apply");
        assert!(app.has(cx, "sql-preview"));
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the applied changes", |app, cx| {
            app.pending(cx) == 0 && app.data_state(cx) == "Rows 1–4 of 4"
        });
        assert_eq!(labels(&app, cx), ["people"]);
        assert_eq!(count("id = 2"), "0");
        assert_eq!(count("name = 'anna' AND qty IS NULL"), "2");
        assert_eq!(count("id = 3 AND note = ''"), "1");
        // The new row took the column's default.
        assert_eq!(count("name = 'dee' AND note = 'n/a'"), "1");

        // One statement fails: none is kept, and the changes still wait.
        app.type_in_cell(cx, 0, 3, "7", "enter");
        select_cell(&app, cx, 1, 2);
        app.click(cx, "data-set-null");
        assert_eq!(app.pending(cx), 2);
        app.click(cx, "data-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the refusal", |app, cx| {
            let tab = app.data(cx);
            cx.update(|cx| tab.read(cx).apply_error().is_some())
        });
        assert!(app.has(cx, "data-apply-error"));
        assert_eq!(app.pending(cx), 2);
        assert_eq!(count("id = 1 AND qty IS NULL"), "1");
        // The page does not move from under pending changes.
        app.click(cx, "data-refresh");
        cx.run_until_parked();
        assert_eq!(app.pending(cx), 2);
        app.click(cx, "data-discard");
        assert_eq!(app.pending(cx), 0);
        assert!(!app.has(cx, "data-apply-error"));

        // A row changed by someone else since it was read is not written over.
        server("UPDATE kerno_ui_edits.people SET id = 100 WHERE id = 3");
        app.type_in_cell(cx, 1, 2, "late", "enter");
        app.click(cx, "data-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the refusal", |app, cx| {
            let tab = app.data(cx);
            cx.update(|cx| tab.read(cx).apply_error().is_some())
        });
        assert_eq!(count("name = 'late'"), "0");
        app.click(cx, "data-discard");
        app.click(cx, "data-refresh");
        app.wait_until(cx, "the fresh rows", |app, cx| {
            app.data_state(cx) == "Rows 1–4 of 4" && app.data_rows(cx)[3][0] == "100"
        });
    }
}

#[gpui_kit::test]
fn rows_that_cannot_be_addressed_are_read_only_and_say_why(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_keys CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_keys",
            "CREATE TABLE kerno_ui_keys.keyless (name text, body text)",
            "INSERT INTO kerno_ui_keys.keyless VALUES ('a', repeat('long ', 100))",
            "CREATE TABLE kerno_ui_keys.keyed (id int PRIMARY KEY, doubled int GENERATED ALWAYS AS (id * 2) STORED)",
            "INSERT INTO kerno_ui_keys.keyed VALUES (1)",
            "CREATE VIEW kerno_ui_keys.names AS SELECT name FROM kerno_ui_keys.keyless",
        ],
    );
    let reason = |app: &Fixture, cx: &mut TestAppContext| {
        let tab = app.data(cx);
        cx.update(|cx| tab.read(cx).read_only_reason(cx))
    };
    let app = connected(cx, Engine::Postgres);
    // No key: a row cannot be told from another.
    app.open_table(cx, "kerno_ui_keys", "keyless", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    assert!(reason(&app, cx).unwrap().contains("no primary key"));
    assert!(app.has(cx, "data-read-only") && !app.has(cx, "data-add-row"));
    // A double click shows the value instead of typing in it.
    app.on_data(cx, |tab, window, cx| tab.edit_cell(0, 2, window, cx));
    assert!(app.has(cx, "value-text"));
    app.click(cx, "value-close");
    app.on_data(cx, |tab, window, cx| tab.add_row(window, cx));
    assert_eq!(app.pending(cx), 0);
    // A view.
    app.open_table(cx, "kerno_ui_keys", "names", RelationKind::View);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    assert!(reason(&app, cx).unwrap().contains("view"));
    // A table with a key takes changes, except in its computed column.
    app.open_table(cx, "kerno_ui_keys", "keyed", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    assert_eq!(reason(&app, cx), None);
    assert!(app.has(cx, "data-add-row"));
    app.on_data(cx, |tab, window, cx| tab.edit_cell(0, 2, window, cx));
    assert!(app.has(cx, "value-text"));
    app.click(cx, "value-close");
    assert_eq!(app.titles(cx), ["keyless", "names", "keyed"]);

    // A read-only connection takes none at all.
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    app.open_table(cx, "kerno_ui_keys", "keyed", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    assert!(reason(&app, cx).unwrap().contains("read-only connection"));
    assert!(!app.has(cx, "data-add-row"));
}

#[gpui_kit::test]
fn foreign_keys_lead_to_the_referenced_row_and_back(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_fk");
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                "CREATE TABLE kerno_ui_fk.authors (id INT PRIMARY KEY, name VARCHAR(20))",
                "CREATE TABLE kerno_ui_fk.books (id INT PRIMARY KEY, author_id INT, title VARCHAR(20), \
                 CONSTRAINT books_author FOREIGN KEY (author_id) REFERENCES kerno_ui_fk.authors (id))",
                "INSERT INTO kerno_ui_fk.authors VALUES (1, 'ann'), (2, 'bob')",
                "INSERT INTO kerno_ui_fk.books VALUES (10, 1, 'a1'), (11, 1, 'a2'), (12, 2, 'b1'), (13, NULL, 'orphan')",
            ],
        );
        let app = connected(cx, engine);
        app.open_table(cx, "kerno_ui_fk", "books", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–4 of 4");
        let related = |app: &Fixture, cx: &mut TestAppContext, row: usize| {
            let tab = app.data(cx);
            cx.update(|cx| tab.read(cx).related(row, cx))
        };
        // A book leads to its author; one without an author leads nowhere.
        let to_author = related(&app, cx, 0);
        assert_eq!(
            to_author
                .iter()
                .map(|related| related.label.as_str())
                .collect::<Vec<_>>(),
            ["Go to authors (author_id = 1)"]
        );
        assert_eq!(related(&app, cx, 3), []);
        app.on_data(cx, |tab, _, cx| tab.follow(&to_author[0], cx));
        app.wait_for_rows(cx, "Rows 1–1 of 1");
        assert_eq!(app.data_rows(cx), [["1", "ann"]]);
        // And the author back to every book of theirs.
        let to_books = related(&app, cx, 0);
        assert_eq!(
            to_books
                .iter()
                .map(|related| related.label.as_str())
                .collect::<Vec<_>>(),
            ["Rows of books referencing this one (author_id)"]
        );
        app.on_data(cx, |tab, _, cx| tab.follow(&to_books[0], cx));
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(app.data_rows(cx), [["10", "1", "a1"], ["11", "1", "a2"]]);
        assert_eq!(app.titles(cx), ["books", "authors", "books"]);
    }
}

#[gpui_kit::test]
fn a_slow_page_is_cancelled_and_the_tab_stays_usable(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "shop", "categories", RelationKind::Table);
    app.wait_until(cx, "the rows", |app, cx| {
        app.data_state(cx).contains(" of ")
    });
    let rows = app.data_state(cx);
    app.on_data(cx, |tab, window, cx| {
        let slow = Filter {
            conditions: Vec::new(),
            raw: "pg_sleep(30) IS NOT NULL".into(),
        };
        tab.set_filter(slow, window, cx)
    });
    // Let the statement reach the server before stopping it.
    std::thread::sleep(Duration::from_millis(300));
    cx.run_until_parked();
    assert_eq!(app.data_state(cx), "loading");
    assert!(app.status_bar(cx).contains(&"running…".to_string()));
    app.press(cx, &["secondary-."]);
    app.wait_until(cx, "the cancellation", |app, cx| {
        app.data_state(cx).starts_with("failed")
    });
    app.click(cx, "filter-clear");
    app.wait_for_rows(cx, &rows);
}

#[gpui_kit::test]
async fn a_table_opened_from_the_tree_comes_back_after_a_restart(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
    app.toggle(cx, &["kerno", "shop"]);
    app.wait_until(cx, "the folders", |app, cx| {
        app.tree(cx).contains(&row(2, "Tables"))
    });
    app.toggle(cx, &["kerno", "shop", "Tables"]);
    app.wait_until(cx, "the tables", |app, cx| {
        app.tree(cx)
            .iter()
            .any(|line| line.trim_start().starts_with("categories"))
    });
    let categories = app
        .tree(cx)
        .into_iter()
        .find(|line| line.trim_start().starts_with("categories"))
        .unwrap();
    app.act(
        cx,
        Action::OpenData,
        &["kerno", "shop", "Tables", categories.trim_start()],
    );
    assert_eq!(app.titles(cx), ["categories"]);
    app.wait_until(cx, "the rows", |app, cx| {
        app.data_state(cx).contains(" of ")
    });
    let rows = app.data_state(cx);
    let stored = app.wait_for_stored_tabs(cx, |tabs| !tabs.is_empty()).await;
    assert!(
        matches!(&stored[..], [TabSnapshot::Data { schema, table, view: false, .. }]
            if schema == "shop" && table == "categories"),
        "{stored:?}"
    );

    // The next launch shows the tab; its rows load once connected.
    let restarted = open(cx, app.storage.clone());
    assert_eq!(restarted.titles(cx), ["categories"]);
    assert_eq!(restarted.data_state(cx), "waiting");
    assert!(restarted.has(cx, "data-waiting"));
    let id = cx.update(|cx| restarted.connections.read(cx).saved()[0].id.clone());
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    restarted.click(cx, "data-connect");
    restarted.wait_for_rows(cx, &rows);
}

// Structure ---------------------------------------------------------------------

impl Fixture {
    /// Opens the structure of a table of the first connection.
    fn open_structure(
        &self,
        cx: &mut TestAppContext,
        schema: &str,
        table: &str,
        kind: RelationKind,
    ) {
        let connection = cx.update(|cx| self.connections.read(cx).saved()[0].id.clone());
        let target = Target {
            connection,
            database: None,
            schema: None,
        };
        let table = TableRef::new(schema, table);
        self.step(cx, |window, cx| {
            self.workspace.update(cx, |workspace, cx| {
                workspace.open_structure(target, table, kind, window, cx);
            })
        });
    }

    /// The active tab, which shows a table's structure.
    fn structure(&self, cx: &mut TestAppContext) -> Entity<StructureTab> {
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .active_structure_tab()
                .expect("a structure tab")
                .clone()
        })
    }

    /// Does something to the active structure tab.
    fn on_structure(
        &self,
        cx: &mut TestAppContext,
        act: impl FnOnce(&mut StructureTab, &mut gpui_kit::Window, &mut gpui_kit::Context<StructureTab>),
    ) {
        let tab = self.structure(cx);
        self.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| act(tab, window, cx))
        });
    }

    /// Waits for the active structure tab to have read its table.
    fn wait_for_structure(&self, cx: &mut TestAppContext) {
        self.wait_until(cx, "the structure", |app, cx| {
            let tab = app.structure(cx);
            let state = cx.update(|cx| tab.read(cx).state());
            assert!(!state.starts_with("failed"), "{state}");
            state == "ready"
        });
    }

    /// The rows of a list of the active structure tab.
    fn structure_rows(&self, cx: &mut TestAppContext, list: List) -> Vec<Vec<String>> {
        let tab = self.structure(cx);
        cx.update(|cx| tab.read(cx).rows_of(list, cx))
    }

    /// One column of a list of the active structure tab.
    fn structure_column(&self, cx: &mut TestAppContext, list: List, column: usize) -> Vec<String> {
        self.structure_rows(cx, list)
            .into_iter()
            .map(|row| row[column].clone())
            .collect()
    }

    fn structure_ddl(&self, cx: &mut TestAppContext) -> String {
        let tab = self.structure(cx);
        cx.update(|cx| tab.read(cx).ddl().to_string())
    }
}

/// Where the fixture's shop is: a schema on PostgreSQL, a database on MySQL.
fn shop(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "shop",
        Engine::MySql | Engine::MariaDb => "kerno",
    }
}

#[gpui_kit::test]
fn the_structure_of_a_table_is_shown_in_tabs(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let schema = shop(engine);
        let app = connected(cx, engine);
        app.open_structure(cx, schema, "orders", RelationKind::Table);
        assert_eq!(app.titles(cx), ["orders · structure"]);
        app.wait_for_structure(cx);
        assert!(app.has(cx, "structure-table-name"), "{engine:?}");

        // Columns, in table order, with their type and key.
        let columns = app.structure_rows(cx, List::Columns);
        let names: Vec<&str> = columns.iter().map(|row| row[1].as_str()).collect();
        let expected: &[&str] = match engine {
            Engine::Postgres => &[
                "id",
                "customer_id",
                "status",
                "invoice_number",
                "placed_at",
                "shipped_at",
                "total",
                "currency",
                "notes",
            ],
            Engine::MySql | Engine::MariaDb => &[
                "id",
                "customer_id",
                "status",
                "placed_at",
                "shipped_at",
                "total",
                "currency",
                "notes",
            ],
        };
        assert_eq!(names, expected, "{engine:?}");
        assert_eq!(columns[0][0], "1");
        assert_eq!(columns[0][5], "PK", "{engine:?}");
        assert_eq!(columns[1][3], "no", "{engine:?}");
        match engine {
            Engine::Postgres => {
                assert_eq!(columns[2][2], "shop.order_status");
                assert_eq!(columns[2][4], "'pending'::shop.order_status");
                assert_eq!(columns[0][6], "identity, always");
            }
            Engine::MySql | Engine::MariaDb => {
                // MariaDB keeps the display width: "bigint(20) unsigned".
                let customer_id = &columns[1][2];
                assert!(
                    customer_id.starts_with("bigint") && customer_id.ends_with(" unsigned"),
                    "{engine:?}: {customer_id}"
                );
                assert_eq!(columns[0][6], "auto increment", "{engine:?}");
            }
        }

        let indexes = app.structure_column(cx, List::Indexes, 0);
        for index in ["orders_customer_idx", "orders_status_placed_idx"] {
            assert!(
                indexes.iter().any(|name| name == index),
                "{engine:?}: {indexes:?}"
            );
        }
        let primary = app.structure_rows(cx, List::Constraints);
        assert!(
            primary
                .iter()
                .any(|row| row[1] == "PRIMARY KEY" && row[2] == "id"),
            "{engine:?}: {primary:?}"
        );
        // The foreign key to customers, and the tables that reference orders.
        let references = app.structure_rows(cx, List::ForeignKeys);
        assert_eq!(references.len(), 1, "{engine:?}: {references:?}");
        assert_eq!(references[0][1], "customer_id");
        assert_eq!(references[0][2], format!("{schema}.customers"));
        assert_eq!(references[0][3], "id");
        assert_eq!(references[0][5], "RESTRICT", "{engine:?}");
        let referencing = app.structure_column(cx, List::ReferencedBy, 1);
        let items = referencing
            .iter()
            .position(|table| *table == format!("{schema}.order_items"))
            .unwrap_or_else(|| panic!("{engine:?}: {referencing:?}"));
        assert_eq!(
            app.structure_column(cx, List::Triggers, 0),
            ["orders_log_status"],
            "{engine:?}"
        );
        assert!(
            app.structure_ddl(cx).starts_with("CREATE TABLE"),
            "{engine:?}: {}",
            app.structure_ddl(cx)
        );

        // Each sub-tab shows its own list; the DDL is in a code view.
        app.on_structure(cx, |tab, _, cx| tab.select(DDL_SECTION, cx));
        assert!(app.has(cx, "structure-ddl"), "{engine:?}");
        app.on_structure(cx, |tab, _, cx| tab.select(4, cx));
        assert!(!app.has(cx, "structure-ddl"), "{engine:?}");

        // Cmd+C copies the selected cell.
        let tab = app.structure(cx);
        let grid = cx.update(|cx| tab.read(cx).grid(List::Columns)).unwrap();
        app.on_structure(cx, |tab, _, cx| tab.select(0, cx));
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| table.set_selected_cell(1, 2, cx));
            grid.read(cx).focus_handle(cx).focus(window, cx);
        });
        app.press(cx, &["secondary-c"]);
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some(columns[1][2].as_str()),
            "{engine:?}"
        );

        // A double click on a key opens the table at its other end.
        let referencing = cx
            .update(|cx| tab.read(cx).grid(List::ReferencedBy))
            .unwrap();
        app.step(cx, |_, cx| {
            referencing.update(cx, |_, cx| cx.emit(TableEvent::DoubleClickedCell(items, 0)))
        });
        assert_eq!(
            app.titles(cx),
            ["orders · structure", "order_items · structure"]
        );
        app.wait_for_structure(cx);
        assert_eq!(
            app.structure_column(cx, List::ForeignKeys, 2),
            [format!("{schema}.orders"), format!("{schema}.products")],
            "{engine:?}"
        );
        // Its rows are a click away, and its structure one back.
        app.click(cx, "structure-open-data");
        assert_eq!(
            app.titles(cx),
            [
                "orders · structure",
                "order_items · structure",
                "order_items"
            ]
        );
        // (Its rows are read and counted before the next engine's runtime
        // replaces this one.)
        app.wait_until(cx, "the rows", |app, cx| {
            app.data_state(cx).contains(" of ")
        });
        app.click(cx, "data-open-structure");
        assert_eq!(app.titles(cx).len(), 3, "{engine:?}");
        assert_eq!(app.active(cx), Some(1));
    }
}

#[gpui_kit::test]
fn a_view_shows_its_columns_and_ddl(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let schema = shop(engine);
        let app = connected(cx, engine);
        app.open_structure(cx, schema, "order_totals", RelationKind::View);
        app.wait_for_structure(cx);
        assert_eq!(
            app.structure_column(cx, List::Columns, 1),
            [
                "order_id",
                "customer_id",
                "status",
                "placed_at",
                "line_count",
                "computed_total"
            ],
            "{engine:?}"
        );
        // What a view does not have says so.
        assert_eq!(
            app.structure_rows(cx, List::Indexes),
            Vec::<Vec<String>>::new()
        );
        app.on_structure(cx, |tab, _, cx| tab.select(1, cx));
        assert!(app.has(cx, ("structure-empty", 1usize)), "{engine:?}");
        app.on_structure(cx, |tab, _, cx| tab.select(3, cx));
        assert!(
            app.has(cx, ("structure-empty", 3usize)) && app.has(cx, ("structure-empty", 4usize)),
            "{engine:?}"
        );
        let ddl = app.structure_ddl(cx);
        assert!(
            ddl.contains(" VIEW ") && ddl.contains("order_totals"),
            "{engine:?}: {ddl}"
        );

        // A materialized view has its own DDL, and indexes.
        if engine == Engine::Postgres {
            app.open_structure(cx, schema, "product_sales", RelationKind::MaterializedView);
            app.wait_for_structure(cx);
            assert_eq!(app.structure_rows(cx, List::Columns).len(), 5);
            assert_eq!(
                app.structure_column(cx, List::Indexes, 0),
                ["product_sales_product_idx"]
            );
            let ddl = app.structure_ddl(cx);
            assert!(ddl.contains("MATERIALIZED VIEW"), "{ddl}");
        }
    }
}

#[gpui_kit::test]
async fn the_structure_tab_comes_back_after_a_restart(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        // From the tree: the database, then on PostgreSQL the schema.
        let path: Vec<&str> = match engine {
            Engine::Postgres => vec!["kerno", "shop", "Tables"],
            Engine::MySql | Engine::MariaDb => vec!["kerno", "Tables"],
        };
        for depth in 1..path.len() {
            let folder = path[depth];
            app.wait_until(cx, folder, |app, cx| {
                app.tree(cx).contains(&row(depth, folder))
            });
            app.toggle(cx, &path[..=depth]);
        }
        app.wait_until(cx, "the tables", |app, cx| {
            app.tree(cx)
                .iter()
                .any(|line| line.trim_start().starts_with("orders"))
        });
        let orders = app
            .tree(cx)
            .into_iter()
            .find(|line| line.trim_start().starts_with("orders"))
            .unwrap();
        let mut node = path.clone();
        node.push(orders.trim_start());
        app.act(cx, Action::OpenStructure, &node);
        assert_eq!(app.titles(cx), ["orders · structure"], "{engine:?}");
        app.wait_for_structure(cx);
        let columns = app.structure_rows(cx, List::Columns);
        let stored = app.wait_for_stored_tabs(cx, |tabs| !tabs.is_empty()).await;
        assert!(
            matches!(&stored[..], [TabSnapshot::Structure { table, view: false, materialized: false, .. }]
                if table == "orders"),
            "{engine:?}: {stored:?}"
        );

        // The next launch shows the tab; the structure loads once connected.
        let restarted = open(cx, app.storage.clone());
        assert_eq!(restarted.titles(cx), ["orders · structure"]);
        assert!(restarted.has(cx, "structure-waiting"), "{engine:?}");
        let id = cx.update(|cx| restarted.connections.read(cx).saved()[0].id.clone());
        let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
        kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
        restarted.click(cx, "structure-connect");
        restarted.wait_for_structure(cx);
        assert_eq!(
            restarted.structure_rows(cx, List::Columns),
            columns,
            "{engine:?}"
        );
    }
}

// Table form --------------------------------------------------------------------

impl Fixture {
    /// The active tab, which is a table form.
    fn editor(&self, cx: &mut TestAppContext) -> Entity<TableEditor> {
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .active_table_editor()
                .expect("a table form")
                .clone()
        })
    }

    /// Waits for the active table form to show its table.
    fn wait_for_editor(&self, cx: &mut TestAppContext) {
        self.wait_until(cx, "the table form", |app, cx| {
            let editor = app.editor(cx);
            cx.update(|cx| editor.read(cx).ready())
        });
    }

    /// Replaces what an input holds with `text`, as typed.
    fn fill_in(&self, cx: &mut TestAppContext, id: impl Into<gpui_kit::ElementId>, text: &str) {
        self.click(cx, id);
        self.press(cx, &["secondary-a", "backspace"]);
        self.type_text(cx, text);
    }

    /// The statements of the form's preview, one script, or its problems.
    fn editor_sql(&self, cx: &mut TestAppContext) -> Result<String, Vec<String>> {
        let editor = self.editor(cx);
        cx.update(|cx| {
            editor
                .read(cx)
                .statements(cx)
                .map(|(statements, _)| statements.join(";\n"))
        })
    }

    /// Opens the folders down to the tables of `schema` in the tree, and
    /// gives the path of that folder.
    fn open_tables(&self, cx: &mut TestAppContext, engine: Engine, schema: &str) -> Vec<String> {
        let path: Vec<String> = match engine {
            Engine::Postgres => vec!["kerno".into(), schema.into(), "Tables".into()],
            Engine::MySql | Engine::MariaDb => vec![schema.into(), "Tables".into()],
        };
        let start = path.len() - 2;
        self.wait_until(cx, schema, |app, cx| {
            app.tree(cx).contains(&row(start, &path[start]))
        });
        self.toggle(
            cx,
            &path[..=start]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        self.wait_until(cx, "the folders", |app, cx| {
            app.tree_row_of(cx, &path).is_some()
        });
        self.toggle(cx, &path.iter().map(String::as_str).collect::<Vec<_>>());
        self.wait_until(cx, "the tables", |app, cx| {
            app.tree(cx).contains(&row(path.len(), "existing"))
        });
        path
    }

    fn tree_row_of(&self, cx: &mut TestAppContext, path: &[String]) -> Option<usize> {
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .navigator(cx)
                .read(cx)
                .row_of(&path, cx)
        })
    }
}

/// Column names of a table on a test server, in order.
fn columns_on_server(engine: Engine, schema: &str, table: &str) -> String {
    let aggregate = match engine {
        Engine::Postgres => "string_agg(column_name::text, ',' ORDER BY ordinal_position)",
        Engine::MySql | Engine::MariaDb => {
            "GROUP_CONCAT(column_name ORDER BY ordinal_position SEPARATOR ',')"
        }
    };
    on_server(
        engine,
        &format!(
            "SELECT {aggregate} FROM information_schema.columns \
             WHERE table_schema = '{schema}' AND table_name = '{table}'"
        ),
    )
}

#[gpui_kit::test]
fn a_table_is_created_from_the_form(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_form_create";
        let (drop_schema, create_schema) = scratch_schema(engine, schema);
        // A table already there: an empty folder does not stay open.
        let existing = format!("CREATE TABLE {schema}.existing (id INT PRIMARY KEY)");
        let _scratch = Scratch::new(engine, &[&drop_schema], &[&create_schema, &existing]);
        let app = connected(cx, engine);
        let tables = app.open_tables(cx, engine, schema);
        let tables: Vec<&str> = tables.iter().map(String::as_str).collect();

        app.act(cx, Action::CreateTable, &tables);
        assert_eq!(app.titles(cx), ["New table"], "{engine:?}");
        app.wait_for_editor(cx);
        // A new table starts with an auto-numbered key and needs a name.
        assert_eq!(
            app.editor_sql(cx),
            Err(vec!["The table needs a name.".to_string()])
        );
        assert!(app.has(cx, "editor-problems"));

        app.fill_in(cx, "editor-name", "gadgets");
        app.click(cx, "editor-add-column");
        app.fill_in(cx, ("column-name", 1usize), "label");
        app.fill_in(cx, ("column-type", 1usize), "varchar(80)");
        app.click(cx, ("column-null", 1usize));
        app.click(cx, "editor-add-column");
        app.fill_in(cx, ("column-name", 2usize), "price");
        let price = match engine {
            Engine::Postgres => "numeric(10,2)",
            Engine::MySql | Engine::MariaDb => "decimal(10,2)",
        };
        app.fill_in(cx, ("column-type", 2usize), price);
        app.fill_in(cx, ("column-default", 2usize), "0");
        app.click(cx, "editor-add-index");
        app.fill_in(cx, ("index-name", 0usize), "gadgets_label_idx");
        app.click(cx, "index-0-column-1".to_string());
        app.click(cx, ("index-unique", 0usize));
        // A foreign key to the table already there; its menus are driven
        // through what their entries call.
        app.click(cx, "editor-add-column");
        app.fill_in(cx, ("column-name", 3usize), "existing_id");
        app.fill_in(cx, ("column-type", 3usize), "int");
        app.click(cx, "editor-add-key");
        app.fill_in(cx, ("key-name", 0usize), "gadgets_existing_fk");
        let editor = app.editor(cx);
        app.step(cx, |window, cx| {
            editor.update(cx, |editor, cx| {
                editor.reference(0, "existing", window, cx);
                editor.set_action(0, true, "CASCADE", cx);
            })
        });
        app.fill_in(cx, ("key-referenced", 0usize), "id");
        app.click(cx, "key-0-column-3".to_string());

        let sql = app
            .editor_sql(cx)
            .unwrap_or_else(|problems| panic!("{problems:?}"));
        assert!(sql.starts_with("CREATE TABLE"), "{engine:?}: {sql}");
        assert!(
            sql.contains("REFERENCES")
                && sql.contains("existing")
                && sql.contains("ON DELETE CASCADE"),
            "{engine:?}: {sql}"
        );
        assert!(
            sql.contains("label") && sql.contains("varchar(80) NOT NULL") && sql.contains(price),
            "{engine:?}: {sql}"
        );
        assert!(sql.contains("UNIQUE"), "{engine:?}: {sql}");
        assert!(app.has(cx, "editor-preview"));
        assert_eq!(app.titles(cx), ["New table"]);
        assert_eq!(
            cx.update(|cx| app.workspace.read(cx).tab_labels(cx)),
            ["New table •"]
        );

        app.click(cx, "editor-apply");
        assert!(app.has(cx, "sql-preview"), "{engine:?}");
        app.click(cx, "sql-confirm");
        // The form gives way to the structure of the new table.
        app.wait_until(cx, "the new table", |app, cx| {
            app.titles(cx) == ["gadgets · structure"]
        });
        app.wait_for_structure(cx);
        assert_eq!(
            app.structure_column(cx, List::Columns, 1),
            ["id", "label", "price", "existing_id"],
            "{engine:?}"
        );
        let keys = app.structure_rows(cx, List::ForeignKeys);
        assert_eq!(keys.len(), 1, "{engine:?}: {keys:?}");
        assert_eq!(
            (
                keys[0][0].as_str(),
                keys[0][1].as_str(),
                keys[0][2].clone(),
                keys[0][5].as_str()
            ),
            (
                "gadgets_existing_fk",
                "existing_id",
                format!("{schema}.existing"),
                "CASCADE"
            ),
            "{engine:?}"
        );
        assert!(
            app.structure_column(cx, List::Indexes, 0)
                .contains(&"gadgets_label_idx".to_string()),
            "{engine:?}"
        );
        assert_eq!(
            columns_on_server(engine, schema, "gadgets"),
            "id,label,price,existing_id"
        );
        // The tree shows it in its folder.
        let depth = tables.len();
        app.wait_until(cx, "the table in the tree", |app, cx| {
            app.tree(cx).contains(&row(depth, "gadgets"))
        });
    }
}

#[gpui_kit::test]
fn a_table_is_altered_from_the_form_with_a_preview(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_form_alter";
        let (drop_schema, create_schema) = scratch_schema(engine, schema);
        // MySQL keeps an auto-updated timestamp through a comment change.
        let stamp = match engine {
            Engine::Postgres => "",
            Engine::MySql | Engine::MariaDb => {
                ", stamp TIMESTAMP NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP"
            }
        };
        let create = scratch_table(
            engine,
            &format!("{schema}.items"),
            &format!("name varchar(50) NOT NULL, note text, qty int{stamp}"),
        );
        let _scratch = Scratch::new(engine, &[&drop_schema], &[&create_schema, &create]);
        let app = connected(cx, engine);
        app.open_structure(cx, schema, "items", RelationKind::Table);
        app.wait_for_structure(cx);
        if engine != Engine::Postgres {
            // The auto-update is shown with the columns.
            let columns = app.structure_rows(cx, List::Columns);
            assert!(
                columns[4][8].to_lowercase().contains("current_timestamp"),
                "{engine:?}: {columns:?}"
            );
        }
        app.click(cx, "structure-edit");
        assert_eq!(app.titles(cx), ["items · structure", "items · edit"]);
        app.wait_for_editor(cx);
        assert!(app.has(cx, "editor-no-change"), "{engine:?}");
        assert_eq!(app.editor_sql(cx), Ok(String::new()));

        // Rename, retype, add, index, then drop.
        app.fill_in(cx, ("column-name", 1usize), "title");
        app.fill_in(cx, ("column-type", 3usize), "bigint");
        if engine != Engine::Postgres {
            app.fill_in(cx, ("column-comment", 4usize), "touched");
        }
        app.click(cx, "editor-add-column");
        let added: usize = if engine == Engine::Postgres { 4 } else { 5 };
        app.fill_in(cx, ("column-name", added), "added");
        app.fill_in(cx, ("column-type", added), "int");
        app.click(cx, "editor-add-index");
        app.fill_in(cx, ("index-name", 0usize), "items_added_idx");
        app.click(cx, format!("index-0-column-{added}"));
        app.click(cx, ("column-remove", 2usize));

        let sql = app
            .editor_sql(cx)
            .unwrap_or_else(|problems| panic!("{problems:?}"));
        for part in ["DROP COLUMN", "RENAME COLUMN", "ADD COLUMN", "INDEX"] {
            assert!(sql.contains(part), "{engine:?}: {part} in {sql}");
        }
        assert!(sql.contains("note") && sql.contains("title"), "{sql}");
        assert!(sql.to_lowercase().contains("bigint"), "{sql}");
        if engine != Engine::Postgres {
            assert!(sql.contains("ON UPDATE"), "{engine:?}: {sql}");
        }
        assert_eq!(
            cx.update(|cx| app.workspace.read(cx).tab_labels(cx)),
            ["items · structure", "items · edit •"]
        );

        // Cancelling runs nothing.
        app.click(cx, "editor-apply");
        assert!(app.has(cx, "sql-preview"));
        app.click(cx, "sql-cancel");
        assert!(!app.has(cx, "sql-preview"));
        let before = match engine {
            Engine::Postgres => "id,name,note,qty",
            Engine::MySql | Engine::MariaDb => "id,name,note,qty,stamp",
        };
        assert_eq!(columns_on_server(engine, schema, "items"), before);

        // Confirming applies, and the form shows the table as it is now.
        app.click(cx, "editor-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the alter", |app, cx| app.has(cx, "editor-no-change"));
        let editor = app.editor(cx);
        assert_eq!(
            cx.update(|cx| editor.read(cx).error().map(str::to_string)),
            None
        );
        let after = match engine {
            Engine::Postgres => "id,title,qty,added",
            Engine::MySql | Engine::MariaDb => "id,title,qty,stamp,added",
        };
        assert_eq!(columns_on_server(engine, schema, "items"), after);
        assert_eq!(
            cx.update(|cx| app.workspace.read(cx).tab_labels(cx)),
            ["items · structure", "items · edit"]
        );
        if engine != Engine::Postgres {
            let extra = on_server(
                engine,
                &format!(
                    "SELECT EXTRA FROM information_schema.columns \
                     WHERE table_schema = '{schema}' AND table_name = 'items' \
                     AND column_name = 'stamp'"
                ),
            );
            assert!(
                extra.to_lowercase().contains("on update"),
                "{engine:?}: {extra}"
            );
        }
        // The structure tab was read again.
        app.open_structure(cx, schema, "items", RelationKind::Table);
        app.wait_until(cx, "the structure again", |app, cx| {
            app.structure_column(cx, List::Columns, 1)
                .contains(&"added".to_string())
        });
        assert!(
            app.structure_column(cx, List::Indexes, 0)
                .contains(&"items_added_idx".to_string()),
            "{engine:?}"
        );
    }
}

#[gpui_kit::test]
fn the_form_refuses_what_is_invalid(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
    app.act(cx, Action::CreateTable, &["kerno", "shop"]);
    app.wait_for_editor(cx);
    let problems = |app: &Fixture, cx: &mut TestAppContext| app.editor_sql(cx).unwrap_err();

    app.fill_in(cx, "editor-name", "kerno_ui_never");
    assert!(app.editor_sql(cx).is_ok());
    // A column without a name, another named like the first.
    app.click(cx, "editor-add-column");
    app.fill_in(cx, ("column-type", 1usize), "text");
    assert_eq!(problems(&app, cx), ["Column 2 has no name."]);
    app.fill_in(cx, ("column-name", 1usize), "id");
    assert_eq!(problems(&app, cx), ["There are two columns named \"id\"."]);
    app.fill_in(cx, ("column-name", 1usize), "label");
    // A nullable key column, an index without columns.
    app.click(cx, ("column-key", 1usize));
    app.click(cx, ("column-null", 1usize));
    app.click(cx, "editor-add-index");
    assert_eq!(
        problems(&app, cx),
        [
            "Column \"label\" is in the primary key and cannot allow NULL.",
            "Index 1 has no name.",
            "Index \"\" has no column.",
        ]
    );
    // What is wrong shows instead of the statements, and nothing can run.
    assert!(app.has(cx, "editor-problems") && !app.has(cx, "editor-preview"));
    app.click(cx, "editor-apply");
    assert!(!app.has(cx, "sql-preview"));

    // Put right, the statements are back.
    app.click(cx, ("index-remove", 0usize));
    app.click(cx, ("column-null", 1usize));
    let sql = app
        .editor_sql(cx)
        .unwrap_or_else(|problems| panic!("{problems:?}"));
    assert!(sql.contains("PRIMARY KEY (\"id\", \"label\")"), "{sql}");
    // Removing a key column takes it out of the key.
    app.click(cx, ("column-remove", 1usize));
    let sql = app
        .editor_sql(cx)
        .unwrap_or_else(|problems| panic!("{problems:?}"));
    assert!(sql.contains("PRIMARY KEY (\"id\")"), "{sql}");
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM pg_tables WHERE tablename = 'kerno_ui_never'"
        ),
        "0"
    );
}

#[gpui_kit::test]
fn a_read_only_connection_cannot_apply(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
    assert_eq!(
        app.why_not(cx, Action::CreateTable, &["kerno", "shop"]),
        Some("read-only connection")
    );
    app.act(cx, Action::CreateTable, &["kerno", "shop"]);
    assert_eq!(app.titles(cx), Vec::<String>::new());

    // The structure tab says why there is no "Edit table…".
    app.open_structure(cx, "shop", "orders", RelationKind::Table);
    app.wait_for_structure(cx);
    assert!(!app.has(cx, "structure-edit") && app.has(cx, "structure-edit-disabled"));
    // A view has none at all.
    app.open_structure(cx, "shop", "order_totals", RelationKind::View);
    app.wait_for_structure(cx);
    assert!(!app.has(cx, "structure-edit") && !app.has(cx, "structure-edit-disabled"));

    // The form itself writes the statements but cannot run them.
    let target = Target {
        connection: cx.update(|cx| app.connections.read(cx).saved()[0].id.clone()),
        database: None,
        schema: None,
    };
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_table_editor(
                target,
                "shop".into(),
                Some(TableRef::new("shop", "orders")),
                window,
                cx,
            );
        })
    });
    app.wait_for_editor(cx);
    app.fill_in(cx, ("column-comment", 1usize), "who ordered");
    assert!(
        app.editor_sql(cx)
            .is_ok_and(|sql| sql.contains("COMMENT ON COLUMN"))
    );
    assert!(app.has(cx, "editor-read-only") && !app.has(cx, "editor-apply"));
}

#[gpui_kit::test]
fn production_asks_twice_before_a_table_is_altered(cx: &mut TestAppContext) {
    let engine = Engine::MariaDb;
    if !testing::available(engine) {
        return;
    }
    let schema = "kerno_ui_form_prod";
    let (drop_schema, create_schema) = scratch_schema(engine, schema);
    let create = format!("CREATE TABLE {schema}.t (id INT PRIMARY KEY, a INT)");
    let _scratch = Scratch::new(engine, &[&drop_schema], &[&create_schema, &create]);
    let mut config = testing::config(engine);
    config.environment = kerno_core::config::Environment::Production;
    let app = connected_as(cx, config);
    app.open_structure(cx, schema, "t", RelationKind::Table);
    app.wait_for_structure(cx);
    app.click(cx, "structure-edit");
    app.wait_for_editor(cx);
    app.fill_in(cx, ("column-name", 1usize), "b");

    app.click(cx, "editor-apply");
    assert!(app.has(cx, "sql-production"));
    app.click(cx, "sql-confirm");
    // One click arms the button and runs nothing.
    assert!(app.has(cx, "sql-preview"));
    assert_eq!(columns_on_server(engine, schema, "t"), "id,a");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the rename", |_, _| {
        columns_on_server(engine, schema, "t") == "id,b"
    });
}

#[gpui_kit::test]
fn a_failed_alter_changes_nothing_on_postgresql(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let schema = "kerno_ui_form_fail";
    let (drop_schema, create_schema) = scratch_schema(engine, schema);
    let _scratch = Scratch::new(
        engine,
        &[&drop_schema],
        &[
            &create_schema,
            "CREATE TABLE kerno_ui_form_fail.t (id int PRIMARY KEY, a int)",
            "INSERT INTO kerno_ui_form_fail.t VALUES (1, 1)",
        ],
    );
    let app = connected(cx, engine);
    app.open_structure(cx, schema, "t", RelationKind::Table);
    app.wait_for_structure(cx);
    app.click(cx, "structure-edit");
    app.wait_for_editor(cx);
    // The rename runs first; the NOT NULL column cannot be added to a
    // table that has a row.
    app.fill_in(cx, ("column-name", 1usize), "b");
    app.click(cx, "editor-add-column");
    app.fill_in(cx, ("column-name", 2usize), "c");
    app.fill_in(cx, ("column-type", 2usize), "int");
    app.click(cx, ("column-null", 2usize));
    app.click(cx, "editor-apply");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the failure", |app, cx| app.has(cx, "editor-error"));
    let editor = app.editor(cx);
    let error = cx
        .update(|cx| editor.read(cx).error().map(str::to_string))
        .unwrap_or_default();
    assert!(
        error.starts_with("Nothing was changed. Statement 2 failed:")
            && error.contains("ADD COLUMN"),
        "{error}"
    );
    // Rolled back: the rename did not land, and the form keeps the edits.
    assert_eq!(columns_on_server(engine, schema, "t"), "id,a");
    assert!(
        app.editor_sql(cx)
            .is_ok_and(|sql| sql.contains("RENAME COLUMN"))
    );
}

#[gpui_kit::test]
async fn a_table_form_is_not_restored_after_a_restart(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    let target = Target {
        connection: "not-saved".into(),
        database: None,
        schema: None,
    };
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_table_editor(target, "shop".into(), None, window, cx);
        })
    });
    app.press(cx, &["secondary-t", "secondary-t"]);
    // From "Query 2" round to "Query 1", past the form.
    app.press(cx, &["ctrl-tab", "ctrl-tab"]);
    assert_eq!(app.titles(cx), ["New table", "Query 1", "Query 2"]);
    assert_eq!(app.active(cx), Some(1));
    // Only the query tabs are stored, and the active one among them.
    let stored = storage.clone();
    cx.wait_for(app.window.into(), Duration::from_secs(10), move |_, _| {
        stored.tabs::<TabSnapshot>().unwrap().len() == 2
            && stored
                .setting::<usize>(crate::state::ACTIVE_TAB_KEY)
                .unwrap()
                == Some(0)
    })
    .await;

    let restarted = open(cx, storage);
    assert_eq!(restarted.titles(cx), ["Query 1", "Query 2"]);
    assert_eq!(restarted.active(cx), Some(0));
}

// Export and import ------------------------------------------------------------

impl Fixture {
    /// Answers the export dialog's "Choose file…" with `path`, then waits
    /// for the export to end.
    fn export_to(&self, cx: &mut TestAppContext, path: &Path) {
        self.click(cx, "export-choose-file");
        let path = path.to_path_buf();
        cx.simulate_new_path_selection(move |_| Some(path));
        self.wait_until(cx, "the export", |app, cx| {
            ["export-done", "export-failed", "export-cancelled"]
                .into_iter()
                .any(|id| app.has(cx, id))
        });
    }

    /// The connection the tests use, as a target.
    fn target(&self, cx: &mut TestAppContext, database: Option<&str>) -> Target {
        let connection = cx.update(|cx| self.connections.read(cx).saved()[0].id.clone());
        Target {
            connection,
            database: database.map(str::to_string),
            schema: None,
        }
    }

    /// Opens the import dialog into `table`, or into a new table of `schema`.
    fn import_dialog(
        &self,
        cx: &mut TestAppContext,
        schema: &str,
        table: Option<TableRef>,
    ) -> Option<Entity<crate::import_dialog::ImportDialog>> {
        let target = self.target(cx, None);
        let (workspace, schema) = (self.workspace.clone(), schema.to_string());
        self.step(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.import_csv_into(target, schema, table, window, cx)
            })
        })
    }

    /// Chooses `path` in an import dialog and waits for its preview.
    fn import_file(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<crate::import_dialog::ImportDialog>,
        path: &Path,
    ) -> String {
        self.click(cx, "import-choose-file");
        let path = path.to_path_buf();
        cx.simulate_path_prompt_response(move |_| Some(vec![path]));
        self.wait_until(cx, "the preview", |_, cx| {
            cx.update(|cx| dialog.read(cx).state(cx))
                .starts_with("options: ")
        });
        cx.update(|cx| dialog.read(cx).state(cx))
    }

    /// Maps, confirms and runs an import, and waits for it to end.
    fn run_import(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<crate::import_dialog::ImportDialog>,
    ) -> String {
        // "Map columns…" waits for the columns of an existing table.
        self.wait_until(cx, "the mapping", |app, cx| {
            app.click(cx, "import-next");
            !cx.update(|cx| dialog.read(cx).state(cx))
                .starts_with("options")
        });
        let state = cx.update(|cx| dialog.read(cx).state(cx));
        assert_eq!(state, "mapping");
        self.click(cx, "import-start");
        assert!(self.has(cx, "sql-preview"), "the SQL is shown first");
        self.click(cx, "sql-confirm");
        self.wait_until(cx, "the import", |_, cx| {
            let state = cx.update(|cx| dialog.read(cx).state(cx));
            ["done", "failed", "cancelled"]
                .iter()
                .any(|end| state.starts_with(end))
        });
        cx.update(|cx| dialog.read(cx).state(cx))
    }
}

#[gpui_kit::test]
fn a_filtered_table_is_exported_to_csv_and_json_through_the_dialog(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let (cleanup, create) = scratch_schema(engine, "kerno_ui_export");
        let _scratch = Scratch::new(
            engine,
            &[&cleanup],
            &[
                &create,
                "CREATE TABLE kerno_ui_export.t (id INT PRIMARY KEY, name TEXT, note TEXT)",
                "INSERT INTO kerno_ui_export.t VALUES \
                 (1, 'plain', NULL), (2, 'a, \"quoted\" one', 'x'), (3, 'third', NULL)",
            ],
        );
        let app = connected(cx, engine);
        let dir = tempfile::tempdir().unwrap();
        app.open_table(cx, "kerno_ui_export", "t", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–3 of 3");
        app.on_data(cx, |tab, window, cx| {
            tab.set_filter(
                Filter {
                    conditions: vec![Condition {
                        column: "id".into(),
                        operator: Operator::Greater,
                        value: "1".into(),
                    }],
                    raw: String::new(),
                },
                window,
                cx,
            )
        });
        app.wait_for_rows(cx, "Rows 1–2 of 2");

        // CSV: every row the filter keeps, not one page of them.
        let tab = app.data(cx);
        let dialog = app
            .step(cx, |window, cx| {
                tab.update(cx, |tab, cx| tab.export(window, cx))
            })
            .expect("the export dialog");
        let csv = dir.path().join("t.csv");
        app.export_to(cx, &csv);
        assert_eq!(
            cx.update(|cx| dialog.read(cx).state()),
            "done: 2 rows",
            "{engine:?}"
        );
        assert!(app.has(cx, "export-reveal"));
        assert_eq!(
            std::fs::read_to_string(&csv).unwrap(),
            "id,name,note\r\n2,\"a, \"\"quoted\"\" one\",x\r\n3,third,\r\n",
            "{engine:?}"
        );
        app.click(cx, "export-close");
        assert!(!app.has(cx, "export-dialog"));

        // JSON, from the toolbar button this time.
        app.click(cx, "data-export");
        app.click(cx, ("export-format", 2usize));
        let json = dir.path().join("t.json");
        app.export_to(cx, &json);
        assert!(app.has(cx, "export-done"), "{engine:?}");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&json).unwrap()).unwrap();
        assert_eq!(
            written,
            serde_json::json!([
                {"id": 2, "name": "a, \"quoted\" one", "note": "x"},
                {"id": 3, "name": "third", "note": null},
            ]),
            "{engine:?}"
        );
        app.click(cx, "export-close");

        // The tree offers it too, for the whole table.
        let path: &[&str] = match engine {
            Engine::Postgres => &["kerno", "kerno_ui_export", "Tables", "t"],
            _ => &["kerno_ui_export", "Tables", "t"],
        };
        app.open_path(cx, path);
        app.act(cx, Action::Export, path);
        let all = dir.path().join("all.csv");
        app.export_to(cx, &all);
        assert_eq!(
            std::fs::read_to_string(&all).unwrap().lines().count(),
            4,
            "{engine:?}"
        );
    }
}

#[gpui_kit::test]
fn a_slow_export_is_cancelled_and_leaves_no_file(cx: &mut TestAppContext) {
    for (engine, sql) in [
        (Engine::Postgres, "SELECT pg_sleep(60) AS slept"),
        (Engine::MariaDb, "SELECT SLEEP(60) AS slept"),
    ] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        let dir = tempfile::tempdir().unwrap();
        let target = app.target(cx, None);
        let connections = app.connections.clone();
        let dialog = app
            .step(cx, |window, cx| {
                crate::export_dialog::open(
                    connections,
                    target,
                    crate::export_dialog::Source::Query { sql: sql.into() },
                    window,
                    cx,
                )
            })
            .expect("the export dialog");
        let file = dir.path().join("slow.csv");
        app.click(cx, "export-choose-file");
        let chosen = file.clone();
        cx.simulate_new_path_selection(move |_| Some(chosen));
        cx.run_until_parked();
        assert_eq!(cx.update(|cx| dialog.read(cx).state()), "running");
        assert!(app.has(cx, "export-progress"));
        // The server says nothing for a minute; the cancel does not wait.
        app.click(cx, "export-cancel");
        app.wait_until(cx, "the cancel", |_, cx| {
            cx.update(|cx| dialog.read(cx).state()) != "running"
        });
        assert_eq!(cx.update(|cx| dialog.read(cx).state()), "cancelled");
        assert!(!file.exists(), "{engine:?}");
        assert!(!dir.path().join("slow.csv.tmp").exists(), "{engine:?}");
    }
}

#[gpui_kit::test]
fn a_query_result_is_exported_by_running_its_statement_again(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_export_result CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_export_result",
            "CREATE TABLE kerno_ui_export_result.t (id serial PRIMARY KEY, name text)",
            "INSERT INTO kerno_ui_export_result.t (name) VALUES ('a'), ('b'), ('c')",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let dir = tempfile::tempdir().unwrap();
    app.run_in_new_tab(
        cx,
        "select id, name from kerno_ui_export_result.t order by id",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["3 rows"]);
    app.click(cx, "result-export");
    assert!(app.has(cx, "export-dialog"));
    app.click(cx, ("export-format", 3usize));
    let file = dir.path().join("result.ndjson");
    app.export_to(cx, &file);
    assert!(app.has(cx, "export-done"));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "{\"id\":1,\"name\":\"a\"}\n{\"id\":2,\"name\":\"b\"}\n{\"id\":3,\"name\":\"c\"}\n"
    );
    app.click(cx, "export-close");

    // A statement that writes returns rows, but is not run again.
    app.run_in_new_tab(
        cx,
        "insert into kerno_ui_export_result.t (name) values ('d') returning id",
        "secondary-enter",
    );
    app.wait_until(cx, "the insert", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert!(!app.has(cx, "result-export"));

    // A tab that runs in a schema exports in that schema too.
    let schema = ["kerno", "kerno_ui_export_result"];
    app.open_path(cx, &schema);
    app.act(cx, Action::NewQueryHere, &schema);
    app.wait_until(cx, "the tab of the schema", |app, cx| {
        app.titles(cx).last().map(String::as_str) == Some("kerno_ui_export_result")
    });
    let tab = app.tab(cx);
    let editor = cx.update(|cx| tab.read(cx).editor().clone());
    app.step(cx, |window, cx| {
        editor.update(cx, |editor, cx| {
            editor.replace_all("select count(*) as n from t", window, cx)
        })
    });
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the count", |app, cx| app.outcomes(cx) == ["1 rows"]);
    app.click(cx, "result-export");
    let file = dir.path().join("count.csv");
    app.export_to(cx, &file);
    assert!(app.has(cx, "export-done"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "n\r\n4\r\n");
}

#[gpui_kit::test]
fn a_csv_file_is_imported_into_an_existing_table_and_into_a_new_one(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let (cleanup, create) = scratch_schema(engine, "kerno_ui_import");
        let _scratch = Scratch::new(
            engine,
            &[&cleanup],
            &[
                &create,
                "CREATE TABLE kerno_ui_import.people (id INT PRIMARY KEY, name TEXT, age INT)",
            ],
        );
        let server = |sql: &str| on_server(engine, sql);
        let app = connected(cx, engine);
        let dir = tempfile::tempdir().unwrap();
        let tables: &[&str] = match engine {
            Engine::Postgres => &["kerno", "kerno_ui_import", "Tables"],
            _ => &["kerno_ui_import", "Tables"],
        };
        app.open_path(cx, &[tables, &["people"]].concat());
        app.open_table(cx, "kerno_ui_import", "people", RelationKind::Table);
        app.wait_for_rows(cx, "No rows");

        // Into the table: semicolons, columns in another order, one the
        // table does not have, and an empty age that is NULL.
        let file = dir.path().join("people.csv");
        std::fs::write(
            &file,
            "Name;ID;city;age\nAda;1;London;36\n\"Bob; Jr\";2;Paris;\n",
        )
        .unwrap();
        let dialog = app
            .import_dialog(
                cx,
                "kerno_ui_import",
                Some(TableRef::new("kerno_ui_import", "people")),
            )
            .expect("the import dialog");
        assert_eq!(
            app.import_file(cx, &dialog, &file),
            "options: 4 columns, 2 rows, delimiter ';'",
            "{engine:?}"
        );
        assert!(app.has(cx, "import-preview"));
        assert_eq!(app.run_import(cx, &dialog), "done: 2 rows", "{engine:?}");
        assert_eq!(
            server(
                "SELECT concat(count(*), '/', count(age), '/', max(name)) \
                 FROM kerno_ui_import.people"
            ),
            "2/1/Bob; Jr",
            "{engine:?}"
        );
        // The open tab shows the new rows.
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        app.click(cx, "import-close");

        // Into a new table named after the file, with guessed types.
        let file = dir.path().join("Prices.csv");
        std::fs::write(
            &file,
            "code,price,day\nA1,1.5,2024-01-02\nB2,2,2024-02-03\n",
        )
        .unwrap();
        let dialog = app
            .import_dialog(cx, "kerno_ui_import", None)
            .expect("the import dialog");
        app.import_file(cx, &dialog, &file);
        assert_eq!(app.run_import(cx, &dialog), "done: 2 rows", "{engine:?}");
        assert_eq!(
            server("SELECT sum(price) FROM kerno_ui_import.prices"),
            if engine == Engine::Postgres {
                "3.5"
            } else {
                "3.5000000000"
            },
            "{engine:?}"
        );
        // The tree lists it.
        app.wait_until(cx, "the new table in the tree", |app, cx| {
            app.shows(cx, &[tables, &["prices"]].concat())
        });
        app.click(cx, "import-close");

        // The tree's entry opens the same dialog, and so do the commands,
        // for the table on screen.
        app.act(cx, Action::ImportCsv, tables);
        assert!(app.has(cx, "import-dialog"), "{engine:?}");
        app.click(cx, "import-close");
        for (command, dialog, close) in [
            (
                Box::new(crate::actions::ImportCsv) as Box<dyn gpui_kit::Action>,
                "import-dialog",
                "import-close",
            ),
            (
                Box::new(crate::actions::ExportData),
                "export-dialog",
                "export-close",
            ),
        ] {
            app.step(cx, |window, cx| window.dispatch_action(command, cx));
            assert!(app.has(cx, dialog), "{engine:?} {dialog}");
            app.click(cx, close);
        }
    }
}

#[gpui_kit::test]
fn a_failed_import_says_which_line_and_keeps_nothing(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let (cleanup, create) = scratch_schema(engine, "kerno_ui_import_fail");
        let _scratch = Scratch::new(
            engine,
            &[&cleanup],
            &[
                &create,
                "CREATE TABLE kerno_ui_import_fail.t (id INT PRIMARY KEY, age INT)",
            ],
        );
        let server = |sql: &str| on_server(engine, sql);
        let app = connected(cx, engine);
        let dir = tempfile::tempdir().unwrap();

        // A value the server refuses, at line 4.
        let file = dir.path().join("bad.csv");
        std::fs::write(&file, "id,age\n1,10\n2,20\nx3,30\n").unwrap();
        let dialog = app
            .import_dialog(
                cx,
                "kerno_ui_import_fail",
                Some(TableRef::new("kerno_ui_import_fail", "t")),
            )
            .expect("the import dialog");
        app.import_file(cx, &dialog, &file);
        let report = app.run_import(cx, &dialog);
        assert!(
            report.starts_with("failed: line 4: "),
            "{engine:?}: {report}"
        );
        assert!(app.has(cx, "import-failed"));
        assert_eq!(
            server("SELECT count(*) FROM kerno_ui_import_fail.t"),
            "0",
            "{engine:?}"
        );
        app.click(cx, "import-close");

        // A new table that exists already: nothing is created, and the
        // table that was there is not dropped.
        on_server(engine, "CREATE TABLE kerno_ui_import_fail.taken (n INT)");
        on_server(
            engine,
            "INSERT INTO kerno_ui_import_fail.taken VALUES (1), (2)",
        );
        let file = dir.path().join("taken.csv");
        std::fs::write(&file, "n\n3\n").unwrap();
        let dialog = app
            .import_dialog(cx, "kerno_ui_import_fail", None)
            .expect("the import dialog");
        app.import_file(cx, &dialog, &file);
        let report = app.run_import(cx, &dialog);
        assert!(report.starts_with("failed: "), "{engine:?}: {report}");
        assert!(!report.contains("dropped"), "{engine:?}: {report}");
        assert_eq!(
            server("SELECT count(*) FROM kerno_ui_import_fail.taken"),
            "2",
            "{engine:?}"
        );
        app.click(cx, "import-close");

        // A new table whose 62nd line does not fit the type guessed from
        // the first rows: the table does not stay.
        let mut text = String::from("n\n");
        for n in 1..=60 {
            text.push_str(&format!("{n}\n"));
        }
        text.push_str("oops\n");
        let file = dir.path().join("numbers.csv");
        std::fs::write(&file, text).unwrap();
        let dialog = app
            .import_dialog(cx, "kerno_ui_import_fail", None)
            .expect("the import dialog");
        app.import_file(cx, &dialog, &file);
        let report = app.run_import(cx, &dialog);
        assert!(
            report.starts_with("failed: line 62: "),
            "{engine:?}: {report}"
        );
        // MariaDB created it on its own and dropped it again; PostgreSQL
        // rolled the CREATE back with the rest.
        assert_eq!(
            report.ends_with("(table dropped)"),
            engine == Engine::MariaDb,
            "{report}"
        );
        assert_eq!(
            server(
                "SELECT count(*) FROM information_schema.tables \
                 WHERE table_schema = 'kerno_ui_import_fail' AND table_name = 'numbers'"
            ),
            "0",
            "{engine:?}"
        );
    }
}

#[gpui_kit::test]
fn a_read_only_connection_cannot_import(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    let tables = ["kerno", "shop", "Tables"];
    app.open_path(cx, &[&tables[..], &["customers"]].concat());
    assert_eq!(
        app.why_not(cx, Action::ImportCsv, &tables),
        Some("read-only connection")
    );
    app.act(cx, Action::ImportCsv, &tables);
    assert!(!app.has(cx, "import-dialog"));
    assert!(app.import_dialog(cx, "shop", None).is_none());
    // Exporting only reads: it stays offered.
    assert_eq!(
        app.why_not(
            cx,
            Action::Export,
            &["kerno", "shop", "Tables", "customers"]
        ),
        None
    );
}

// Menus of the tabs, the editor and the status bar ---------------------------------

impl Fixture {
    /// Clicks the element with this id with the middle button.
    fn middle_click(&self, cx: &mut TestAppContext, id: impl Into<gpui_kit::ElementId>) {
        use gpui_kit::test::ClickOptions;
        let options = ClickOptions::new().with_button(gpui_kit::MouseButton::Middle);
        self.step(cx, |window, cx| window.click_with_options(id, options, cx));
    }

    /// Runs a command as its shortcut or the palette does.
    fn dispatch(&self, cx: &mut TestAppContext, action: impl gpui_kit::Action) {
        self.step(cx, |window, cx| {
            window.dispatch_action(Box::new(action), cx)
        });
    }

    fn clipboard(&self, cx: &mut TestAppContext) -> Option<String> {
        cx.read_from_clipboard().and_then(|item| item.text())
    }

    fn set_confirm_close(&self, cx: &mut TestAppContext, confirm: bool) {
        cx.update(|cx| {
            AppState::update_settings(cx, |settings| settings.confirm_close_tab = confirm)
        });
    }
}

#[gpui_kit::test]
fn tabs_answer_a_right_click_and_close_or_come_back_from_their_menus(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(
        cx,
        &["secondary-t", "secondary-t", "secondary-t", "secondary-t"],
    );
    app.type_text(cx, "select 'fourth'");

    // A tab: what DBeaver's tab menu offers.
    assert!(!app.shows_label(cx, "Close tabs to the right"));
    app.right_click(cx, ("tab-title", 1usize), None);
    for label in [
        "Close",
        "Close others",
        "Close tabs to the right",
        "Close all",
        "Rename…",
        "Duplicate",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.click_label(cx, "Close tabs to the right");
    assert_eq!(app.titles(cx), ["Query 1", "Query 2"]);

    // The empty area of the bar: the last tab closed comes back with its text.
    assert!(!app.shows_label(cx, "Reopen closed tab"));
    app.right_click(cx, "tab-bar-empty", None);
    for label in ["New query tab", "Reopen closed tab", "Close all"] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.click_label(cx, "Reopen closed tab");
    assert_eq!(app.titles(cx), ["Query 1", "Query 2", "Query 4"]);
    assert_eq!(app.active(cx), Some(2));
    assert_eq!(app.active_query(cx).1, "select 'fourth'");

    // Close others, from the menu of a tab that is not the active one.
    app.right_click(cx, ("tab-title", 2usize), None);
    app.click_label(cx, "Close others");
    assert_eq!(app.titles(cx), ["Query 4"]);
    // What does not apply to the only tab says why.
    app.right_click(cx, ("tab-title", 0usize), None);
    assert!(app.shows_label(cx, "Close others (no other tab)"));
    assert!(app.shows_label(cx, "Close tabs to the right (none to the right)"));
    // A copy of the tab, its text and where it runs.
    app.click_label(cx, "Duplicate");
    assert_eq!(app.titles(cx), ["Query 4", "Query 5"]);
    assert_eq!(app.active_query(cx).1, "select 'fourth'");

    // A middle click closes the tab under the mouse; the active one stays.
    app.press(cx, &["secondary-t", "ctrl-tab"]);
    assert_eq!(app.active(cx), Some(0));
    app.middle_click(cx, ("tab-title", 1usize));
    assert_eq!(app.titles(cx), ["Query 4", "Query 6"]);
    assert_eq!(app.active(cx), Some(0));

    // Close all from its shortcut; the empty window still reopens, the last
    // closed first.
    app.press(cx, &["secondary-shift-w"]);
    assert_eq!(app.titles(cx), Vec::<String>::new());
    app.right_click(cx, "tabs-empty", None);
    assert!(app.shows_label(cx, "Close all (no tab open)"));
    app.click_label(cx, "Reopen closed tab");
    assert_eq!(app.titles(cx), ["Query 6"]);
    // And from the palette's command.
    app.dispatch(cx, actions::ReopenClosedTab);
    assert_eq!(app.titles(cx), ["Query 6", "Query 4"]);
    assert_eq!(app.active_query(cx).1, "select 'fourth'");
}

#[gpui_kit::test]
async fn a_renamed_query_tab_keeps_its_name_after_a_restart(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    app.press(cx, &["secondary-t"]);
    app.right_click(cx, ("tab-title", 0usize), None);
    app.click_label(cx, "Rename…");
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "Monthly report");
    app.click(cx, "tab-name-ok");
    assert_eq!(app.titles(cx), ["Monthly report"]);

    // From the palette's command; an empty name is refused.
    app.dispatch(cx, actions::RenameTab);
    app.press(cx, &["secondary-a", "backspace"]);
    app.click(cx, "tab-name-ok");
    assert!(app.has(cx, "tab-name-ok"), "the dialog stays open");
    app.click(cx, "tab-name-cancel");
    assert_eq!(app.titles(cx), ["Monthly report"]);

    app.wait_for_stored_tabs(
        cx,
        |tabs| matches!(tabs, [TabSnapshot::Query { title, .. }] if title == "Monthly report"),
    )
    .await;
    let restarted = open(cx, storage);
    assert_eq!(restarted.titles(cx), ["Monthly report"]);
}

#[gpui_kit::test]
fn a_file_with_unsaved_text_asks_before_its_tab_closes(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.sql");
    std::fs::write(&file, "select 1;\n").unwrap();
    let (workspace, path) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.open_sql_file_from(path, window, cx)
        })
    });
    app.wait_until(cx, "the file", |app, cx| app.titles(cx) == ["report.sql"]);
    let unsaved = |app: &Fixture, cx: &mut TestAppContext| {
        let tab = app.tab(cx);
        cx.update(|cx| tab.read(cx).unsaved())
    };

    // A file tab: named after its file, which can be found.
    app.right_click(cx, ("tab-title", 0usize), None);
    assert!(app.shows_label(cx, "Rename… (named after its file)"));
    assert!(app.shows_label(cx, &crate::tabs::TabAction::Reveal.label()));
    app.click_label(cx, "Copy file path");
    assert_eq!(app.clipboard(cx), Some(file.display().to_string()));

    // A tab that holds nothing closes without a question.
    app.type_text(cx, "-- edited\n");
    app.press(cx, &["secondary-t", "secondary-w"]);
    assert_eq!(app.titles(cx), ["report.sql"]);
    assert!(!app.has(cx, "ok"));

    // Cmd+W asks; Cancel keeps the tab and its text.
    app.press(cx, &["secondary-w"]);
    assert!(app.shows_label(cx, "Discard and close"));
    app.click(cx, "cancel");
    assert_eq!(app.titles(cx), ["report.sql"]);
    assert!(unsaved(&app, cx));
    // A middle click asks the same; confirming closes it.
    app.middle_click(cx, ("tab-title", 0usize));
    assert!(app.shows_label(cx, "Discard and close"));
    app.click(cx, "ok");
    assert_eq!(app.titles(cx), Vec::<String>::new());

    // It comes back with its file and its text, still not saved.
    app.dispatch(cx, actions::ReopenClosedTab);
    assert_eq!(app.titles(cx), ["report.sql"]);
    assert_eq!(app.active_query(cx).1, "-- edited\nselect 1;\n");
    assert!(unsaved(&app, cx));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "select 1;\n");

    // With the setting off, it closes without a question.
    app.set_confirm_close(cx, false);
    app.press(cx, &["secondary-w"]);
    assert_eq!(app.titles(cx), Vec::<String>::new());
    assert!(!app.has(cx, "ok"));
}

#[gpui_kit::test]
fn the_editor_answers_a_right_click_with_its_commands(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 1 as one, 2 as two");
    // On the text of the first line.
    let on_text = Some(gpui_kit::point(px(120.), px(10.)));

    assert!(!app.shows_label(cx, "Toggle comment"));
    app.right_click(cx, "query-editor", on_text);
    for label in [
        "Run statement (not connected)",
        "Run script (not connected)",
        "Explain plan (not connected)",
        "Explain analyze (not connected)",
        "Format SQL",
        "Undo",
        "Redo",
        "Cut (no selection)",
        "Copy (no selection)",
        "Paste",
        "Select all",
        "Toggle comment",
        "Find…",
        "Replace…",
        "Save",
        "Save as…",
        "Save as query…",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    // The editor's own actions run from the menu.
    app.click_label(cx, "Select all");
    let tab = app.tab(cx);
    let selection = cx.update(|cx| tab.read(cx).editor().read(cx).selected_range());
    assert_eq!(selection, 0..25);
    app.right_click(cx, "query-editor", on_text);
    app.click_label(cx, "Copy");
    assert_eq!(
        app.clipboard(cx).as_deref(),
        Some("select 1 as one, 2 as two")
    );
    // So do the app's commands.
    app.right_click(cx, "query-editor", on_text);
    app.click_label(cx, "Toggle comment");
    assert_eq!(app.active_query(cx).1, "-- select 1 as one, 2 as two");
    app.right_click(cx, "query-editor", on_text);
    app.click_label(cx, "Find…");
    assert_eq!(cx.update(|cx| tab.read(cx).searching(cx)), Some(false));
}

#[gpui_kit::test]
fn a_read_only_connection_explains_itself_in_the_status_bar(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    app.press(cx, &["secondary-t"]);
    assert!(!app.shows_label(cx, "What read-only means…"));
    app.right_click(cx, "status-read-only", None);
    assert!(app.shows_label(cx, "What read-only means…"));
    assert!(app.shows_label(cx, "Edit connection…"));
    app.click_label(cx, "What read-only means…");
    assert!(app.has(cx, "ok"), "the explanation is shown");
}

#[gpui_kit::test]
fn closing_rolls_back_and_discards_only_once_confirmed(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let server = |sql: &str| on_server(Engine::Postgres, sql);
    let _scratch = Scratch::new(
        Engine::Postgres,
        // A failed run may leave its transaction open: it would hold the drop.
        &[
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE query LIKE '%kerno_ui_close.entries values (2%' AND pid <> pg_backend_pid()",
            "DROP SCHEMA IF EXISTS kerno_ui_close CASCADE",
        ],
        &[
            "CREATE SCHEMA kerno_ui_close",
            "CREATE TABLE kerno_ui_close.entries (id int PRIMARY KEY, note text)",
            "INSERT INTO kerno_ui_close.entries VALUES (1, 'kept')",
        ],
    );
    // Sessions of this test left in a transaction.
    let open_on_server = || {
        server(
            "SELECT count(*) FROM pg_stat_activity WHERE state LIKE 'idle in transaction%' \
             AND query LIKE '%kerno_ui_close.entries values (2%'",
        )
    };
    let app = connected(cx, Engine::Postgres);

    // A transaction left open in a query tab, its statement run from the
    // editor's menu.
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(true, cx)));
    app.type_text(cx, "insert into kerno_ui_close.entries values (2, 'open')");
    app.right_click(cx, "query-editor", Some(gpui_kit::point(px(120.), px(10.))));
    app.click_label(cx, "Run statement");
    app.wait_until(cx, "the insert", |app, cx| {
        app.outcomes(cx) == ["done, 1 affected"] && !app.running(cx)
    });
    assert!(app.status_bar(cx).contains(&"transaction open".to_string()));
    assert_eq!(open_on_server(), "1");

    // The status bar's items answer a right-click, and a click.
    app.right_click(cx, "status-connection", None);
    for label in [
        "Disconnect (transaction open)",
        "Reconnect (transaction open)",
        "Edit connection…",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.press(cx, &["escape"]);
    app.right_click(cx, "status-transaction", None);
    for label in [
        "Commit transaction",
        "Roll back transaction",
        "Switch to auto-commit",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.press(cx, &["escape"]);
    assert!(!app.shows_label(cx, "Roll back transaction"));
    app.click(cx, "status-transaction-button");
    assert!(app.shows_label(cx, "Roll back transaction"));
    app.press(cx, &["escape"]);
    let database = app.status_bar(cx)[1].clone();
    app.wait_until(cx, "the databases", |app, cx| {
        app.has(cx, "status-database")
    });
    app.right_click(cx, "status-database", None);
    assert!(app.shows_label(cx, &format!("{database} (current)")));
    app.press(cx, &["escape"]);
    let timing = app.status_bar(cx).last().cloned().unwrap();
    app.right_click(cx, "status-timing", None);
    app.click_label(cx, "Copy");
    assert_eq!(app.clipboard(cx), Some(timing));
    app.right_click(cx, "status-memory-only", None);
    app.click_label(cx, "What this means…");
    assert!(app.has(cx, "ok"), "the explanation is shown");
    app.click(cx, "ok");
    // From the palette, a disconnection that would lose the transaction is refused.
    app.dispatch(cx, actions::DisconnectTabConnection);
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));

    // A data tab with a change waiting.
    app.open_table(cx, "kerno_ui_close", "entries", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    app.type_in_cell(cx, 0, 2, "changed", "enter");
    assert_eq!(app.pending(cx), 1);
    app.right_click(cx, "status-pending", None);
    assert!(app.shows_label(cx, "Review SQL…"));
    assert!(app.shows_label(cx, "Discard changes"));
    app.press(cx, &["escape"]);

    // A tab that holds nothing: a running statement is cancelled from the
    // status bar, and the tab then closes without a question.
    app.run_in_new_tab(
        cx,
        "select pg_sleep(60) as kerno_ui_close",
        "secondary-enter",
    );
    // On the server: a cancel request that arrives before the statement is
    // ignored there.
    app.wait_until(cx, "the statement to start", |app, cx| {
        app.running(cx)
            && server(
                "SELECT count(*) FROM pg_stat_activity WHERE state = 'active' \
                 AND query LIKE 'select pg_sleep(60) as kerno_ui_close%'",
            ) == "1"
    });
    app.right_click(cx, "status-running", None);
    app.click_label(cx, "Cancel running statement");
    app.wait_until(cx, "the cancellation", |app, cx| !app.running(cx));
    assert_eq!(app.outcomes(cx), ["cancelled"]);
    app.press(cx, &["secondary-w"]);
    assert_eq!(app.titles(cx), ["Query 1", "entries"]);
    assert!(!app.has(cx, "ok"));

    // Closing all asks once for both; Cancel closes nothing and keeps both.
    app.press(cx, &["secondary-shift-w"]);
    assert!(app.shows_label(cx, "Roll back and close"));
    app.click(cx, "cancel");
    assert_eq!(app.titles(cx), ["Query 1", "entries"]);
    assert_eq!(app.pending(cx), 1);
    assert_eq!(open_on_server(), "1");

    // With the setting off the pending change no longer asks, the
    // transaction still does; confirming rolls it back.
    app.set_confirm_close(cx, false);
    app.press(cx, &["secondary-shift-w"]);
    assert!(app.shows_label(cx, "Roll back and close"));
    app.click(cx, "ok");
    assert_eq!(app.titles(cx), Vec::<String>::new());
    app.wait_until(cx, "the rollback", |_, _| open_on_server() == "0");
    assert_eq!(server("SELECT count(*) FROM kerno_ui_close.entries"), "1");
    assert_eq!(
        server("SELECT note FROM kerno_ui_close.entries WHERE id = 1"),
        "kept"
    );

    // The query tab comes back with its text and where it ran, without its
    // transaction.
    app.dispatch(cx, actions::ReopenClosedTab);
    let connection = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let tab = app.tab(cx);
    let target = cx.update(|cx| {
        tab.read(cx)
            .target()
            .map(|target| target.connection.clone())
    });
    assert_eq!(target, Some(connection));
    assert_eq!(
        app.active_query(cx).1,
        "insert into kerno_ui_close.entries values (2, 'open')"
    );
    assert!(!app.status_bar(cx).contains(&"transaction open".to_string()));
}
