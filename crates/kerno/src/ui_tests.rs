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

mod ai;
mod ai_ask;
mod ai_chat;
mod ai_endpoints;
mod ai_inline;
mod alter_form;
mod backup_simple;
mod brackets;
mod compare;
mod connection_feedback;
mod connection_scripts;
mod coverage;
mod critique;
mod daily_use;
mod data_compare;
mod dbeaver_import;
mod editing_v11;
mod editor_v11;
mod environments;
mod erd;
mod extensions;
mod feedback;
mod fk_preview;
mod folders;
mod grid_v11;
mod k40;
mod k44;
mod k45;
mod k49;
mod k54;
mod k55;
mod k56;
mod k59;
mod k62;
mod k72;
mod k75;
mod k76;
mod k80;
mod legacy;
mod menus;
mod multi_run;
mod mysql_folder;
mod open_url;
mod openssh;
// A fake `aws` script (`kerno_core::testing::fake_aws`): Unix only (K64).
#[cfg(unix)]
mod password_sources;
mod portability;
mod reconnect;
mod reorder;
mod restore_extensions;
mod restore_roles;
mod run_marks;
mod schema_export;
mod script_progress;
mod secret_store;
mod settings_look;
mod shortcuts;
mod sidebar_tree;
mod split;
mod sqlite;
mod table_loads;
mod tabs_v11;
mod tasks;
// A fake `aws` script (`kerno_core::testing::fake_aws`): Unix only (K64).
#[cfg(unix)]
mod tool_passwords;
mod tool_wizards;
mod toolbar;
mod tools_menu;
mod transfer;
mod updates;
mod value_panel;
mod vim;
mod visual4;
mod xlsx;

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
    testing::exclusive_run();
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
            secret_stores: crate::state::memory_stores,
            settings,
            appearance_override: None,
        });
        theme::init(appearance, cx).unwrap();
        let problems = actions::bind_keys(Path::new("/nonexistent/kerno-tests/keymap.json"), cx);
        assert_eq!(problems, Vec::<String>::new());

        let saved = storage.connections().unwrap();
        let drivers = Drivers::default()
            .with(kerno_postgres::PostgresDriver)
            .with(kerno_mysql::MysqlDriver)
            .with(kerno_sqlite::SqliteDriver);
        // No user known_hosts: tests never read the developer's SSH files.
        let connections = cx.new(|_| Connections::new(saved, drivers, None));
        let tabs = storage.tabs().unwrap();
        let active_tab = storage
            .setting(crate::state::ACTIVE_TAB_KEY)
            .unwrap()
            .unwrap_or(0);
        let layout = storage.setting(crate::state::TAB_LAYOUT_KEY).unwrap();
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(1200.), px(800.)),
            })),
            ..Default::default()
        };
        let manager = connections.clone();
        let (window, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
            let workspace =
                cx.new(|cx| Workspace::new(connections, tabs, active_tab, layout, window, cx));
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
            if std::time::Instant::now() >= deadline {
                panic!("timed out waiting for {what}; {}", self.situation(cx));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// What the window shows, for the message of a wait that timed out:
    /// the tabs, the active query tab's results, the status bar.
    fn situation(&self, cx: &mut TestAppContext) -> String {
        let tabs = self.titles(cx);
        let status = self.status_bar(cx);
        let query = cx.update(|cx| {
            let tab = self.workspace.read(cx).active_query_tab()?.read(cx);
            Some(format!(
                "results {:?}, {:?}, messages {:?}",
                tab.outcomes(cx),
                tab.activity(),
                tab.messages()
            ))
        });
        format!("tabs {tabs:?}, status bar {status:?}, query tab: {query:?}")
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

    /// Whether an item of the status bar starts with `text` (an item may go
    /// on with more: "transaction open · 12 s").
    fn status_says(&self, cx: &mut TestAppContext, text: &str) -> bool {
        self.status_bar(cx)
            .iter()
            .any(|item| item.starts_with(text))
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

    /// Double-clicks the row of the saved connection `index`, scrolled into
    /// view first: the open connections above it may have pushed it below
    /// the rows the list draws.
    fn open_connection_row(&self, cx: &mut TestAppContext, index: usize) {
        let navigator = cx.update(|cx| self.workspace.read(cx).navigator(cx));
        let id = cx.update(|cx| self.connections.read(cx).saved()[index].id.clone());
        cx.update(|cx| {
            navigator.update(cx, |navigator, cx| navigator.scroll_to_connection(&id, cx))
        });
        self.step(cx, |window, cx| {
            window.double_click(("connection", index), cx)
        });
    }

    /// Right-clicks the empty area of the sidebar's tree, below its rows.
    fn right_click_below_the_tree(&self, cx: &mut TestAppContext) {
        let height = self.step(cx, |window, _| {
            window.find("navigator-tree").bounds().size.height
        });
        self.right_click(
            cx,
            "navigator-tree",
            Some(gpui_kit::point(px(40.), height - px(8.))),
        );
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
    // (One Escape closes it, with text typed or not: K55.)
    app.press(cx, &["escape"]);
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

    // J15: the file keeps the connection's id, so importing it where it
    // came from finds it unchanged and adds nothing: Import is off (K77).
    let (workspace, source) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.import_connections_from(source, window, cx)
        });
    });
    app.wait_until(cx, "the import preview", |app, cx| {
        app.has(cx, "import-dialog")
    });
    assert!(app.has(cx, "import-nothing"));
    app.click(cx, "import-next");
    assert!(!app.has(cx, "import-done"));
    app.click(cx, "import-cancel");
    assert_eq!(app.storage.connections().unwrap().len(), 1);

    // A file that is not an export changes nothing.
    std::fs::write(&file, "{}").unwrap();
    let (workspace, source) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.import_connections_from(source, window, cx)
        });
    });
    cx.run_until_parked();
    assert!(!app.has(cx, "import-dialog"));
    assert_eq!(app.storage.connections().unwrap().len(), 1);
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
    for area in ["navigator-tree", "connections-header"] {
        assert!(!app.shows_label(cx, "New folder…"));
        if area == "navigator-tree" {
            app.right_click_below_the_tree(cx);
        } else {
            app.right_click(cx, area, None);
        }
        for label in [
            "New connection…",
            "New folder…",
            "Import connections…",
            "Refresh",
            "Collapse all",
            "Link with editor",
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
    // Only the row's menu opens, not the list's under it.
    assert!(!app.shows_label(cx, "New folder…"));
    app.press(cx, &["escape"]);

    // Around the filter too (the field has no menu of its own).
    app.right_click(cx, "navigator-filter", None);
    assert!(app.shows_label(cx, "New folder…"));
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
    app.right_click_below_the_tree(cx);
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
        window.drag_to(("connection", 0usize), "navigator-tree", cx)
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
    app.wait_until(cx, "an open database", |app, cx| {
        app.tree(cx).iter().any(|row| row.starts_with("  "))
    });
    assert!(!app.shows_label(cx, "Link with editor"));
    // The list's menu, the one of every empty area of the sidebar: "New
    // query here" is the connection's "New SQL editor" now.
    app.right_click_below_the_tree(cx);
    for label in [
        "New connection…",
        "New folder…",
        "Refresh",
        "Collapse all",
        "Link with editor",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    assert!(!app.shows_label(cx, "New query here"));
    // Collapse all closes the connection's row and leaves it connected.
    app.click_label(cx, "Collapse all");
    assert_eq!(app.tree(cx), Vec::<String>::new());
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    app.open_connection_row(cx, 0);
    assert!(
        app.tree(cx).iter().any(|row| row.starts_with("  ")),
        "{:#?}",
        app.tree(cx)
    );

    // The tree's own entries are palette commands too.
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

    // Disconnecting closes the connection's row; the row, the filter and
    // the rest of the sidebar stay.
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::Disconnect);
    assert_eq!(app.status(cx, 0), Status::Disconnected);
    assert_eq!(app.tree(cx), Vec::<String>::new());
    assert!(app.has(cx, "navigator-filter"));
    assert!(app.has(cx, ("connection", 0usize)));

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
    on_database(engine, None, sql)
}

/// [`on_server`] in `database`, or in the test server's default one.
fn on_database(engine: Engine, database: Option<&str>, sql: &str) -> String {
    use kerno_core::Driver as _;
    let sql = sql.to_string();
    let database = database.map(str::to_string);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let mut params = testing::params(engine);
            if database.is_some() {
                params.database = database;
            }
            let session = match engine {
                Engine::Postgres => kerno_postgres::PostgresDriver.connect(&params).await,
                Engine::MySql | Engine::MariaDb => kerno_mysql::MysqlDriver.connect(&params).await,
                kerno_core::Engine::Sqlite => {
                    unreachable!("SQLite has no test server: these tests run on the server engines")
                }
            }
            .expect("the test server");
            // A session a test left open must make the statement fail, with
            // the server's message, not wait forever (and every later run
            // with it, behind the exclusive test lock).
            let (timeout, sessions) = match engine {
                Engine::Postgres => (
                    "SET lock_timeout = '15s'",
                    "SELECT string_agg(concat_ws(' | ', pid, usename, datname, state, \
                     left(query, 120)), E'\\n') FROM pg_stat_activity \
                     WHERE pid <> pg_backend_pid() AND backend_type = 'client backend'",
                ),
                Engine::MySql | Engine::MariaDb => (
                    "SET SESSION lock_wait_timeout = 15",
                    "SELECT GROUP_CONCAT(CONCAT_WS(' | ', ID, USER, DB, COMMAND, STATE, \
                     LEFT(INFO, 120)) SEPARATOR '\\n') FROM information_schema.processlist \
                     WHERE ID <> CONNECTION_ID()",
                ),
                kerno_core::Engine::Sqlite => {
                    unreachable!("SQLite has no test server: these tests run on the server engines")
                }
            };
            kerno_core::exec::collect(session.clone(), timeout)
                .await
                .expect("the lock timeout");
            // A database a test drops goes with whatever is still in it.
            let sql = if engine == Engine::Postgres
                && sql
                    .trim_start()
                    .to_ascii_uppercase()
                    .starts_with("DROP DATABASE")
                && !sql.to_ascii_uppercase().contains("FORCE")
            {
                format!("{} WITH (FORCE)", sql.trim_end().trim_end_matches(';'))
            } else {
                sql
            };
            let collected = match kerno_core::exec::collect(session.clone(), sql.clone()).await {
                Ok(collected) => collected,
                Err(error) => {
                    let open = kerno_core::exec::collect(session.clone(), sessions)
                        .await
                        .ok()
                        .and_then(|listed| {
                            listed
                                .results
                                .last()?
                                .rows()
                                .first()?
                                .first()
                                .cloned()
                                .flatten()
                        })
                        .unwrap_or_default();
                    panic!("{sql}: {error}\nsessions on the server:\n{open}")
                }
            };
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

/// Whether a statement whose text holds `marker` is running on the server,
/// in a session other than the one asking. A cancel sent before that is
/// ignored by the server: tests that cancel wait for it.
fn running_on_server(engine: Engine, marker: &str) -> bool {
    let sql = match engine {
        Engine::Postgres => format!(
            "SELECT count(*) FROM pg_stat_activity WHERE state = 'active' \
             AND query LIKE '%{marker}%' AND pid <> pg_backend_pid()"
        ),
        Engine::MySql | Engine::MariaDb => format!(
            "SELECT count(*) FROM information_schema.processlist WHERE COMMAND = 'Query' \
             AND INFO LIKE '%{marker}%' AND ID <> CONNECTION_ID()"
        ),
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    };
    on_server(engine, &sql) != "0"
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
            if !std::thread::panicking() {
                on_server(self.engine, sql);
                continue;
            }
            // A second panic would abort the whole run, and leave every
            // other running test's objects behind too: say why and go on
            // (the next run of this test drops what is left first).
            let (engine, owned) = (self.engine, sql.clone());
            if std::thread::spawn(move || on_server(engine, &owned))
                .join()
                .is_err()
            {
                eprintln!("cleanup of a failed test did not run: {sql}");
            }
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
    app.click(cx, "maintenance-run");
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
        config.environment = kerno_core::environment::PRODUCTION.into();
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
        // The tree read the objects again, the view open on its columns
        // again (K80), its new column there.
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
    app.open_path(cx, &["kerno_ui_ext", "Extensions", "plpgsql"]);
    // K27: the folder lists what is installed only; a new database has only
    // plpgsql. The rest is in the extensions manager.
    let listed = |app: &Fixture, cx: &mut TestAppContext| {
        let tree = app.tree(cx);
        let database = tree
            .iter()
            .position(|line| *line == row(0, "kerno_ui_ext"))
            .unwrap();
        let folder = database
            + tree[database..]
                .iter()
                .position(|line| *line == row(1, "Extensions"))
                .unwrap();
        tree[folder + 1..]
            .iter()
            .take_while(|line| line.starts_with("    "))
            .map(|line| line.trim().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(listed(&app, cx), ["plpgsql"]);

    // Installed from the manager, it shows in the tree.
    app.act(cx, Action::ManageExtensions, &extensions);
    let tab = app.extensions_manager(cx);
    app.wait_until(cx, "the extensions", |_, cx| {
        cx.update(|cx| tab.read(cx).state_text()) == "ready"
    });
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| tab.ask_install("citext", window, cx))
    });
    assert!(app.has(cx, "sql-preview"));
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the install", |app, cx| {
        listed(app, cx) == ["citext", "plpgsql"]
    });

    // Dropped from the tree, it leaves it.
    app.act(cx, Action::Drop, &["kerno_ui_ext", "Extensions", "citext"]);
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the drop", |app, cx| listed(app, cx) == ["plpgsql"]);
    assert!(!app.shows(cx, &[&extensions[..], &["citext"]].concat()));
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
        (
            Engine::Postgres,
            "select pg_sleep(60) as kerno_ui_cancel_run",
        ),
        (
            Engine::MySql,
            "select count(*) as kerno_ui_cancel_run from kerno.orders where sleep(0.05) = 0",
        ),
        (
            Engine::MariaDb,
            "select count(*) as kerno_ui_cancel_run from kerno.orders where sleep(0.05) = 0",
        ),
    ] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        app.run_in_new_tab(cx, slow, "secondary-enter");
        app.wait_until(cx, "the statement to start", |app, cx| {
            app.running(cx) && running_on_server(engine, "kerno_ui_cancel_run")
        });
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
    // Nothing runs (C6): the messages and the bar above the editor say where to pick one.
    let tab = app.tab(cx);
    assert_eq!(app.outcomes(cx), Vec::<String>::new());
    assert_eq!(
        cx.update(|cx| tab.read(cx).messages()),
        ["Warning: Nothing ran. Pick a connection to run this script."]
    );
    assert!(app.has(cx, "unbound-pick"));
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
        let open = |app: &Fixture, cx: &mut TestAppContext| app.status_says(cx, "transaction open");

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
            assert!(app.status_says(cx, "transaction failed: roll back"));
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
    use gpui_kit::test::ClickOptions;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(cx, "select 1 as a, 'x' as b", "secondary-enter");
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    let menu_has =
        |app: &Fixture, cx: &mut TestAppContext, label: &str| menu_entry(app, cx, label).is_some();
    assert!(!menu_has(&app, cx, "Copy as"));
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
    // The menu of a cell of a result (its entries: `query::grid::tests`).
    for label in [
        "Copy",
        "Copy as",
        "Order by this column",
        "View value",
        "Column",
        "Export…",
        // A SELECT's result is filtered by the server (G5).
        "Filter by this value",
    ] {
        assert!(menu_has(&app, cx, label), "{label}");
    }
    // A result has no edits.
    for label in ["Paste", "Edit cell", "Row"] {
        assert!(!menu_has(&app, cx, label), "{label}");
    }
    // The column's entries are in its submenu.
    click_menu_entry(&app, cx, "Column");
    for label in ["Copy column name", "Hide column"] {
        assert!(menu_has(&app, cx, label), "{label}");
    }
}

// Grid menus (docs/plans/CTX.md Part 3) -------------------------------------------

/// Where the entry of an open menu with this label is drawn (a button
/// elsewhere may have the same label).
fn menu_entry(
    app: &Fixture,
    cx: &mut TestAppContext,
    label: &str,
) -> Option<Point<gpui_kit::Pixels>> {
    use gpui_kit::test::TestQueryExt as _;
    let popup = gpui_kit::ElementId::Name("popup-menu".into());
    app.step(cx, |window, _| {
        window
            .find_all_by_label(label)
            .into_iter()
            .find(|entry| entry.path().contains(&popup))
            .map(|entry| entry.bounds().center())
    })
}

/// Clicks the entry of an open menu with this label.
fn click_menu_entry(app: &Fixture, cx: &mut TestAppContext, label: &str) {
    let centre = menu_entry(app, cx, label).unwrap_or_else(|| panic!("no menu entry {label:?}"));
    app.step(cx, |window, cx| window.drag(centre, centre, cx));
}

#[gpui_kit::test]
fn the_menu_of_a_result_cell_acts_on_the_selection_or_on_its_cell(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g as n, 'v' || g as label from generate_series(1, 3) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["3 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    let clipboard = |cx: &mut TestAppContext| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    };
    // Two rows by two columns selected; a right-click inside keeps them.
    cx.update(|cx| {
        grid.update(cx, |table, cx| {
            table.set_selected_cell(0, 1, cx);
            table.delegate_mut().extend_to(1, 2);
        })
    });
    app.right_click(cx, "table-cell-1:2", None);
    click_menu_entry(&app, cx, "Copy");
    assert_eq!(clipboard(cx), "1\tv1\n2\tv2\n");
    app.right_click(cx, "table-cell-0:1", None);
    click_menu_entry(&app, cx, "Column");
    click_menu_entry(&app, cx, "Copy column name");
    assert_eq!(clipboard(cx), "n\nlabel");
    // Outside the selection, the right-click selects its cell.
    app.right_click(cx, "table-cell-2:2", None);
    click_menu_entry(&app, cx, "Copy");
    assert_eq!(clipboard(cx), "v3");
    // Copy as ▸ JSON, from the submenu.
    app.right_click(cx, "table-cell-2:2", None);
    click_menu_entry(&app, cx, "Copy as");
    click_menu_entry(&app, cx, "JSON");
    assert_eq!(clipboard(cx), "[{\"label\":\"v3\"}]");

    // Order ▸ Descending sorts what was fetched; Clear order undoes it.
    let first_column = |cx: &mut TestAppContext| -> Vec<String> {
        cx.update(|cx| grid.read(cx).delegate().shown())
            .into_iter()
            .map(|row| row[0].clone())
            .collect()
    };
    app.right_click(cx, "table-cell-0:1", None);
    click_menu_entry(&app, cx, "Order by this column");
    click_menu_entry(&app, cx, "Descending");
    assert_eq!(first_column(cx), ["3", "2", "1"]);
    app.right_click(cx, "table-cell-0:1", None);
    click_menu_entry(&app, cx, "Order by this column");
    click_menu_entry(&app, cx, "Clear order");
    assert_eq!(first_column(cx), ["1", "2", "3"]);

    // Export… opens the export of this result.
    app.right_click(cx, "table-cell-0:1", None);
    click_menu_entry(&app, cx, "Export…");
    assert!(app.has(cx, "export-dialog"));
}

/// A table with a NULL and a repeated value, for filters from a cell.
fn filter_scratch(engine: Engine, schema: &str) -> Scratch {
    let (drop_schema, create_schema) = scratch_schema(engine, schema);
    Scratch::new(
        engine,
        &[&drop_schema],
        &[
            &create_schema,
            &scratch_table(
                engine,
                &format!("{schema}.items"),
                "name VARCHAR(20) NOT NULL, qty INT",
            ),
            &format!(
                "INSERT INTO {schema}.items (name, qty) \
                 VALUES ('a', 1), ('b', 2), ('c', NULL), ('b', 3)"
            ),
        ],
    )
}

#[gpui_kit::test]
fn filter_by_this_value_and_exclude_it_change_the_rows(cx: &mut TestAppContext) {
    use crate::query::grid::GridAction;

    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let _scratch = filter_scratch(engine, "kerno_ui_cell_filter");
        let app = connected(cx, engine);
        app.open_table(cx, "kerno_ui_cell_filter", "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–4 of 4");
        let ids = |app: &Fixture, cx: &mut TestAppContext| -> Vec<String> {
            app.data_rows(cx)
                .into_iter()
                .map(|row| row[0].clone())
                .collect()
        };
        let perform = |app: &Fixture, cx: &mut TestAppContext, action, row, column| {
            app.on_data(cx, |tab, window, cx| {
                tab.perform(action, row, column, window, cx)
            })
        };

        // A real right-click on the name "b" of the second row.
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("x".into()));
        app.right_click(cx, "table-cell-1:2", None);
        // "name" is NOT NULL: the entry says why it cannot be used.
        for label in [
            "Paste",
            "Edit cell",
            "Set NULL (NOT NULL)",
            "Exclude this value",
        ] {
            assert!(menu_entry(&app, cx, label).is_some(), "{engine:?} {label}");
        }
        click_menu_entry(&app, cx, "Filter by this value");
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(ids(&app, cx), ["2", "4"], "{engine:?}");
        // The condition is in the filter bar, where it can be changed.
        assert!(app.has(cx, ("filter-column", 0usize)), "{engine:?}");
        perform(&app, cx, GridAction::ClearFilters, 0, 2);
        app.wait_for_rows(cx, "Rows 1–4 of 4");
        assert!(!app.has(cx, ("filter-column", 0usize)), "{engine:?}");

        perform(&app, cx, GridAction::ExcludeValue, 1, 2);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(ids(&app, cx), ["1", "3"], "{engine:?}");
        perform(&app, cx, GridAction::ClearFilters, 0, 2);
        app.wait_for_rows(cx, "Rows 1–4 of 4");

        // A NULL cell: IS NULL, and IS NOT NULL.
        perform(&app, cx, GridAction::FilterByValue, 2, 3);
        app.wait_for_rows(cx, "Rows 1–1 of 1");
        assert_eq!(ids(&app, cx), ["3"], "{engine:?}");
        perform(&app, cx, GridAction::ClearFilters, 0, 2);
        app.wait_for_rows(cx, "Rows 1–4 of 4");
        perform(&app, cx, GridAction::ExcludeValue, 2, 3);
        app.wait_for_rows(cx, "Rows 1–3 of 3");
        assert_eq!(ids(&app, cx), ["1", "2", "4"], "{engine:?}");
        perform(&app, cx, GridAction::ClearFilters, 0, 2);
        app.wait_for_rows(cx, "Rows 1–4 of 4");

        // Filter ▸ > puts the condition in the bar without applying it.
        perform(&app, cx, GridAction::FilterWith(Operator::Greater), 1, 3);
        assert!(app.has(cx, ("filter-column", 0usize)), "{engine:?}");
        assert_eq!(app.data_state(cx), "Rows 1–4 of 4", "{engine:?}");
        app.click(cx, "filter-apply");
        app.wait_for_rows(cx, "Rows 1–1 of 1");
        assert_eq!(ids(&app, cx), ["4"], "{engine:?}");
        perform(&app, cx, GridAction::ClearFilters, 0, 2);
        app.wait_for_rows(cx, "Rows 1–4 of 4");

        // With a change pending, a filter is refused and the bar unchanged.
        app.type_in_cell(cx, 0, 2, "z", "enter");
        assert_eq!(app.pending(cx), 1, "{engine:?}");
        perform(&app, cx, GridAction::FilterByValue, 1, 2);
        cx.run_until_parked();
        assert!(!app.has(cx, ("filter-column", 0usize)), "{engine:?}");
        assert_eq!(
            (app.pending(cx), app.data_state(cx).as_str()),
            (1, "Rows 1–4 of 4"),
            "{engine:?}"
        );
        app.click(cx, "data-discard");
    }
}

#[gpui_kit::test]
fn the_menu_of_a_column_header_orders_filters_and_fits(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = filter_scratch(Engine::Postgres, "kerno_ui_header_menu");
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "kerno_ui_header_menu", "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–4 of 4");
    // A real right-click on the header of "qty".
    app.right_click(cx, ("column-header", 3usize), None);
    for label in [
        "Copy column name",
        "Order by this column",
        "Filter on this column…",
        "Pin column",
        "Hide column",
        "Fit width",
        "Fit all widths",
    ] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
    // Order ▸ Descending is the server's work on a table.
    click_menu_entry(&app, cx, "Order by this column");
    click_menu_entry(&app, cx, "Descending");
    app.wait_until(cx, "the sorted page", |app, cx| {
        let tab = app.data(cx);
        cx.update(|cx| tab.read(cx).sort() == Some(("qty", true)))
            && app.data_state(cx) == "Rows 1–4 of 4"
    });
    let qty: Vec<String> = app
        .data_rows(cx)
        .into_iter()
        .map(|row| row[2].clone())
        .collect();
    // PostgreSQL puts NULL first when descending.
    assert_eq!(qty, ["NULL", "3", "2", "1"]);

    // Filter on this column… adds a line on it, to be filled in.
    app.right_click(cx, ("column-header", 3usize), None);
    click_menu_entry(&app, cx, "Filter on this column…");
    assert!(app.has(cx, ("filter-column", 0usize)));
    app.type_text(cx, "2");
    app.press(cx, &["enter"]);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    assert_eq!(app.data_rows(cx)[0][2], "2");

    // A column is first drawn at the width it needs; widened by hand,
    // Fit width brings "name" (short values) back to it.
    let grid = app.data_grid(cx);
    let width = |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).col_width(2).unwrap());
    let default = width(cx);
    let header = app.step(cx, |window, _| window.find(("col-header", 2usize)).bounds());
    let from = gpui_kit::point(header.right() - px(2.), header.center().y);
    let to = gpui_kit::point(from.x + px(80.), from.y);
    app.step(cx, |window, cx| window.drag(from, to, cx));
    let widened = width(cx);
    assert!(widened > default + px(70.), "{default:?} → {widened:?}");
    app.right_click(cx, ("column-header", 2usize), None);
    click_menu_entry(&app, cx, "Fit width");
    let fitted = width(cx);
    assert!(
        (fitted - default).abs() <= px(1.) && fitted > px(40.),
        "{default:?} → {widened:?} → {fitted:?}"
    );

    // Copy column name and Hide column, from the header's own menu.
    app.right_click(cx, ("column-header", 2usize), None);
    click_menu_entry(&app, cx, "Copy column name");
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("name"));
    app.right_click(cx, ("column-header", 3usize), None);
    click_menu_entry(&app, cx, "Hide column");
    let shown = cx.update(|cx| grid.read(cx).delegate().shown_columns());
    assert_eq!(shown, ["id", "name"]);
}

#[gpui_kit::test]
fn pasting_fills_cells_from_the_clipboard_and_delete_empties_them(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_paste CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_paste",
            "CREATE TABLE kerno_ui_paste.items (id integer PRIMARY KEY, \
             name varchar(20) NOT NULL, qty integer, \
             twice integer GENERATED ALWAYS AS (qty * 2) STORED, note varchar(20))",
            "INSERT INTO kerno_ui_paste.items (id, name, qty, note) \
             VALUES (1, 'a', 1, 'x'), (2, 'b', 2, 'y'), (3, 'c', 3, 'z')",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "kerno_ui_paste", "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–3 of 3");
    let grid = app.data_grid(cx);
    // Selects from a cell to another and gives the grid the keys.
    let select =
        |app: &Fixture, cx: &mut TestAppContext, from: (usize, usize), to: (usize, usize)| {
            app.step(cx, |window, cx| {
                grid.update(cx, |table, cx| {
                    table.delegate_mut().clear_selection();
                    table.set_selected_cell(from.0, from.1, cx);
                    if to != from {
                        table.delegate_mut().extend_to(to.0, to.1);
                    }
                });
                grid.read(cx).focus_handle(cx).focus(window, cx);
            });
        };
    let paste = |app: &Fixture, cx: &mut TestAppContext, text: &str| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.to_string()));
        app.press(cx, &["secondary-v"]);
    };
    let row = |cells: [&str; 5]| cells.map(str::to_string).to_vec();

    // One value: in the selected cell, then in each of a range.
    select(&app, cx, (0, 5), (0, 5));
    paste(&app, cx, "q");
    assert_eq!(app.data_rows(cx)[0], row(["1", "a", "1", "2", "q"]));
    select(&app, cx, (1, 3), (2, 3));
    paste(&app, cx, "9\n");
    assert_eq!(app.data_rows(cx)[1][2], "9");
    assert_eq!(app.data_rows(cx)[2][2], "9");
    assert_eq!(app.pending(cx), 3);
    app.click(cx, "data-discard");

    // A block from the selected cell: the computed column is skipped, an
    // empty number is NULL, and nothing goes past the last row or column.
    select(&app, cx, (1, 3), (1, 3));
    paste(&app, cx, "10\t99\tn1\textra\n\t99\tn2\n30\t99\tn3\n");
    assert_eq!(
        app.data_rows(cx),
        [
            row(["1", "a", "1", "2", "x"]),
            row(["2", "b", "10", "4", "n1"]),
            row(["3", "c", "NULL", "6", "n2"]),
        ]
    );
    assert_eq!(app.pending(cx), 2);
    let tab = app.data(cx);
    let (_, statements) = cx.update(|cx| tab.read(cx).pending_sql(cx)).unwrap();
    assert_eq!(
        statements[0],
        "UPDATE \"kerno_ui_paste\".\"items\" SET \"qty\" = 10, \"note\" = 'n1' WHERE \"id\" = 2"
    );
    app.click(cx, "data-discard");

    // Delete sets a nullable cell to NULL; a NOT NULL one is left alone.
    select(&app, cx, (0, 2), (0, 3));
    app.press(cx, &["delete"]);
    assert_eq!(app.data_rows(cx)[0][..3], ["1", "a", "NULL"]);
    assert_eq!(app.pending(cx), 1);
    select(&app, cx, (1, 3), (1, 3));
    app.press(cx, &["backspace"]);
    assert_eq!(app.data_rows(cx)[1][2], "NULL");
    assert_eq!(app.pending(cx), 2);
    // The menu's Set NULL skips the NOT NULL cells of a range too.
    select(&app, cx, (2, 2), (2, 3));
    app.on_data(cx, |tab, window, cx| {
        tab.perform(crate::query::grid::GridAction::SetNull, 2, 2, window, cx)
    });
    assert_eq!(app.data_rows(cx)[2][..3], ["3", "c", "NULL"]);
    assert_eq!(app.pending(cx), 3);
    app.click(cx, "data-discard");
}

#[gpui_kit::test]
fn data_keys_edit_rows_and_apply(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_data_keys");
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &scratch_table(
                    engine,
                    "kerno_ui_data_keys.items",
                    "name VARCHAR(20) NOT NULL, qty INT, note VARCHAR(20) DEFAULT 'n/a'",
                ),
                "INSERT INTO kerno_ui_data_keys.items (name, qty, note) \
                 VALUES ('a', 1, 'x'), ('b', 2, 'y'), ('c', 3, 'z')",
            ],
        );
        let app = connected(cx, engine);
        app.open_table(cx, "kerno_ui_data_keys", "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–3 of 3");
        let grid = app.data_grid(cx);
        // Selects a cell (or a row) and gives the grid the keys. Columns:
        // the row number, id, name, qty, note.
        let select = |app: &Fixture, cx: &mut TestAppContext, row: usize, column: Option<usize>| {
            app.step(cx, |window, cx| {
                grid.update(cx, |table, cx| {
                    table.delegate_mut().clear_selection();
                    match column {
                        Some(column) => table.set_selected_cell(row, column, cx),
                        None => table.set_selected_row(row, cx),
                    }
                });
                grid.read(cx).focus_handle(cx).focus(window, cx);
            });
        };
        let statements = |app: &Fixture, cx: &mut TestAppContext| {
            let tab = app.data(cx);
            cx.update(|cx| tab.read(cx).pending_sql(cx)).unwrap().1
        };
        let verbs = |statements: &[String]| -> Vec<String> {
            statements
                .iter()
                .map(|statement| statement.split(' ').next().unwrap().to_string())
                .collect()
        };
        let row = |cells: [&str; 4]| cells.map(str::to_string).to_vec();

        // Ctrl+Alt+D: the selected cell takes the value of the row below.
        select(&app, cx, 0, Some(3));
        app.press(cx, &["ctrl-alt-d"]);
        assert_eq!(
            app.data_rows(cx)[0],
            row(["1", "a", "2", "x"]),
            "{engine:?}"
        );
        // Esc takes it back; with nothing to take back, Esc clears the
        // selection, as it did before; Cmd+Z undoes the revert.
        app.press(cx, &["escape"]);
        assert_eq!(
            (app.pending(cx), app.data_rows(cx)[0][2].as_str()),
            (0, "1")
        );
        assert!(cx.update(|cx| grid.read(cx).selected_cell()).is_some());
        app.press(cx, &["escape"]);
        assert!(cx.update(|cx| grid.read(cx).selected_cell()).is_none());
        app.press(cx, &["secondary-z"]);
        assert_eq!(
            (app.pending(cx), app.data_rows(cx)[0][2].as_str()),
            (1, "2")
        );

        // Alt+Insert adds a row; Ctrl+D fills it from the row above, its
        // generated key left to the server; Esc on one cell takes it back
        // to its default.
        select(&app, cx, 2, None);
        app.press(cx, &["alt-insert"]);
        assert_eq!(app.pending(cx), 2);
        assert_eq!(cx.update(|cx| grid.read(cx).selected_row()), Some(3));
        app.press(cx, &["ctrl-d"]);
        assert_eq!(app.data_rows(cx)[3][1..], ["c", "3", "z"]);
        select(&app, cx, 3, Some(4));
        app.press(cx, &["escape"]);
        assert_eq!(app.data_rows(cx)[3][1..], ["c", "3", "NULL"]);
        // The menu entry does what the key does.
        app.on_data(cx, |tab, window, cx| {
            tab.perform(
                crate::query::grid::GridAction::CopyFromRow(true),
                3,
                4,
                window,
                cx,
            )
        });
        assert_eq!(app.data_rows(cx)[3][1..], ["c", "3", "z"]);
        app.press(cx, &["escape"]);
        assert_eq!(app.data_rows(cx)[3][1..], ["c", "3", "NULL"]);

        // Alt+Delete marks a row deleted.
        select(&app, cx, 1, None);
        app.press(cx, &["alt-delete"]);
        assert_eq!(app.pending(cx), 3);

        // A typed value is one step of undo too.
        app.type_in_cell(cx, 2, 2, "cc", "enter");
        assert_eq!(
            (app.pending(cx), app.data_rows(cx)[2][1].as_str()),
            (4, "cc")
        );
        app.press(cx, &["secondary-z"]);
        assert_eq!(
            (app.pending(cx), app.data_rows(cx)[2][1].as_str()),
            (3, "c")
        );
        // With a cell open, Esc only closes it (the cell's pending value
        // stays), and Cmd+Z undoes typing, not a change.
        app.type_in_cell(cx, 0, 3, "5", "escape");
        assert_eq!(
            (app.pending(cx), app.data_rows(cx)[0][2].as_str()),
            (3, "2")
        );
        app.on_data(cx, |tab, window, cx| tab.edit_cell(2, 2, window, cx));
        app.type_text(cx, "zz");
        app.press(cx, &["secondary-z"]);
        let editing = |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).delegate().editing());
        assert_eq!(editing(cx), Some((2, 1)));
        app.press(cx, &["escape"]);
        assert_eq!(editing(cx), None);
        assert_eq!(app.pending(cx), 3);
        assert_eq!(app.data_rows(cx)[2][1], "c");

        // Cmd+S opens the review of the three statements; cancel keeps them.
        let written = statements(&app, cx);
        assert_eq!(
            verbs(&written),
            ["DELETE", "UPDATE", "INSERT"],
            "{engine:?}"
        );
        if engine == Engine::Postgres {
            assert_eq!(
                written[2],
                "INSERT INTO \"kerno_ui_data_keys\".\"items\" (\"name\", \"qty\") VALUES ('c', 3)"
            );
        }
        select(&app, cx, 0, Some(2));
        app.press(cx, &["secondary-s"]);
        // The review is built from these statements (`DataTab::review`).
        assert!(app.has(cx, "sql-preview"), "{engine:?}");
        app.click(cx, "sql-cancel");
        assert_eq!(app.pending(cx), 3);
        assert_eq!(
            on_server(engine, "SELECT count(*) FROM kerno_ui_data_keys.items"),
            "3"
        );

        // The Mac keys: Cmd+Shift+N adds, Cmd+Backspace deletes, Cmd+D and
        // Ctrl+Alt+Insert duplicate; Ctrl+Backspace sets the default.
        select(&app, cx, 2, None);
        app.press(cx, &["secondary-shift-n"]);
        assert_eq!(app.pending(cx), 4);
        app.press(cx, &["cmd-backspace"]);
        assert_eq!(app.pending(cx), 3);
        select(&app, cx, 2, None);
        app.press(cx, &["cmd-d"]);
        select(&app, cx, 2, None);
        app.press(cx, &["ctrl-alt-insert"]);
        assert_eq!(app.pending(cx), 5);
        select(&app, cx, 2, Some(4));
        app.press(cx, &["ctrl-backspace"]);
        assert!(
            statements(&app, cx)
                .iter()
                .any(|statement| statement.starts_with("UPDATE")
                    && statement.contains("= DEFAULT")),
            "{:?}",
            statements(&app, cx)
        );

        // Cmd+Shift+C copies with the column names.
        select(&app, cx, 0, Some(2));
        app.step(cx, |_, cx| {
            grid.update(cx, |table, _| table.delegate_mut().extend_to(1, 3))
        });
        app.press(cx, &["secondary-shift-c"]);
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref().and_then(|text| text.lines().next()),
            Some("name\tqty")
        );

        // Nothing pending: Cmd+S opens nothing.
        app.click(cx, "data-discard");
        select(&app, cx, 0, Some(2));
        app.press(cx, &["secondary-s"]);
        assert!(!app.has(cx, "sql-preview"));
    }

    // Cmd+Shift+C in a query result too.
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(cx, "select 1 as one, 2 as two", "secondary-enter");
    app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["1 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    app.step(cx, |window, cx| {
        grid.update(cx, |table, cx| table.set_selected_cell(0, 1, cx));
        grid.read(cx).focus_handle(cx).focus(window, cx);
    });
    app.press(cx, &["secondary-shift-c"]);
    let copied = cx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("one\n1\n"));
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
    config.environment = kerno_core::environment::PRODUCTION.into();
    // Production tabs start in manual commit; this test is about confirming.
    config.auto_commit = Some(true);
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
    // Production confirms every run (its environment's rule), but a read
    // is asked once: only writes ask twice.
    app.run_in_new_tab(
        cx,
        "select count(*) from kerno_ui_guard.items",
        "secondary-enter",
    );
    assert!(app.has(cx, "sql-preview") && !app.has(cx, "sql-production"));
    app.click(cx, "sql-confirm");
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

    // Its key (Cmd+Y, Ctrl+Alt+E on Linux): the history, newest first; the
    // search narrows it; a click opens the statement in a tab on the
    // connection it ran on.
    let history = actions::COMMANDS
        .iter()
        .find(|command| command.id == "show_history")
        .unwrap();
    app.press(cx, &[history.keys[0]]);
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
    app.press(cx, &["secondary-f"]);
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
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
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
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    }
}

const ENGINES: [Engine; 3] = [Engine::Postgres, Engine::MySql, Engine::MariaDb];

#[gpui_kit::test]
fn a_table_is_read_in_loads_sorted_and_filtered_by_the_server(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_pages");
        let day = match engine {
            Engine::Postgres => "DATE '2020-01-01' + n",
            Engine::MySql | Engine::MariaDb => "DATE_ADD('2020-01-01', INTERVAL n DAY)",
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
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

        // Loads, added under the rows there (K36).
        app.click(cx, "data-fetch-more");
        app.wait_for_rows(cx, "Rows 1–200 of 250");
        assert_eq!(app.data_rows(cx)[100][..2], ["101", "item 101"]);
        assert_eq!(app.data_rows(cx)[0][0], "1");
        // K21: the way on says what the last load reads.
        assert_eq!(app.label_of(cx, "data-fetch-more"), "Fetch 50 more");
        app.click(cx, "data-fetch-more");
        app.wait_for_rows(cx, "Rows 1–250 of 250");
        // The last load of a counted table ends the way on.
        assert_eq!(app.label_of(cx, "data-fetch-more"), "All rows fetched");
        assert!(!app.has(cx, "data-fetch-all"));
        app.on_data(cx, |tab, window, cx| tab.fetch_more(false, window, cx));
        cx.run_until_parked();
        assert_eq!(app.data_state(cx), "Rows 1–250 of 250");
        assert_eq!(app.data_rows(cx).len(), 250);

        // A click on a header sorts on the server, from the first load.
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
        &["DROP SCHEMA IF EXISTS kerno_ui_keyless CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_keyless",
            "CREATE TABLE kerno_ui_keyless.keyless (name text, body text)",
            "INSERT INTO kerno_ui_keyless.keyless VALUES ('a', repeat('long ', 100))",
            "CREATE TABLE kerno_ui_keyless.keyed (id int PRIMARY KEY, doubled int GENERATED ALWAYS AS (id * 2) STORED)",
            "INSERT INTO kerno_ui_keyless.keyed VALUES (1)",
            "CREATE VIEW kerno_ui_keyless.names AS SELECT name FROM kerno_ui_keyless.keyless",
        ],
    );
    let reason = |app: &Fixture, cx: &mut TestAppContext| {
        let tab = app.data(cx);
        cx.update(|cx| tab.read(cx).read_only_reason(cx))
    };
    let app = connected(cx, Engine::Postgres);
    // No key: a row cannot be told from another.
    app.open_table(cx, "kerno_ui_keyless", "keyless", RelationKind::Table);
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
    app.open_table(cx, "kerno_ui_keyless", "names", RelationKind::View);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    assert!(reason(&app, cx).unwrap().contains("view"));
    // A table with a key takes changes, except in its computed column.
    app.open_table(cx, "kerno_ui_keyless", "keyed", RelationKind::Table);
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
    app.open_table(cx, "kerno_ui_keyless", "keyed", RelationKind::Table);
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
            ["Open kerno_ui_fk.authors where id = 1"]
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
    app.wait_until(cx, "the slow page on the server", |_, _| {
        running_on_server(Engine::Postgres, "pg_sleep(30) IS NOT NULL")
    });
    assert_eq!(app.data_state(cx), "loading");
    assert!(app.status_bar(cx).contains(&"running…".to_string()));
    app.press(cx, &["secondary-."]);
    // Said as a cancel, not as a failure of the filter (C3).
    app.wait_until(cx, "the cancellation", |app, cx| {
        app.data_state(cx) == "cancelled"
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
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
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
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
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
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
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
        app.on_structure(cx, |tab, _, cx| tab.select(1, cx));
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
        app.on_structure(cx, |tab, _, cx| tab.select(2, cx));
        assert!(app.has(cx, ("structure-empty", 1usize)), "{engine:?}");
        app.on_structure(cx, |tab, _, cx| tab.select(4, cx));
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
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
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
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
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
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
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
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
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
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
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
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
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
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
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
    config.environment = kerno_core::environment::PRODUCTION.into();
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

    assert!(!app.shows_label(cx, "Execute"));
    app.right_click(cx, "query-editor", on_text);
    for label in [
        "Run statement (not connected)",
        "Run script (not connected)",
        "Explain plan (not connected)",
        "Execute",
        "Undo",
        "Redo",
        "Cut (no selection)",
        "Copy (no selection)",
        "Copy statement",
        "Paste",
        "Select all",
        "Format",
        "Find…",
        "Replace…",
        "File",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    // DBeaver's submenus (G15): Execute ▸, Format ▸, File ▸.
    click_menu_entry(&app, cx, "Execute");
    assert!(app.shows_label(cx, "Explain analyze (not connected)"));
    click_menu_entry(&app, cx, "Format");
    for label in [
        "Format SQL",
        "Upper case (no selection)",
        "Toggle block comment",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    click_menu_entry(&app, cx, "File");
    for label in [
        "Save",
        "Save as…",
        "Save as query…",
        "Revert to saved file (no file)",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.press(cx, &["escape"]);
    app.right_click(cx, "query-editor", on_text);
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
    click_menu_entry(&app, cx, "Format");
    app.click_label(cx, "Toggle line comment");
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
    assert!(app.status_says(cx, "transaction open"));
    assert_eq!(open_on_server(), "1");

    // The status bar's items answer a right-click, and a click.
    app.right_click(cx, "status-connection", None);
    for label in ["Disconnect", "Reconnect", "Edit connection…"] {
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
    // From the palette, a disconnection that would end the transaction
    // asks first; Cancel keeps it.
    app.dispatch(cx, actions::DisconnectTabConnection);
    assert!(app.has(cx, "end-question"));
    app.click(cx, "end-cancel");
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
    assert!(!app.status_says(cx, "transaction open"));
}

// Navigator tree completeness (G10) ------------------------------------------

impl Fixture {
    /// Runs the whole text of the active query tab and waits until every
    /// statement is done.
    fn run_active_query(&self, cx: &mut TestAppContext) {
        let tab = self.tab(cx);
        self.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| {
                tab.run(crate::query::tab::Scope::All, window, cx)
            })
        });
        self.wait_until(cx, "the run", |app, cx| {
            !app.running(cx) && !app.outcomes(cx).is_empty()
        });
        let outcomes = self.outcomes(cx);
        assert!(
            outcomes.iter().all(|outcome| outcome.starts_with("done")),
            "{outcomes:?} running\n{}",
            self.active_query(cx).1
        );
    }

    /// The labels of the rows just below the row at `path`, which is open.
    fn children_of(&self, cx: &mut TestAppContext, path: &[&str]) -> Vec<String> {
        let tree = self.tree(cx);
        // Rows are counted from the top of the sidebar, the outline from
        // the connection's first row.
        let first = self.tree_row(cx, &[tree[0].as_str()]);
        let at = self.tree_row(cx, path) - first;
        let indent = "  ".repeat(path.len());
        tree[at + 1..]
            .iter()
            .take_while(|line| line.starts_with(&indent))
            .filter(|line| !line[indent.len()..].starts_with(' '))
            .map(|line| line.trim().to_string())
            .collect()
    }
}

#[gpui_kit::test]
fn a_table_lists_its_constraints_keys_references_and_triggers(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let pg = engine == Engine::Postgres;
        let server = |sql: &str| on_server(engine, sql);
        // The keys of the fixture, named as the server named them.
        let (key, reference, primary) = if pg {
            (
                server(
                    "SELECT conname FROM pg_constraint \
                     WHERE conrelid = 'shop.orders'::regclass AND contype = 'f'",
                ),
                server(
                    "SELECT conname FROM pg_constraint \
                     WHERE conrelid = 'shop.order_items'::regclass \
                     AND confrelid = 'shop.orders'::regclass",
                ),
                "orders_pkey",
            )
        } else {
            (
                "orders_customer_fk".to_string(),
                "order_items_order_fk".to_string(),
                "PRIMARY",
            )
        };
        let root: &[&str] = if pg { &["kerno", "shop"] } else { &["kerno"] };
        let at = |rest: &[&str]| -> Vec<String> {
            root.iter()
                .chain(rest)
                .map(|part| part.to_string())
                .collect()
        };
        fn path(parts: &[String]) -> Vec<&str> {
            parts.iter().map(String::as_str).collect()
        }
        let app = connected(cx, engine);
        let orders = at(&["Tables", "orders"]);
        app.open_path(cx, &path(&at(&["Tables", "orders", "Triggers"])));

        // DBeaver's folders, in its order for the engine, from one load.
        let folders = if pg {
            [
                "Columns",
                "Constraints",
                "Foreign keys",
                "Indexes",
                "References",
                "Triggers",
            ]
        } else {
            [
                "Columns",
                "Constraints",
                "Foreign keys",
                "References",
                "Triggers",
                "Indexes",
            ]
        };
        assert_eq!(app.children_of(cx, &path(&orders)), folders, "{engine:?}");
        for folder in ["Constraints", "Foreign keys", "References", "Triggers"] {
            app.toggle(cx, &path(&at(&["Tables", "orders", folder])));
        }
        for (folder, name) in [
            ("Constraints", primary),
            ("Foreign keys", key.as_str()),
            ("References", reference.as_str()),
            ("Triggers", "orders_log_status"),
        ] {
            assert!(
                app.shows(cx, &path(&at(&["Tables", "orders", folder, name]))),
                "{engine:?}: {name} in {folder}: {:#?}",
                app.tree(cx)
            );
        }

        // A double click on a foreign key opens the referenced table.
        let schema = if pg { "shop" } else { "kerno" };
        app.click_row(
            cx,
            &path(&at(&["Tables", "orders", "Foreign keys", &key])),
            true,
        );
        app.wait_for_structure(cx);
        let opened = cx.update(|cx| {
            app.workspace
                .read(cx)
                .active_structure_tab()
                .map(|tab| tab.read(cx).table().clone())
        });
        assert_eq!(
            opened,
            Some(TableRef::new(schema, "customers")),
            "{engine:?}"
        );

        // A reference selects the table it comes from.
        app.act(
            cx,
            Action::RevealTable,
            &path(&at(&["Tables", "orders", "References", &reference])),
        );
        let order_items = at(&["Tables", "order_items"]);
        assert_eq!(
            app.selected_row(cx),
            Some(app.tree_row(cx, &path(&order_items))),
            "{engine:?}"
        );

        // Revealing a table of a schema never opened loads the way to it.
        let (other, other_root): (&str, &[&str]) = if pg {
            ("analytics", &["kerno", "analytics"])
        } else {
            ("kerno_analytics", &["kerno_analytics"])
        };
        let table = server(&format!(
            "SELECT min(table_name) FROM information_schema.tables \
             WHERE table_schema = '{other}' AND table_type = 'BASE TABLE'"
        ));
        assert!(!app.shows(cx, &[other_root, &["Tables"]].concat()));
        let navigator = cx.update(|cx| app.workspace.read(cx).navigator(cx));
        let connection = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
        let target = Target {
            connection,
            database: None,
            schema: None,
        };
        let revealed = TableRef::new(other, table.clone());
        app.step(cx, |_, cx| {
            navigator.update(cx, |navigator, cx| {
                navigator.reveal(&target, &revealed, RelationKind::Table, cx)
            })
        });
        let revealed_path = [other_root, &["Tables", table.as_str()]].concat();
        app.wait_until(cx, "the revealed table", |app, cx| {
            app.shows(cx, &revealed_path)
                && app.selected_row(cx) == Some(app.tree_row(cx, &revealed_path))
        });
    }
}

#[gpui_kit::test]
fn columns_are_added_renamed_and_dropped_from_the_tree(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let pg = engine == Engine::Postgres;
        let (cleanup, create) = if pg {
            (
                "DROP SCHEMA IF EXISTS kerno_ui_g10_columns CASCADE",
                "CREATE SCHEMA kerno_ui_g10_columns",
            )
        } else {
            (
                "DROP DATABASE IF EXISTS kerno_ui_g10_columns",
                "CREATE DATABASE kerno_ui_g10_columns",
            )
        };
        let _scratch = Scratch::new(
            engine,
            &[cleanup],
            &[
                create,
                "CREATE TABLE kerno_ui_g10_columns.t (id int PRIMARY KEY, old_name int)",
                "CREATE VIEW kerno_ui_g10_columns.v AS SELECT id FROM kerno_ui_g10_columns.t",
            ],
        );
        let has_column = |name: &str| {
            on_server(
                engine,
                &format!(
                    "SELECT count(*) FROM information_schema.columns \
                     WHERE table_schema = 'kerno_ui_g10_columns' AND table_name = 't' \
                     AND column_name = '{name}'"
                ),
            ) == "1"
        };
        let root: &[&str] = if pg {
            &["kerno", "kerno_ui_g10_columns"]
        } else {
            &["kerno_ui_g10_columns"]
        };
        let at = |rest: &[&'static str]| [root, rest].concat();
        let column = |name: &'static str| at(&["Tables", "t", "Columns", name]);
        let app = connected(cx, engine);
        app.open_path(cx, &column("old_name"));

        // A view's columns come from its query: they are not changed here.
        app.open_path(cx, &at(&["Views", "v", "Columns", "id"]));
        for action in [Action::RenameColumn, Action::DropColumn, Action::AddColumn] {
            assert_eq!(
                app.why_not(cx, action, &at(&["Views", "v", "Columns", "id"])),
                Some("a view's columns come from its query"),
                "{engine:?}"
            );
        }

        // Rename: the new name is typed, the table is read again.
        app.act(cx, Action::RenameColumn, &column("old_name"));
        assert!(app.has(cx, "sql-name"), "{engine:?}");
        app.press(cx, &["secondary-a"]);
        app.type_text(cx, "new_name");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the rename", |app, cx| {
            app.shows(cx, &column("new_name"))
        });
        assert!(
            has_column("new_name") && !has_column("old_name"),
            "{engine:?}"
        );

        // Drop: the statement is shown, then run.
        app.act(cx, Action::DropColumn, &column("new_name"));
        assert!(app.has(cx, "sql-preview"), "{engine:?}");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the drop", |app, cx| {
            !app.shows(cx, &column("new_name")) && app.shows(cx, &column("id"))
        });
        assert!(!has_column("new_name"), "{engine:?}");

        // Add (CM-73): the table's form opens with a new column, the cursor
        // in its name; nothing runs until the form is applied.
        app.act(cx, Action::AddColumn, &at(&["Tables", "t", "Columns"]));
        app.wait_until(cx, "the new column", |app, cx| {
            form_columns(app, cx) == Some((vec!["id".into(), String::new()], Some(1)))
        });

        // F4 on a column (CM-72) opens that form on the column.
        app.click_row(cx, &column("id"), false);
        app.press(cx, &["f4"]);
        app.wait_until(cx, "the column's name", |app, cx| {
            form_columns(app, cx).is_some_and(|(_, focused)| focused == Some(0))
        });
        let opened = cx.update(|cx| {
            app.workspace
                .read(cx)
                .active_table_editor()
                .and_then(|tab| tab.read(cx).table().cloned())
        });
        assert_eq!(
            opened,
            Some(TableRef::new("kerno_ui_g10_columns", "t")),
            "{engine:?}"
        );
        // A view's column has no form: F4 shows the view's structure.
        app.click_row(cx, &at(&["Views", "v", "Columns", "id"]), false);
        app.press(cx, &["f4"]);
        app.wait_for_structure(cx);
        let opened = cx.update(|cx| {
            app.workspace
                .read(cx)
                .active_structure_tab()
                .map(|tab| tab.read(cx).table().clone())
        });
        assert_eq!(
            opened,
            Some(TableRef::new("kerno_ui_g10_columns", "v")),
            "{engine:?}"
        );
    }
}

#[gpui_kit::test]
fn create_entries_open_templates_that_run_once_named(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let pg = engine == Engine::Postgres;
        let (cleanup, create) = if pg {
            (
                "DROP SCHEMA IF EXISTS kerno_ui_g10_create CASCADE",
                "CREATE SCHEMA kerno_ui_g10_create",
            )
        } else {
            (
                "DROP DATABASE IF EXISTS kerno_ui_g10_create",
                "CREATE DATABASE kerno_ui_g10_create",
            )
        };
        let _scratch = Scratch::new(
            engine,
            &[cleanup],
            &[create, "CREATE TABLE kerno_ui_g10_create.t (id int)"],
        );
        let root: &[&str] = if pg {
            &["kerno", "kerno_ui_g10_create"]
        } else {
            &["kerno_ui_g10_create"]
        };
        let view = (
            "Views",
            Action::CreateView,
            "SELECT count(*) FROM information_schema.views \
             WHERE table_schema = 'kerno_ui_g10_create' AND table_name = 'new_view'"
                .to_string(),
        );
        // Folder, entry, and how the server counts what it made.
        let made: Vec<(&str, Action, String)> = if pg {
            let proc = |name: &str| {
                format!(
                    "SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                     WHERE n.nspname = 'kerno_ui_g10_create' AND p.proname = '{name}'"
                )
            };
            vec![
                view,
                ("Functions", Action::CreateFunction, proc("new_function")),
                ("Procedures", Action::CreateProcedure, proc("new_procedure")),
                (
                    "Sequences",
                    Action::CreateSequence,
                    "SELECT count(*) FROM information_schema.sequences \
                     WHERE sequence_schema = 'kerno_ui_g10_create' \
                     AND sequence_name = 'new_sequence'"
                        .into(),
                ),
                (
                    "Triggers",
                    Action::CreateTrigger,
                    "SELECT count(*) FROM pg_trigger \
                     WHERE tgrelid = 'kerno_ui_g10_create.t'::regclass AND tgname = 'new_trigger'"
                        .into(),
                ),
            ]
        } else {
            let routine = |name: &str| {
                format!(
                    "SELECT count(*) FROM information_schema.routines \
                     WHERE routine_schema = 'kerno_ui_g10_create' AND routine_name = '{name}'"
                )
            };
            vec![
                view,
                ("Routines", Action::CreateFunction, routine("new_function")),
                (
                    "Routines",
                    Action::CreateProcedure,
                    routine("new_procedure"),
                ),
                (
                    "Triggers",
                    Action::CreateTrigger,
                    "SELECT count(*) FROM information_schema.triggers \
                     WHERE trigger_schema = 'kerno_ui_g10_create' AND trigger_name = 'new_trigger'"
                        .into(),
                ),
                (
                    "Events",
                    Action::CreateEvent,
                    "SELECT count(*) FROM information_schema.events \
                     WHERE event_schema = 'kerno_ui_g10_create' AND event_name = 'new_event'"
                        .into(),
                ),
            ]
        };
        let app = connected(cx, engine);
        app.open_path(cx, &[root, &["Tables"]].concat());
        for (index, (folder, action, count)) in made.into_iter().enumerate() {
            app.act(cx, action, &[root, &[folder]].concat());
            app.wait_until(cx, "the template", |app, cx| {
                app.titles(cx).len() == index + 1
            });
            assert_eq!(on_server(engine, &count), "0", "{engine:?} {action:?}");
            // The view's template is in the schema of its folder.
            if action == Action::CreateView {
                let sql = app.active_query(cx).1;
                assert!(
                    sql.starts_with("CREATE VIEW") && sql.contains("kerno_ui_g10_create"),
                    "{engine:?}: {sql}"
                );
            }
            // A trigger made from the schema's folder names its table.
            if app.active_query(cx).1.contains("table_name") {
                app.edit_query(cx, "table_name", "t");
            }
            app.run_active_query(cx);
            assert_eq!(on_server(engine, &count), "1", "{engine:?} {action:?}");
        }
    }
}

#[gpui_kit::test]
fn a_drop_with_cascade_also_drops_what_depends_on_the_table(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let pg = |sql: &str| on_server(Engine::Postgres, sql);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_g10_cascade CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_g10_cascade",
            "CREATE TABLE kerno_ui_g10_cascade.t (id int)",
            "CREATE VIEW kerno_ui_g10_cascade.v AS SELECT id FROM kerno_ui_g10_cascade.t",
            "CREATE TABLE kerno_ui_g10_cascade.marker (id int)",
        ],
    );
    let count = |kind: &str, name: &str| {
        pg(&format!(
            "SELECT count(*) FROM information_schema.{kind} \
             WHERE table_schema = 'kerno_ui_g10_cascade' AND table_name = '{name}'"
        ))
    };
    let app = connected(cx, Engine::Postgres);
    let table = |name: &'static str| ["kerno", "kerno_ui_g10_cascade", "Tables", name];
    app.open_path(cx, &table("t"));

    // A plain drop fails: the view depends on the table.
    app.act(cx, Action::Drop, &table("t"));
    app.click(cx, "sql-confirm");
    // The navigator's statements run one after the other on its session:
    // once a rename after it is seen, the drop has been tried.
    app.act(cx, Action::Rename, &table("marker"));
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "marker_seen");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the rename", |app, cx| {
        app.shows(cx, &table("marker_seen"))
    });
    assert_eq!(
        (count("tables", "t"), count("views", "v")),
        ("1".into(), "1".into())
    );

    // With cascade, the table and the view go. The button waits for the list
    // of what the cascade drops (C11).
    app.act(cx, Action::DropCascade, &table("t"));
    assert!(app.has(cx, "sql-preview"));
    app.wait_until(cx, "the list of what the cascade drops", |app, cx| {
        app.has(cx, "sql-reach")
            && app
                .label_of(cx, "sql-reach")
                .contains("view kerno_ui_g10_cascade.v")
    });
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the drop", |app, cx| !app.shows(cx, &table("t")));
    assert_eq!(
        (count("tables", "t"), count("views", "v")),
        ("0".into(), "0".into())
    );
}

// Server administration (docs/plans/M8.md Part 4) -----------------------------------

use crate::admin::tab::{AdminTab, Pane, ROLES, SETTINGS, SIZES};

/// A session of the test's own on a test server, busy with a slow
/// statement marked `marker`; terminated when dropped if still running.
struct Sleeper {
    engine: Engine,
    id: String,
    done: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl Sleeper {
    fn start(engine: Engine, marker: &str) -> Self {
        use kerno_core::Driver as _;
        let (own_id, sql) = match engine {
            Engine::Postgres => (
                "SELECT pg_backend_pid()",
                format!("SELECT '{marker}', pg_sleep(60)"),
            ),
            // Interrupted, SLEEP() fails the query it is part of (alone in
            // it, it would return 1 instead).
            Engine::MySql | Engine::MariaDb => (
                "SELECT CONNECTION_ID()",
                format!("SELECT '{marker}', COUNT(*) FROM kerno.orders WHERE SLEEP(0.02) = 0"),
            ),
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let done = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let params = testing::params(engine);
                let session = match engine {
                    Engine::Postgres => kerno_postgres::PostgresDriver.connect(&params).await,
                    Engine::MySql | Engine::MariaDb => {
                        kerno_mysql::MysqlDriver.connect(&params).await
                    }
                    kerno_core::Engine::Sqlite => unreachable!(
                        "SQLite has no test server: these tests run on the server engines"
                    ),
                }
                .expect("the test server");
                let id = kerno_core::exec::collect(session.clone(), own_id)
                    .await
                    .unwrap()
                    .results[0]
                    .rows()[0][0]
                    .clone()
                    .unwrap();
                sender.send(id).unwrap();
                let outcome = kerno_core::exec::collect(session.clone(), sql)
                    .await
                    .map(drop)
                    .map_err(|error| error.to_string());
                session.close().await;
                outcome
            })
        });
        let id = receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("the sleeper's session");
        Self {
            engine,
            id,
            done: Some(done),
        }
    }

    /// How its statement ended, once it has.
    fn outcome(&mut self) -> Option<Result<(), String>> {
        if !self.done.as_ref()?.is_finished() {
            return None;
        }
        self.done
            .take()
            .map(|done| done.join().expect("the sleeper"))
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        if self.done.as_ref().is_some_and(|done| !done.is_finished()) {
            // This test's own session, and no other.
            let sql = match self.engine {
                Engine::Postgres => format!("SELECT pg_terminate_backend({})", self.id),
                Engine::MySql | Engine::MariaDb => format!("KILL CONNECTION {}", self.id),
                kerno_core::Engine::Sqlite => {
                    unreachable!("SQLite has no test server: these tests run on the server engines")
                }
            };
            let engine = self.engine;
            // A failure here must not abort a test that is already failing.
            let _ = std::thread::spawn(move || on_server(engine, &sql)).join();
        }
    }
}

impl Fixture {
    /// The active tab, which administers a server.
    fn admin(&self, cx: &mut TestAppContext) -> Entity<AdminTab> {
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .active_admin_tab()
                .expect("an administration tab")
                .clone()
        })
    }

    /// Does something to the active administration tab.
    fn on_admin(
        &self,
        cx: &mut TestAppContext,
        act: impl FnOnce(&mut AdminTab, &mut gpui_kit::Window, &mut gpui_kit::Context<AdminTab>),
    ) {
        let tab = self.admin(cx);
        self.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| act(tab, window, cx))
        });
    }

    /// Waits for a list of the administration tab to be read.
    fn wait_admin(&self, cx: &mut TestAppContext, pane: Pane) {
        let tab = self.admin(cx);
        self.wait_until(cx, "the list", |_, cx| {
            let state = cx.update(|cx| tab.read(cx).state(pane));
            assert!(!state.starts_with("failed"), "{pane:?}: {state}");
            state == "ready"
        });
    }

    /// The headers and rows a list of the administration tab shows.
    fn admin_shown(&self, cx: &mut TestAppContext, pane: Pane) -> (Vec<String>, Vec<Vec<String>>) {
        let tab = self.admin(cx);
        cx.update(|cx| tab.read(cx).shown(pane, cx))
    }

    /// Reads the sub-tab on screen again until its rows of `pane` satisfy
    /// `done`; gives them.
    fn admin_rows_until(
        &self,
        cx: &mut TestAppContext,
        pane: Pane,
        what: &str,
        done: impl Fn(&[Vec<String>]) -> bool,
    ) -> Vec<Vec<String>> {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            self.wait_admin(cx, pane);
            let (_, rows) = self.admin_shown(cx, pane);
            if done(&rows) {
                return rows;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}: {rows:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
            self.on_admin(cx, |tab, window, cx| tab.refresh(window, cx));
        }
    }

    /// Types `text` in the filter box of the sub-tab, in place of what it
    /// holds.
    fn admin_filter(&self, cx: &mut TestAppContext, text: &str) {
        self.click(cx, "admin-filter");
        self.press(cx, &["secondary-a", "backspace"]);
        self.type_text(cx, text);
    }

    /// Why the selected session cannot be signalled, when it cannot.
    fn admin_why_not(&self, cx: &mut TestAppContext) -> Option<&'static str> {
        let tab = self.admin(cx);
        cx.update(|cx| tab.read(cx).why_not_signal(cx))
    }

    fn admin_asked(&self, cx: &mut TestAppContext) -> Option<String> {
        let tab = self.admin(cx);
        cx.update(|cx| tab.read(cx).asked().map(str::to_string))
    }

    fn admin_message(&self, cx: &mut TestAppContext) -> Option<(bool, String)> {
        let tab = self.admin(cx);
        cx.update(|cx| tab.read(cx).message().cloned())
    }
}

/// Waits for a sleeper's statement to end; gives how.
fn sleeper_ended(
    app: &Fixture,
    cx: &mut TestAppContext,
    sleeper: &mut Sleeper,
) -> Result<(), String> {
    let mut ended = None;
    app.wait_until(cx, "the end of the slow statement", |_, _| {
        ended = sleeper.outcome();
        ended.is_some()
    });
    ended.unwrap()
}

#[gpui_kit::test]
fn the_sessions_list_cancels_and_terminates_what_the_test_opened(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
        // From the connection's menu in the sidebar.
        app.sidebar_does(
            cx,
            Subject::Connection(id),
            SidebarAction::Tool(crate::navigator::actions::ToolEntry::Dashboard),
        );
        assert_eq!(
            app.titles(cx),
            [format!("test {} · administration", engine.label())]
        );
        app.wait_admin(cx, Pane::Sessions);
        assert!(app.has(cx, "admin-version"), "{engine:?}");

        // The tab's own session is marked, and nothing is offered on it.
        let (headers, rows) = app.admin_shown(cx, Pane::Sessions);
        assert!(headers.contains(&"Query".to_string()), "{headers:?}");
        assert!(
            !headers.contains(&"This session".to_string()),
            "{headers:?}"
        );
        let own = rows
            .iter()
            .position(|row| row[0].ends_with(" · this tab"))
            .unwrap_or_else(|| panic!("{engine:?}: no own session in {rows:?}"));
        app.click(cx, ("admin-sessions-cell", own << 12));
        assert_eq!(app.admin_why_not(cx), Some("this tab's session"));

        let mut cancelled = Sleeper::start(engine, "kerno_ui_admin_cancel");
        let mut terminated = Sleeper::start(engine, "kerno_ui_admin_terminate");

        // Cancel query, from the toolbar, on the row selected.
        app.admin_filter(cx, "kerno_ui_admin_cancel");
        app.admin_rows_until(cx, Pane::Sessions, "the session to cancel", |rows| {
            rows.len() == 1 && rows[0][0] == cancelled.id
        });
        app.click(cx, ("admin-sessions-cell", 0usize));
        assert_eq!(app.admin_why_not(cx), None);
        app.click(cx, "admin-cancel");
        assert!(app.has(cx, "sql-preview"), "{engine:?}");
        let expected = match engine {
            Engine::Postgres => format!("SELECT pg_catalog.pg_cancel_backend({})", cancelled.id),
            Engine::MySql | Engine::MariaDb => format!("KILL QUERY {}", cancelled.id),
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
        };
        assert_eq!(app.admin_asked(cx), Some(expected));
        app.click(cx, "sql-confirm");
        let error = sleeper_ended(&app, cx, &mut cancelled).unwrap_err();
        let lower = error.to_lowercase();
        assert!(
            lower.contains("cancel") || lower.contains("interrupted"),
            "{engine:?}: {error}"
        );
        app.wait_until(cx, "the outcome", |app, cx| {
            app.admin_message(cx)
                == Some((
                    false,
                    format!("Cancelled the statement of session {}.", cancelled.id),
                ))
        });

        // Terminate session, from the menu of its row, opened with a real
        // right-click.
        app.admin_filter(cx, "kerno_ui_admin_terminate");
        app.admin_rows_until(cx, Pane::Sessions, "the session to terminate", |rows| {
            rows.len() == 1 && rows[0][0] == terminated.id
        });
        app.right_click(cx, ("admin-sessions-cell", 0usize), None);
        assert!(menu_entry(&app, cx, "Copy").is_some(), "{engine:?}");
        assert!(menu_entry(&app, cx, "Cancel query").is_some(), "{engine:?}");
        click_menu_entry(&app, cx, "Terminate session");
        assert!(app.has(cx, "sql-preview"), "{engine:?}");
        let expected = match engine {
            Engine::Postgres => {
                format!("SELECT pg_catalog.pg_terminate_backend({})", terminated.id)
            }
            Engine::MySql | Engine::MariaDb => format!("KILL CONNECTION {}", terminated.id),
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
        };
        assert_eq!(app.admin_asked(cx), Some(expected));
        app.click(cx, "sql-confirm");
        assert!(
            sleeper_ended(&app, cx, &mut terminated).is_err(),
            "{engine:?}"
        );
        app.wait_until(cx, "the outcome", |app, cx| {
            app.admin_message(cx) == Some((false, format!("Terminated session {}.", terminated.id)))
        });
        // Gone from the server, gone from the list.
        app.admin_rows_until(cx, Pane::Sessions, "the session to go", |rows| {
            rows.is_empty()
        });
        assert!(app.has(cx, ("admin-empty", Pane::Sessions as usize)));
    }
}

#[gpui_kit::test]
fn roles_and_their_grants_sizes_and_settings_are_listed(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        // From the palette, on the connection selected in the sidebar.
        app.dispatch(cx, actions::OpenAdmin);
        app.wait_admin(cx, Pane::Sessions);

        // Roles; the grants of the one selected below them.
        app.on_admin(cx, |tab, window, cx| tab.select(ROLES, window, cx));
        app.wait_admin(cx, Pane::Roles);
        app.wait_admin(cx, Pane::Grants);
        let (_, roles) = app.admin_shown(cx, Pane::Roles);
        assert!(roles.iter().any(|role| role[0] == "kerno"), "{roles:?}");
        assert!(app.has(cx, ("admin-empty", Pane::Grants as usize)));
        let readonly = roles
            .iter()
            .position(|role| role[0] == "kerno_readonly")
            .unwrap();
        app.click(cx, ("admin-roles-cell", readonly << 12));
        let (headers, grants) = app.admin_shown(cx, Pane::Grants);
        let column = |name: &str| headers.iter().position(|header| header == name).unwrap();
        let grantee = match engine {
            Engine::Postgres => "kerno_readonly",
            Engine::MySql | Engine::MariaDb => "'kerno_readonly'@'%'",
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
        };
        assert!(grants.iter().all(|grant| grant[0] == grantee), "{grants:?}");
        assert!(
            grants
                .iter()
                .any(|grant| grant[column("Object")] == "audit_log"
                    && grant[column("Privileges")] == "INSERT, SELECT"),
            "{engine:?}: {grants:?}"
        );

        // Sizes, for people, largest first.
        app.on_admin(cx, |tab, window, cx| tab.select(SIZES, window, cx));
        app.wait_admin(cx, Pane::Databases);
        app.wait_admin(cx, Pane::Tables);
        let (headers, databases) = app.admin_shown(cx, Pane::Databases);
        let size = headers.iter().position(|header| header == "Size").unwrap();
        let kerno = databases.iter().find(|row| row[0] == "kerno").unwrap();
        assert!(
            ["B", "kB", "MB", "GB"]
                .iter()
                .any(|unit| kerno[size].ends_with(&format!(" {unit}"))),
            "{kerno:?}"
        );
        let (headers, tables) = app.admin_shown(cx, Pane::Tables);
        let total = headers
            .iter()
            .position(|header| header == "Total size")
            .unwrap();
        // Other tests' scratch tables come and go meanwhile; the fixture's
        // biggest table comes before the rest of the fixture.
        let fixture: Vec<_> = tables
            .iter()
            .filter(|row| !row[0].starts_with("kerno_ui_") && !row[0].starts_with("kerno_test_"))
            .collect();
        assert_eq!(fixture[0][1], "big_table", "{engine:?}: {tables:?}");
        assert!(fixture[0][total].ends_with(" MB"), "{:?}", fixture[0]);

        // Settings, filtered.
        app.on_admin(cx, |tab, window, cx| tab.select(SETTINGS, window, cx));
        app.wait_admin(cx, Pane::Settings);
        app.admin_filter(cx, "max_connections");
        let (_, settings) = app.admin_shown(cx, Pane::Settings);
        assert!(
            settings.iter().all(|row| row
                .iter()
                .any(|cell| cell.to_lowercase().contains("max_connections"))),
            "{settings:?}"
        );
        let row = settings
            .iter()
            .position(|row| row[0] == "max_connections")
            .unwrap();
        // Cmd+C copies the selected cell.
        app.click(cx, ("admin-settings-cell", (row << 12) | 1));
        app.press(cx, &["secondary-c"]);
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some(settings[row][1].as_str()));
        assert_eq!(settings[row][1], "100", "{engine:?}");
    }
}

#[gpui_kit::test]
fn the_sessions_refresh_on_their_own_only_while_shown(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    app.sidebar_does(
        cx,
        Subject::Connection(id),
        SidebarAction::Tool(crate::navigator::actions::ToolEntry::Dashboard),
    );
    app.wait_admin(cx, Pane::Sessions);
    let (_, rows) = app.admin_shown(cx, Pane::Sessions);
    let own = rows
        .iter()
        .position(|row| row[0].ends_with(" · this tab"))
        .unwrap();
    app.click(cx, ("admin-sessions-cell", own << 12));
    let tab = app.admin(cx);
    let reads = |cx: &mut TestAppContext| cx.update(|cx| tab.read(cx).reads());
    let before = reads(cx);
    app.on_admin(cx, |tab, window, cx| {
        tab.set_auto_refresh(Some(Duration::from_secs(5)), window, cx)
    });
    cx.executor().advance_clock(Duration::from_secs(5));
    app.wait_until(cx, "an automatic read", |_, cx| reads(cx) == before + 1);
    app.wait_admin(cx, Pane::Sessions);
    // The selection stays on the same session.
    assert_eq!(app.admin_why_not(cx), Some("this tab's session"));

    // Behind another tab, nothing is read.
    app.press(cx, &["secondary-t"]);
    for _ in 0..3 {
        cx.executor().advance_clock(Duration::from_secs(5));
        app.step(cx, |_, _| ());
    }
    assert_eq!(reads(cx), before + 1);

    // Back in front, it goes on; turned off, it stops.
    app.press(cx, &["ctrl-shift-tab"]);
    cx.executor().advance_clock(Duration::from_secs(5));
    app.wait_until(cx, "the next read", |_, cx| reads(cx) == before + 2);
    app.wait_admin(cx, Pane::Sessions);
    app.on_admin(cx, |tab, window, cx| tab.set_auto_refresh(None, window, cx));
    cx.executor().advance_clock(Duration::from_secs(30));
    app.step(cx, |_, _| ());
    assert_eq!(reads(cx), before + 2);
}

#[gpui_kit::test]
async fn the_admin_tab_opens_from_the_status_bar_and_comes_back_after_a_restart(
    cx: &mut TestAppContext,
) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    app.right_click(cx, "status-connection", None);
    click_menu_entry(&app, cx, "Administration");
    assert_eq!(
        app.titles(cx),
        ["Query 1", "test PostgreSQL · administration"]
    );
    app.wait_admin(cx, Pane::Sessions);
    let stored = app
        .wait_for_stored_tabs(cx, |tabs| {
            tabs.iter()
                .any(|tab| matches!(tab, TabSnapshot::Admin { .. }))
        })
        .await;
    assert!(
        matches!(&stored[1], TabSnapshot::Admin { target } if target.database.is_none()),
        "{stored:?}"
    );

    // The next launch shows the tab; its lists are read once connected.
    let restarted = open(cx, app.storage.clone());
    assert_eq!(
        restarted.titles(cx),
        ["Query 1", "test PostgreSQL · administration"]
    );
    assert!(restarted.has(cx, ("admin-waiting", 0usize)));
}

/// Opens the app on a test server as `user`.
fn connected_as_user(
    cx: &mut TestAppContext,
    engine: Engine,
    user: &str,
    password: &str,
) -> Fixture {
    let storage = memory_storage();
    let config = ConnectionConfig {
        user: user.into(),
        ..testing::config(engine)
    };
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let password = Secrets {
        password: Some(Secret::new(password)),
        ..Secrets::default()
    };
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &password).unwrap();
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

#[gpui_kit::test]
fn an_account_without_the_privileges_sees_what_the_server_refused(cx: &mut TestAppContext) {
    // An account that sees every session (PROCESS) but may signal only its
    // own, and cannot read the accounts (the mysql database).
    for engine in [Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let account = "'kerno_ui_admin_watcher'@'%'";
        // Declared first, dropped last: after the app's sessions.
        let _scratch = Scratch::new(
            engine,
            &[&format!("DROP USER IF EXISTS {account}")],
            &[
                &format!("CREATE USER {account} IDENTIFIED BY 'kerno_ui_watch_pw'"),
                &format!("GRANT PROCESS ON *.* TO {account}"),
                &format!("GRANT SELECT ON kerno.* TO {account}"),
            ],
        );
        let app = connected_as_user(cx, engine, "kerno_ui_admin_watcher", "kerno_ui_watch_pw");
        app.dispatch(cx, actions::OpenAdmin);
        app.wait_admin(cx, Pane::Sessions);

        let sleeper = Sleeper::start(engine, "kerno_ui_admin_refused");
        app.admin_filter(cx, "kerno_ui_admin_refused");
        app.admin_rows_until(cx, Pane::Sessions, "the other session", |rows| {
            rows.len() == 1 && rows[0][0] == sleeper.id
        });
        app.click(cx, ("admin-sessions-cell", 0usize));
        app.click(cx, "admin-cancel");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the refusal", |app, cx| app.admin_message(cx).is_some());
        let (failed, text) = app.admin_message(cx).unwrap();
        assert!(failed, "{engine:?}: {text}");
        assert!(text.contains("not owner of thread"), "{engine:?}: {text}");

        // A list it may not read says what the server said.
        app.on_admin(cx, |tab, window, cx| tab.select(ROLES, window, cx));
        let tab = app.admin(cx);
        app.wait_until(cx, "the refusal", |_, cx| {
            cx.update(|cx| tab.read(cx).state(Pane::Roles))
                .starts_with("failed")
        });
        let state = cx.update(|cx| tab.read(cx).state(Pane::Roles));
        assert!(state.contains("denied"), "{engine:?}: {state}");
        assert!(app.has(cx, ("admin-error", Pane::Roles as usize)));
    }
}

#[gpui_kit::test]
fn the_selected_session_stays_selected_through_a_refresh_and_a_sort(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.dispatch(cx, actions::OpenAdmin);
    app.wait_admin(cx, Pane::Sessions);
    let sleepers: Vec<Sleeper> = (0..3)
        .map(|_| Sleeper::start(Engine::Postgres, "kerno_ui_admin_moves"))
        .collect();
    app.admin_filter(cx, "kerno_ui_admin_moves");
    let rows = app.admin_rows_until(cx, Pane::Sessions, "the three sessions", |rows| {
        rows.len() == 3
    });
    // The middle one; the one above it goes.
    app.click(cx, ("admin-sessions-cell", 1usize << 12));
    let tab = app.admin(cx);
    let selected = |cx: &mut TestAppContext| cx.update(|cx| tab.read(cx).selected_id(cx));
    let kept = rows[1][0].clone();
    assert_eq!(selected(cx), Some(kept.clone()));
    let grid = cx.update(|cx| tab.read(cx).grid(Pane::Sessions));
    let offset = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            grid.read(cx)
                .vertical_scroll_handle
                .0
                .borrow()
                .base_handle
                .offset()
        })
    };
    let before = offset(cx);
    assert!(sleepers.iter().any(|sleeper| sleeper.id == rows[0][0]));
    on_server(
        Engine::Postgres,
        &format!("SELECT pg_terminate_backend({})", rows[0][0]),
    );
    let rows = app.admin_rows_until(cx, Pane::Sessions, "two sessions", |rows| rows.len() == 2);
    assert_eq!(rows[0][0], kept, "{rows:?}");
    assert_eq!(selected(cx), Some(kept.clone()));
    assert_eq!(offset(cx), before);

    // A sort on the client moves the row; the selection goes with it.
    for sort in [ColumnSort::Ascending, ColumnSort::Descending] {
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| {
                table.delegate_mut().perform_sort(0, sort, window, cx)
            })
        });
        assert_eq!(selected(cx), Some(kept.clone()), "{sort:?}");
    }
}

#[gpui_kit::test]
fn a_read_only_connection_sends_no_signal(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected_as(
        cx,
        ConnectionConfig {
            read_only: true,
            ..testing::config(Engine::Postgres)
        },
    );
    app.dispatch(cx, actions::OpenAdmin);
    app.wait_admin(cx, Pane::Sessions);
    let (_, rows) = app.admin_shown(cx, Pane::Sessions);
    // Another session: the one the schema tree reads on.
    let other = rows
        .iter()
        .position(|row| !row[0].ends_with(" · this tab"))
        .unwrap();
    app.click(cx, ("admin-sessions-cell", other << 12));
    assert_eq!(app.admin_why_not(cx), Some("read-only connection"));
    app.right_click(cx, ("admin-sessions-cell", other << 12), None);
    assert!(menu_entry(&app, cx, "Terminate session (read-only connection)").is_some());
    assert!(menu_entry(&app, cx, "Cancel query (read-only connection)").is_some());
    app.press(cx, &["escape"]);
}

// Brand (M10 part 5) ---------------------------------------------------------

#[gpui_kit::test]
fn the_about_dialog_shows_the_lockup_and_the_version(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    assert!(!app.has(cx, "about-lockup"));
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::About), cx)
    });
    assert!(app.has(cx, "about-lockup"));
    assert!(app.has(cx, "about-version"));
}

#[gpui_kit::test]
fn the_empty_state_shows_the_mark_and_the_theme_carries_the_brand(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    assert!(app.has(cx, "brand-mark"));
    // The theme file's button slots reach the UI kit: Lime with Ink.
    let (button, label) = cx.update(|cx| {
        (
            cx.theme().button_primary,
            cx.theme().button_primary_foreground,
        )
    });
    assert_eq!(button, gpui_kit::rgb(0xC8F03C).into());
    assert_eq!(label, gpui_kit::rgb(0x111111).into());
    app.click(cx, "empty-new-query");
    assert!(!app.has(cx, "brand-mark"));
}

// Connection settings (G13) --------------------------------------------------

#[gpui_kit::test]
fn a_production_connection_starts_its_tabs_in_manual_commit(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    use kerno_core::environment::{LOCAL, PRODUCTION};
    let storage = memory_storage();
    let cases = [
        (PRODUCTION, None, true),
        (PRODUCTION, Some(true), false),
        (LOCAL, None, false),
        (LOCAL, Some(false), true),
    ];
    for (environment, auto_commit, _) in cases {
        let config = ConnectionConfig {
            environment: environment.into(),
            auto_commit,
            ..testing::config(Engine::Postgres)
        };
        storage.save_connection(None, &config).unwrap();
    }
    let ids: Vec<String> = storage
        .connections()
        .unwrap()
        .into_iter()
        .map(|saved| saved.id)
        .collect();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &ids[0], &testing::secrets()).unwrap();

    // Cmd+T on the connected production connection.
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    assert!(cx.update(|cx| tab.read(cx).manual_commit()));

    // A tab of each connection starts in the connection's mode.
    for (id, (environment, auto_commit, manual)) in ids.into_iter().zip(cases) {
        let connections = app.connections.clone();
        let tab = app.step(cx, |window, cx| {
            cx.new(|cx| {
                crate::query::tab::QueryTab::new(
                    "t",
                    "",
                    Some(Target {
                        connection: id,
                        database: None,
                        schema: None,
                    }),
                    connections,
                    window,
                    cx,
                )
            })
        });
        assert_eq!(
            cx.update(|cx| tab.read(cx).manual_commit()),
            manual,
            "{environment:?} {auto_commit:?}"
        );
    }
}

#[gpui_kit::test]
fn bootstrap_statements_run_in_each_tab_and_a_failing_one_is_named(cx: &mut TestAppContext) {
    let mut apps = Vec::new();
    for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let (init_sql, query, expected) = match engine {
            Engine::Postgres => (
                "SET TIME ZONE 'Pacific/Auckland'",
                "select current_setting('TimeZone') || ' ' || current_schema()",
                "Pacific/Auckland shop",
            ),
            _ => (
                "SET time_zone = '+05:30'",
                "select @@session.time_zone",
                "+05:30",
            ),
        };
        let app = connected_as(
            cx,
            ConnectionConfig {
                init_sql: vec![init_sql.into()],
                default_schema: Some("shop".into()),
                ..testing::config(engine)
            },
        );
        app.run_in_new_tab(cx, query, "secondary-enter");
        app.wait_until(cx, "the query", |app, cx| app.outcomes(cx) == ["1 rows"]);
        assert_eq!(app.result(cx, 0, 1).1, [cells(&[expected])], "{engine:?}");
        apps.push(app);

        // A failing statement fails the connection, and says which one.
        let storage = memory_storage();
        let config = ConnectionConfig {
            init_sql: vec![init_sql.into(), "SET kerno_ui_no_such = 1".into()],
            ..testing::config(engine)
        };
        let id = storage.save_connection(None, &config).unwrap();
        let app = open(cx, storage);
        let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
        kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
        app.step(cx, |window, cx| {
            window.double_click(("connection", 0usize), cx)
        });
        app.wait_until(cx, "the failure", |app, cx| {
            matches!(app.status(cx, 0), Status::Failed { .. })
        });
        let Status::Failed { message } = app.status(cx, 0) else {
            unreachable!()
        };
        assert!(
            message.contains("bootstrap statement 2 failed: SET kerno_ui_no_such = 1"),
            "{engine:?}: {message}"
        );
        assert!(!message.contains(testing::DB_PASSWORD), "{message}");
        apps.push(app);
    }
}

#[gpui_kit::test]
fn the_connection_dialog_keeps_the_commit_mode_and_the_bootstrap(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let config = ConnectionConfig {
        name: "Boot".into(),
        auto_commit: Some(false),
        default_schema: Some("shop".into()),
        init_sql: vec![
            "SET TIME ZONE 'UTC'".into(),
            "SET application_name = 'a;b'".into(),
        ],
        ignore_init_errors: true,
        ..testing::config(Engine::Postgres)
    };
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::Edit);
    app.wait_until(cx, "the connection dialog", |app, cx| {
        app.has(cx, "conn-name")
    });
    app.click(cx, "conn-name");
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "Booted");
    app.click(cx, "conn-save");
    app.wait_until(cx, "the change to be stored", |app, _| {
        app.first_stored().name == "Booted"
    });
    assert_eq!(
        app.first_stored(),
        ConnectionConfig {
            name: "Booted".into(),
            ..config
        }
    );
}

#[gpui_kit::test]
fn a_refused_bootstrap_statement_is_shown_in_the_advanced_section(cx: &mut TestAppContext) {
    let storage = memory_storage();
    // Stored by an older build or by hand: the dialog refuses to save it.
    let config = ConnectionConfig {
        init_sql: vec![
            "SET TIME ZONE 'UTC'".into(),
            "DELETE FROM shop.orders".into(),
        ],
        ..testing::config(Engine::Postgres)
    };
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::Edit);
    app.wait_until(cx, "the connection dialog", |app, cx| {
        app.has(cx, "conn-name")
    });
    assert!(
        !app.has(cx, "conn-default-schema"),
        "the General section is shown"
    );
    app.click(cx, "conn-save");
    // The dialog stays, on the Advanced section, with the error.
    assert!(app.has(cx, "conn-save"));
    assert!(app.has(cx, "conn-default-schema") && app.has(cx, "conn-field-error"));
    assert_eq!(app.first_stored(), config);
}

// Real-app pass of 2026-11-13 (docs/plans/M10.md, Part 4) ------------------------

/// The sessions the server has for `role`.
fn sessions_of(role: &str) -> usize {
    on_server(
        Engine::Postgres,
        &format!("SELECT count(*) FROM pg_stat_activity WHERE usename = '{role}'"),
    )
    .parse()
    .unwrap()
}

#[gpui_kit::test]
fn restored_tabs_open_a_session_once_shown_and_structure_tabs_close_theirs(
    cx: &mut TestAppContext,
) {
    // A role of its own: the sessions counted are this test's only.
    const ROLE: &str = "kerno_ui_sessions";
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE usename = 'kerno_ui_sessions'",
            "DROP SCHEMA IF EXISTS kerno_ui_sessions CASCADE",
            "DROP ROLE IF EXISTS kerno_ui_sessions",
        ],
        &[
            "CREATE ROLE kerno_ui_sessions LOGIN PASSWORD 'kerno_ui_sessions_pw'",
            "CREATE SCHEMA kerno_ui_sessions AUTHORIZATION kerno_ui_sessions",
            "CREATE TABLE kerno_ui_sessions.items (id INT PRIMARY KEY, name TEXT)",
            "ALTER TABLE kerno_ui_sessions.items OWNER TO kerno_ui_sessions",
            "INSERT INTO kerno_ui_sessions.items VALUES (1, 'a'), (2, 'b')",
        ],
    );
    let storage = memory_storage();
    let config = ConnectionConfig {
        user: ROLE.into(),
        ..testing::config(Engine::Postgres)
    };
    let id = storage.save_connection(None, &config).unwrap();
    let target = Target {
        connection: id.clone(),
        database: None,
        schema: None,
    };
    let (schema, table) = (ROLE.to_string(), "items".to_string());
    storage
        .save_tabs(&[
            TabSnapshot::Query {
                title: "Query 1".into(),
                sql: "select 1".into(),
                target: Some(target.clone()),
                file: None,
                unsaved: false,
                source: None,
            },
            TabSnapshot::Data {
                target: target.clone(),
                schema: schema.clone(),
                table: table.clone(),
                view: false,
            },
            TabSnapshot::Structure {
                target: target.clone(),
                schema: schema.clone(),
                table: table.clone(),
                view: false,
                materialized: false,
            },
            TabSnapshot::Admin {
                target: target.clone(),
            },
        ])
        .unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let password = Secrets {
        password: Some(Secret::new("kerno_ui_sessions_pw")),
        ..Secrets::default()
    };
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &password).unwrap();
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    // The schema tree reads on a session of its own meanwhile (D-087: a
    // read waiting for a lock there must not hold the connection's).
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, ROLE))
    });
    // The connection holds its session and the tree's; the tabs behind the
    // query tab (which opens its own on its first run) hold none.
    app.step(cx, |_, _| ());
    let connection = sessions_of(ROLE);
    assert_eq!(connection, 2);
    // Nor have they read anything.
    let (data, structure, admin) = cx.update(|cx| {
        let workspace = app.workspace.read(cx);
        (
            workspace.data_tab_at(1).unwrap(),
            workspace.structure_tab_at(2).unwrap(),
            workspace.admin_tab_at(3).unwrap(),
        )
    });
    cx.update(|cx| {
        assert_eq!(data.read(cx).state(cx), "waiting");
        assert_eq!(structure.read(cx).state(), "waiting");
        assert_eq!(admin.read(cx).reads(), 0);
    });

    // Shown, the data tab reads its rows and keeps its session.
    app.press(cx, &["ctrl-tab"]);
    app.wait_for_rows(cx, "Rows 1–2 of 2");
    app.wait_until(cx, "the data tab's session", |_, _| {
        sessions_of(ROLE) == connection + 1
    });
    // The structure tab reads its table, then closes its session.
    app.press(cx, &["ctrl-tab"]);
    app.wait_for_structure(cx);
    app.wait_until(cx, "the structure tab's session to close", |_, _| {
        sessions_of(ROLE) == connection + 1
    });
    // So does the table form, opened from it.
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_table_editor(
                target.clone(),
                schema.clone(),
                Some(TableRef::new(schema.clone(), table.clone())),
                window,
                cx,
            )
        });
    });
    app.wait_for_editor(cx);
    app.wait_until(cx, "the table form's session to close", |_, _| {
        sessions_of(ROLE) == connection + 1
    });
    // The administration tab, just before the form, keeps one.
    app.press(cx, &["ctrl-shift-tab"]);
    app.wait_admin(cx, Pane::Sessions);
    app.wait_until(cx, "the administration tab's session", |_, _| {
        sessions_of(ROLE) == connection + 2
    });
    // Its times show to the second.
    let (headers, rows) = app.admin_shown(cx, Pane::Sessions);
    let connected_at = headers.iter().position(|header| header == "Connected at");
    let connected_at = &rows[0][connected_at.expect("a Connected at column")];
    assert!(
        !connected_at.contains('.'),
        "{connected_at:?} shows a fraction of a second"
    );
    drop(app);
}

#[gpui_kit::test]
fn the_tab_strip_keeps_the_active_tab_in_view(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    for _ in 0..15 {
        app.press(cx, &["secondary-t"]);
    }
    let in_view = |app: &Fixture, cx: &mut TestAppContext| {
        // The frames that scroll the strip: a strip never laid out yet
        // scrolls on its second frame (the app asks for it at once).
        app.step(cx, |_, _| ());
        app.step(cx, |_, _| ());
        cx.update(|cx| app.workspace.read(cx).active_tab_in_view())
    };
    // A new tab, at the far end of a strip that overflows.
    assert_eq!(app.active(cx), Some(14));
    assert_eq!(in_view(&app, cx), (true, true));
    // Activated by key, from one end to the other.
    app.press(cx, &["ctrl-tab"]);
    assert_eq!(app.active(cx), Some(0));
    assert_eq!(in_view(&app, cx), (true, true));
    app.press(cx, &["ctrl-shift-tab"]);
    assert_eq!(app.active(cx), Some(14));
    assert_eq!(in_view(&app, cx), (true, true));

    // Restored, the active tab is in view from the first frame.
    let storage = memory_storage();
    let tabs: Vec<TabSnapshot> = (1..=15)
        .map(|number| TabSnapshot::Query {
            title: format!("Query {number}"),
            sql: String::new(),
            target: None,
            file: None,
            unsaved: false,
            source: None,
        })
        .collect();
    storage.save_tabs(&tabs).unwrap();
    storage
        .set_setting(crate::state::ACTIVE_TAB_KEY, &12usize)
        .unwrap();
    let restored = open(cx, storage);
    assert_eq!(restored.active(cx), Some(12));
    assert_eq!(in_view(&restored, cx), (true, true));
}

#[gpui_kit::test]
fn a_table_dropped_under_its_data_tab_says_so_and_offers_to_close_it(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_dropped";
        let (drop_schema, create_schema) = scratch_schema(engine, schema);
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &scratch_table(engine, &format!("{schema}.items"), "name VARCHAR(10)"),
                &format!("INSERT INTO {schema}.items (name) VALUES ('a')"),
            ],
        );
        let app = connected(cx, engine);
        app.open_table(cx, schema, "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–1 of 1");
        on_server(engine, &format!("DROP TABLE {schema}.items"));
        app.click(cx, "data-refresh");
        app.wait_until(cx, "the failure", |app, cx| {
            app.data_state(cx).starts_with("failed")
        });
        let advice = cx.update(|cx| {
            app.workspace
                .read(cx)
                .active_data_tab()
                .unwrap()
                .read(cx)
                .advice()
        });
        assert!(
            advice.ends_with(". The table may have been dropped or renamed."),
            "{engine:?}: {advice}"
        );
        assert!(app.has(cx, "data-retry"), "{engine:?}");
        app.click(cx, "data-close-tab");
        assert!(app.titles(cx).is_empty(), "{engine:?}");
    }
}

#[gpui_kit::test]
fn a_cell_menu_taller_than_the_window_scrolls_inside_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = filter_scratch(Engine::Postgres, "kerno_ui_menu_fits");
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "kerno_ui_menu_fits", "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–4 of 4");
    // A short window: the data cell's menu is taller than it.
    let height = px(360.);
    cx.simulate_window_resize(app.window.into(), size(px(1200.), height));
    app.right_click(cx, "table-cell-3:2", None);
    let menu = app.step(cx, |window, _| window.find("popup-menu").bounds());
    assert!(
        menu.top() >= px(0.) && menu.bottom() <= height,
        "{menu:?} in a window {height:?} tall"
    );
    // The entries past the bottom are reached by scrolling, and the row's
    // and the column's are in their submenus.
    assert!(menu_entry(&app, cx, "Copy").is_some());
    click_menu_entry(&app, cx, "Copy as");
    assert!(menu_entry(&app, cx, "JSON").is_some());
}

#[gpui_kit::test]
fn the_status_bar_menus_open_inside_the_window(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let inside = |app: &Fixture, cx: &mut TestAppContext, how: &str| {
        let menu = app.step(cx, |window, _| window.find("popup-menu").bounds());
        assert!(
            menu.left() >= px(0.)
                && menu.right() <= px(1200.)
                && menu.top() >= px(0.)
                && menu.bottom() <= px(800.),
            "{how}: {menu:?}"
        );
    };
    // A click on the connection opens its menu above the bar; so does a
    // right-click.
    app.click(cx, "status-connection-button");
    inside(&app, cx, "click");
    app.press(cx, &["escape"]);
    app.right_click(cx, "status-connection", None);
    inside(&app, cx, "right-click");
}

// Running in the SQL editor and result tabs (G4) -------------------------------

/// The app with `config` saved (and its password, when it saves one) and a
/// query tab on it holding `sql`, as after a restart: not connected.
fn closed_tab_on(cx: &mut TestAppContext, config: ConnectionConfig, sql: &str) -> Fixture {
    let storage = memory_storage();
    let id = storage.save_connection(None, &config).unwrap();
    storage
        .save_tabs(&[TabSnapshot::Query {
            title: "Query 1".into(),
            sql: sql.into(),
            target: Some(Target {
                connection: id.clone(),
                database: None,
                schema: None,
            }),
            file: None,
            unsaved: false,
            source: None,
        }])
        .unwrap();
    let save_password = config.save_password;
    let app = open(cx, storage);
    if save_password {
        let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
        kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    }
    assert_eq!(app.status(cx, 0), Status::Disconnected);
    app
}

impl Fixture {
    /// Replaces the text of the active query tab's editor by `sql`.
    fn retype(&self, cx: &mut TestAppContext, sql: &str) {
        self.click(cx, "query-editor");
        self.press(cx, &["secondary-a", "backspace"]);
        self.type_text(cx, sql);
    }

    /// The labels of the active query tab's result tabs, `*` when pinned.
    fn result_tabs(&self, cx: &mut TestAppContext) -> Vec<String> {
        let tab = self.tab(cx);
        cx.update(|cx| tab.read(cx).result_tabs())
    }

    /// The column names of a result of the active query tab.
    fn columns_of(&self, cx: &mut TestAppContext, index: usize) -> Vec<String> {
        self.result(cx, index, 0).0
    }
}

#[gpui_kit::test]
fn running_in_a_tab_of_a_closed_connection_connects_first(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    // A saved password: Run connects, then runs the statement, once.
    let app = closed_tab_on(
        cx,
        testing::config(Engine::Postgres),
        "select 41 + 1 as answer",
    );
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the connection and the rows", |app, cx| {
        app.outcomes(cx) == ["1 rows"]
    });
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    assert_eq!(app.result(cx, 0, 1).1, [cells(&["42"])]);
    // The run that connected does not make the selectors' lists late.
    let tab = app.tab(cx);
    app.wait_until(cx, "the databases and schemas of the selectors", |_, cx| {
        cx.update(|cx| {
            let tab = tab.read(cx);
            !tab.databases(cx).is_empty() && tab.schemas().iter().any(|schema| schema == "public")
        })
    });
    app.wait_until(cx, "the history", |app, _| {
        !app.storage.history(None, "", 10).unwrap().is_empty()
    });
    assert_eq!(app.storage.history(None, "", 10).unwrap().len(), 1);

    // A password asked for: declining runs nothing, and the tab says why.
    let mut config = testing::config(Engine::Postgres);
    config.save_password = false;
    let app = closed_tab_on(cx, config, "select 'asked' as how");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the password prompt", |app, cx| {
        app.has(cx, "secret-input")
    });
    assert!(app.has(cx, "results-waiting"), "the tab says it connects");
    app.click(cx, "secret-cancel");
    assert_eq!(app.status(cx, 0), Status::Disconnected);
    assert_eq!(app.outcomes(cx), ["failed []"]);
    assert!(app.has(cx, "result-error"));
    assert!(app.storage.history(None, "", 10).unwrap().is_empty());
    // Running again asks again; with the password it runs.
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the password prompt", |app, cx| {
        app.has(cx, "secret-input")
    });
    app.type_text(cx, testing::DB_PASSWORD);
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert_eq!(app.result(cx, 0, 1).1, [cells(&["asked"])]);

    // A read-only connection is opened, then still refuses what writes.
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = closed_tab_on(cx, config, "delete from kerno_ui_never_there");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the refusal", |app, cx| app.outcomes(cx).len() == 1);
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    assert_eq!(app.outcomes(cx), ["failed []"], "refused in the client");
    assert!(app.storage.history(None, "", 10).unwrap().is_empty());
}

#[gpui_kit::test]
fn result_tabs_close_pin_and_rename(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(cx, "select 1 as one", "secondary-enter");
    app.wait_until(cx, "result 1", |app, cx| app.outcomes(cx) == ["1 rows"]);
    // Ctrl+Shift+P pins it: the next run keeps it and adds its own.
    app.press(cx, &["ctrl-shift-p"]);
    assert_eq!(app.result_tabs(cx), ["*Result 1"]);
    app.retype(cx, "select 2 as two");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "result 2", |app, cx| app.outcomes(cx).len() == 2);
    assert_eq!(app.result_tabs(cx), ["*Result 1", "Result 2"]);
    assert_eq!(app.columns_of(cx, 0), ["one"]);
    assert_eq!(app.columns_of(cx, 1), ["two"]);

    // Cmd+\ keeps every earlier result and shows the new one.
    app.retype(cx, "select 3 as three");
    app.press(cx, &["secondary-\\"]);
    app.wait_until(cx, "result 3", |app, cx| app.outcomes(cx).len() == 3);
    assert_eq!(app.result_tabs(cx), ["*Result 1", "Result 2", "Result 3"]);
    let tab = app.tab(cx);
    assert_eq!(cx.update(|cx| tab.read(cx).active_result()), 2);
    // A plain run replaces what is not pinned; the kept result is whole.
    app.retype(cx, "select 4 as four");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "result 4", |app, cx| {
        app.outcomes(cx).len() == 2 && app.columns_of(cx, 1) == ["four"]
    });
    assert_eq!(app.outcomes(cx), ["1 rows", "1 rows"]);
    assert_eq!(app.result(cx, 0, 1).1, [cells(&["1"])]);

    // Its menu, from a real right-click: rename it, copy its statement.
    app.right_click(cx, ("result-tab", 0usize), None);
    assert!(menu_entry(&app, cx, "Unpin tab").is_some());
    assert!(menu_entry(&app, cx, "Close all").is_some());
    click_menu_entry(&app, cx, "Rename…");
    assert!(app.has(cx, "result-name"));
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "kept");
    app.press(cx, &["enter"]);
    assert_eq!(app.result_tabs(cx), ["*kept", "Result 2"]);
    // The editor has the keyboard again.
    let editor_focused = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, cx| {
            tab.read(cx)
                .editor()
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx)
        })
    };
    assert!(editor_focused(&app, cx));
    app.right_click(cx, ("result-tab", 0usize), None);
    click_menu_entry(&app, cx, "Copy statement");
    assert_eq!(app.clipboard(cx).as_deref(), Some("select 1 as one"));

    // A middle click closes a result, and frees it.
    let closed = cx
        .update(|cx| tab.read(cx).grid_of(1).map(|grid| grid.downgrade()))
        .expect("a grid");
    app.middle_click(cx, ("result-tab", 1usize));
    assert_eq!(app.result_tabs(cx), ["*kept"]);
    // Once the frame that drew it is gone.
    app.step(cx, |window, _| window.refresh());
    assert!(closed.upgrade().is_none(), "its rows are freed");

    // The bulk closes leave the pinned one.
    for n in 5..8 {
        app.retype(cx, &format!("select {n} as n{n}"));
        app.press(cx, &["secondary-\\"]);
        app.wait_until(cx, "another result", |app, cx| {
            app.outcomes(cx).len() == n - 3
        });
    }
    assert_eq!(app.result_tabs(cx).len(), 4);
    app.right_click(cx, ("result-tab", 2usize), None);
    click_menu_entry(&app, cx, "Close tabs to the left");
    assert_eq!(app.result_tabs(cx), ["*kept", "Result 2", "Result 3"]);
    assert_eq!(app.columns_of(cx, 1), ["n6"]);
    app.right_click(cx, ("result-tab", 1usize), None);
    click_menu_entry(&app, cx, "Close all");
    assert_eq!(app.result_tabs(cx), ["*kept"]);

    // Closing a result still waiting to be paged stops its statement.
    app.retype(cx, "select g from generate_series(1, 2500) g");
    app.press(cx, &["secondary-\\"]);
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx) == ["1 rows", "1000 rows, more"]
    });
    let holds = |app: &Fixture, cx: &mut TestAppContext| {
        let tab = app.tab(cx);
        cx.update(|cx| tab.read(cx).holds_session())
    };
    assert!(holds(&app, cx), "the statement waits to be paged");
    // Nothing to stop while it only waits for Fetch more.
    assert!(!app.has(cx, "tab-cancel"));
    app.middle_click(cx, ("result-tab", 1usize));
    assert_eq!(app.result_tabs(cx), ["*kept"]);
    assert!(!holds(&app, cx), "its statement stopped");

    // A result added beside a kept one pages; the kept one's cells copy
    // and export from their menu.
    app.retype(cx, "select g from generate_series(1, 2500) g");
    app.press(cx, &["secondary-\\"]);
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx) == ["1 rows", "1000 rows, more"]
    });
    app.click(cx, "fetch-more");
    app.wait_until(cx, "the second page", |app, cx| {
        app.outcomes(cx) == ["1 rows", "2000 rows, more"]
    });
    app.click(cx, ("result-tab", 0usize));
    app.right_click(cx, "table-cell-0:1", None);
    click_menu_entry(&app, cx, "Copy");
    assert_eq!(app.clipboard(cx).as_deref(), Some("1"));
    app.right_click(cx, "table-cell-0:1", None);
    click_menu_entry(&app, cx, "Export…");
    assert!(app.has(cx, "export-dialog"));
    app.press(cx, &["escape"]);
    assert!(!app.has(cx, "export-dialog"));

    // Cancel stops what runs and leaves the kept results alone; the
    // result that was waiting to be paged stops there.
    app.retype(cx, "select pg_sleep(60) as kerno_ui_result_tabs");
    app.press(cx, &["secondary-\\"]);
    app.wait_until(cx, "the statement to start", |app, cx| {
        app.running(cx) && running_on_server(Engine::Postgres, "kerno_ui_result_tabs")
    });
    app.press(cx, &["secondary-."]);
    app.wait_until(cx, "the cancellation", |app, cx| !app.running(cx));
    assert_eq!(
        app.outcomes(cx),
        ["1 rows", "2000 rows, incomplete", "cancelled"]
    );
    assert_eq!(app.result(cx, 0, 1).1, [cells(&["1"])]);

    // Ctrl+| (DBeaver's Ctrl+Shift+\) closes the result on screen, pinned or not.
    app.press(cx, &["ctrl-|", "ctrl-|"]);
    assert_eq!(app.result_tabs(cx), ["*kept"]);
    app.press(cx, &["ctrl-|"]);
    assert!(app.result_tabs(cx).is_empty());
    assert!(!app.has(cx, "results"), "nothing left to show");
}

#[gpui_kit::test]
fn the_result_tab_bar_scrolls_to_the_result_on_screen(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let script: Vec<String> = (1..=20).map(|n| format!("select {n}")).collect();
    app.run_in_new_tab(cx, &script.join(";\n"), "secondary-shift-enter");
    app.wait_until(cx, "the script", |app, cx| {
        app.outcomes(cx).len() == 20 && !app.running(cx)
    });
    let tab = app.tab(cx);
    assert_eq!(cx.update(|cx| tab.read(cx).result_bar_offset()), px(0.));
    // The cursor is in the last statement: Cmd+\ adds a 21st result, shown.
    app.press(cx, &["secondary-\\"]);
    app.wait_until(cx, "the 21st result", |app, cx| {
        app.outcomes(cx).len() == 21 && !app.running(cx)
    });
    app.step(cx, |window, _| window.refresh());
    assert!(
        cx.update(|cx| tab.read(cx).result_bar_offset()) < px(0.),
        "the bar scrolled to its last tab"
    );
}

#[gpui_kit::test]
fn statements_are_reached_and_run_from_the_cursor(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let script = "select 1 as a;\nselect 2 as b;\nselect 3 as c;";
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, script);
    let tab = app.tab(cx);
    let cursor = |cx: &mut TestAppContext| cx.update(|cx| tab.read(cx).editor().read(cx).cursor());
    let start = |n: &str| script.find(&format!("select {n}")).unwrap();
    // Alt+Up goes back a statement at a time, and stops at the first.
    for expected in [start("3"), start("2"), start("1"), start("1")] {
        app.press(cx, &["alt-up"]);
        assert_eq!(cursor(cx), expected);
    }
    app.press(cx, &["alt-down"]);
    assert_eq!(cursor(cx), start("2"));
    // Alt+P runs from the statement at the cursor to the end.
    app.press(cx, &["alt-p"]);
    app.wait_until(cx, "two results", |app, cx| {
        app.outcomes(cx).len() == 2 && !app.running(cx)
    });
    assert_eq!(app.columns_of(cx, 0), ["b"]);
    assert_eq!(app.columns_of(cx, 1), ["c"]);
}

#[gpui_kit::test]
fn the_rows_of_a_statement_are_counted(cx: &mut TestAppContext) {
    for (engine, table) in [
        (Engine::Postgres, "shop.customers"),
        (Engine::MySql, "kerno.customers"),
    ] {
        if !testing::available(engine) {
            return;
        }
        let expected = on_server(engine, &format!("select count(*) from {table}"));
        let app = connected(cx, engine);
        app.run_in_new_tab(
            cx,
            &format!("select id from {table} order by id"),
            "secondary-enter",
        );
        app.wait_until(cx, "the rows", |app, cx| !app.outcomes(cx).is_empty());
        app.dispatch(cx, actions::CountStatementRows);
        app.wait_until(cx, "the count", |app, cx| {
            let tab = app.tab(cx);
            !cx.update(|cx| tab.read(cx).messages()).is_empty()
        });
        let tab = app.tab(cx);
        let messages = cx.update(|cx| tab.read(cx).messages());
        assert_eq!(
            messages,
            [format!(
                "Count: {expected} rows in the result of the statement."
            )],
            "{engine:?}"
        );
        // The result is kept; only a SELECT is counted.
        assert_eq!(app.outcomes(cx).len(), 1, "{engine:?}");
        app.retype(cx, "delete from kerno_ui_never_there");
        app.dispatch(cx, actions::CountStatementRows);
        assert_eq!(cx.update(|cx| tab.read(cx).messages()), messages);
    }
}

#[gpui_kit::test]
fn the_results_panel_hides_maximizes_and_takes_the_keyboard(cx: &mut TestAppContext) {
    use crate::query::tab::Panels;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(cx, "select 1 as one", "secondary-enter");
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["1 rows"]);
    let tab = app.tab(cx);
    let panels = |cx: &mut TestAppContext| cx.update(|cx| tab.read(cx).panels());
    let shown = |app: &Fixture, cx: &mut TestAppContext| {
        (app.has(cx, "query-editor"), app.has(cx, "results"))
    };
    assert_eq!(shown(&app, cx), (true, true));
    // Alt+Shift+T hides the results, and shows them again.
    app.press(cx, &["alt-shift-t"]);
    assert_eq!(panels(cx), Panels::Editor);
    assert_eq!(shown(&app, cx), (true, false));
    app.press(cx, &["alt-shift-t"]);
    assert_eq!(shown(&app, cx), (true, true));
    // Ctrl+Shift+T shows only the results, which get the keyboard.
    app.press(cx, &["ctrl-shift-t"]);
    assert_eq!(panels(cx), Panels::Results);
    assert_eq!(shown(&app, cx), (false, true));
    let results_focused = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, cx| tab.read(cx).results_focused(window, cx))
    };
    assert!(results_focused(&app, cx));
    // Alt+T goes back to the editor, shown again; and to the results again.
    app.press(cx, &["alt-t"]);
    assert_eq!(shown(&app, cx), (true, true));
    assert!(!results_focused(&app, cx));
    app.press(cx, &["alt-t"]);
    assert!(results_focused(&app, cx));
    // A run with the results hidden shows them.
    app.press(cx, &["alt-t", "alt-shift-t"]);
    assert_eq!(shown(&app, cx), (true, false));
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert_eq!(shown(&app, cx), (true, true));
}

#[gpui_kit::test]
fn tabs_to_the_left_of_the_active_one_close(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t", "secondary-t", "secondary-t"]);
    assert_eq!(app.titles(cx), ["Query 1", "Query 2", "Query 3"]);
    app.press(cx, &["ctrl-shift-tab"]);
    app.dispatch(cx, actions::CloseTabsToTheLeft);
    assert_eq!(app.titles(cx), ["Query 2", "Query 3"]);
}

// Sessions that outlive their owner (docs/plans/M10.md, Part 4, second list) ----

/// What `role` holds on the server: its sessions, and how many of them sit
/// idle inside a transaction.
fn held_by(engine: Engine, role: &str) -> (usize, usize) {
    let sql = match engine {
        Engine::Postgres => format!(
            "SELECT count(*) || '/' || count(*) FILTER (WHERE state LIKE 'idle in transaction%') \
             FROM pg_stat_activity WHERE usename = '{role}'"
        ),
        // The transaction as the server's SQL sees it (begun, not ended), not
        // InnoDB's own: after a read of `information_schema.TABLES`' sizes
        // MySQL 8.4 can keep an InnoDB transaction on the idle session, with
        // autocommit on and the statement's transaction committed, until the
        // session's next statement (K71; in the app the connection's 5 s
        // keep-alive is that statement, in the tests the clock stands still).
        Engine::MySql | Engine::MariaDb => format!(
            "SELECT concat(count(*), '/', \
             count(CASE WHEN p.command = 'Sleep' AND e.state = 'ACTIVE' THEN 1 END)) \
             FROM information_schema.processlist p \
             LEFT JOIN performance_schema.threads t ON t.processlist_id = p.id \
             LEFT JOIN performance_schema.events_transactions_current e \
             ON e.thread_id = t.thread_id \
             WHERE p.user = '{role}'"
        ),
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    };
    let held = on_server(engine, &sql);
    let (sessions, idle) = held.split_once('/').expect("two counts");
    (sessions.parse().unwrap(), idle.parse().unwrap())
}

/// The sessions of `role` and what each last ran, for the message of a
/// count that never became the expected one.
fn session_history(engine: Engine, role: &str) -> String {
    let sql = match engine {
        Engine::Postgres => format!(
            "SELECT string_agg(concat_ws(' | ', pid, state, backend_start, left(query, 200)), \
             E'\\n') FROM pg_stat_activity WHERE usename = '{role}'"
        ),
        Engine::MySql | Engine::MariaDb => format!(
            "SELECT json_pretty(json_arrayagg(concat_ws(' | ', t.processlist_id, \
             t.processlist_command, t.processlist_time, h.event_id, left(h.sql_text, 200)))) \
             FROM performance_schema.threads t \
             LEFT JOIN performance_schema.events_statements_history h USING (thread_id) \
             WHERE t.processlist_user = '{role}'"
        ),
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    };
    let sessions = on_server(engine, &sql);
    if engine == Engine::Postgres {
        return sessions;
    }
    // The open transactions: when and how each began, and the statement
    // that began it when the server still has it.
    let transactions = on_server(
        engine,
        &format!(
            "SELECT json_pretty(json_arrayagg(concat_ws(' | ', x.trx_mysql_thread_id, \
             x.trx_state, x.trx_started, x.trx_is_read_only, x.trx_isolation_level, \
             x.trx_tables_locked, x.trx_rows_locked, v.variable_value, e.state, e.access_mode, \
             e.autocommit, e.nesting_event_type, left(s.sql_text, 200)))) \
             FROM information_schema.innodb_trx x \
             JOIN performance_schema.threads t ON t.processlist_id = x.trx_mysql_thread_id \
             LEFT JOIN performance_schema.variables_by_thread v ON v.thread_id = t.thread_id \
             AND v.variable_name = 'autocommit' \
             LEFT JOIN performance_schema.events_transactions_current e \
             ON e.thread_id = t.thread_id \
             LEFT JOIN performance_schema.events_statements_history s \
             ON s.thread_id = t.thread_id AND s.event_id = e.nesting_event_id \
             WHERE t.processlist_user = '{role}'"
        ),
    );
    format!("{sessions}\nopen transactions: {transactions}")
}

/// Lets the app and the server work for up to twenty seconds, until `role`
/// holds `expected`; gives what it holds then.
fn settle(
    cx: &mut TestAppContext,
    engine: Engine,
    role: &str,
    expected: (usize, usize),
) -> (usize, usize) {
    // Long enough for a close queued behind other tests' work on the shared
    // runtime; the wait ends as soon as the count is the expected one.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        let held = held_by(engine, role);
        if held == expected {
            return held;
        }
        if std::time::Instant::now() > deadline {
            eprintln!(
                "{engine:?}: {role} holds {held:?}, not {expected:?}:\n{}",
                session_history(engine, role)
            );
            return held;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[gpui_kit::test]
fn every_owner_of_a_session_closes_it_when_it_goes_away(cx: &mut TestAppContext) {
    // A role of its own: the sessions counted are this test's only.
    const ROLE: &str = "kerno_ui_owners";
    const PASSWORD: &str = "kerno_ui_owners_pw";
    // Both engines are measured before anything is asserted.
    let mut measured = Vec::new();
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let (cleanup, setup, database, other) = match engine {
            Engine::Postgres => (
                vec![
                    format!(
                        "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
                         WHERE usename = '{ROLE}'"
                    ),
                    format!("DROP SCHEMA IF EXISTS {ROLE} CASCADE"),
                    format!("DROP ROLE IF EXISTS {ROLE}"),
                ],
                vec![
                    format!("CREATE ROLE {ROLE} LOGIN PASSWORD '{PASSWORD}'"),
                    format!("CREATE SCHEMA {ROLE} AUTHORIZATION {ROLE}"),
                    format!("CREATE TABLE {ROLE}.items (id INT PRIMARY KEY, name TEXT)"),
                    format!("ALTER TABLE {ROLE}.items OWNER TO {ROLE}"),
                    format!("INSERT INTO {ROLE}.items VALUES (1, 'a'), (2, 'b')"),
                ],
                "kerno",
                "kerno_other",
            ),
            _ => (
                vec![
                    format!("DROP USER IF EXISTS '{ROLE}'@'%'"),
                    format!("DROP DATABASE IF EXISTS {ROLE}"),
                ],
                vec![
                    format!("CREATE DATABASE {ROLE}"),
                    format!("CREATE TABLE {ROLE}.items (id INT PRIMARY KEY, name TEXT)"),
                    format!("INSERT INTO {ROLE}.items VALUES (1, 'a'), (2, 'b')"),
                    format!("CREATE USER '{ROLE}'@'%' IDENTIFIED BY '{PASSWORD}'"),
                    format!("GRANT ALL ON {ROLE}.* TO '{ROLE}'@'%'"),
                ],
                ROLE,
                "information_schema",
            ),
        };
        let cleanup: Vec<&str> = cleanup.iter().map(String::as_str).collect();
        let setup: Vec<&str> = setup.iter().map(String::as_str).collect();
        // Declared first, dropped last: after the app's sessions.
        let _scratch = Scratch::new(engine, &cleanup, &setup);

        let storage = memory_storage();
        let config = ConnectionConfig {
            user: ROLE.into(),
            database: Some(database.into()),
            ..testing::config(engine)
        };
        let id = storage.save_connection(None, &config).unwrap();
        let app = open(cx, storage);
        let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
        let password = Secrets {
            password: Some(Secret::new(PASSWORD)),
            ..Secrets::default()
        };
        kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &password).unwrap();
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
        let target = app.target(cx, None);
        let table = TableRef::new(ROLE, "items");
        let dir = tempfile::tempdir().unwrap();
        let items = format!("{ROLE}.items");

        // What each owner held while open, and what was left once it went.
        let mut open = Vec::new();
        let mut left = Vec::new();
        // The connection holds its own session and the tree's (D-087).
        left.push(("connected", settle(cx, engine, ROLE, (2, 0))));

        let ran = |app: &Fixture, cx: &mut TestAppContext| {
            app.run_in_new_tab(cx, &format!("select name from {items}"), "secondary-enter");
            app.wait_until(cx, "the query", |app, cx| {
                !app.running(cx) && !app.outcomes(cx).is_empty()
            });
        };
        let in_transaction = |app: &Fixture, cx: &mut TestAppContext| {
            app.press(cx, &["secondary-t"]);
            let tab = app.tab(cx);
            cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(true, cx)));
            app.type_text(cx, &format!("insert into {items} values (10, 'open')"));
            app.press(cx, &["secondary-enter"]);
            app.wait_until(cx, "the insert", |app, cx| {
                !app.running(cx)
                    && cx.update(|cx| tab.read(cx).tx_state()) == kerno_core::TxState::Open
            });
            // Rows on screen, read inside the transaction: their grid holds
            // the session too.
            app.press(cx, &["secondary-a"]);
            app.type_text(cx, &format!("select name from {items}"));
            app.press(cx, &["secondary-enter"]);
            app.wait_until(cx, "the rows", |app, cx| {
                !app.running(cx)
                    && app
                        .outcomes(cx)
                        .iter()
                        .any(|outcome| outcome.ends_with(" rows"))
            });
        };

        ran(&app, cx);
        open.push(("query tab", settle(cx, engine, ROLE, (3, 0))));
        app.press(cx, &["secondary-w"]);
        left.push(("query tab", settle(cx, engine, ROLE, (2, 0))));

        // A tab moved to another database, its results still on screen.
        ran(&app, cx);
        let tab = app.tab(cx);
        let elsewhere = Target {
            database: Some(other.into()),
            ..target.clone()
        };
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_target(Some(elsewhere), cx)));
        app.press(cx, &["secondary-a"]);
        app.type_text(cx, "select 1");
        app.press(cx, &["secondary-enter"]);
        app.wait_until(cx, "the query", |app, cx| {
            !app.running(cx)
                && app
                    .outcomes(cx)
                    .iter()
                    .any(|outcome| outcome.contains("1 rows"))
        });
        open.push(("moved query tab", settle(cx, engine, ROLE, (3, 0))));
        drop(tab);
        app.press(cx, &["secondary-w"]);
        left.push(("moved query tab", settle(cx, engine, ROLE, (2, 0))));

        // Moved with a transaction open: the old session goes, and with it
        // the transaction; the tab holds one in its new database (for
        // completion there).
        in_transaction(&app, cx);
        let tab = app.tab(cx);
        let elsewhere = Target {
            database: Some(other.into()),
            ..target.clone()
        };
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_target(Some(elsewhere), cx)));
        drop(tab);
        left.push(("moved in a transaction", settle(cx, engine, ROLE, (3, 0))));
        app.press(cx, &["secondary-w"]);
        left.push(("moved, then closed", settle(cx, engine, ROLE, (2, 0))));

        in_transaction(&app, cx);
        open.push(("manual commit", settle(cx, engine, ROLE, (3, 1))));
        app.press(cx, &["secondary-w"]);
        assert!(app.shows_label(cx, "Roll back and close"), "{engine:?}");
        app.click(cx, "ok");
        left.push(("manual commit", settle(cx, engine, ROLE, (2, 0))));
        assert_eq!(
            on_server(engine, &format!("SELECT count(*) FROM {items}")),
            "2"
        );

        app.open_table(cx, ROLE, "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        open.push(("data tab", settle(cx, engine, ROLE, (3, 0))));
        app.press(cx, &["secondary-w"]);
        left.push(("data tab", settle(cx, engine, ROLE, (2, 0))));

        app.open_structure(cx, ROLE, "items", RelationKind::Table);
        app.wait_for_structure(cx);
        open.push(("structure tab", settle(cx, engine, ROLE, (2, 0))));
        app.press(cx, &["secondary-w"]);
        left.push(("structure tab", settle(cx, engine, ROLE, (2, 0))));

        app.step(cx, |window, cx| {
            app.workspace.update(cx, |workspace, cx| {
                workspace.open_table_editor(
                    target.clone(),
                    ROLE.into(),
                    Some(table.clone()),
                    window,
                    cx,
                )
            });
        });
        app.wait_for_editor(cx);
        open.push(("table editor", settle(cx, engine, ROLE, (2, 0))));
        app.press(cx, &["secondary-w"]);
        left.push(("table editor", settle(cx, engine, ROLE, (2, 0))));

        app.dispatch(cx, actions::OpenAdmin);
        app.wait_admin(cx, Pane::Sessions);
        open.push(("admin tab", settle(cx, engine, ROLE, (3, 0))));
        app.press(cx, &["secondary-w"]);
        left.push(("admin tab", settle(cx, engine, ROLE, (2, 0))));

        let connections = app.connections.clone();
        let source = crate::export_dialog::Source::Query {
            sql: format!("select * from {items}"),
        };
        app.step(cx, |window, cx| {
            crate::export_dialog::open(connections, target.clone(), source, window, cx)
        })
        .expect("the export dialog");
        app.export_to(cx, &dir.path().join("items.csv"));
        assert!(app.has(cx, "export-done"), "{engine:?}");
        open.push(("export", settle(cx, engine, ROLE, (2, 0))));
        app.click(cx, "export-close");
        left.push(("export", settle(cx, engine, ROLE, (2, 0))));

        let file = dir.path().join("more.csv");
        std::fs::write(&file, "id,name\n20,c\n").unwrap();
        let dialog = app
            .import_dialog(cx, ROLE, Some(table.clone()))
            .expect("the import dialog");
        app.import_file(cx, &dialog, &file);
        assert_eq!(app.run_import(cx, &dialog), "done: 1 rows", "{engine:?}");
        open.push(("import", settle(cx, engine, ROLE, (2, 0))));
        app.click(cx, "import-close");
        left.push(("import", settle(cx, engine, ROLE, (2, 0))));

        app.backup_dialog(cx, database, false)
            .expect("the backup dialog");
        open.push(("backup dialog", settle(cx, engine, ROLE, (2, 0))));
        app.click(cx, "backup-close");
        // The dialogs do not give the keyboard back to the window.
        app.step(cx, |window, cx| {
            app.workspace.focus_handle(cx).focus(window, cx)
        });
        left.push(("backup dialog", settle(cx, engine, ROLE, (2, 0))));

        // Several at once, one of them in a transaction.
        ran(&app, cx);
        in_transaction(&app, cx);
        app.open_table(cx, ROLE, "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–3 of 3");
        app.open_structure(cx, ROLE, "items", RelationKind::Table);
        app.wait_for_structure(cx);
        app.dispatch(cx, actions::OpenAdmin);
        app.wait_admin(cx, Pane::Sessions);
        open.push(("all of them", settle(cx, engine, ROLE, (6, 1))));
        app.press(cx, &["secondary-shift-w"]);
        assert!(app.shows_label(cx, "Roll back and close"), "{engine:?}");
        app.click(cx, "ok");
        assert!(app.titles(cx).is_empty(), "{engine:?}");
        left.push(("close all", settle(cx, engine, ROLE, (2, 0))));

        // Tabs open when the connection is closed let their sessions go too.
        ran(&app, cx);
        in_transaction(&app, cx);
        app.open_table(cx, ROLE, "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–3 of 3");
        app.dispatch(cx, actions::OpenAdmin);
        app.wait_admin(cx, Pane::Sessions);
        open.push(("before disconnecting", settle(cx, engine, ROLE, (6, 1))));
        // The open transaction is asked about first: Cancel keeps every
        // session, the transaction still open.
        let disconnect = |app: &Fixture, cx: &mut TestAppContext| {
            app.sidebar_does(
                cx,
                Subject::Connection(id.clone()),
                SidebarAction::Disconnect,
            );
            assert!(app.has(cx, "end-question"), "{engine:?}");
        };
        disconnect(&app, cx);
        app.click(cx, "end-cancel");
        open.push(("disconnect cancelled", settle(cx, engine, ROLE, (6, 1))));
        // Rolled back, everything goes.
        disconnect(&app, cx);
        app.click(cx, "end-rollback");
        left.push(("disconnected", settle(cx, engine, ROLE, (0, 0))));

        measured.push((engine, left, open));
    }
    for (engine, left, open) in &measured {
        eprintln!("{engine:?} sessions / idle in a transaction while open: {open:?}");
        eprintln!("{engine:?} left once gone: {left:?}");
    }
    for (engine, left, open) in measured {
        // The counter of transactions counts: the open one was seen.
        assert!(
            open.iter()
                .any(|(what, held)| *what == "manual commit" && held.1 == 1),
            "{engine:?}: {open:?}"
        );
        // A cancelled disconnection leaves every session as it was, the
        // transaction included.
        let held = |name: &str| {
            open.iter()
                .find(|(what, _)| *what == name)
                .map(|(_, held)| *held)
        };
        assert_eq!(
            held("disconnect cancelled"),
            held("before disconnecting"),
            "{engine:?}: {open:?}"
        );
        assert_eq!(
            held("disconnect cancelled").map(|held| held.1),
            Some(1),
            "{engine:?}"
        );
        let expected: Vec<_> = left
            .iter()
            .map(|(what, _)| {
                // The connection's own session and the tree's (D-087), then
                // what each owner held on top.
                let sessions = match *what {
                    "disconnected" => 0,
                    "moved in a transaction" => 3,
                    _ => 2,
                };
                (*what, (sessions, 0))
            })
            .collect();
        assert_eq!(left, expected, "{engine:?}; while open: {open:?}");
    }
}
#[gpui_kit::test]
fn the_new_tab_button_stays_whole_beside_many_tabs(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    for _ in 0..15 {
        app.press(cx, &["secondary-t"]);
    }
    let button = app.step(cx, |window, _| window.find("new-tab").bounds());
    assert!(
        button.left() >= px(0.) && button.right() <= px(1200.),
        "{button:?} in a window 1200 wide"
    );
    assert!(button.size.width >= px(16.), "{button:?}");
}

#[gpui_kit::test]
fn the_palette_lists_the_title_typed_before_keyword_matches(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    // "Settings" is the title of one command and a word or keyword of others.
    app.press(cx, &["secondary-k"]);
    app.type_text(cx, "settings");
    app.press(cx, &["enter"]);
    // The Settings dialog is open: its appearance choice works.
    app.step(cx, |window, cx| {
        window.within("appearance").click(2usize, cx)
    });
    cx.update(|cx| assert!(cx.theme().is_dark()));
}

#[gpui_kit::test]
fn a_failed_statement_is_marked_as_an_error(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(cx, "select nope_kerno_ui", "secondary-enter");
    app.wait_until(cx, "the failure", |app, cx| {
        !app.running(cx)
            && app
                .outcomes(cx)
                .iter()
                .any(|outcome| outcome.starts_with("failed"))
    });
    let tab = app.tab(cx);
    let severities: Vec<_> = cx.update(|cx| {
        tab.read(cx)
            .editor()
            .read(cx)
            .diagnostics()
            .map(|marks| marks.iter().map(|mark| mark.severity).collect())
            .unwrap_or_default()
    });
    assert_eq!(
        severities,
        [gpui_kit::base::input::DiagnosticSeverity::Error]
    );
}

#[gpui_kit::test]
fn a_data_tab_whose_table_is_gone_shows_no_rows_and_the_tree_forgets_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let schema = "kerno_ui_gone_rows";
    let (drop_schema, create_schema) = scratch_schema(Engine::Postgres, schema);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[&drop_schema],
        &[
            &create_schema,
            &scratch_table(Engine::Postgres, &format!("{schema}.items"), "name text"),
            &format!("INSERT INTO {schema}.items (name) VALUES ('a')"),
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let path = ["kerno", schema, "Tables", "items"];
    app.open_path(cx, &path);
    app.open_table(cx, schema, "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    on_server(Engine::Postgres, &format!("DROP TABLE {schema}.items"));
    app.click(cx, "data-refresh");
    app.wait_until(cx, "the failure", |app, cx| {
        app.data_state(cx).starts_with("failed")
    });
    // Nothing about rows: neither in the status bar nor in the tab's status.
    let tab = app.data(cx);
    let status = cx.update(|cx| tab.read(cx).status(cx));
    assert_eq!((status.rows, status.elapsed), (None, None));
    assert!(
        !app.status_bar(cx)
            .iter()
            .any(|item| item.ends_with(" rows")),
        "{:?}",
        app.status_bar(cx)
    );
    // No row can be added to a table that is not there.
    app.click(cx, "data-add-row");
    assert_eq!(app.pending(cx), 0);
    assert!(!app.has(cx, "data-fetch-more"), "no rows to fetch");
    // The tree reads the schema's tables again.
    app.wait_until(cx, "the tree to forget the table", |app, cx| {
        !app.shows(cx, &path)
    });
}

#[gpui_kit::test]
fn the_about_text_is_one_centred_block(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::About), cx)
    });
    let (version, what) = app.step(cx, |window, _| {
        (
            window.find("about-version").bounds(),
            window.find("about-what").bounds(),
        )
    });
    let centre =
        |bounds: gpui_kit::Bounds<gpui_kit::Pixels>| bounds.left() + bounds.size.width / 2.;
    assert!(
        (centre(version) - centre(what)).abs() < px(1.),
        "{version:?} {what:?}"
    );
}

// Maintenance tools (docs/plans/PARITY-GAPS.md G12) ----------------------------

impl Fixture {
    /// Opens the maintenance tools of the tree row at `path`, on the tool
    /// like `wanted`.
    fn tools_dialog(
        &self,
        cx: &mut TestAppContext,
        path: &[&str],
        wanted: Option<kerno_core::maintenance::Maintenance>,
    ) -> Option<Entity<crate::maintenance_dialog::MaintenanceDialog>> {
        let row = self.tree_row(cx, path);
        let navigator = cx.update(|cx| self.workspace.read(cx).navigator(cx));
        self.step(cx, |window, cx| {
            navigator.update(cx, |navigator, cx| {
                let id = navigator.id_at(row, cx);
                navigator.open_tools(&id, wanted.as_ref(), window, cx)
            })
        })
    }

    /// The tools of the tree row at `path`, by label.
    fn tools_at(&self, cx: &mut TestAppContext, path: &[&str]) -> Vec<&'static str> {
        let row = self.tree_row(cx, path);
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .navigator(cx)
                .read(cx)
                .tools_at(row, cx)
        })
    }

    fn tool_state(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<crate::maintenance_dialog::MaintenanceDialog>,
    ) -> String {
        cx.update(|cx| dialog.read(cx).state())
    }

    /// Runs the dialog's tool through its confirmation; its state once it ended.
    fn run_tool(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<crate::maintenance_dialog::MaintenanceDialog>,
    ) -> String {
        self.click(cx, "maintenance-run");
        assert!(
            self.has(cx, "sql-preview"),
            "the statement is confirmed first"
        );
        self.click(cx, "sql-confirm");
        self.wait_tool(cx, dialog)
    }

    /// The dialog's state once its run ended.
    fn wait_tool(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<crate::maintenance_dialog::MaintenanceDialog>,
    ) -> String {
        self.wait_until(cx, "the tool to end", |app, cx| {
            app.tool_state(cx, dialog) != "running"
        });
        self.tool_state(cx, dialog)
    }
}

#[gpui_kit::test]
fn postgres_tools_analyze_vacuum_refresh_switch_triggers_and_truncate(cx: &mut TestAppContext) {
    use kerno_core::maintenance::{Maintenance, RefreshOptions};
    if !testing::available(Engine::Postgres) {
        return;
    }
    let pg = |sql: &str| on_server(Engine::Postgres, sql);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_maint CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_maint",
            "CREATE TABLE kerno_ui_maint.items \
             (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, note text)",
            "INSERT INTO kerno_ui_maint.items (note) SELECT 'n' FROM generate_series(1, 5)",
            "CREATE MATERIALIZED VIEW kerno_ui_maint.stock AS SELECT id FROM kerno_ui_maint.items",
            "CREATE FUNCTION kerno_ui_maint.mark() RETURNS trigger LANGUAGE plpgsql \
             AS $$ BEGIN NEW.note := 'fired'; RETURN NEW; END $$",
            "CREATE TRIGGER mark BEFORE INSERT ON kerno_ui_maint.items \
             FOR EACH ROW EXECUTE FUNCTION kerno_ui_maint.mark()",
        ],
    );
    let estimate = || {
        pg("SELECT reltuples::bigint FROM pg_class WHERE oid = 'kerno_ui_maint.items'::regclass")
    };
    let app = connected(cx, Engine::Postgres);
    let items = ["kerno", "kerno_ui_maint", "Tables", "items"];
    app.open_path(cx, &items);

    // ANALYZE fills the estimate of a table never analysed, and reports
    // through notices.
    assert_eq!(estimate(), "-1");
    let dialog = app
        .tools_dialog(cx, &items, Some(Maintenance::Analyze))
        .expect("the dialog");
    assert_eq!(
        cx.update(|cx| dialog.read(cx).tools()),
        ["Vacuum", "Analyze", "Truncate"]
    );
    let state = app.tool_state(cx, &dialog);
    assert!(
        state.starts_with("options: ANALYZE VERBOSE ") && state.contains("items"),
        "{state}"
    );
    assert_eq!(app.run_tool(cx, &dialog), "done");
    let (_, notices) = cx.update(|cx| dialog.read(cx).report());
    assert!(
        notices.iter().any(|notice| notice.contains("analyzing")),
        "{notices:?}"
    );
    assert!(app.has(cx, "maintenance-outcome") && app.has(cx, "maintenance-log"));
    assert_eq!(estimate(), "5");
    app.click(cx, "maintenance-close");

    // VACUUM with ANALYZE, its options at their defaults first; FULL keeps
    // DISABLE_PAGE_SKIPPING off.
    pg("DELETE FROM kerno_ui_maint.items WHERE id > 3");
    let dialog = app.tools_dialog(cx, &items, None).expect("the dialog");
    assert!(
        app.tool_state(cx, &dialog)
            .starts_with("options: VACUUM (VERBOSE) ")
    );
    app.click(cx, ("maintenance-option", 2usize));
    app.click(cx, ("maintenance-option", 0usize));
    app.click(cx, ("maintenance-option", 3usize));
    assert!(
        app.tool_state(cx, &dialog)
            .starts_with("options: VACUUM (FULL, VERBOSE, ANALYZE) ")
    );
    app.click(cx, ("maintenance-option", 0usize));
    assert!(
        app.tool_state(cx, &dialog)
            .starts_with("options: VACUUM (VERBOSE, ANALYZE) ")
    );
    assert_eq!(app.run_tool(cx, &dialog), "done");
    assert_eq!(estimate(), "3");
    app.click(cx, "maintenance-close");

    // A refresh shows the rows added since; it keeps the data by default.
    pg("INSERT INTO kerno_ui_maint.items (note) VALUES ('new')");
    assert_eq!(pg("SELECT count(*) FROM kerno_ui_maint.stock"), "5");
    let stock = ["kerno", "kerno_ui_maint", "Materialized views", "stock"];
    app.open_path(cx, &stock);
    let dialog = app
        .tools_dialog(
            cx,
            &stock,
            Some(Maintenance::RefreshMaterializedView(
                RefreshOptions::default(),
            )),
        )
        .expect("the dialog");
    let state = app.tool_state(cx, &dialog);
    assert!(
        state.starts_with("options: REFRESH MATERIALIZED VIEW ") && state.ends_with(" WITH DATA"),
        "{state}"
    );
    // CONCURRENTLY needs a unique index, which the view lacks: the server's
    // error is shown, and Back offers the options again.
    app.click(cx, ("maintenance-option", 1usize));
    let failed = app.run_tool(cx, &dialog);
    assert!(
        failed.starts_with("failed: ") && failed.contains("unique index"),
        "{failed}"
    );
    assert!(app.has(cx, "maintenance-outcome"));
    app.click(cx, "maintenance-back");
    app.click(cx, ("maintenance-option", 1usize));
    assert!(
        !app.tool_state(cx, &dialog).contains("CONCURRENTLY"),
        "{}",
        app.tool_state(cx, &dialog)
    );
    assert_eq!(app.run_tool(cx, &dialog), "done");
    assert_eq!(pg("SELECT count(*) FROM kerno_ui_maint.stock"), "4");
    app.click(cx, "maintenance-close");

    // A trigger is disabled, then (shown as such in the tree) enabled again.
    let trigger = [
        "kerno",
        "kerno_ui_maint",
        "Tables",
        "items",
        "Triggers",
        "mark",
    ];
    let state = || {
        pg("SELECT tgenabled FROM pg_trigger WHERE tgname = 'mark' \
            AND tgrelid = 'kerno_ui_maint.items'::regclass")
    };
    app.open_path(cx, &trigger);
    assert_eq!(app.tools_at(cx, &trigger), ["Disable trigger"]);
    let dialog = app.tools_dialog(cx, &trigger, None).expect("the dialog");
    assert_eq!(app.run_tool(cx, &dialog), "done");
    assert_eq!(state(), "D");
    app.click(cx, "maintenance-close");
    app.wait_until(cx, "the tree to show the trigger disabled", |app, cx| {
        app.shows(cx, &trigger) && app.tools_at(cx, &trigger) == ["Enable trigger"]
    });
    let dialog = app.tools_dialog(cx, &trigger, None).expect("the dialog");
    assert_eq!(app.run_tool(cx, &dialog), "done");
    assert_eq!(state(), "O");
    app.click(cx, "maintenance-close");

    // "Truncate…" opens the same dialog, with PostgreSQL's options.
    app.act(cx, Action::Truncate, &items);
    assert!(app.has(cx, "maintenance-dialog") && app.has(cx, ("maintenance-option", 2usize)));
    app.click(cx, ("maintenance-option", 1usize));
    app.click(cx, "maintenance-run");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the truncate", |_, _| {
        pg("SELECT count(*) FROM kerno_ui_maint.items") == "0"
    });
    assert_eq!(
        pg("INSERT INTO kerno_ui_maint.items (note) VALUES ('x') RETURNING id"),
        "1",
        "RESTART IDENTITY starts the ids again"
    );
}

#[gpui_kit::test]
fn a_postgres_database_is_vacuumed_and_analysed_from_its_node(cx: &mut TestAppContext) {
    use kerno_core::maintenance::Maintenance;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let database = "kerno_ui_maint_db";
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP DATABASE IF EXISTS kerno_ui_maint_db WITH (FORCE)"],
        &["CREATE DATABASE kerno_ui_maint_db"],
    );
    on_database(
        Engine::Postgres,
        Some(database),
        "CREATE TABLE t AS SELECT generate_series(1, 30) AS n",
    );
    let app = connected(cx, Engine::Postgres);
    app.wait_until(cx, "the scratch database", |app, cx| {
        app.shows(cx, &[database])
    });
    assert_eq!(app.tools_at(cx, &[database]), ["Vacuum", "Analyze"]);
    let dialog = app
        .tools_dialog(cx, &[database], Some(Maintenance::Analyze))
        .expect("the dialog");
    // The whole database: the one the dialog's session is opened on.
    assert_eq!(app.tool_state(cx, &dialog), "options: ANALYZE VERBOSE");
    assert_eq!(app.run_tool(cx, &dialog), "done");
    assert_eq!(
        on_database(
            Engine::Postgres,
            Some(database),
            "SELECT reltuples::bigint FROM pg_class WHERE oid = 'public.t'::regclass"
        ),
        "30"
    );
    app.click(cx, "maintenance-back");
    app.click(cx, ("maintenance-tool", 0usize));
    assert_eq!(app.tool_state(cx, &dialog), "options: VACUUM (VERBOSE)");
    assert_eq!(app.run_tool(cx, &dialog), "done");
    let (_, notices) = cx.update(|cx| dialog.read(cx).report());
    assert!(!notices.is_empty());
    app.click(cx, "maintenance-close");
}

#[gpui_kit::test]
fn mysql_and_mariadb_tools_show_their_status_rows(cx: &mut TestAppContext) {
    use kerno_core::maintenance::Maintenance;
    for engine in [Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let server = |sql: &str| on_server(engine, sql);
        let _scratch = Scratch::new(
            engine,
            &["DROP DATABASE IF EXISTS kerno_ui_maint_my"],
            &[
                "CREATE DATABASE kerno_ui_maint_my",
                "CREATE TABLE kerno_ui_maint_my.t (id INT PRIMARY KEY) ENGINE = InnoDB",
                "INSERT INTO kerno_ui_maint_my.t VALUES (1), (2)",
            ],
        );
        let app = connected(cx, engine);
        let table = ["kerno_ui_maint_my", "Tables", "t"];
        app.open_path(cx, &table);
        assert_eq!(
            app.tools_at(cx, &table),
            ["Analyze", "Truncate", "Check", "Optimize", "Repair"],
            "{engine:?}"
        );
        let ends_ok = |rows: &[Vec<String>], op: &str| {
            rows.iter().all(|row| row[1] == op)
                && rows
                    .last()
                    .is_some_and(|row| row[2] == "status" && row[3] == "OK")
        };

        // OPTIMIZE answers with its status rows, shown as a table.
        let dialog = app
            .tools_dialog(cx, &table, Some(Maintenance::Optimize))
            .expect("the dialog");
        assert_eq!(
            app.tool_state(cx, &dialog),
            "options: OPTIMIZE TABLE `kerno_ui_maint_my`.`t`"
        );
        // Truncate is set apart: last, after a gap.
        let (truncate, repair) = app.step(cx, |window, _| {
            (
                window.find(("maintenance-tool", 1usize)).bounds(),
                window.find(("maintenance-tool", 4usize)).bounds(),
            )
        });
        assert!(
            truncate.left() >= repair.right() + px(12.),
            "{truncate:?} {repair:?}"
        );
        assert_eq!(app.run_tool(cx, &dialog), "done", "{engine:?}");
        let (rows, _) = cx.update(|cx| dialog.read(cx).report());
        assert!(ends_ok(&rows, "optimize"), "{engine:?}: {rows:?}");
        assert!(app.has(cx, "maintenance-rows") && app.has(cx, ("maintenance-row", 0usize)));

        // CHECK with one of its options.
        app.click(cx, "maintenance-back");
        app.click(cx, ("maintenance-tool", 2usize));
        app.click(cx, ("maintenance-check", 2usize));
        assert_eq!(
            app.tool_state(cx, &dialog),
            "options: CHECK TABLE `kerno_ui_maint_my`.`t` QUICK"
        );
        assert_eq!(app.run_tool(cx, &dialog), "done", "{engine:?}");
        let (rows, _) = cx.update(|cx| dialog.read(cx).report());
        assert!(ends_ok(&rows, "check"), "{engine:?}: {rows:?}");

        // ANALYZE ends OK; InnoDB has no REPAIR, which MySQL says in a note
        // (MariaDB answers OK): the rows are shown as returned.
        app.click(cx, "maintenance-back");
        app.click(cx, ("maintenance-tool", 0usize));
        assert_eq!(app.run_tool(cx, &dialog), "done", "{engine:?}");
        let (rows, _) = cx.update(|cx| dialog.read(cx).report());
        assert!(ends_ok(&rows, "analyze"), "{engine:?}: {rows:?}");
        app.click(cx, "maintenance-back");
        app.click(cx, ("maintenance-tool", 4usize));
        assert_eq!(
            app.tool_state(cx, &dialog),
            "options: REPAIR TABLE `kerno_ui_maint_my`.`t`"
        );
        assert_eq!(app.run_tool(cx, &dialog), "done", "{engine:?}");
        let (rows, _) = cx.update(|cx| dialog.read(cx).report());
        assert!(
            !rows.is_empty() && rows.iter().all(|row| row[1] == "repair"),
            "{engine:?}: {rows:?}"
        );

        // TRUNCATE has no option on MySQL.
        app.click(cx, "maintenance-back");
        app.click(cx, ("maintenance-tool", 1usize));
        assert!(!app.has(cx, ("maintenance-option", 0usize)));
        assert_eq!(app.run_tool(cx, &dialog), "done", "{engine:?}");
        assert_eq!(server("SELECT COUNT(*) FROM kerno_ui_maint_my.t"), "0");
        app.click(cx, "maintenance-close");
    }
}

#[gpui_kit::test]
fn the_tools_menu_and_command_open_the_dialog_and_read_only_or_production_guard_it(
    cx: &mut TestAppContext,
) {
    use kerno_core::maintenance::Maintenance;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_maint_menu CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_maint_menu",
            "CREATE TABLE kerno_ui_maint_menu.t AS SELECT generate_series(1, 3) AS n",
        ],
    );
    let table = ["kerno", "kerno_ui_maint_menu", "Tables", "t"];
    let right_click = |app: &Fixture, cx: &mut TestAppContext| {
        let row = app.tree_row(cx, &table);
        app.right_click(cx, ("node", row), None);
        click_menu_entry(app, cx, "Tools");
    };

    // A real right-click: Tools ▸ lists the table's tools and opens one.
    let app = connected(cx, Engine::Postgres);
    app.open_path(cx, &table);
    right_click(&app, cx);
    for label in ["Vacuum…", "Analyze…", "Truncate…"] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
    click_menu_entry(&app, cx, "Analyze…");
    assert!(app.has(cx, "maintenance-dialog"));
    app.click(cx, "maintenance-close");
    assert!(!app.has(cx, "maintenance-dialog"));

    // The palette command acts on the selected node, and says why a schema
    // has no tool.
    let palette = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::MaintenanceTools), cx)
        })
    };
    app.click_row(cx, &["kerno", "kerno_ui_maint_menu"], false);
    palette(&app, cx);
    assert!(!app.has(cx, "maintenance-dialog"));
    app.click_row(cx, &table, false);
    palette(&app, cx);
    assert!(app.has(cx, "maintenance-dialog"));
    app.click(cx, "maintenance-close");

    // Read-only: every tool is disabled with the reason, and none opens.
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    app.open_path(cx, &table);
    for action in [Action::Tools, Action::Truncate] {
        assert_eq!(
            app.why_not(cx, action, &table),
            Some("read-only connection")
        );
    }
    right_click(&app, cx);
    assert!(menu_entry(&app, cx, "Analyze… (read-only connection)").is_some());
    assert!(app.tools_dialog(cx, &table, None).is_none());
    app.act(cx, Action::Truncate, &table);
    assert!(!app.has(cx, "maintenance-dialog"));

    // Production: the confirmation asks twice.
    let mut config = testing::config(Engine::Postgres);
    config.environment = kerno_core::environment::PRODUCTION.into();
    let app = connected_as(cx, config);
    app.open_path(cx, &table);
    let dialog = app
        .tools_dialog(cx, &table, Some(Maintenance::Analyze))
        .expect("the dialog");
    app.click(cx, "maintenance-run");
    app.click(cx, "sql-confirm");
    assert!(app.has(cx, "sql-production"));
    assert!(app.tool_state(cx, &dialog).starts_with("options: "));
    app.click(cx, "sql-confirm");
    assert_eq!(app.wait_tool(cx, &dialog), "done");
}

#[gpui_kit::test]
fn a_vacuum_waiting_for_a_lock_is_cancelled(cx: &mut TestAppContext) {
    vacuum_behind_a_lock(cx, "kerno_ui_maint_cancel", false);
}

#[gpui_kit::test]
fn closing_the_dialog_stops_a_vacuum_waiting_for_a_lock(cx: &mut TestAppContext) {
    vacuum_behind_a_lock(cx, "kerno_ui_maint_close", true);
}

/// A vacuum that waits behind another session's lock, then stopped with the
/// Cancel button or, with `close`, by closing the dialog.
fn vacuum_behind_a_lock(cx: &mut TestAppContext, schema: &str, close: bool) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let pg = |sql: &str| on_server(Engine::Postgres, sql);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[&format!("DROP SCHEMA IF EXISTS {schema} CASCADE")],
        &[
            &format!("CREATE SCHEMA {schema}"),
            &format!("CREATE TABLE {schema}.t (n int)"),
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let table = ["kerno", schema, "Tables", "t"];
    app.open_path(cx, &table);
    // Another session holds the table for a few seconds.
    let lock = format!("LOCK TABLE {schema}.t IN ACCESS EXCLUSIVE MODE");
    let locker = std::thread::spawn(move || {
        use kerno_core::Driver as _;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let session = kerno_postgres::PostgresDriver
                .connect(&testing::params(Engine::Postgres))
                .await
                .expect("the test server");
            for sql in ["BEGIN", lock.as_str(), "SELECT pg_sleep(4)", "COMMIT"] {
                kerno_core::exec::collect(session.clone(), sql)
                    .await
                    .unwrap_or_else(|error| panic!("{sql}: {error}"));
            }
            session.close().await;
        })
    });
    let locks = format!(
        "SELECT count(*) FROM pg_locks l JOIN pg_class c ON c.oid = l.relation \
         WHERE c.relname = 't' AND l.mode = 'AccessExclusiveLock' \
         AND c.relnamespace = '{schema}'::regnamespace"
    );
    app.wait_until(cx, "the lock", |_, _| pg(&locks) == "1");
    let dialog = app.tools_dialog(cx, &table, None).expect("the dialog");
    app.click(cx, "maintenance-run");
    app.click(cx, "sql-confirm");
    let waiting = format!(
        "SELECT count(*) FROM pg_stat_activity \
         WHERE query LIKE 'VACUUM%{schema}%' AND wait_event_type = 'Lock'"
    );
    app.wait_until(cx, "the vacuum to wait for the lock", |_, _| {
        pg(&waiting) == "1"
    });
    assert_eq!(app.tool_state(cx, &dialog), "running");
    if close {
        // Nothing but the dialog holds its view: closed, it is gone, and
        // the statement is stopped while the lock is still held.
        drop(dialog);
        app.step(cx, |window, cx| {
            use gpui_kit::component::WindowExt as _;
            window.close_dialog(cx);
        });
        app.wait_until(cx, "the vacuum to be stopped", |_, _| pg(&waiting) == "0");
        assert_eq!(
            pg(&locks),
            "1",
            "stopped by the cancel, not by the lock's end"
        );
    } else {
        app.click(cx, "maintenance-cancel");
        assert_eq!(app.wait_tool(cx, &dialog), "cancelled");
        assert!(app.has(cx, "maintenance-back"));
    }
    locker.join().expect("the lock was held and released");
}

// Ending what sessions hold; focus after dialogs; tag colours; menus (M10
// Part 4 follow-ups, G13 leftovers) ------------------------------------------

impl Fixture {
    /// Whether the active query tab's editor has the keyboard.
    fn editor_has_keyboard(&self, cx: &mut TestAppContext) -> bool {
        let tab = self.tab(cx);
        self.step(cx, |window, cx| {
            tab.read(cx).focus_handle(cx).contains_focused(window, cx)
        })
    }

    /// Whether a dialog (or alert, or the palette) is open.
    fn dialog_open(&self, cx: &mut TestAppContext) -> bool {
        use gpui_kit::component::WindowExt as _;
        self.step(cx, |window, cx| window.has_active_dialog(cx))
    }

    fn tx_state(&self, cx: &mut TestAppContext) -> kerno_core::TxState {
        let tab = self.tab(cx);
        cx.update(|cx| tab.read(cx).tx_state())
    }

    /// Types `sql` in place of the active editor's text and runs it in a
    /// transaction of its own, which stays open.
    fn open_transaction(&self, cx: &mut TestAppContext, sql: &str) {
        let tab = self.tab(cx);
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(true, cx)));
        self.press(cx, &["secondary-a", "backspace"]);
        self.type_text(cx, sql);
        self.press(cx, &["secondary-enter"]);
        self.wait_until(cx, "the open transaction", |app, cx| {
            !app.running(cx) && app.tx_state(cx) == kerno_core::TxState::Open
        });
    }
}

#[gpui_kit::test]
fn disconnecting_ends_open_transactions_only_as_answered(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let server = |sql: &str| on_server(Engine::Postgres, sql);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE query LIKE '%kerno_ui_end.entries values%' AND pid <> pg_backend_pid()",
            "DROP SCHEMA IF EXISTS kerno_ui_end CASCADE",
        ],
        &[
            "CREATE SCHEMA kerno_ui_end",
            "CREATE TABLE kerno_ui_end.entries (id int PRIMARY KEY, note text)",
            "INSERT INTO kerno_ui_end.entries VALUES (1, 'kept')",
        ],
    );
    let count = || server("SELECT count(*) FROM kerno_ui_end.entries");
    let app = connected(cx, Engine::Postgres);

    // A transaction left open, and a table with a change not applied.
    app.press(cx, &["secondary-t"]);
    let query = app.tab(cx);
    app.open_transaction(cx, "insert into kerno_ui_end.entries values (2, 'open')");
    app.open_table(cx, "kerno_ui_end", "entries", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    app.type_in_cell(cx, 0, 2, "changed", "enter");
    assert_eq!(app.pending(cx), 1);

    // Disconnect from the sidebar's menu: one question lists both; Enter
    // does not answer it, Cancel changes nothing.
    app.right_click(cx, ("connection", 0usize), None);
    app.click_label(cx, "Disconnect");
    assert!(app.has(cx, "end-question"));
    for line in [
        "“Query 1”: a transaction is open.",
        "“entries”: 1 pending change is discarded.",
        "Commit and continue",
        "Roll back and continue",
    ] {
        assert!(app.shows_label(cx, line), "{line}");
    }
    app.press(cx, &["enter"]);
    assert!(app.has(cx, "end-question"));
    app.click(cx, "end-cancel");
    assert!(!app.dialog_open(cx));
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    assert_eq!(app.pending(cx), 1);
    assert_eq!(
        cx.update(|cx| query.read(cx).tx_state()),
        kerno_core::TxState::Open
    );
    assert_eq!(count(), "1");

    // Reconnect from the status bar, committing first.
    app.right_click(cx, "status-connection", None);
    app.click_label(cx, "Reconnect");
    app.click(cx, "end-commit");
    app.wait_until(cx, "the commit, then the reconnection", |app, cx| {
        app.pending(cx) == 0 && matches!(app.status(cx, 0), Status::Connected { .. })
    });
    assert_eq!(count(), "2");
    assert_eq!(
        cx.update(|cx| query.read(cx).tx_state()),
        kerno_core::TxState::Idle
    );

    // Disconnect all from the palette, rolling back; the tab does not say
    // the server did it.
    app.click(cx, ("tab-title", 0usize));
    app.open_transaction(
        cx,
        "insert into kerno_ui_end.entries values (3, 'rolled back')",
    );
    app.dispatch(cx, actions::DisconnectAll);
    assert!(app.shows_label(cx, "Disconnect all connections?"));
    app.click(cx, "end-rollback");
    app.wait_until(cx, "the rollback, then the disconnection", |app, cx| {
        app.status(cx, 0) == Status::Disconnected
    });
    assert_eq!(count(), "2");
    let messages = cx.update(|cx| query.read(cx).messages());
    assert!(
        !messages
            .iter()
            .any(|message| message.contains("rolled it back")),
        "{messages:?}"
    );
    // With nothing open, Disconnect all says so.
    app.dispatch(cx, actions::DisconnectAll);
    assert!(!app.dialog_open(cx));
}

/// Two connections of the test server: the first in its usual auto-commit,
/// the second named `second` and set to start in manual commit.
fn two_connections(cx: &mut TestAppContext, second: &str) -> Fixture {
    let storage = memory_storage();
    let first = storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let manual = ConnectionConfig {
        name: second.into(),
        auto_commit: Some(false),
        ..testing::config(Engine::Postgres)
    };
    let other = storage.save_connection(None, &manual).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    for id in [&first, &other] {
        kerno_core::secrets::save_secrets(secrets.as_ref(), id, &testing::secrets()).unwrap();
    }
    for row in [0usize, 1] {
        app.open_connection_row(cx, row);
    }
    app.wait_until(cx, "both connections", |app, cx| {
        (0..2).all(|index| matches!(app.status(cx, index), Status::Connected { .. }))
    });
    app
}

#[gpui_kit::test]
fn a_tab_moved_with_a_transaction_asks_and_takes_its_new_commit_mode(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let server = |sql: &str| on_server(Engine::Postgres, sql);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE query LIKE '%kerno_ui_move.entries values%' AND pid <> pg_backend_pid()",
            "DROP SCHEMA IF EXISTS kerno_ui_move CASCADE",
        ],
        &[
            "CREATE SCHEMA kerno_ui_move",
            "CREATE TABLE kerno_ui_move.entries (id int PRIMARY KEY)",
        ],
    );
    let app = two_connections(cx, "kerno_ui_move manual");
    let (first, second) = cx.update(|cx| {
        let saved = app.connections.read(cx).saved();
        (saved[0].id.clone(), saved[1].id.clone())
    });
    let on = |id: &str| Target {
        connection: id.to_string(),
        database: None,
        schema: None,
    };
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    cx.update(|cx| tab.update(cx, |tab, cx| tab.set_target(Some(on(&first)), cx)));
    assert!(!cx.update(|cx| tab.read(cx).manual_commit()));
    app.open_transaction(cx, "insert into kerno_ui_move.entries values (1)");

    // The toolbar's connection selector asks; Cancel keeps the tab where
    // it was, its transaction open.
    let connection_of = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            tab.read(cx)
                .target()
                .map(|target| target.connection.clone())
        })
    };
    app.click(cx, "tab-connection");
    app.click_menu_label(cx, "kerno_ui_move manual");
    assert!(app.shows_label(cx, "Switch “Query 1” to “kerno_ui_move manual”?"));
    app.click(cx, "end-cancel");
    assert_eq!(connection_of(cx), Some(first.clone()));
    assert_eq!(app.tx_state(cx), kerno_core::TxState::Open);

    // Rolled back, the tab moves and starts in the manual commit of its new
    // connection.
    app.click(cx, "tab-connection");
    app.click_menu_label(cx, "kerno_ui_move manual");
    app.click(cx, "end-rollback");
    app.wait_until(cx, "the move", |_, cx| {
        connection_of(cx) == Some(second.clone())
    });
    assert_eq!(app.tx_state(cx), kerno_core::TxState::Idle);
    assert!(cx.update(|cx| tab.read(cx).manual_commit()));
    assert_eq!(server("SELECT count(*) FROM kerno_ui_move.entries"), "0");

    // Without a transaction it moves at once, and takes auto-commit back.
    let first_name = cx.update(|cx| app.connections.read(cx).saved()[0].config.name.clone());
    app.click(cx, "tab-connection");
    app.click_menu_label(cx, &first_name);
    assert!(!app.dialog_open(cx));
    assert_eq!(connection_of(cx), Some(first));
    assert!(!cx.update(|cx| tab.read(cx).manual_commit()));
}

#[gpui_kit::test]
fn a_lost_connection_says_the_server_rolled_its_transaction_back(cx: &mut TestAppContext) {
    // A role of its own: its sessions are the ones cut.
    const ROLE: &str = "kerno_ui_lost";
    const PASSWORD: &str = "kerno_ui_lost_pw";
    if !testing::available(Engine::Postgres) {
        return;
    }
    let server = |sql: &str| on_server(Engine::Postgres, sql);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[
            &format!(
                "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE usename = '{ROLE}'"
            ),
            &format!("DROP SCHEMA IF EXISTS {ROLE} CASCADE"),
            &format!("DROP ROLE IF EXISTS {ROLE}"),
        ],
        &[
            &format!("CREATE ROLE {ROLE} LOGIN PASSWORD '{PASSWORD}'"),
            &format!("CREATE SCHEMA {ROLE} AUTHORIZATION {ROLE}"),
            &format!("CREATE TABLE {ROLE}.entries (id int PRIMARY KEY)"),
            &format!("ALTER TABLE {ROLE}.entries OWNER TO {ROLE}"),
        ],
    );
    let storage = memory_storage();
    let config = ConnectionConfig {
        user: ROLE.into(),
        ..testing::config(Engine::Postgres)
    };
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let password = Secrets {
        password: Some(Secret::new(PASSWORD)),
        ..Secrets::default()
    };
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &password).unwrap();
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    app.open_transaction(cx, &format!("insert into {ROLE}.entries values (1)"));

    // The server goes away: nothing can ask, the tab says what happened.
    server(&format!(
        "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE usename = '{ROLE}'"
    ));
    app.dispatch(cx, actions::RefreshConnections);
    app.wait_until(cx, "the tab to say so", |_, cx| {
        cx.update(|cx| tab.read(cx).messages())
            .iter()
            .any(|message| message.contains("the server rolled it back"))
    });
    app.wait_until(cx, "the reconnection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    assert_eq!(app.tx_state(cx), kerno_core::TxState::Idle);
    assert_eq!(server(&format!("SELECT count(*) FROM {ROLE}.entries")), "0");
}

#[gpui_kit::test]
fn read_only_is_toggled_saved_and_reopens_the_sessions(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let session_read_only = |app: &Fixture, cx: &mut TestAppContext| {
        app.press(cx, &["secondary-a", "backspace"]);
        app.type_text(cx, "show transaction_read_only");
        app.press(cx, &["secondary-enter"]);
        app.wait_until(cx, "the setting", |app, cx| {
            !app.running(cx) && app.outcomes(cx) == ["1 rows"]
        });
        app.result(cx, 0, 1).1[0][0].clone().unwrap()
    };
    app.press(cx, &["secondary-t"]);
    assert_eq!(session_read_only(&app, cx), "off");

    // From the connection's menu: a checked entry, saved, every session
    // reopened read-only.
    app.right_click(cx, ("connection", 0usize), None);
    app.click_label(cx, "Read-only");
    app.wait_until(cx, "the reconnection", |app, cx| {
        app.first_stored().read_only && matches!(app.status(cx, 0), Status::Connected { .. })
    });
    assert!(app.status_bar(cx).contains(&"read-only".to_string()));
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let sidebar = cx.update(|cx| app.workspace.read(cx).sidebar());
    let entries = cx.update(|cx| sidebar.read(cx).menu_of(&Subject::Connection(id), cx));
    assert!(entries.contains(&crate::sidebar::Entry::Item(
        SidebarAction::ReadOnly(true),
        None
    )));
    assert_eq!(session_read_only(&app, cx), "on");

    // From the palette, with a transaction open: it asks first.
    app.open_transaction(cx, "select 1");
    app.dispatch(cx, actions::ToggleReadOnly);
    assert!(app.has(cx, "end-question"));
    app.click(cx, "end-cancel");
    assert!(app.first_stored().read_only);
    app.dispatch(cx, actions::ToggleReadOnly);
    app.click(cx, "end-rollback");
    app.wait_until(cx, "changes allowed again", |app, cx| {
        !app.first_stored().read_only && matches!(app.status(cx, 0), Status::Connected { .. })
    });
    let tab = app.tab(cx);
    cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(false, cx)));
    assert_eq!(session_read_only(&app, cx), "off");
}

#[gpui_kit::test]
fn tabs_and_the_status_bar_carry_the_connection_colour(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let tagged = ConnectionConfig {
        name: "tagged".into(),
        color: Some("blue".into()),
        ..testing::config(Engine::Postgres)
    };
    let production = ConnectionConfig {
        name: "production".into(),
        environment: kerno_core::environment::PRODUCTION.into(),
        ..testing::config(Engine::Postgres)
    };
    let tagged_id = storage.save_connection(None, &tagged).unwrap();
    let production_id = storage.save_connection(None, &production).unwrap();
    let query = |title: &str, id: &str| TabSnapshot::Query {
        title: title.into(),
        sql: String::new(),
        target: Some(Target {
            connection: id.into(),
            database: None,
            schema: None,
        }),
        file: None,
        unsaved: false,
        source: None,
    };
    storage
        .save_tabs(&[
            query("Query 1", &tagged_id),
            query("Query 2", &production_id),
            TabSnapshot::Query {
                title: "Query 3".into(),
                sql: String::new(),
                target: None,
                file: None,
                unsaved: false,
                source: None,
            },
        ])
        .unwrap();
    let app = open(cx, storage);
    // A dot for a tagged connection only, the engine's logo on each tab of
    // a connection, nothing on a tab without one; production's title takes
    // its environment's colour instead.
    assert!(app.has(cx, ("tab-dot", 0usize)));
    assert!(!app.has(cx, ("tab-dot", 1usize)));
    assert!(!app.has(cx, ("tab-dot", 2usize)));
    assert!(app.has(cx, ("tab-logo", 0usize)));
    assert!(app.has(cx, ("tab-logo", 1usize)));
    assert!(!app.has(cx, ("tab-logo", 2usize)));
    assert!(app.has(cx, ("tab-env", 1usize)));
    assert!(!app.has(cx, ("tab-env", 0usize)));
    // The dot's colour is the tag's; without a tag there is none.
    cx.update(|cx| {
        assert_eq!(
            theme::connection_color(&tagged, cx),
            theme::tag_color("blue", cx)
        );
        assert_eq!(theme::connection_color(&production, cx), None);
    });
    // The status bar's connection item carries it too, and the engine's
    // logo before the name.
    app.click(cx, ("tab-title", 0usize));
    assert!(app.has(cx, "status-connection-dot"));
    let (logo, name) = app.step(cx, |window, _| {
        (
            window.find("status-connection-logo").bounds(),
            window.find("status-connection").bounds(),
        )
    });
    assert!(logo.left() >= name.left() && logo.right() <= name.right());
    app.click(cx, ("tab-title", 2usize));
    assert!(!app.has(cx, "status-connection-dot"));
    assert!(!app.has(cx, "status-connection-logo"));
}

#[gpui_kit::test]
fn an_incomplete_result_says_so_and_runs_again_from_its_menu(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select generate_series(1, 5000) as kerno_ui_incomplete",
        "secondary-enter",
    );
    app.wait_until(cx, "the first page", |app, cx| {
        !app.running(cx) && app.outcomes(cx) == ["1000 rows, more"]
    });
    assert!(!app.has(cx, ("result-incomplete", 0usize)));

    // Another statement on the session, from the toolbar's Run dropdown,
    // keeps the first result: its paging stops.
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "select 1 as kerno_ui_other");
    app.click(cx, "tab-run-menu");
    for label in [
        "Run script from the cursor",
        "Count rows of statement",
        "Go to next statement",
        "Toggle results panel",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.click_label(cx, "Run statement in new result tab");
    app.wait_until(cx, "the second result", |app, cx| {
        !app.running(cx) && app.outcomes(cx).len() == 2
    });
    assert_eq!(app.outcomes(cx), ["1000 rows, incomplete", "1 rows"]);
    assert!(app.has(cx, ("result-incomplete", 0usize)));

    // Its menu runs it again, in its place.
    app.right_click(cx, ("result-tab", 0usize), None);
    app.click_label(cx, "Run again");
    app.wait_until(cx, "the run again", |app, cx| {
        !app.running(cx) && app.outcomes(cx) == ["1 rows", "1000 rows, more"]
    });
    assert!(!app.has(cx, ("result-incomplete", 0usize)));
    assert!(!app.has(cx, ("result-incomplete", 1usize)));

    // The editor's menu has G4's commands too, the rarer ones in Execute ▸.
    app.right_click(cx, "query-editor", Some(gpui_kit::point(px(120.), px(10.))));
    for label in [
        "Go to previous statement",
        "Maximize results panel",
        "Focus results / editor",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    click_menu_entry(&app, cx, "Execute");
    for label in [
        "Run statement in new result tab",
        "Run script from the cursor",
        "Count rows of statement",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    click_menu_entry(&app, cx, "Count rows of statement");
    let tab = app.tab(cx);
    app.wait_until(cx, "the count", |_, cx| {
        cx.update(|cx| tab.read(cx).messages())
            .iter()
            .any(|message| message.contains("1 row in the result"))
    });
}

#[gpui_kit::test]
fn the_keyboard_comes_back_after_every_dialog_and_the_palette(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    // After each: the editor has the keyboard, and a global key works.
    let back = |app: &Fixture, cx: &mut TestAppContext, what: &str| {
        assert!(!app.dialog_open(cx), "{what}: still open");
        assert!(
            app.editor_has_keyboard(cx),
            "{what}: the editor lost the keyboard"
        );
        let open = app.titles(cx).len();
        app.press(cx, &["secondary-t"]);
        assert_eq!(app.titles(cx).len(), open + 1, "{what}: Cmd+T did nothing");
        app.press(cx, &["secondary-w"]);
        assert_eq!(app.titles(cx).len(), open, "{what}");
        assert!(app.editor_has_keyboard(cx), "{what}");
    };
    let replace = |app: &Fixture, cx: &mut TestAppContext, sql: &str| {
        app.press(cx, &["secondary-a", "backspace"]);
        app.type_text(cx, sql);
    };

    app.press(cx, &["secondary-,"]);
    assert!(app.dialog_open(cx));
    app.press(cx, &["escape"]);
    back(&app, cx, "settings");

    app.dispatch(cx, actions::About);
    app.click(cx, "ok");
    back(&app, cx, "about");

    app.press(cx, &["secondary-n"]);
    app.click(cx, "conn-cancel");
    back(&app, cx, "the connection dialog");

    replace(&app, cx, "drop table if exists kerno_ui_focus_nothing");
    app.press(cx, &["secondary-enter"]);
    app.click(cx, "sql-cancel");
    back(&app, cx, "the SQL confirmation");

    replace(&app, cx, "select 1 as kerno_ui_focus");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["1 rows"]);
    app.click(cx, "result-export");
    app.click(cx, "export-close");
    back(&app, cx, "export");

    app.dispatch(cx, actions::BackupDatabase);
    app.click(cx, "backup-close");
    back(&app, cx, "backup");

    app.press(cx, &["secondary-k"]);
    app.press(cx, &["escape"]);
    back(&app, cx, "the palette");
    // A command of the palette that opens a dialog.
    app.press(cx, &["secondary-k"]);
    app.type_text(cx, "Settings");
    app.press(cx, &["enter"]);
    assert!(app.dialog_open(cx));
    app.press(cx, &["escape"]);
    back(&app, cx, "settings from the palette");

    // The close question and the end question, with a transaction open.
    app.open_transaction(cx, "select 1 as kerno_ui_focus_tx");
    app.press(cx, &["secondary-w"]);
    app.click(cx, "cancel");
    back(&app, cx, "the close question");
    app.dispatch(cx, actions::DisconnectTabConnection);
    app.click(cx, "end-cancel");
    back(&app, cx, "the end question");
    app.dispatch(cx, actions::RollbackTransaction);
}

#[gpui_kit::test]
fn disconnect_others_and_a_folder_close_only_those_connections(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = two_connections(cx, "kerno_ui_others second");
    let (first, second) = cx.update(|cx| {
        let saved = app.connections.read(cx).saved();
        (saved[0].id.clone(), saved[1].id.clone())
    });
    let open = |app: &Fixture, cx: &mut TestAppContext, index| {
        matches!(app.status(cx, index), Status::Connected { .. })
    };

    // Disconnect others (CON-34): the second closes, the first stays.
    app.sidebar_does(
        cx,
        Subject::Connection(first.clone()),
        SidebarAction::DisconnectOthers,
    );
    app.wait_until(cx, "the other to close", |app, cx| {
        app.status(cx, 1) == Status::Disconnected
    });
    assert!(open(&app, cx, 0));

    // A folder's Disconnect (CM-12): only what the folder holds.
    cx.update(|cx| {
        app.connections.update(cx, |connections, cx| {
            connections.move_connection(&second, Some("kerno_ui_folder"), cx)
        })
    });
    app.sidebar_does(cx, Subject::Connection(second), SidebarAction::Connect);
    app.wait_until(cx, "the second again", |app, cx| open(app, cx, 1));
    app.sidebar_does(
        cx,
        Subject::Folder("kerno_ui_folder".into()),
        SidebarAction::Disconnect,
    );
    app.wait_until(cx, "the folder's connection to close", |app, cx| {
        app.status(cx, 1) == Status::Disconnected
    });
    assert!(open(&app, cx, 0));
}

// Secrets never reach the disk or the log (M10 Part 2) -------------------------

/// The files under `dir`, at any depth.
fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.push(path);
        }
    }
    files
}

#[gpui_kit::test]
fn no_secret_is_written_to_the_data_directory_or_the_log(cx: &mut TestAppContext) {
    const ROLE: &str = "kerno_ui_secrets";
    const PASSWORD: &str = "kerno-ui-Pw-4f7c1e";
    const WRONG: &str = "kerno-ui-Wrong-9b2d0a";
    const PASSPHRASE: &str = "kerno-ui-Phrase-61e8c3";
    // In a statement the user runs: the history keeps it redacted.
    const LITERAL: &str = "kerno-ui-Literal-d05a7f";
    // In a bootstrap statement: stored with the connection, never logged.
    const BOOTSTRAP: &str = "kerno_ui_boot_3c9e51";
    const MARKER: &str = "kerno-ui-log-marker";
    if !testing::available(Engine::Postgres) {
        return;
    }
    // Declared first, dropped last: after the app's sessions.
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[
            &format!(
                "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE usename = '{ROLE}'"
            ),
            &format!("DROP ROLE IF EXISTS {ROLE}"),
        ],
        &[&format!("CREATE ROLE {ROLE} LOGIN PASSWORD '{PASSWORD}'")],
    );
    // The store and the logs in one data directory, as the app has them.
    let data = tempfile::tempdir().unwrap();
    let paths = AppPaths::under(data.path().to_path_buf());
    let app = open(cx, Arc::new(Storage::open(&paths.database()).unwrap()));
    // Kerno's own lines at their most verbose.
    cx.update(|cx| {
        crate::logging::init_with(
            &paths.logs(),
            tracing_subscriber::EnvFilter::new("warn,kerno=debug"),
            cx,
        )
    })
    .unwrap();

    let save =
        |app: &Fixture, cx: &mut TestAppContext, config: ConnectionConfig, secrets: Secrets| {
            let before = cx.update(|cx| app.connections.read(cx).saved().len());
            let connections = app.connections.clone();
            cx.update(|cx| {
                connections.update(cx, |connections, cx| {
                    connections.save(None, config, secrets, None, false, cx)
                })
            });
            app.wait_until(cx, "the save", |app, cx| {
                cx.update(|cx| app.connections.read(cx).saved().len() == before + 1)
            });
            cx.update(|cx| app.connections.read(cx).saved()[before].id.clone())
        };
    let connect = |app: &Fixture, cx: &mut TestAppContext, id: &str| {
        let connections = app.connections.clone();
        cx.update(|cx| connections.update(cx, |connections, cx| connections.connect(id, cx)));
    };
    let as_role = |name: &str| ConnectionConfig {
        name: name.into(),
        user: ROLE.into(),
        ..testing::config(Engine::Postgres)
    };
    let password = |secret: &str| Secrets {
        password: Some(Secret::new(secret)),
        ..Secrets::default()
    };

    // Connect with the password and run a statement holding one.
    let config = ConnectionConfig {
        init_sql: vec![format!("SET application_name = '{BOOTSTRAP}'")],
        ..as_role("kerno_ui right")
    };
    let id = save(&app, cx, config, password(PASSWORD));
    connect(&app, cx, &id);
    app.wait_until(cx, "the connection", |app, cx| {
        let status = app.status(cx, 0);
        assert!(!matches!(status, Status::Failed { .. }), "{status:?}");
        matches!(status, Status::Connected { .. })
    });
    let statement = format!("ALTER ROLE {ROLE} PASSWORD '{LITERAL}'");
    app.run_in_new_tab(cx, &statement, "secondary-enter");
    app.wait_until(cx, "the history", |app, _| {
        !app.storage.history(None, "", 10).unwrap().is_empty()
    });
    assert_eq!(
        app.storage.history(None, "", 10).unwrap()[0].sql,
        format!("ALTER ROLE {ROLE} PASSWORD '***'")
    );

    // A wrong password fails; an SSH key passphrase is saved.
    let id = save(&app, cx, as_role("kerno_ui wrong"), password(WRONG));
    connect(&app, cx, &id);
    app.wait_until(cx, "the refusal", |app, cx| {
        matches!(app.status(cx, 1), Status::Failed { .. })
    });
    let tunnel = ConnectionConfig {
        ssh: Some(testing::ssh(kerno_core::config::SshAuth::KeyFile {
            path: "/nonexistent/kerno-ui/id_ed25519".into(),
        })),
        ..as_role("kerno_ui tunnel")
    };
    let passphrase = Secrets {
        ssh_key_passphrase: Some(Secret::new(PASSPHRASE)),
        ..password(PASSWORD)
    };
    save(&app, cx, tunnel, passphrase);

    let export = tempfile::tempdir().unwrap();
    let file = export.path().join("connections.json");
    let (workspace, target) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.export_connections_to(target, window, cx)
        });
    });
    app.wait_until(cx, "the export", |_, _| file.exists());

    // Quit: the window goes, the log writer writes what it holds.
    tracing::warn!(marker = MARKER, "end of the secrets test");
    cx.update_window(app.window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    cx.update(crate::logging::flush);

    let contains = |bytes: &[u8], text: &str| {
        bytes
            .windows(text.len())
            .any(|window| window == text.as_bytes())
    };
    let mut files = files_under(data.path());
    files.extend(files_under(app._data_dir.path()));
    files.push(file);
    assert!(files.contains(&paths.database()), "{files:?}");
    let logs: Vec<_> = files
        .iter()
        .filter(|file| file.starts_with(paths.logs()))
        .collect();
    assert_eq!(logs.len(), 1, "{files:?}");
    assert!(contains(&std::fs::read(logs[0]).unwrap(), MARKER));
    for file in &files {
        let bytes = std::fs::read(file).unwrap();
        for secret in [PASSWORD, WRONG, PASSPHRASE] {
            assert!(!contains(&bytes, secret), "{secret} in {}", file.display());
        }
    }
    // The statement's text stays in its tab, which is restored after a
    // restart; the history and the log never get it.
    let log = std::fs::read(logs[0]).unwrap();
    for text in [LITERAL, BOOTSTRAP] {
        assert!(!contains(&log, text), "{text} in the log");
    }
    let exported = std::fs::read(export.path().join("connections.json")).unwrap();
    assert!(!contains(&exported, LITERAL));
}

// M10 Part 3 — settings, keymap, keyboard coverage; the last paths that ended
// a connection's sessions without asking ---------------------------------------

/// A scratch schema `schema` with a one-column `entries` table; its cleanup
/// first ends the sessions a failed test left inside a transaction on it.
fn entries_scratch(schema: &str) -> Scratch {
    Scratch::new(
        Engine::Postgres,
        &[
            &format!(
                "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
                 WHERE query LIKE '%{schema}.entries values%' AND pid <> pg_backend_pid()"
            ),
            &format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
        ],
        &[
            &format!("CREATE SCHEMA {schema}"),
            &format!("CREATE TABLE {schema}.entries (id int PRIMARY KEY)"),
        ],
    )
}

#[gpui_kit::test]
fn deleting_an_open_connection_asks_about_its_transaction(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = entries_scratch("kerno_ui_delete_conn");
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let query = app.tab(cx);
    app.open_transaction(cx, "insert into kerno_ui_delete_conn.entries values (1)");

    // Confirmed in the alert, the deletion still asks about the transaction;
    // Cancel keeps the connection, its session and its transaction.
    let delete = |app: &Fixture, cx: &mut TestAppContext| {
        app.right_click(cx, ("connection", 0usize), None);
        app.click_label(cx, "Delete…");
        app.click(cx, "ok");
    };
    delete(&app, cx);
    assert!(app.has(cx, "end-question"));
    assert!(app.shows_label(cx, "“Query 1”: a transaction is open."));
    app.click(cx, "end-cancel");
    assert!(!app.dialog_open(cx));
    assert_eq!(app.storage.connections().unwrap().len(), 1);
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    assert_eq!(
        cx.update(|cx| query.read(cx).tx_state()),
        kerno_core::TxState::Open
    );

    // Rolled back, the connection goes.
    delete(&app, cx);
    app.click(cx, "end-rollback");
    app.wait_until(cx, "the deletion", |app, _| {
        app.storage.connections().unwrap().is_empty()
    });
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM kerno_ui_delete_conn.entries"
        ),
        "0"
    );
}

#[gpui_kit::test]
fn saving_an_open_connection_asks_before_it_reconnects(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = entries_scratch("kerno_ui_save_conn");
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let query = app.tab(cx);
    app.open_transaction(cx, "insert into kerno_ui_save_conn.entries values (1)");

    let save_and_connect = |app: &Fixture, cx: &mut TestAppContext| {
        app.right_click(cx, ("connection", 0usize), None);
        app.click_label(cx, "Edit…");
        app.wait_until(cx, "the connection dialog", |app, cx| {
            app.has(cx, "conn-save-connect")
        });
        app.click(cx, "conn-save-connect");
        app.wait_until(cx, "the question", |app, cx| app.has(cx, "end-question"));
    };
    // Cancel: saved, but the connection, its session and its transaction stay.
    save_and_connect(&app, cx);
    app.click(cx, "end-cancel");
    assert!(!app.dialog_open(cx));
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    assert_eq!(
        cx.update(|cx| query.read(cx).tx_state()),
        kerno_core::TxState::Open
    );
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM pg_stat_activity WHERE state LIKE 'idle in transaction%' \
             AND query LIKE '%kerno_ui_save_conn.entries values%'"
        ),
        "1"
    );

    // Rolled back, it reconnects with the saved settings.
    save_and_connect(&app, cx);
    app.click(cx, "end-rollback");
    app.wait_until(cx, "the reconnection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
            && cx.update(|cx| query.read(cx).tx_state()) == kerno_core::TxState::Idle
    });
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM kerno_ui_save_conn.entries"
        ),
        "0"
    );
}

/// The settings as the app holds them now.
fn settings_now(cx: &mut TestAppContext) -> Settings {
    cx.update(|cx| AppState::get(cx).settings.clone())
}

#[gpui_kit::test]
fn every_action_of_kerno_is_a_command(cx: &mut TestAppContext) {
    let _app = open(cx, memory_storage());
    let names: Vec<&str> = cx.update(|cx| cx.all_action_names().to_vec());
    let commands: Vec<String> = actions::COMMANDS
        .iter()
        .map(|command| (command.action)().name().to_string())
        .collect();
    let missing: Vec<&&str> = names
        .iter()
        .filter(|name| name.starts_with("kerno::") && **name != "kerno::RunCommand")
        .filter(|name| !commands.iter().any(|command| command == *name))
        .collect();
    assert_eq!(missing, Vec::<&&str>::new());
}

#[gpui_kit::test]
fn commit_rollback_and_the_commit_mode_have_keys(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = entries_scratch("kerno_ui_tx_keys");
    let count = || {
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM kerno_ui_tx_keys.entries",
        )
    };
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    let manual = |cx: &mut TestAppContext| cx.update(|cx| tab.read(cx).manual_commit());
    // Ctrl+Alt+A switches the tab to manual commit, and back.
    app.press(cx, &["ctrl-alt-a"]);
    assert!(manual(cx));
    app.press(cx, &["ctrl-alt-a"]);
    assert!(!manual(cx));
    app.press(cx, &["ctrl-alt-a"]);
    app.type_text(cx, "insert into kerno_ui_tx_keys.entries values (1)");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the open transaction", |app, cx| {
        !app.running(cx) && app.tx_state(cx) == kerno_core::TxState::Open
    });
    // Ctrl+8 rolls it back.
    app.press(cx, &["ctrl-8"]);
    app.wait_until(cx, "the rollback", |app, cx| {
        app.tx_state(cx) == kerno_core::TxState::Idle
    });
    assert_eq!(count(), "0");
    // Ctrl+4 commits.
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the open transaction", |app, cx| {
        !app.running(cx) && app.tx_state(cx) == kerno_core::TxState::Open
    });
    app.press(cx, &["ctrl-4"]);
    app.wait_until(cx, "the commit", |app, cx| {
        app.tx_state(cx) == kerno_core::TxState::Idle
    });
    assert_eq!(count(), "1");
}

#[gpui_kit::test]
fn a_key_puts_the_keyboard_in_the_tree_filter(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let navigator = cx.update(|cx| app.workspace.read(cx).navigator(cx));
    let in_filter = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, cx| {
            navigator.read(cx).filter_focused(window, cx)
        })
    };
    assert!(!in_filter(&app, cx));
    app.press(cx, &["secondary-alt-f"]);
    assert!(in_filter(&app, cx));
    // What is typed filters the tree, once its schemas are read.
    app.type_text(cx, "shop");
    app.wait_until(cx, "shop in the filtered tree", |app, cx| {
        app.tree(cx).iter().any(|row| row.trim() == "shop")
    });
}

#[gpui_kit::test]
fn open_table_finds_a_table_by_name_and_opens_its_rows_or_structure(cx: &mut TestAppContext) {
    for (engine, schema) in [(Engine::Postgres, "shop"), (Engine::MySql, "kerno")] {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        // A query tab whose statement Cmd+Enter must not run.
        app.press(cx, &["secondary-t"]);
        app.type_text(cx, "select 1");
        let query = app.tab(cx);
        let open_table = |app: &Fixture, cx: &mut TestAppContext, keys: &str| {
            app.press(cx, &["secondary-p"]);
            assert!(app.has(cx, "quick-open"), "{engine:?}");
            app.wait_until(cx, "the tables", |app, cx| {
                app.shows_label(cx, "Enter opens the rows, Cmd+Enter the structure.")
            });
            app.type_text(cx, "custom");
            assert!(
                app.shows_label(cx, &format!("{schema}.customers")),
                "{engine:?}"
            );
            app.press(cx, &[keys]);
            assert!(!app.dialog_open(cx), "{engine:?}");
        };
        // Enter: the rows.
        open_table(&app, cx, "enter");
        let data = cx.update(|cx| app.workspace.read(cx).active_data_tab().cloned());
        assert!(data.is_some(), "{engine:?}");
        assert_eq!(app.titles(cx), ["Query 1", "customers"], "{engine:?}");
        app.wait_until(cx, "the rows", |app, cx| {
            app.data_state(cx).starts_with("Rows 1–")
        });
        // Cmd+Enter: the structure, and no statement runs anywhere.
        open_table(&app, cx, "secondary-enter");
        let structure = cx.update(|cx| app.workspace.read(cx).active_structure_tab().cloned());
        assert!(structure.is_some(), "{engine:?}");
        assert_eq!(app.titles(cx).len(), 3, "{engine:?}");
        assert_eq!(
            cx.update(|cx| query.read(cx).outcomes(cx)),
            Vec::<String>::new(),
            "{engine:?}"
        );
    }
}

#[gpui_kit::test]
fn a_key_opens_the_rows_or_structure_of_the_table_at_the_cursor(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    new_tab_with_catalog(&app, cx);
    app.type_text(cx, "select * from shop.customers");
    app.press(cx, &["f4"]);
    assert!(cx.update(|cx| app.workspace.read(cx).active_data_tab().is_some()));
    assert_eq!(app.titles(cx)[1], "customers");
    app.click(cx, ("tab-title", 0usize));
    app.press(cx, &["shift-f4"]);
    assert!(cx.update(|cx| app.workspace.read(cx).active_structure_tab().is_some()));
}

#[gpui_kit::test]
fn soft_wrap_is_toggled_by_its_key_in_every_editor(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t", "secondary-t"]);
    let tabs: Vec<_> = (0..2usize)
        .map(|index| {
            app.click(cx, ("tab-title", index));
            app.tab(cx)
        })
        .collect();
    let wrapped = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            tabs.iter()
                .map(|tab| tab.read(cx).editor_options().0)
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(wrapped(cx), [false, false]);
    app.press(cx, &["ctrl-alt-shift-w"]);
    assert!(settings_now(cx).soft_wrap);
    assert_eq!(wrapped(cx), [true, true]);
    app.dispatch(cx, actions::ToggleSoftWrap);
    assert_eq!(wrapped(cx), [false, false]);
}

#[gpui_kit::test]
fn completion_opens_on_demand_even_when_it_does_not_open_by_itself(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.auto_completion = false));
    new_tab_with_catalog(&app, cx);
    app.type_text(cx, "select * from shop.cust");
    assert!(!completion_menu(&app, cx).0, "off: typing opens nothing");
    app.press(cx, &["ctrl-space"]);
    let (open, items) = completion_menu(&app, cx);
    assert!(open);
    assert_eq!(items[0], "customers");
    app.press(cx, &["escape"]);
    assert!(!completion_menu(&app, cx).0);
    app.press(cx, &["secondary-i"]);
    assert!(completion_menu(&app, cx).0);
}

#[gpui_kit::test]
fn fetch_keys_page_a_result_and_zoom_keys_size_the_grid(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g from generate_series(1, 2500) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx) == ["1000 rows, more"]
    });
    // DBeaver's Ctrl+Alt+N and Ctrl+Shift+= (reported as `+`).
    app.press(cx, &["ctrl-alt-n"]);
    app.wait_until(cx, "the second page", |app, cx| {
        app.outcomes(cx) == ["2000 rows, more"]
    });
    app.press(cx, &["ctrl-+"]);
    app.wait_until(cx, "the rest", |app, cx| app.outcomes(cx) == ["2500 rows"]);

    // In the grid, Cmd+= and Cmd+- change the text size; Cmd+0 resets it.
    app.press(cx, &["cmd-alt-t"]);
    let size = |cx: &mut TestAppContext| settings_now(cx).grid_font_size;
    let usual = Settings::default().grid_font_size;
    app.press(cx, &["secondary-="]);
    assert_eq!(size(cx), usual + 1.0);
    app.press(cx, &["secondary-=", "secondary-="]);
    assert_eq!(size(cx), usual + 3.0);
    app.press(cx, &["secondary--"]);
    assert_eq!(size(cx), usual + 2.0);
    app.press(cx, &["secondary-0"]);
    assert_eq!(size(cx), usual);
    // Copy as CSV, from the palette's command.
    app.press(cx, &["secondary-a"]);
    app.dispatch(cx, actions::CopyAsCsv);
    let copied = app.clipboard(cx).unwrap_or_default();
    assert!(copied.starts_with("g\r\n1\r\n2\r\n"), "{copied:?}");
}

#[gpui_kit::test]
fn settings_apply_at_once_and_are_stored(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    app.press(cx, &["secondary-,"]);
    assert!(app.has(cx, "settings"));

    // Fonts: the interface size follows, and survives a theme switch.
    app.click(cx, ("settings-section", 1usize));
    app.click(cx, "ui-font-larger");
    app.click(cx, "editor-font-larger");
    let sizes =
        |cx: &mut TestAppContext| cx.update(|cx| (cx.theme().font_size, cx.theme().mono_font_size));
    assert_eq!(sizes(cx), (px(14.), px(13.5)));
    cx.update(|cx| crate::theme::apply(Appearance::Dark, None, cx));
    assert_eq!(sizes(cx), (px(14.), px(13.5)));
    app.click(cx, "ui-font-reset");
    assert_eq!(sizes(cx).0, px(13.));

    // Editor: the open editor follows.
    app.click(cx, ("settings-section", 2usize));
    app.click(cx, "settings-soft-wrap");
    app.click(cx, "settings-line-numbers");
    app.click(cx, "tab-size-4");
    assert_eq!(
        cx.update(|cx| tab.read(cx).editor_options()),
        (true, false, 4)
    );

    // Grid: page size and NULL text.
    app.click(cx, ("settings-section", 3usize));
    app.click(cx, "settings-page-size");
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "250");
    app.click(cx, "settings-null-text");
    app.press(cx, &["secondary-a", "backspace"]);
    assert_eq!(settings_now(cx).null_text, "NULL", "a blank box keeps NULL");
    app.type_text(cx, "(null)");
    app.click(cx, "settings-time-zone");

    // Confirmations, and the local clients (tested in tool_wizards.rs).
    app.click(cx, ("settings-section", 4usize));
    app.click(cx, "settings-confirm-destructive");
    app.click(cx, ("settings-section", 5usize));
    assert!(app.has(cx, "local-clients") && app.has(cx, "local-clients-add"));

    let now = settings_now(cx);
    assert_eq!(
        (now.page_size, now.null_text.as_str(), now.show_time_zone),
        (250, "(null)", false)
    );
    assert!(!now.confirm_destructive);
    app.wait_until(cx, "the stored settings", |app, _| {
        Settings::load(&app.storage) == now
    });

    // "Open keymap file" makes the file with an example that changes
    // nothing, and keeps one that exists (the headless platform cannot show
    // a file in the file manager: the writing half is called here).
    assert!(app.has(cx, "open-keymap-file"));
    let keymap = cx.update(|cx| AppState::get(cx).paths.keymap());
    assert!(!keymap.exists());
    crate::settings_view::write_keymap_example(&keymap).unwrap();
    let problems = cx.update(|cx| actions::bind_keys(&keymap, cx));
    assert_eq!(problems, Vec::<String>::new());
    std::fs::write(&keymap, r#"{"bindings": {"about": "f1"}}"#).unwrap();
    crate::settings_view::write_keymap_example(&keymap).unwrap();
    assert_eq!(
        std::fs::read_to_string(&keymap).unwrap(),
        r#"{"bindings": {"about": "f1"}}"#
    );
}

#[gpui_kit::test]
fn a_bad_keymap_file_is_reported_and_the_defaults_stay(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("keymap.json");
    std::fs::write(
        &file,
        "{ \"bindings\": { \"new_query_tab\": \"secondary-entr\" ",
    )
    .unwrap();
    let problems = cx.update(|cx| actions::bind_keys(&file, cx));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("not valid JSON"), "{problems:?}");
    // The app goes on with its keys (the JSON error left the defaults).
    app.press(cx, &["secondary-t"]);
    assert_eq!(app.titles(cx), ["Query 1"]);
    // A bad entry: its command has no key (the entry replaces the defaults,
    // as `problems_in_the_keymap_file_are_reported_not_fatal` pins), the
    // others keep theirs. Binding again replaces Kerno's keys (K20): before,
    // the first call's keys stayed underneath.
    std::fs::write(
        &file,
        r#"{"bindings": {"new_query_tab": "secondary-entr", "no_such": "f2"}}"#,
    )
    .unwrap();
    let problems = cx.update(|cx| actions::bind_keys(&file, cx));
    assert_eq!(problems.len(), 2, "{problems:?}");
    app.press(cx, &["secondary-t"]);
    assert_eq!(app.titles(cx), ["Query 1"]);
    app.press(cx, &["secondary-w"]);
    assert_eq!(app.titles(cx), Vec::<String>::new());
}

#[gpui_kit::test]
fn result_tabs_and_selected_connection_commands_from_the_palette(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(cx, "select 1 as a; select 2 as b", "secondary-shift-enter");
    app.wait_until(cx, "the script", |app, cx| {
        app.outcomes(cx).len() == 2 && !app.running(cx)
    });
    let tab = app.tab(cx);
    let shown = |cx: &mut TestAppContext| cx.update(|cx| tab.read(cx).active_result());
    let start = shown(cx);
    app.dispatch(cx, actions::NextResultTab);
    assert_eq!(shown(cx), (start + 1) % 2);
    app.dispatch(cx, actions::PreviousResultTab);
    assert_eq!(shown(cx), start);

    // Duplicate and delete the connection selected in the sidebar.
    app.click(cx, ("connection", 0usize));
    app.dispatch(cx, actions::DuplicateConnection);
    app.wait_until(cx, "the copy", |app, _| {
        app.storage.connections().unwrap().len() == 2
    });
    app.click(cx, ("connection", 1usize));
    app.dispatch(cx, actions::DeleteConnection);
    app.click(cx, "ok");
    app.wait_until(cx, "the deletion", |app, _| {
        app.storage.connections().unwrap().len() == 1
    });
}

// Generate SQL from rows and routines (G7), editor commands (G15), Link with
// editor and the column form (G10 leftovers), Paste and folder Refresh in the
// sidebar --------------------------------------------------------------------

/// The column names of the active table form, and the one whose name has
/// the keyboard; `None` without a form, or while it is read.
fn form_columns(app: &Fixture, cx: &mut TestAppContext) -> Option<(Vec<String>, Option<usize>)> {
    let editor = cx.update(|cx| app.workspace.read(cx).active_table_editor().cloned())?;
    app.step(cx, |window, cx| {
        let editor = editor.read(cx);
        editor.ready().then(|| editor.column_focus(window, cx))
    })
}

/// The target of the active query tab.
fn active_target(app: &Fixture, cx: &mut TestAppContext) -> Option<Target> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).target().cloned())
}

/// Runs a command as the menu bar or the palette does.
fn dispatch(app: &Fixture, cx: &mut TestAppContext, action: impl gpui_kit::Action) {
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(action), cx)
    });
}

#[gpui_kit::test]
fn generate_sql_writes_the_selected_rows_into_a_tab_and_runs_nothing(cx: &mut TestAppContext) {
    use crate::query::grid::GridAction;
    use kerno_core::sql::RowSql;
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let pg = engine == Engine::Postgres;
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_g7_rows");
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                "CREATE TABLE kerno_ui_g7_rows.t (id int PRIMARY KEY, name varchar(20))",
                "INSERT INTO kerno_ui_g7_rows.t VALUES (1, 'a'), (2, NULL)",
            ],
        );
        let q = |name: &str| {
            if pg {
                format!("\"{name}\"")
            } else {
                format!("`{name}`")
            }
        };
        let table = format!("{}.{}", q("kerno_ui_g7_rows"), q("t"));
        let app = connected(cx, engine);
        app.open_table(cx, "kerno_ui_g7_rows", "t", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        let grid = app.data_grid(cx);
        let select = |app: &Fixture, cx: &mut TestAppContext, rows: (usize, usize)| {
            app.step(cx, |_, cx| {
                grid.update(cx, |table, cx| {
                    table.delegate_mut().clear_selection();
                    table.set_selected_row(rows.0, cx);
                    if rows.1 != rows.0 {
                        table.delegate_mut().extend_to(rows.1, 1);
                    }
                })
            });
        };

        // SELECT … IN of both rows (RES-66): a new tab in the table's schema.
        select(&app, cx, (0, 1));
        app.on_data(cx, |tab, window, cx| {
            tab.perform(GridAction::Generate(RowSql::SelectIn), 0, 1, window, cx)
        });
        let (id, name) = (q("id"), q("name"));
        assert_eq!(
            app.active_query(cx),
            (
                "t".to_string(),
                format!("SELECT * FROM {table} WHERE {id} IN (1, 2);\n")
            ),
            "{engine:?}"
        );
        let target = active_target(&app, cx).expect("a target");
        let place = if pg {
            target.schema.clone()
        } else {
            target.database.clone()
        };
        assert_eq!(place.as_deref(), Some("kerno_ui_g7_rows"), "{engine:?}");
        assert_eq!(app.outcomes(cx), Vec::<String>::new(), "nothing ran");
        // What it wrote runs as written.
        app.press(cx, &["secondary-enter"]);
        app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["2 rows"]);

        // INSERT and DELETE of the second row: its NULL is a literal; the
        // DELETE is only text.
        app.open_table(cx, "kerno_ui_g7_rows", "t", RelationKind::Table);
        select(&app, cx, (1, 1));
        app.on_data(cx, |tab, window, cx| {
            tab.perform(GridAction::Generate(RowSql::Insert), 1, 1, window, cx)
        });
        assert_eq!(
            app.active_query(cx).1,
            format!("INSERT INTO {table} ({id}, {name}) VALUES (2, NULL);\n")
        );
        app.open_table(cx, "kerno_ui_g7_rows", "t", RelationKind::Table);
        select(&app, cx, (1, 1));
        app.on_data(cx, |tab, window, cx| {
            tab.perform(GridAction::Generate(RowSql::Delete), 1, 1, window, cx)
        });
        assert_eq!(
            app.active_query(cx).1,
            format!("DELETE FROM {table} WHERE {id} = 2;\n")
        );
        assert_eq!(
            on_server(engine, "SELECT count(*) FROM kerno_ui_g7_rows.t"),
            "2"
        );
    }
}

#[gpui_kit::test]
fn a_row_menu_generates_sql_and_a_query_result_says_why_it_cannot(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_g7_menu CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_g7_menu",
            "CREATE TABLE kerno_ui_g7_menu.t (id int PRIMARY KEY, name text)",
            "INSERT INTO kerno_ui_g7_menu.t VALUES (7, 'seven')",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "kerno_ui_g7_menu", "t", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    // CM-115: Generate SQL ▸ UPDATE from the row's own menu.
    app.right_click(cx, "table-cell-0:2", None);
    click_menu_entry(&app, cx, "Generate SQL");
    click_menu_entry(&app, cx, "UPDATE");
    assert_eq!(
        app.active_query(cx).1,
        "UPDATE \"kerno_ui_g7_menu\".\"t\" SET \"name\" = 'seven' WHERE \"id\" = 7;\n"
    );
    // A read-only query result's rows are no table's: each entry says why
    // (G8: a result of one table with its key generates as table data).
    app.run_in_new_tab(cx, "select 1 as one", "secondary-enter");
    app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["1 rows"]);
    app.right_click(cx, "table-cell-0:1", None);
    click_menu_entry(&app, cx, "Generate SQL");
    for label in [
        "SELECT by key (no column of a table)",
        "INSERT (no column of a table)",
    ] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
}

#[gpui_kit::test]
fn routines_are_called_and_tables_read_in_a_console(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let pg = engine == Engine::Postgres;
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_g7_calls");
        let routines: [&str; 2] = if pg {
            [
                "CREATE FUNCTION kerno_ui_g7_calls.add_one(a integer) RETURNS integer \
                 LANGUAGE sql AS $$ SELECT a + 1 $$",
                "CREATE PROCEDURE kerno_ui_g7_calls.touch(a integer, OUT b integer) \
                 LANGUAGE plpgsql AS $$ BEGIN b := a; END $$",
            ]
        } else {
            [
                "CREATE FUNCTION kerno_ui_g7_calls.add_one(a INT) RETURNS INT DETERMINISTIC \
                 RETURN a + 1",
                "CREATE PROCEDURE kerno_ui_g7_calls.touch(IN a INT, OUT b INT) SET b = a",
            ]
        };
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[&create_schema, routines[0], routines[1]],
        );
        let app = connected(cx, engine);
        let (function, procedure): (Vec<&str>, Vec<&str>) = if pg {
            (
                vec![
                    "kerno",
                    "kerno_ui_g7_calls",
                    "Functions",
                    "add_one(a integer)",
                ],
                vec![
                    "kerno",
                    "kerno_ui_g7_calls",
                    "Procedures",
                    "touch(IN a integer, OUT b integer)",
                ],
            )
        } else {
            (
                vec!["kerno_ui_g7_calls", "Routines", "add_one(IN a int)"],
                vec![
                    "kerno_ui_g7_calls",
                    "Routines",
                    "touch(IN a int, OUT b int)",
                ],
            )
        };
        app.open_path(cx, &procedure);
        app.open_path(cx, &function);

        // NAV-53 / CM-81: Generate CALL writes the call; nothing runs.
        app.act(cx, Action::GenerateCall, &procedure);
        let call = if pg {
            "CALL \"kerno_ui_g7_calls\".\"touch\"(:a, NULL);\n"
        } else {
            "CALL `kerno_ui_g7_calls`.`touch`(:a, @b);\nSELECT @b;\n"
        };
        app.wait_until(cx, "the call", |app, cx| {
            app.titles(cx).contains(&"touch".to_string())
        });
        assert_eq!(
            app.active_query(cx),
            ("touch".to_string(), call.to_string())
        );
        assert_eq!(app.outcomes(cx), Vec::<String>::new(), "nothing ran");

        // CM-80: Execute… runs it, asking for its parameters first.
        let (executed, value, expected) = if pg {
            (&function, "41", "42")
        } else {
            (&procedure, "5", "5")
        };
        app.act(cx, Action::ExecuteRoutine, executed);
        app.wait_until(cx, "the prompt", |app, cx| app.has(cx, ("param", 0usize)));
        app.type_text(cx, value);
        app.click(cx, "params-run");
        app.wait_until(cx, "the result", |app, cx| {
            !app.running(cx) && app.outcomes(cx).last().is_some_and(|last| last == "1 rows")
        });
        let last = app.outcomes(cx).len() - 1;
        assert_eq!(
            app.result(cx, last, 1).1,
            [cells(&[expected])],
            "{engine:?}"
        );

        // NAV-46 / CM-52: Read data in SQL console runs a SELECT of the table.
        let customers: Vec<&str> = if pg {
            vec!["kerno", "shop", "Tables", "customers"]
        } else {
            vec!["kerno", "Tables", "customers"]
        };
        app.open_path(cx, &customers);
        app.act(cx, Action::ReadInConsole, &customers);
        app.wait_until(cx, "the rows", |app, cx| {
            app.titles(cx)
                .last()
                .is_some_and(|title| title == "customers")
                && !app.running(cx)
                && app.outcomes(cx).len() == 1
        });
        assert!(app.active_query(cx).1.starts_with("SELECT"), "{engine:?}");
        assert!(
            app.outcomes(cx)[0].ends_with("rows"),
            "{:?}",
            app.outcomes(cx)
        );
    }
}

#[gpui_kit::test]
fn the_editor_changes_case_comments_a_block_and_copies_its_statement(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 'Ab', \"Cd\" from t;\nselect 2");
    let text = |app: &Fixture, cx: &mut TestAppContext| app.active_query(cx).1;
    // SQL-35: the case of what is selected, strings and quoted names aside.
    app.press(cx, &["secondary-a", "secondary-shift-x"]);
    assert_eq!(text(&app, cx), "SELECT 'Ab', \"Cd\" FROM T;\nSELECT 2");
    app.press(cx, &["secondary-shift-y"]);
    assert_eq!(text(&app, cx), "select 'Ab', \"Cd\" from t;\nselect 2");
    // One step to undo.
    app.press(cx, &["secondary-z"]);
    assert_eq!(text(&app, cx), "SELECT 'Ab', \"Cd\" FROM T;\nSELECT 2");
    // SQL-33: a block comment around the selection, and off again.
    app.press(cx, &["secondary-a", "secondary-shift-/"]);
    assert_eq!(
        text(&app, cx),
        "/* SELECT 'Ab', \"Cd\" FROM T;\nSELECT 2 */"
    );
    app.press(cx, &["secondary-shift-/"]);
    assert_eq!(text(&app, cx), "SELECT 'Ab', \"Cd\" FROM T;\nSELECT 2");
    // SQL-48 / CM-100: the statement at the cursor goes to the clipboard.
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.editor().update(cx, |editor, cx| {
                editor.set_cursor_position(
                    gpui_kit::component::input::Position::new(1, 3),
                    window,
                    cx,
                )
            })
        })
    });
    dispatch(&app, cx, actions::CopySelectedQuery);
    assert_eq!(app.clipboard(cx).as_deref(), Some("SELECT 2"));
    // Upper case without a selection takes the cursor's line.
    app.press(cx, &["secondary-shift-y"]);
    assert_eq!(text(&app, cx), "SELECT 'Ab', \"Cd\" FROM T;\nselect 2");
}

#[gpui_kit::test]
fn refresh_completion_metadata_finds_a_table_made_elsewhere(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    app.wait_until(cx, "the catalog", |_, cx| {
        cx.update(|cx| tab.read(cx).catalog_ready())
    });
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_g15_refresh CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_g15_refresh",
            "CREATE TABLE kerno_ui_g15_refresh.fresh_table (id int)",
        ],
    );
    let sql = "select * from kerno_ui_g15_refresh.";
    let proposes = |cx: &mut TestAppContext| {
        cx.update(|cx| tab.read(cx).completions(sql, sql.len(), cx))
            .iter()
            .any(|(label, _)| label == "fresh_table")
    };
    assert!(
        !proposes(cx),
        "the catalog was read before the table existed"
    );
    // SQL-52: the command reads it again.
    dispatch(&app, cx, actions::RefreshCompletionMetadata);
    app.wait_until(cx, "the new table", |_, cx| proposes(cx));
}

#[gpui_kit::test]
fn the_editor_menu_sets_the_schema_and_exports_from_the_query(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    app.wait_until(cx, "the schemas", |_, cx| {
        cx.update(|cx| tab.read(cx).schemas().iter().any(|schema| schema == "shop"))
    });
    app.type_text(cx, "select 1 as one");
    // CM-95: Execute ▸ Active schema ▸ shop.
    let on_text = Some(gpui_kit::point(px(120.), px(10.)));
    app.right_click(cx, "query-editor", on_text);
    click_menu_entry(&app, cx, "Execute");
    click_menu_entry(&app, cx, "Active schema");
    click_menu_entry(&app, cx, "shop");
    app.wait_until(cx, "the schema", |app, cx| {
        active_target(app, cx).and_then(|target| target.schema) == Some("shop".into())
    });
    // CM-93: Execute ▸ Export from query… opens the export of the statement.
    app.right_click(cx, "query-editor", on_text);
    click_menu_entry(&app, cx, "Execute");
    for label in ["Count rows of statement", "Fetch all rows (no rows left)"] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
    click_menu_entry(&app, cx, "Export from query…");
    app.wait_until(cx, "the export", |app, cx| app.has(cx, "export-dialog"));
}

#[gpui_kit::test]
fn revert_puts_back_the_text_of_the_saved_file(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kept.sql");
    std::fs::write(&path, "select 1").unwrap();
    let app = open(cx, memory_storage());
    let opening = path.clone();
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_sql_file_from(opening, window, cx)
        })
    });
    app.wait_until(cx, "the file", |app, cx| app.titles(cx) == ["kept.sql"]);
    app.type_text(cx, " -- changed");
    let tab = app.tab(cx);
    assert!(cx.update(|cx| tab.read(cx).unsaved()));
    // CM-96: File ▸ Revert to saved file.
    dispatch(&app, cx, actions::RevertFile);
    app.wait_until(cx, "the revert", |app, cx| {
        app.active_query(cx).1 == "select 1" && !cx.update(|cx| tab.read(cx).unsaved())
    });
}

#[gpui_kit::test]
fn link_with_editor_selects_the_table_of_the_active_tab(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.open_table(cx, "shop", "customers", RelationKind::Table);
    let row_of = |app: &Fixture, cx: &mut TestAppContext, table: &str| {
        cx.update(|cx| {
            app.workspace
                .read(cx)
                .navigator(cx)
                .read(cx)
                .row_of(&["kerno", "shop", "Tables", table], cx)
        })
    };
    // NAV-21 / CM-07: on (from the tree's menu), the active tab's table is
    // selected now…
    app.wait_until(cx, "the databases", |app, cx| !app.tree(cx).is_empty());
    let height = app.step(cx, |window, _| {
        window.find("navigator-tree").bounds().size.height
    });
    app.right_click(
        cx,
        "navigator-tree",
        Some(gpui_kit::point(px(40.), height - px(8.))),
    );
    app.click_label(cx, "Link with editor");
    cx.run_until_parked();
    assert!(settings_now(cx).link_with_editor);
    app.wait_until(cx, "customers selected", |app, cx| {
        let row = row_of(app, cx, "customers");
        row.is_some() && app.selected_row(cx) == row
    });
    // …and as tabs are activated.
    app.press(cx, &["ctrl-shift-tab"]);
    app.wait_until(cx, "orders selected", |app, cx| {
        let row = row_of(app, cx, "orders");
        row.is_some() && app.selected_row(cx) == row
    });
    let selected = cx.update(|cx| {
        let workspace = app.workspace.read(cx);
        workspace.sidebar().read(cx).selected(cx)
    });
    let first = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    assert_eq!(selected, Some(first));
    // Off again (Cmd+Shift+,): activating a tab leaves the tree alone.
    app.press(cx, &["secondary-shift-,"]);
    assert!(!settings_now(cx).link_with_editor);
    let before = app.selected_row(cx);
    app.press(cx, &["ctrl-tab"]);
    cx.run_until_parked();
    assert_eq!(app.selected_row(cx), before);
}

#[gpui_kit::test]
fn paste_saves_the_copied_connection_and_a_folder_refreshes(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let config = testing::config(Engine::Postgres);
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let sidebar = cx.update(|cx| app.workspace.read(cx).sidebar());
    let has = |cx: &mut TestAppContext, subject: Subject, action: SidebarAction| {
        cx.update(|cx| {
            sidebar
                .read(cx)
                .menu_of(&subject, cx)
                .contains(&crate::sidebar::Entry::Item(action, None))
        })
    };
    // CM-09: nothing to paste until a connection is copied.
    assert!(!has(cx, Subject::List, SidebarAction::Paste));
    app.sidebar_does(cx, Subject::Connection(id.clone()), SidebarAction::CopyName);
    assert_eq!(app.clipboard(cx), Some(config.name.clone()));
    assert!(has(cx, Subject::List, SidebarAction::Paste));
    app.sidebar_does(cx, Subject::List, SidebarAction::Paste);
    app.wait_until(cx, "the copy", |app, _| {
        app.storage.connections().unwrap().len() == 2
    });
    let names: Vec<String> = app
        .storage
        .connections()
        .unwrap()
        .into_iter()
        .map(|saved| saved.config.name)
        .collect();
    assert!(
        names.contains(&format!("{} copy", config.name)),
        "{names:?}"
    );

    // CM-15: a folder's Refresh reads the list again and connects nothing.
    let folder = Subject::Folder("Work".into());
    assert!(has(cx, folder.clone(), SidebarAction::Refresh));
    app.sidebar_does(cx, folder, SidebarAction::Refresh);
    cx.run_until_parked();
    assert_eq!(app.status(cx, 0), Status::Disconnected);
    assert_eq!(cx.update(|cx| app.connections.read(cx).saved().len()), 2);
}

// Record view, grid navigation and panels, value panel, boolean and enum
// cells (docs/plans/PARITY-GAPS.md G3, G6, G9; docs/plans/M6.md Part 1) ---

/// A small table to edit: a key, text, a number, JSON, a boolean.
fn record_scratch(schema: &str) -> Scratch {
    Scratch::new(
        Engine::Postgres,
        &[&format!("DROP SCHEMA IF EXISTS {schema} CASCADE")],
        &[
            &format!("CREATE SCHEMA {schema}"),
            &format!(
                "CREATE TABLE {schema}.items (id integer PRIMARY KEY, name varchar(20), \
                 qty integer, doc jsonb, flag boolean)"
            ),
            &format!(
                "INSERT INTO {schema}.items VALUES (1, 'a', 10, '{{\"a\": 1}}', true), \
                 (2, 'b', 20, NULL, false), (3, 'c', 30, '[]', NULL)"
            ),
        ],
    )
}

/// Selects a cell of the grid and gives it the keys.
fn select_cell(
    app: &Fixture,
    cx: &mut TestAppContext,
    grid: &Entity<TableState<Grid>>,
    cell: (usize, usize),
) {
    app.step(cx, |window, cx| {
        grid.update(cx, |table, cx| {
            table.delegate_mut().clear_selection();
            table.set_selected_cell(cell.0, cell.1, cx);
            let selection = table.selection();
            table.delegate_mut().set_anchor(selection);
        });
        grid.read(cx).focus_handle(cx).focus(window, cx);
    });
}

#[gpui_kit::test]
fn record_view_shows_one_row_and_edits_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = record_scratch("kerno_ui_record");
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "kerno_ui_record", "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–3 of 3");
    let grid = app.data_grid(cx);
    // The row the record view shows, from 1.
    let row_label = |app: &Fixture, cx: &mut TestAppContext| -> String {
        app.step(cx, |_, _| ());
        let (row, _) = crate::query::grid::current(cx.update(|cx| grid.read(cx).selection()));
        format!("Row {} of 3", row + 1)
    };
    // Tab while typing in a cell stays in the cell.
    app.on_data(cx, |tab, window, cx| tab.edit_cell(0, 2, window, cx));
    app.press(cx, &["tab"]);
    let (record, editing) = cx.update(|cx| {
        (
            grid.read(cx).delegate().record,
            grid.read(cx).delegate().editing(),
        )
    });
    assert!(!record);
    assert_eq!(editing, Some((0, 1)));
    app.press(cx, &["escape"]);
    // The figures of a selection that stays follow its cells' changes.
    let figures = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |_, _| ());
        cx.update(|cx| {
            crate::query::grid_view::SelectionStats::of(&grid, cx)
                .read(cx)
                .text()
        })
    };
    select_cell(&app, cx, &grid, (0, 3));
    cx.update(|cx| grid.update(cx, |table, _| table.delegate_mut().extend_to(1, 3)));
    assert!(figures(&app, cx).is_some_and(|text| text.contains("sum 30 ")));
    app.press(cx, &["delete"]);
    assert_eq!(
        figures(&app, cx).as_deref(),
        Some("2 cells (2 rows, 1 column) · 1 distinct")
    );
    app.press(cx, &["secondary-z"]);
    assert!(figures(&app, cx).is_some_and(|text| text.contains("sum 30 ")));
    select_cell(&app, cx, &grid, (0, 2));
    // Tab: the row as one line per column.
    app.press(cx, &["tab"]);
    assert!(app.has(cx, "record-view"));
    assert!(!app.has(cx, "table-inner"));
    assert_eq!(row_label(&app, cx), "Row 1 of 3");
    // First and Previous have nowhere to go from the first row.
    let disabled = |app: &Fixture, cx: &mut TestAppContext| -> Vec<bool> {
        app.step(cx, |window, _| {
            [
                "record-first",
                "record-previous",
                "record-next",
                "record-last",
            ]
            .map(|id| window.find(id).disabled() == Some(true))
            .to_vec()
        })
    };
    assert_eq!(disabled(&app, cx), [true, true, false, false]);
    for field in 0usize..5 {
        assert!(app.has(cx, ("record-field", field)), "{field}");
    }
    // Next row, from the keys and the buttons.
    app.press(cx, &["ctrl-alt-right"]);
    assert_eq!(row_label(&app, cx), "Row 2 of 3");
    app.click(cx, "record-last");
    assert_eq!(row_label(&app, cx), "Row 3 of 3");
    assert_eq!(disabled(&app, cx), [false, false, true, true]);
    app.press(cx, &["ctrl-alt-shift-left"]);
    assert_eq!(row_label(&app, cx), "Row 1 of 3");
    app.press(cx, &["ctrl-alt-right"]);
    // A field is picked with the arrows and typed in with Enter.
    app.press(cx, &["down"]);
    assert_eq!(cx.update(|cx| grid.read(cx).selected_cell()), Some((1, 3)));
    app.press(cx, &["enter", "secondary-a", "backspace"]);
    app.type_text(cx, "21");
    app.press(cx, &["enter"]);
    assert_eq!(app.pending(cx), 1);
    assert_eq!(app.data_rows(cx)[1][2], "21");
    // Tab goes back to the grid, on the same row.
    app.press(cx, &["tab"]);
    assert!(!app.has(cx, "record-view"));
    assert_eq!(cx.update(|cx| grid.read(cx).selected_cell()), Some((1, 3)));
    app.click(cx, "data-discard");
    assert_eq!(app.pending(cx), 0);
}

#[gpui_kit::test]
fn selection_figures_show_in_the_footer_and_the_panels(cx: &mut TestAppContext) {
    use crate::query::grid_view::SelectionStats;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g as n, g * 2 as m, 'x' || g as t from generate_series(1, 4) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["4 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    let figures = |app: &Fixture, cx: &mut TestAppContext, grid: &Entity<TableState<Grid>>| {
        app.step(cx, |_, _| ());
        cx.update(|cx| SelectionStats::of(grid, cx).read(cx).text())
    };
    // One cell: no figures.
    select_cell(&app, cx, &grid, (0, 1));
    assert_eq!(figures(&app, cx, &grid), None);
    // The numbers of two columns.
    cx.update(|cx| grid.update(cx, |table, _| table.delegate_mut().extend_to(3, 2)));
    assert_eq!(
        figures(&app, cx, &grid).as_deref(),
        Some("8 cells (4 rows, 2 columns) · 6 distinct · sum 30 · avg 3.75 · min 1 · max 8")
    );
    // The first two rows of `n`, then the first two once ordered
    // descending: the figures are of the cells as shown.
    select_cell(&app, cx, &grid, (0, 1));
    cx.update(|cx| grid.update(cx, |table, _| table.delegate_mut().extend_to(1, 1)));
    assert!(figures(&app, cx, &grid).is_some_and(|text| text.contains("sum 3 ")));
    app.right_click(cx, "table-cell-0:1", None);
    click_menu_entry(&app, cx, "Order by this column");
    click_menu_entry(&app, cx, "Descending");
    cx.update(|cx| grid.update(cx, |table, _| table.delegate_mut().extend_to(1, 1)));
    let ordered = figures(&app, cx, &grid).unwrap_or_default();
    assert!(ordered.contains("sum 7 "), "{ordered}");
    // Text has no sum.
    select_cell(&app, cx, &grid, (0, 3));
    cx.update(|cx| grid.update(cx, |table, _| table.delegate_mut().extend_to(1, 3)));
    assert_eq!(
        figures(&app, cx, &grid).as_deref(),
        Some("2 cells (2 rows, 1 column) · 2 distinct")
    );
    // Layout ▸ Selection statistics turns the line off.
    app.dispatch(cx, actions::ToggleSelectionStats);
    assert!(!cx.update(|cx| AppState::get(cx).settings.selection_stats));
    app.dispatch(cx, actions::ToggleSelectionStats);
    // Panels ▸ Aggregate and Columns.
    app.dispatch(cx, actions::ShowAggregatePanel);
    assert!(app.has(cx, "aggregate-panel"));
    app.press(cx, &["escape"]);
    app.dispatch(cx, actions::ShowColumnsPanel);
    assert!(app.has(cx, "columns-panel"));
    app.press(cx, &["escape"]);

    // A whole column of a large result is counted off the UI thread.
    app.run_in_new_tab(
        cx,
        "select g as n from generate_series(1, 300000) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx)
            .first()
            .is_some_and(|outcome| outcome.contains("rows"))
    });
    let tab = app.tab(cx);
    let large = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    app.dispatch(cx, actions::FetchAllRows);
    app.wait_until(cx, "every row", |_, cx| {
        cx.update(|cx| large.read(cx).delegate().is_complete())
    });
    app.step(cx, |window, cx| {
        large.update(cx, |table, cx| table.set_selected_col(1, cx));
        large.read(cx).focus_handle(cx).focus(window, cx);
    });
    let first = figures(&app, cx, &large).unwrap_or_default();
    assert!(
        first.starts_with("300000 cells (300000 rows, 1 column)"),
        "{first}"
    );
    app.wait_until(cx, "the figures", |app, cx| {
        figures(app, cx, &large).is_some_and(|text| text.contains("sum"))
    });
    assert_eq!(
        figures(&app, cx, &large).as_deref(),
        Some(
            "300000 cells (300000 rows, 1 column) · 300000 distinct · sum 45000150000 · \
             avg 150000.5 · min 1 · max 300000"
        )
    );
}

#[gpui_kit::test]
fn go_to_row_and_column_move_the_selection(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g as first, g as second, g as third from generate_series(1, 2500) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx)
            .first()
            .is_some_and(|outcome| outcome.contains("rows"))
    });
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    assert!(cx.update(|cx| grid.read(cx).delegate().row_count()) < 1500);
    select_cell(&app, cx, &grid, (0, 1));
    // Ctrl+G: a row the server has not sent yet is fetched, then selected.
    app.press(cx, &["ctrl-g"]);
    assert!(app.has(cx, "go-to-row"));
    app.type_text(cx, "1500");
    app.press(cx, &["enter"]);
    app.wait_until(cx, "row 1500", |_, cx| {
        cx.update(|cx| grid.read(cx).selected_cell()) == Some((1499, 1))
    });
    // Ctrl+Shift+G: a column found by a few letters.
    app.press(cx, &["ctrl-shift-g"]);
    assert!(app.has(cx, "go-to-column"));
    app.type_text(cx, "thi");
    app.press(cx, &["enter"]);
    assert_eq!(
        cx.update(|cx| grid.read(cx).selected_cell()),
        Some((1499, 3))
    );
    // First and previous row.
    app.press(cx, &["ctrl-alt-left"]);
    assert_eq!(
        cx.update(|cx| grid.read(cx).selected_cell()),
        Some((1498, 3))
    );
    app.dispatch(cx, actions::FirstRow);
    assert_eq!(cx.update(|cx| grid.read(cx).selected_cell()), Some((0, 3)));
}

#[gpui_kit::test]
fn a_right_click_on_a_row_number_opens_the_menu_of_its_row(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g as n from generate_series(1, 3) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["3 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    app.right_click(cx, ("row-number", 1usize), None);
    assert!(menu_entry(&app, cx, "Copy row numbers").is_some());
    assert!(menu_entry(&app, cx, "Export…").is_some());
    click_menu_entry(&app, cx, "Copy row numbers");
    assert_eq!(app.clipboard(cx).as_deref(), Some("2"));
    // The right-clicked row is selected; inside a selection, it stays.
    cx.update(|cx| grid.update(cx, |table, _| table.delegate_mut().extend_to(2, 1)));
    app.right_click(cx, ("row-number", 2usize), None);
    click_menu_entry(&app, cx, "Copy row numbers");
    assert_eq!(app.clipboard(cx).as_deref(), Some("2\n3"));
    app.dispatch(cx, actions::CopyRowNumbers);
    assert_eq!(app.clipboard(cx).as_deref(), Some("2\n3"));
}

#[gpui_kit::test]
fn the_value_panel_edits_a_json_cell_and_apply_writes_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = record_scratch("kerno_ui_value_panel");
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "kerno_ui_value_panel", "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–3 of 3");
    let grid = app.data_grid(cx);
    select_cell(&app, cx, &grid, (0, 4));
    // Shift+Enter: the value panel, editable, the JSON laid out.
    app.press(cx, &["shift-enter"]);
    assert!(app.has(cx, "value-editor"));
    // Invalid JSON is refused with the parser's message.
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "{\"b\": ");
    app.click(cx, "value-save");
    assert!(app.has(cx, "value-problem"));
    assert_eq!(app.pending(cx), 0);
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "{\"b\":   2}");
    app.click(cx, "value-compact");
    app.click(cx, "value-save");
    assert!(!app.has(cx, "value-editor"));
    assert_eq!(app.pending(cx), 1);
    let tab = app.data(cx);
    let (_, statements) = cx.update(|cx| tab.read(cx).pending_sql(cx)).unwrap();
    assert_eq!(
        statements,
        ["UPDATE \"kerno_ui_value_panel\".\"items\" SET \"doc\" = '{\"b\":2}' WHERE \"id\" = 1"]
    );
    // Set NULL from the panel, on another row; then apply both.
    select_cell(&app, cx, &grid, (2, 2));
    app.press(cx, &["shift-enter"]);
    app.click(cx, "value-null");
    assert_eq!(app.pending(cx), 2);
    assert!(!app.has(cx, "value-editor"));
    app.click(cx, "data-apply");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the applied changes", |app, cx| {
        app.pending(cx) == 0 && app.data_state(cx) == "Rows 1–3 of 3"
    });
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT string_agg(doc::text || '|' || coalesce(name, 'NULL'), E'\\n' ORDER BY id) \
             FROM kerno_ui_value_panel.items WHERE id IN (1, 3)"
        ),
        "{\"b\": 2}|a\n[]|NULL"
    );
    // A query result is only shown. (The page read again is a new grid:
    // the keyboard goes to it first.)
    let grid = app.data_grid(cx);
    select_cell(&app, cx, &grid, (0, 1));
    app.run_in_new_tab(cx, "select '{}'::jsonb as doc", "secondary-enter");
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    let tab = app.tab(cx);
    let result = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    select_cell(&app, cx, &result, (0, 1));
    app.press(cx, &["shift-enter"]);
    assert!(app.has(cx, "value-text"));
    assert!(!app.has(cx, "value-editor"));
}

#[gpui_kit::test]
fn boolean_and_enum_cells_take_their_values(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_choices";
        let (drop_schema, create_schema) = scratch_schema(engine, schema);
        let (types, columns) = match engine {
            Engine::Postgres => (
                format!("CREATE TYPE {schema}.mood AS ENUM ('sad', 'ok', 'happy')"),
                format!("flag boolean, mood {schema}.mood"),
            ),
            _ => (
                "SELECT 1".to_string(),
                "flag TINYINT(1), mood ENUM('sad', 'ok', 'happy')".to_string(),
            ),
        };
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &types,
                &scratch_table(engine, &format!("{schema}.items"), &columns),
                &format!("INSERT INTO {schema}.items (flag, mood) VALUES (true, 'ok')"),
            ],
        );
        let app = connected(cx, engine);
        app.open_table(cx, schema, "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–1 of 1");
        let grid = app.data_grid(cx);
        // Space toggles: true, false, NULL (the column takes it), true.
        select_cell(&app, cx, &grid, (0, 2));
        let flag = |app: &Fixture, cx: &mut TestAppContext| app.data_rows(cx)[0][1].clone();
        assert_eq!(flag(&app, cx), "true");
        app.press(cx, &["space"]);
        assert_eq!(flag(&app, cx), "false");
        app.press(cx, &["space"]);
        assert_eq!(flag(&app, cx), "NULL");
        app.press(cx, &["space"]);
        assert_eq!(flag(&app, cx), "true");
        assert_eq!(app.pending(cx), 0, "{engine:?}: back to what was fetched");
        // A click on the selected cell toggles it.
        app.click(cx, "table-cell-0:2");
        assert_eq!(flag(&app, cx), "false");
        // Enter on an enum: the list of its values, NULL last.
        select_cell(&app, cx, &grid, (0, 3));
        app.press(cx, &["enter"]);
        for label in ["sad", "ok", "happy", "NULL"] {
            assert!(menu_entry(&app, cx, label).is_some(), "{engine:?}: {label}");
        }
        click_menu_entry(&app, cx, "happy");
        assert_eq!(app.data_rows(cx)[0][2], "happy");
        assert_eq!(app.pending(cx), 1);
        app.click(cx, "data-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the applied changes", |app, cx| {
            app.pending(cx) == 0 && app.data_state(cx) == "Rows 1–1 of 1"
        });
        let stored = on_server(
            engine,
            &format!("SELECT CONCAT(flag, '|', mood) FROM {schema}.items"),
        );
        let expected = match engine {
            Engine::Postgres => "f|happy",
            _ => "0|happy",
        };
        assert_eq!(stored, expected, "{engine:?}");
    }
}

// G11, G14, G16, G17 and CM-58: object filters, transaction log, locks and
// idle sessions, properties and database options, backup of tables -----------

/// A session holding a row of `table` in an open transaction, and another
/// waiting for it; each gives up on its own within seconds. Returns their
/// ids and the threads, which end with what each statement came to.
type LockThread = std::thread::JoinHandle<Result<(), String>>;

fn lock_pair(engine: Engine, table: &str) -> (String, String, LockThread, LockThread) {
    use kerno_core::Driver as _;
    let session_of = move |params: kerno_core::ConnectParams| async move {
        match engine {
            Engine::Postgres => kerno_postgres::PostgresDriver.connect(&params).await,
            Engine::MySql | Engine::MariaDb => kerno_mysql::MysqlDriver.connect(&params).await,
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
        }
        .expect("the test server")
    };
    let postgres = engine == Engine::Postgres;
    let own_id = if postgres {
        "SELECT pg_backend_pid()"
    } else {
        "SELECT CONNECTION_ID()"
    };
    let run = |statements: Vec<String>, sender: std::sync::mpsc::Sender<String>| {
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let session = session_of(testing::params(engine)).await;
                let collect = |sql: String| kerno_core::exec::collect(session.clone(), sql);
                let id = collect(own_id.into()).await.unwrap().results[0].rows()[0][0]
                    .clone()
                    .unwrap();
                let mut outcome = Ok(());
                for (index, sql) in statements.into_iter().enumerate() {
                    // The id goes out once the lock is held (the holder's
                    // second statement) or about to be waited for.
                    if index == 2 || (index == 1 && sql.starts_with("SET")) {
                        let _ = sender.send(id.clone());
                    }
                    if let Err(error) = collect(sql).await {
                        outcome = Err(error.to_string());
                        break;
                    }
                }
                session.close().await;
                outcome
            })
        })
    };
    let begin = if postgres {
        "BEGIN"
    } else {
        "START TRANSACTION"
    };
    let (sleep, wait) = if postgres {
        ("SELECT pg_sleep(8)", "SET lock_timeout = '10s'")
    } else {
        (
            "SELECT SLEEP(8)",
            "SET SESSION innodb_lock_wait_timeout = 10",
        )
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let holder = run(
        vec![
            begin.into(),
            format!("UPDATE {table} SET v = 2 WHERE id = 1"),
            sleep.into(),
            "ROLLBACK".into(),
        ],
        sender.clone(),
    );
    let holder_id = receiver.recv_timeout(Duration::from_secs(30)).unwrap();
    let waiter = run(
        vec![
            "SELECT 1".into(),
            wait.into(),
            format!("UPDATE {table} SET v = 3 WHERE id = 1"),
        ],
        sender,
    );
    let waiter_id = receiver.recv_timeout(Duration::from_secs(30)).unwrap();
    (holder_id, waiter_id, holder, waiter)
}

#[gpui_kit::test]
fn the_lock_manager_lists_who_waits_for_whom_and_ends_the_blocker(cx: &mut TestAppContext) {
    use crate::admin::tab::{LOCKS, Pane};
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let (cleanup, setup): (&str, Vec<&str>) = match engine {
            Engine::Postgres => (
                "DROP SCHEMA IF EXISTS kerno_ui_locks CASCADE",
                vec![
                    "CREATE SCHEMA kerno_ui_locks",
                    "CREATE TABLE kerno_ui_locks.t (id int PRIMARY KEY, v int)",
                    "INSERT INTO kerno_ui_locks.t VALUES (1, 1)",
                ],
            ),
            _ => (
                "DROP DATABASE IF EXISTS kerno_ui_locks",
                vec![
                    "CREATE DATABASE kerno_ui_locks",
                    "CREATE TABLE kerno_ui_locks.t (id INT PRIMARY KEY, v INT) ENGINE = InnoDB",
                    "INSERT INTO kerno_ui_locks.t VALUES (1, 1)",
                ],
            ),
        };
        let _scratch = Scratch::new(engine, &[cleanup], &setup);
        let app = connected(cx, engine);
        let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
        app.sidebar_does(
            cx,
            Subject::Connection(id),
            SidebarAction::Tool(crate::navigator::actions::ToolEntry::Dashboard),
        );
        app.wait_admin(cx, Pane::Sessions);

        // TOOL-13: idle (PostgreSQL) or sleeping sessions are left out, and
        // the choice is kept in the settings.
        let (column, idle) = match engine {
            Engine::Postgres => ("State", "idle"),
            _ => ("Command", "Sleep"),
        };
        app.on_admin(cx, |tab, _, cx| tab.set_hide_idle(true, cx));
        assert!(settings_now(cx).hide_idle_sessions, "{engine:?}");
        let (headers, rows) = app.admin_shown(cx, Pane::Sessions);
        let at = headers.iter().position(|h| h == column).unwrap();
        assert!(
            rows.iter().all(|row| row[at] != idle),
            "{engine:?}: {rows:?}"
        );
        assert!(app.has(cx, "admin-hide-idle"));
        // The sub-tab says how many of the sessions read are shown.
        let admin = app.admin(cx);
        let title = cx.update(|cx| admin.read(cx).section_titles(cx))[0].clone();
        // Showing them again only recomputes the rows read: no new read.
        app.on_admin(cx, |tab, _, cx| tab.set_hide_idle(false, cx));
        let all = app.admin_shown(cx, Pane::Sessions).1.len();
        let expected = if rows.len() < all {
            format!("Sessions ({} of {all})", rows.len())
        } else {
            format!("Sessions ({all})")
        };
        assert_eq!(title, expected, "{engine:?}");

        // TOOL-14: an empty list says what it would show.
        app.on_admin(cx, |tab, window, cx| tab.select(LOCKS, window, cx));
        app.wait_admin(cx, Pane::Locks);
        let (holder, waiter, holder_thread, waiter_thread) = lock_pair(engine, "kerno_ui_locks.t");
        let rows = app.admin_rows_until(cx, Pane::Locks, "the waiting session", |rows| {
            rows.iter().any(|row| row[0] == waiter)
        });
        let (headers, _) = app.admin_shown(cx, Pane::Locks);
        let blocking = headers.iter().position(|h| h == "Blocking id").unwrap();
        let row = rows.iter().position(|row| row[0] == waiter).unwrap();
        assert_eq!(rows[row][blocking], holder, "{engine:?}: {headers:?}");

        // Terminate the session holding the lock, from the toolbar: the
        // waiting one goes on.
        app.click(cx, ("admin-locks-cell", row << 12));
        app.click(cx, "admin-terminate");
        assert!(app.has(cx, "sql-preview"), "{engine:?}");
        let expected = match engine {
            Engine::Postgres => format!("SELECT pg_catalog.pg_terminate_backend({holder})"),
            _ => format!("KILL CONNECTION {holder}"),
        };
        assert_eq!(app.admin_asked(cx), Some(expected));
        app.click(cx, "sql-confirm");
        assert_eq!(waiter_thread.join().unwrap(), Ok(()), "{engine:?}");
        assert!(holder_thread.join().unwrap().is_err(), "{engine:?}");
        app.admin_rows_until(cx, Pane::Locks, "no more waiting", |rows| {
            !rows.iter().any(|row| row[0] == waiter)
        });
    }
}

#[gpui_kit::test]
fn the_transaction_log_lists_the_statements_until_the_transaction_ends(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let (cleanup, setup): (&str, Vec<&str>) = match engine {
            Engine::Postgres => (
                "DROP SCHEMA IF EXISTS kerno_ui_txlog CASCADE",
                vec![
                    "CREATE SCHEMA kerno_ui_txlog",
                    "CREATE TABLE kerno_ui_txlog.entries (id int PRIMARY KEY)",
                ],
            ),
            _ => (
                "DROP DATABASE IF EXISTS kerno_ui_txlog",
                vec![
                    "CREATE DATABASE kerno_ui_txlog",
                    "CREATE TABLE kerno_ui_txlog.entries (id INT PRIMARY KEY)",
                ],
            ),
        };
        let _scratch = Scratch::new(engine, &[cleanup], &setup);
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        let tab = app.tab(cx);
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(true, cx)));
        let log = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                tab.read(cx)
                    .transaction_log()
                    .iter()
                    .map(|entry| (entry.sql.clone(), entry.outcome()))
                    .collect::<Vec<_>>()
            })
        };
        // Nothing open yet: the command says so.
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::ShowTransactionLog), cx)
        });
        assert!(app.has(cx, "tx-log-empty"), "{engine:?}");
        app.press(cx, &["escape"]);

        for (index, sql) in [
            "insert into kerno_ui_txlog.entries values (1)",
            "update kerno_ui_txlog.entries set id = 2 where id = 1",
        ]
        .into_iter()
        .enumerate()
        {
            app.press(cx, &["secondary-a"]);
            app.type_text(cx, sql);
            app.press(cx, &["secondary-enter"]);
            app.wait_until(cx, "the statement", |app, cx| {
                log(cx).len() == index + 1 && !app.running(cx)
            });
        }
        assert_eq!(
            log(cx),
            [
                (
                    "insert into kerno_ui_txlog.entries values (1)".to_string(),
                    "1 row".to_string()
                ),
                (
                    "update kerno_ui_txlog.entries set id = 2 where id = 1".to_string(),
                    "1 row".to_string()
                ),
            ],
            "{engine:?}"
        );
        // From the status bar's transaction item.
        app.click(cx, "status-transaction-button");
        app.click_label(cx, "Transaction log…");
        assert!(
            app.has(cx, ("tx-log-entry", 0usize)) && app.has(cx, ("tx-log-entry", 1usize)),
            "{engine:?}"
        );
        app.press(cx, &["escape"]);
        assert!(!app.dialog_open(cx), "{engine:?}");

        // Rollback empties it.
        app.click(cx, "tab-rollback");
        app.wait_until(cx, "the rollback", |app, cx| {
            !app.status_says(cx, "transaction open")
        });
        assert!(log(cx).is_empty(), "{engine:?}");
        assert_eq!(
            on_server(engine, "SELECT count(*) FROM kerno_ui_txlog.entries"),
            "0"
        );
    }
}

#[gpui_kit::test]
fn properties_open_first_on_f4_and_the_comment_is_set_from_there(cx: &mut TestAppContext) {
    use crate::structure::tab::PROPERTIES_SECTION;
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let (cleanup, setup, path, comment_sql): (&str, Vec<&str>, Vec<&str>, &str) = match engine {
            Engine::Postgres => (
                "DROP SCHEMA IF EXISTS kerno_ui_props CASCADE",
                vec![
                    "CREATE SCHEMA kerno_ui_props",
                    "CREATE TABLE kerno_ui_props.t (id int PRIMARY KEY)",
                    "COMMENT ON TABLE kerno_ui_props.t IS 'first'",
                    "ANALYZE kerno_ui_props.t",
                ],
                vec!["kerno", "kerno_ui_props", "Tables", "t"],
                "SELECT COALESCE(obj_description('kerno_ui_props.t'::regclass, 'pg_class'), '-')",
            ),
            _ => (
                "DROP DATABASE IF EXISTS kerno_ui_props",
                vec![
                    "CREATE DATABASE kerno_ui_props",
                    "CREATE TABLE kerno_ui_props.t (id INT PRIMARY KEY) COMMENT = 'first'",
                ],
                vec!["kerno_ui_props", "Tables", "t"],
                "SELECT COALESCE(NULLIF(TABLE_COMMENT, ''), '-') FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = 'kerno_ui_props' AND TABLE_NAME = 't'",
            ),
        };
        let _scratch = Scratch::new(engine, &[cleanup], &setup);
        let app = connected(cx, engine);
        // The connection's own database opens by itself.
        let start = if path[0] == "kerno" { 2 } else { 1 };
        for depth in start..path.len() {
            let above: Vec<String> = path[..depth].iter().map(|s| s.to_string()).collect();
            app.wait_until(cx, "the way to the table", |app, cx| {
                app.tree_row_of(cx, &above).is_some()
            });
            app.toggle(cx, &path[..depth]);
        }
        let whole: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        app.wait_until(cx, "the table", |app, cx| {
            app.tree_row_of(cx, &whole).is_some()
        });
        // F4 is "Edit": the structure tab on its properties (OBJ-03, CM-33).
        app.click_row(cx, &path, false);
        app.press(cx, &["f4"]);
        let structure = app.structure(cx);
        let properties = |cx: &mut TestAppContext| cx.update(|cx| structure.read(cx).properties());
        app.wait_until(cx, "the properties", |_, cx| properties(cx).is_ok());
        assert_eq!(
            cx.update(|cx| structure.read(cx).section()),
            PROPERTIES_SECTION
        );
        let shown = properties(cx).unwrap();
        let value = |shown: &[(String, String)], name: &str| {
            shown
                .iter()
                .find(|(property, _)| property == name)
                .map(|(_, value)| value.clone())
        };
        match engine {
            Engine::Postgres => {
                assert_eq!(value(&shown, "Owner").as_deref(), Some("kerno"));
                assert_eq!(value(&shown, "Persistence").as_deref(), Some("permanent"));
            }
            _ => assert_eq!(value(&shown, "Engine").as_deref(), Some("InnoDB")),
        }
        assert_eq!(value(&shown, "Comment").as_deref(), Some("first"));
        assert!(app.has(cx, "structure-properties"), "{engine:?}");
        // Times to the second.
        if let Some(analyzed) = value(&shown, "Last analyze") {
            assert!(!analyzed.contains('.'), "{analyzed}");
        }
        // The comment's "Edit…" sits beside its value, not at the far end.
        let (line, edit) = app.step(cx, |window, _| {
            let comment = shown
                .iter()
                .position(|(name, _)| name == "Comment")
                .unwrap();
            (
                window.find(("structure-property", comment)).bounds(),
                window.find("structure-edit-comment").bounds(),
            )
        });
        assert!(
            edit.left() < line.left() + px(400.),
            "{engine:?}: {line:?} {edit:?}"
        );

        // The comment is changed, then removed, each after the statement
        // was shown.
        for (typed, expected, stored) in [("second", "second", "second"), ("", "—", "-")] {
            app.click(cx, "structure-edit-comment");
            assert!(app.has(cx, "sql-name"), "{engine:?}");
            app.press(cx, &["secondary-a", "backspace"]);
            if !typed.is_empty() {
                app.type_text(cx, typed);
            }
            app.click(cx, "sql-confirm");
            app.wait_until(cx, "the new comment", |_, cx| {
                properties(cx)
                    .ok()
                    .is_some_and(|shown| value(&shown, "Comment").as_deref() == Some(expected))
            });
            assert_eq!(on_server(engine, comment_sql), stored, "{engine:?}");
        }
    }
}

#[gpui_kit::test]
fn a_database_is_created_with_the_options_of_the_form(cx: &mut TestAppContext) {
    use crate::database_dialog::Field;
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_created_db";
        let _scratch = Scratch::new(engine, &[&format!("DROP DATABASE IF EXISTS {name}")], &[]);
        let app = connected(cx, engine);
        app.wait_until(cx, "the databases", |app, cx| {
            app.tree(cx).contains(&row(0, "kerno"))
        });
        let kerno = app.tree_row(cx, &["kerno"]);
        let navigator = cx.update(|cx| app.workspace.read(cx).navigator(cx));
        let form = app
            .step(cx, |window, cx| {
                navigator.update(cx, |navigator, cx| {
                    let id = navigator.id_at(kerno, cx);
                    navigator.create_database_form(&id, window, cx)
                })
            })
            .expect("the form");
        app.wait_until(cx, "the server's lists", |_, cx| {
            cx.update(|cx| form.read(cx).ready())
        });
        app.click(cx, "create-database-name");
        app.type_text(cx, name);
        let (expected, check) = match engine {
            Engine::Postgres => {
                cx.update(|cx| {
                    form.update(cx, |form, cx| {
                        assert!(form.offered(Field::Template).contains(&"template0".into()));
                        form.set(Field::Template, Some("template0".into()), cx);
                        form.set(Field::Collation, Some("C".into()), cx);
                        form.set(Field::Ctype, Some("C".into()), cx);
                    })
                });
                (
                    format!(
                        "CREATE DATABASE \"{name}\"\n    TEMPLATE = \"template0\"\n    \
                         LC_COLLATE = 'C'\n    LC_CTYPE = 'C'"
                    ),
                    format!("SELECT datcollate FROM pg_database WHERE datname = '{name}'"),
                )
            }
            _ => {
                cx.update(|cx| {
                    form.update(cx, |form, cx| {
                        form.set(Field::Encoding, Some("latin1".into()), cx);
                        let collations = form.offered(Field::Collation);
                        assert!(collations.contains(&"latin1_bin".into()), "{collations:?}");
                        assert!(
                            !collations.contains(&"utf8mb4_bin".into()),
                            "{collations:?}"
                        );
                        form.set(Field::Collation, Some("latin1_bin".into()), cx);
                    })
                });
                (
                    format!("CREATE DATABASE `{name}` CHARACTER SET latin1 COLLATE latin1_bin"),
                    format!(
                        "SELECT DEFAULT_COLLATION_NAME FROM information_schema.SCHEMATA \
                         WHERE SCHEMA_NAME = '{name}'"
                    ),
                )
            }
        };
        let statement = cx.update(|cx| form.read(cx).statement(cx));
        assert_eq!(statement, Ok(expected.clone()), "{engine:?}");
        // The form gives way to the confirmation, which runs it.
        app.click(cx, "create-database-create");
        assert!(app.has(cx, "sql-preview"), "{engine:?}");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the new database", |app, cx| {
            app.tree(cx).contains(&row(0, name))
        });
        let collation = on_server(engine, &check);
        assert!(
            collation == "C" || collation == "latin1_bin",
            "{engine:?}: {collation}"
        );
    }
}

#[gpui_kit::test]
async fn object_filters_hide_schemas_and_last_after_a_restart(cx: &mut TestAppContext) {
    use crate::navigator::actions::FilterAction;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[
            "DROP SCHEMA IF EXISTS kerno_ui_filter_a CASCADE",
            "DROP SCHEMA IF EXISTS kerno_ui_filter_b CASCADE",
        ],
        &[
            "CREATE SCHEMA kerno_ui_filter_a",
            "CREATE SCHEMA kerno_ui_filter_b",
        ],
    );
    let filter = |app: &Fixture, cx: &mut TestAppContext, action, label: &str| {
        let row = app.tree_row(cx, &["kerno", label]);
        let navigator = cx.update(|cx| app.workspace.read(cx).navigator(cx));
        app.step(cx, |window, cx| {
            navigator.update(cx, |navigator, cx| {
                let id = navigator.id_at(row, cx);
                navigator.perform_filter(action, &id, window, cx)
            })
        });
    };
    let shows =
        |app: &Fixture, cx: &mut TestAppContext, label: &str| app.tree(cx).contains(&row(1, label));
    let app = connected(cx, Engine::Postgres);
    app.wait_until(cx, "the schemas", |app, cx| {
        shows(app, cx, "kerno_ui_filter_a") && shows(app, cx, "kerno_ui_filter_b")
    });
    // The tree's menu has a Filter ▸ entry.
    let row_a = app.tree_row(cx, &["kerno", "kerno_ui_filter_a"]);
    app.right_click(cx, ("node", row_a), None);
    assert!(app.shows_label(cx, "Filter"));
    app.press(cx, &["escape"]);

    // Hide: gone from the tree, counted on its database, kept with the
    // connection.
    filter(&app, cx, FilterAction::Hide, "kerno_ui_filter_a");
    assert!(!shows(&app, cx, "kerno_ui_filter_a") && shows(&app, cx, "kerno_ui_filter_b"));
    let hint = cx.update(|cx| {
        app.workspace
            .read(cx)
            .navigator(cx)
            .read(cx)
            .hint_of("db:kerno")
    });
    assert!(hint.is_some_and(|hint| hint.ends_with("1 hidden")));
    let stored = |app: &Fixture| {
        app.storage.connections().unwrap()[0]
            .config
            .navigator_filters
            .clone()
    };
    app.wait_until(cx, "the stored filter", |app, _| {
        stored(app).contains_key("schemas")
    });

    // After a restart the schema is still hidden.
    let restarted = open(cx, app.storage.clone());
    let id = cx.update(|cx| restarted.connections.read(cx).saved()[0].id.clone());
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    restarted.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    restarted.wait_until(cx, "the schemas", |app, cx| {
        shows(app, cx, "kerno_ui_filter_b")
    });
    assert!(!shows(&restarted, cx, "kerno_ui_filter_a"));

    // Toggle: off shows it, on hides it again; Clear removes the filter.
    filter(&restarted, cx, FilterAction::Toggle, "kerno_ui_filter_b");
    assert!(shows(&restarted, cx, "kerno_ui_filter_a"));
    filter(&restarted, cx, FilterAction::Toggle, "kerno_ui_filter_b");
    assert!(!shows(&restarted, cx, "kerno_ui_filter_a"));
    filter(&restarted, cx, FilterAction::Clear, "kerno_ui_filter_b");
    assert!(shows(&restarted, cx, "kerno_ui_filter_a"));
    restarted.wait_until(cx, "the filter removed", |app, _| stored(app).is_empty());

    // Configure: masks typed one per line.
    filter(&restarted, cx, FilterAction::Configure, "kerno_ui_filter_b");
    assert!(restarted.has(cx, "filter-dialog"));
    // Each field says in one plain line what it does.
    assert_eq!(
        restarted.label_of(cx, "filter-include-label"),
        "Show only: names or masks to keep. Empty keeps every object."
    );
    assert_eq!(
        restarted.label_of(cx, "filter-exclude-label"),
        "Hide: names or masks to leave out."
    );
    restarted.click(cx, "filter-exclude");
    restarted.type_text(cx, "KERNO_UI_FILTER_*");
    restarted.click(cx, "filter-save");
    assert!(
        !shows(&restarted, cx, "kerno_ui_filter_a") && !shows(&restarted, cx, "kerno_ui_filter_b")
    );
    assert!(shows(&restarted, cx, "shop"));
}

#[gpui_kit::test]
fn a_table_is_backed_up_alone_from_the_tree(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        // "Back up…" on a table of the tree: the dialog names it alone.
        let path: Vec<&str> = match engine {
            Engine::Postgres => vec!["kerno", "shop", "Tables", "orders"],
            _ => vec!["kerno", "Tables", "orders"],
        };
        // The connection's own database opens by itself.
        let start = if path[0] == "kerno" { 2 } else { 1 };
        for depth in start..path.len() {
            let above: Vec<String> = path[..depth].iter().map(|s| s.to_string()).collect();
            app.wait_until(cx, "the way to the table", |app, cx| {
                app.tree_row_of(cx, &above).is_some()
            });
            app.toggle(cx, &path[..depth]);
        }
        let whole: Vec<String> = path.iter().map(|s| s.to_string()).collect();
        app.wait_until(cx, "the table", |app, cx| {
            app.tree_row_of(cx, &whole).is_some()
        });
        app.act(cx, Action::Backup, &path);
        // The wizard opens on its settings, once the client is found (or
        // said missing), with the table alone in the command.
        let painted = |app: &Fixture, cx: &mut TestAppContext| -> Vec<String> {
            app.step(cx, |window, _| {
                gpui_kit::base::test_support::texts(window)
                    .into_iter()
                    .map(|text| text.text().to_string())
                    .collect()
            })
        };
        app.wait_until(cx, "the wizard's settings", |app, cx| {
            app.has(cx, "backup-command")
                || (app.has(cx, "backup-tool")
                    && !painted(app, cx)
                        .iter()
                        .any(|text| text.starts_with("Looking for the local clients")))
        });
        let found = app.has(cx, "backup-command");
        if found {
            let command = painted(&app, cx)
                .into_iter()
                .find(|text| text.contains("--verbose"))
                .unwrap_or_default();
            assert!(
                command.contains("orders") && !command.contains("--schema"),
                "{engine:?}: {command}"
            );
        }
        app.click(cx, "backup-close");
        if !found {
            continue;
        }

        // The names are arguments of their own, never read by a shell.
        let dialog = app.backup_dialog(cx, "kerno", false).expect("the dialog");
        let schema = if engine == Engine::Postgres {
            "shop"
        } else {
            "kerno"
        };
        cx.update(|cx| {
            dialog.update(cx, |dialog, cx| {
                dialog.only_tables(vec![TableRef::new(schema, "order items; rm -rf x")], cx)
            })
        });
        let state = app.wait_backup(cx, &dialog, &["no tool", "ready"]);
        if state.starts_with("no tool") {
            app.click(cx, "backup-close");
            continue;
        }
        let command = cx
            .update(|cx| dialog.read(cx).pending_command(cx))
            .expect("a command");
        let expected = match engine {
            Engine::Postgres => "--table=\"shop\".\"order items; rm -rf x\"",
            _ => "order items; rm -rf x",
        };
        assert!(
            command.args.iter().any(|arg| arg == expected),
            "{engine:?}: {:?}",
            command.args
        );
        assert!(state.contains("order items"), "{engine:?}: {state}");
        // Back to the objects: a complete backup again.
        app.click(cx, "wizard-back");
        app.click(cx, "backup-complete");
        let command = cx
            .update(|cx| dialog.read(cx).pending_command(cx))
            .expect("a command");
        assert!(!command.args.iter().any(|arg| arg.contains("order items")));
        app.click(cx, "backup-close");
    }
}

// G18 test debt and the small leftovers of G11–G17 and M10 Part 4
// (docs/plans/PARITY-GAPS.md G18 and its "left" notes) -------------------------

impl Fixture {
    /// Clicks the entry of an open menu with this label, never a row of the
    /// schema tree that has the same name.
    fn click_menu_label(&self, cx: &mut TestAppContext, label: &str) {
        use gpui_kit::test::TestQueryExt as _;
        let tree = gpui_kit::ElementId::from("navigator");
        self.step(cx, |window, cx| {
            let entry = window
                .find_all_by_label(label)
                .into_iter()
                .find(|element| !element.path().contains(&tree))
                .unwrap_or_else(|| panic!("no menu entry {label}"));
            let centre = entry.bounds().center();
            window.drag(centre, centre, cx);
        });
    }

    /// Runs `sql` in the active tab, in place of its text, and waits for its
    /// first row to be `expected`.
    fn run_for_row(&self, cx: &mut TestAppContext, sql: &str, expected: &[&str]) {
        self.press(cx, &["secondary-a", "backspace"]);
        self.type_text(cx, sql);
        self.press(cx, &["secondary-enter"]);
        let expected = cells(expected);
        self.wait_until(cx, sql, |app, cx| {
            !app.running(cx) && app.result(cx, 0, 1).1.first() == Some(&expected)
        });
    }
}

#[gpui_kit::test]
fn the_tab_toolbar_changes_its_connection_database_and_schema(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    let storage = memory_storage();
    let postgres = storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let mysql = storage
        .save_connection(None, &testing::config(Engine::MySql))
        .unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    for id in [&postgres, &mysql] {
        kerno_core::secrets::save_secrets(secrets.as_ref(), id, &testing::secrets()).unwrap();
    }
    for row in [0usize, 1] {
        app.open_connection_row(cx, row);
    }
    app.wait_until(cx, "both connections", |app, cx| {
        (0..2).all(|index| matches!(app.status(cx, index), Status::Connected { .. }))
    });
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    cx.update(|cx| {
        tab.update(cx, |tab, cx| {
            tab.set_target(Some(connection_target(&postgres)), cx)
        })
    });
    let target = |cx: &mut TestAppContext| cx.update(|cx| tab.read(cx).target().cloned());

    // The schema selector: the next run searches that schema first.
    app.wait_until(cx, "the schemas", |_, cx| {
        cx.update(|cx| tab.read(cx).schemas().contains(&"analytics".to_string()))
    });
    app.click(cx, "tab-schema");
    app.click_menu_label(cx, "analytics");
    assert_eq!(
        target(cx).and_then(|target| target.schema),
        Some("analytics".into())
    );
    app.run_for_row(
        cx,
        "select current_database(), current_schema()",
        &["kerno", "analytics"],
    );

    // The database selector: a session on the other database.
    app.wait_until(cx, "the databases", |_, cx| {
        cx.update(|cx| {
            tab.read(cx)
                .databases(cx)
                .contains(&"kerno_other".to_string())
        })
    });
    app.click(cx, "tab-database");
    app.click_menu_label(cx, "kerno_other");
    app.run_for_row(
        cx,
        "select current_database(), current_schema()",
        &["kerno_other", "public"],
    );

    // The connection selector: the tab runs on MySQL now, then on another
    // of its databases.
    let name = cx.update(|cx| app.connections.read(cx).saved()[1].config.name.clone());
    app.click(cx, "tab-connection");
    app.click_menu_label(cx, &name);
    assert_eq!(target(cx).map(|target| target.connection), Some(mysql));
    assert!(!app.has(cx, "tab-schema"));
    app.run_for_row(cx, "select database()", &["kerno"]);
    app.wait_until(cx, "the MySQL databases", |_, cx| {
        cx.update(|cx| {
            tab.read(cx)
                .databases(cx)
                .contains(&"kerno_other".to_string())
        })
    });
    app.click(cx, "tab-database");
    app.click_menu_label(cx, "kerno_other");
    app.run_for_row(cx, "select database()", &["kerno_other"]);
}

/// A target on a connection's own database.
fn connection_target(id: &str) -> Target {
    Target {
        connection: id.to_string(),
        database: None,
        schema: None,
    }
}

#[gpui_kit::test]
fn the_editor_closes_brackets_and_quotes(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    let typed = |cx: &mut TestAppContext, text: &str| {
        for character in text.chars() {
            app.type_text(cx, &character.to_string());
        }
    };
    typed(cx, "select (");
    assert_eq!(app.active_query(cx).1, "select ()");
    // Typing the closer steps over the one already there.
    typed(cx, "1)");
    assert_eq!(app.active_query(cx).1, "select (1)");
    typed(cx, " || '");
    assert_eq!(app.active_query(cx).1, "select (1) || ''");
    typed(cx, "a'");
    assert_eq!(app.active_query(cx).1, "select (1) || 'a'");
    typed(cx, " || \"x");
    assert_eq!(app.active_query(cx).1, "select (1) || 'a' || \"x\"");
    // Inside a word nothing is added.
    app.press(cx, &["secondary-a", "backspace"]);
    typed(cx, "ab");
    app.press(cx, &["left"]);
    typed(cx, "(");
    assert_eq!(app.active_query(cx).1, "a(b");
}

#[gpui_kit::test]
fn the_editor_adds_a_cursor_above_and_below(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "a\nb\nc");
    // The editor's keys: Cmd+Alt+Up / Down on macOS, Shift+Alt+Up / Down and
    // Ctrl+Home on Linux (K65), Ctrl+Alt+Up / Down and Ctrl+Home on Windows (K68).
    let (above, below, start) = if cfg!(target_os = "macos") {
        ("cmd-alt-up", "cmd-alt-down", "cmd-up")
    } else if cfg!(windows) {
        ("ctrl-alt-up", "ctrl-alt-down", "ctrl-home")
    } else {
        ("shift-alt-up", "shift-alt-down", "ctrl-home")
    };
    // From the last line, two cursors above: one letter typed on each line.
    app.press(cx, &[above, above]);
    app.type_text(cx, "x");
    assert_eq!(app.active_query(cx).1, "ax\nbx\ncx");
    // From the first line, one cursor below.
    app.press(cx, &["escape", start, below]);
    app.type_text(cx, "-");
    assert_eq!(app.active_query(cx).1, "-ax\n-bx\ncx");
}

#[gpui_kit::test]
fn scrolling_to_the_end_of_a_result_fetches_the_next_page(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g from generate_series(1, 2500) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx) == ["1000 rows, more"]
    });
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    // The list scrolled to its last row asks for the next page as it is
    // drawn (the kit's `load_more`); nothing is clicked.
    app.step(cx, |_, cx| {
        grid.update(cx, |table, cx| table.scroll_to_row(999, cx))
    });
    app.step(cx, |_, _| {});
    app.wait_until(cx, "the second page", |app, cx| {
        app.outcomes(cx) == ["2000 rows, more"]
    });
    // Not past it: the next page waits for the next scroll.
    app.step(cx, |_, _| {});
    assert_eq!(app.outcomes(cx), ["2000 rows, more"]);
}

#[gpui_kit::test]
fn a_dragged_column_keeps_its_new_place(cx: &mut TestAppContext) {
    use gpui_kit::component::table::{ColumnSort, TableDelegate as _};

    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g as n, 'v' || g as label, g % 2 = 0 as even from generate_series(1, 3) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["3 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    let columns =
        |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).delegate().shown_columns());
    let shown = |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).delegate().shown());
    // The first column's header dragged onto the last (the row numbers are
    // column 0 of the table).
    app.step(cx, |window, cx| {
        window.drag_to(("col-header", 1usize), ("col-header", 3usize), cx)
    });
    assert_eq!(columns(cx), ["label", "even", "n"]);
    assert_eq!(shown(cx)[0], ["v1", "false", "1"]);
    // Sorted afterwards, the columns stay where they were put.
    app.step(cx, |window, cx| {
        grid.update(cx, |table, cx| {
            table
                .delegate_mut()
                .perform_sort(3, ColumnSort::Descending, window, cx)
        })
    });
    assert_eq!(columns(cx), ["label", "even", "n"]);
    assert_eq!(shown(cx)[0], ["v3", "false", "3"]);
}

#[gpui_kit::test]
fn a_table_form_draft_survives_a_disconnect_and_reconnect(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_table_editor(
                connection_target(&id),
                "shop".into(),
                Some(TableRef::new("shop", "orders")),
                window,
                cx,
            );
        })
    });
    app.wait_for_editor(cx);
    app.fill_in(cx, "editor-comment", "kerno_ui draft comment");
    let editor = app.editor(cx);
    let draft = cx.update(|cx| editor.read(cx).draft(cx));
    let sql = app.editor_sql(cx);
    assert!(
        sql.as_ref()
            .is_ok_and(|sql| sql.contains("kerno_ui draft comment"))
    );

    // A form is not a transaction: the disconnect does not ask, and the
    // draft stays as it was typed, through the reconnection.
    app.sidebar_does(
        cx,
        Subject::Connection(id.clone()),
        SidebarAction::Disconnect,
    );
    assert!(!app.dialog_open(cx));
    app.wait_until(cx, "the disconnection", |app, cx| {
        app.status(cx, 0) == Status::Disconnected
    });
    assert!(cx.update(|cx| editor.read(cx).dirty(cx)));
    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::Connect);
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    app.wait_for_editor(cx);
    assert!(cx.update(|cx| editor.read(cx).draft(cx)) == draft);
    assert!(cx.update(|cx| editor.read(cx).dirty(cx)));
    assert_eq!(app.editor_sql(cx), sql);
}

#[gpui_kit::test]
fn read_only_and_its_question_on_mysql(cx: &mut TestAppContext) {
    if !testing::available(Engine::MySql) {
        return;
    }
    let app = connected(cx, Engine::MySql);
    app.press(cx, &["secondary-t"]);
    app.run_for_row(cx, "select @@session.transaction_read_only", &["0"]);

    // A transaction open: the toggle asks; Cancel changes nothing.
    app.open_transaction(cx, "select 1");
    app.dispatch(cx, actions::ToggleReadOnly);
    assert!(app.has(cx, "end-question"));
    app.click(cx, "end-cancel");
    assert!(!app.first_stored().read_only);
    assert_eq!(app.tx_state(cx), kerno_core::TxState::Open);

    // Rolled back: saved, and every session reopened read-only.
    app.dispatch(cx, actions::ToggleReadOnly);
    app.click(cx, "end-rollback");
    app.wait_until(cx, "the read-only reconnection", |app, cx| {
        app.first_stored().read_only && matches!(app.status(cx, 0), Status::Connected { .. })
    });
    let tab = app.tab(cx);
    cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(false, cx)));
    app.run_for_row(cx, "select @@session.transaction_read_only", &["1"]);

    // No transaction: off again without a question.
    app.dispatch(cx, actions::ToggleReadOnly);
    assert!(!app.dialog_open(cx));
    app.wait_until(cx, "changes allowed again", |app, cx| {
        !app.first_stored().read_only && matches!(app.status(cx, 0), Status::Connected { .. })
    });
    app.run_for_row(cx, "select @@session.transaction_read_only", &["0"]);
}

#[gpui_kit::test]
fn a_new_transaction_after_a_lost_connection_starts_an_empty_log(cx: &mut TestAppContext) {
    // A role of its own: its sessions are the ones cut.
    const ROLE: &str = "kerno_ui_txlog_lost";
    const PASSWORD: &str = "kerno_ui_txlog_lost_pw";
    if !testing::available(Engine::Postgres) {
        return;
    }
    let server = |sql: &str| on_server(Engine::Postgres, sql);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[
            &format!(
                "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE usename = '{ROLE}'"
            ),
            &format!("DROP SCHEMA IF EXISTS {ROLE} CASCADE"),
            &format!("DROP ROLE IF EXISTS {ROLE}"),
        ],
        &[
            &format!("CREATE ROLE {ROLE} LOGIN PASSWORD '{PASSWORD}'"),
            &format!("CREATE SCHEMA {ROLE} AUTHORIZATION {ROLE}"),
            &format!("CREATE TABLE {ROLE}.entries (id int PRIMARY KEY)"),
            &format!("ALTER TABLE {ROLE}.entries OWNER TO {ROLE}"),
        ],
    );
    let storage = memory_storage();
    let config = ConnectionConfig {
        user: ROLE.into(),
        ..testing::config(Engine::Postgres)
    };
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let password = Secrets {
        password: Some(Secret::new(PASSWORD)),
        ..Secrets::default()
    };
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &password).unwrap();
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    let log = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            tab.read(cx)
                .transaction_log()
                .iter()
                .map(|entry| entry.sql.clone())
                .collect::<Vec<_>>()
        })
    };
    let first = format!("insert into {ROLE}.entries values (1)");
    app.open_transaction(cx, &first);
    assert_eq!(log(cx), std::slice::from_ref(&first));

    // The server cuts the session: its transaction is gone; its log stays, marked rolled back
    // by the server, until the next statement (K16, C15).
    server(&format!(
        "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE usename = '{ROLE}'"
    ));
    app.dispatch(cx, actions::RefreshConnections);
    app.wait_until(cx, "the tab to say so", |_, cx| {
        cx.update(|cx| tab.read(cx).messages())
            .iter()
            .any(|message| message.contains("the server rolled it back"))
    });
    app.wait_until(cx, "the reconnection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    assert_eq!(log(cx), [first]);

    // The next statement opens a new transaction whose log holds it alone.
    let second = format!("insert into {ROLE}.entries values (2)");
    app.open_transaction(cx, &second);
    assert_eq!(log(cx), [second]);
}

/// The notifications on screen.
fn notification_count(app: &Fixture, cx: &mut TestAppContext) -> usize {
    use gpui_kit::component::WindowExt as _;
    app.step(cx, |window, cx| window.notifications(cx).len())
}

#[gpui_kit::test]
fn f4_on_a_database_or_a_schema_shows_its_properties(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let pg = engine == Engine::Postgres;
        let (cleanup, setup): (&str, Vec<&str>) = if pg {
            (
                "DROP SCHEMA IF EXISTS kerno_ui_f4_props CASCADE",
                vec![
                    "CREATE SCHEMA kerno_ui_f4_props",
                    "CREATE TABLE kerno_ui_f4_props.items (id int PRIMARY KEY)",
                    "COMMENT ON SCHEMA kerno_ui_f4_props IS 'kerno props'",
                ],
            )
        } else if engine == Engine::MariaDb {
            (
                "DROP DATABASE IF EXISTS kerno_ui_f4_props",
                vec![
                    "CREATE DATABASE kerno_ui_f4_props CHARACTER SET latin1 COLLATE latin1_bin \
                     COMMENT 'kerno props'",
                    "CREATE TABLE kerno_ui_f4_props.items (id INT PRIMARY KEY)",
                ],
            )
        } else {
            (
                "DROP DATABASE IF EXISTS kerno_ui_f4_props",
                vec![
                    "CREATE DATABASE kerno_ui_f4_props CHARACTER SET latin1 COLLATE latin1_bin",
                    "CREATE TABLE kerno_ui_f4_props.items (id INT PRIMARY KEY)",
                ],
            )
        };
        let _scratch = Scratch::new(engine, &[cleanup], &setup);
        let app = connected(cx, engine);
        let node: Vec<&str> = if pg {
            vec!["kerno", "kerno_ui_f4_props"]
        } else {
            vec!["kerno_ui_f4_props"]
        };
        app.wait_until(cx, "the scratch node", |app, cx| app.shows(cx, &node));
        app.click_row(cx, &node, false);
        app.press(cx, &["f4"]);
        app.wait_until(cx, "the properties", |app, cx| {
            app.has(cx, "object-properties")
        });
        let expected: &[&str] = if pg {
            &["Owner: kerno", "Tables: 1", "Comment: kerno props"]
        } else if engine == Engine::MariaDb {
            &[
                "Character set: latin1",
                "Collation: latin1_bin",
                "Tables: 1",
                "Comment: kerno props",
            ]
        } else {
            &[
                "Character set: latin1",
                "Collation: latin1_bin",
                "Tables: 1",
            ]
        };
        for label in expected {
            assert!(app.shows_label(cx, label), "{engine:?}: {label}");
        }
        app.press(cx, &["escape"]);
        assert!(!app.dialog_open(cx), "{engine:?}");
        // The DDL is still a menu entry of its own.
        if !pg {
            assert_eq!(app.why_not(cx, Action::ShowDdl, &node), None);
            continue;
        }
        // A PostgreSQL database too, from its menu entry.
        app.act(cx, Action::Properties, &["kerno"]);
        app.wait_until(cx, "the database's properties", |app, cx| {
            app.has(cx, "object-properties")
        });
        assert!(app.shows_label(cx, "Encoding: UTF8"));
        app.press(cx, &["escape"]);
    }
}

#[gpui_kit::test]
fn cmd_alt_enter_opens_a_console_on_the_selected_node(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let analytics = ["kerno", "analytics"];
    app.wait_until(cx, "the schema", |app, cx| app.shows(cx, &analytics));
    app.click_row(cx, &analytics, false);
    // DBeaver's Ctrl+Alt+Enter (`core.sql.editor.console`), beside Cmd+].
    app.press(cx, &["secondary-alt-enter"]);
    app.wait_until(cx, "the console", |app, cx| app.titles(cx).len() == 1);
    let tab = app.tab(cx);
    let target = cx
        .update(|cx| tab.read(cx).target().cloned())
        .expect("a target");
    assert_eq!(
        (target.database.as_deref(), target.schema.as_deref()),
        (Some("kerno"), Some("analytics"))
    );
    app.run_for_row(cx, "select current_schema()", &["analytics"]);
}

#[gpui_kit::test]
fn link_with_editor_says_when_a_filter_hides_the_table(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let orders = ["kerno", "shop", "Tables", "orders"];
    app.open_path(cx, &orders);
    app.click_row(cx, &orders, false);
    app.dispatch(cx, actions::HideObjectInTree);
    app.wait_until(cx, "orders hidden", |app, cx| !app.shows(cx, &orders));
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    let before = notification_count(&app, cx);
    // On: its own notice, then the reason nothing is selected.
    app.dispatch(cx, actions::LinkWithEditor);
    app.wait_until(cx, "the warning", |app, cx| {
        notification_count(app, cx) == before + 2
    });
    // The filter is left as it was.
    assert!(!app.shows(cx, &orders));
    assert_eq!(app.selected_row(cx), None);
}

#[gpui_kit::test]
fn tree_entries_have_palette_commands_on_the_selected_node(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let customers = ["kerno", "shop", "Tables", "customers"];
    let function = [
        "kerno",
        "shop",
        "Functions",
        "customer_lifetime_value(p_customer_id bigint)",
    ];
    app.open_path(cx, &customers);
    app.open_path(cx, &function);

    // Read data in SQL console.
    app.click_row(cx, &customers, false);
    app.dispatch(cx, actions::ReadObjectInConsole);
    app.wait_until(cx, "the rows", |app, cx| {
        app.titles(cx)
            .last()
            .is_some_and(|title| title == "customers")
            && !app.running(cx)
            && app.outcomes(cx).len() == 1
    });
    assert!(app.active_query(cx).1.starts_with("SELECT"));

    // Generate CALL, then Execute… which asks for the parameter.
    app.click_row(cx, &function, false);
    app.dispatch(cx, actions::GenerateRoutineCall);
    app.wait_until(cx, "the call", |app, cx| {
        app.titles(cx)
            .contains(&"customer_lifetime_value".to_string())
    });
    assert!(app.active_query(cx).1.contains("customer_lifetime_value"));
    assert_eq!(app.outcomes(cx), Vec::<String>::new(), "nothing ran");
    app.click_row(cx, &function, false);
    app.dispatch(cx, actions::ExecuteRoutine);
    app.wait_until(cx, "the prompt", |app, cx| app.has(cx, ("param", 0usize)));
    app.press(cx, &["escape"]);

    // Add column…: the table's form on a new column.
    app.click_row(cx, &customers, false);
    app.dispatch(cx, actions::AddColumnToObject);
    app.wait_until(cx, "the form", |app, cx| {
        cx.update(|cx| app.workspace.read(cx).active_table_editor().is_some())
    });

    // Back up…: the dialog, on that table alone.
    app.click_row(cx, &customers, false);
    app.dispatch(cx, actions::BackupObject);
    app.wait_until(cx, "the backup", |app, cx| app.has(cx, "backup-close"));
    app.click(cx, "backup-close");
}

#[gpui_kit::test]
fn the_lock_manager_opens_from_the_connection_menu_and_the_palette(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let section = |app: &Fixture, cx: &mut TestAppContext| {
        let tab = app.admin(cx);
        cx.update(|cx| tab.read(cx).section())
    };
    app.sidebar_does(
        cx,
        Subject::Connection(id.clone()),
        SidebarAction::Tool(crate::navigator::actions::ToolEntry::LockManager),
    );
    assert_eq!(section(&app, cx), crate::admin::tab::LOCKS);
    app.on_admin(cx, |tab, window, cx| {
        tab.select(crate::admin::tab::SESSIONS, window, cx)
    });
    // From the Database menu's command: the same tab, on its locks.
    app.dispatch(cx, actions::OpenLockManager);
    assert_eq!(app.titles(cx).len(), 1);
    assert_eq!(section(&app, cx), crate::admin::tab::LOCKS);
}

#[gpui_kit::test]
fn the_grid_has_the_keyboard_after_apply(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (drop_schema, create_schema) = scratch_schema(Engine::Postgres, "kerno_ui_apply_focus");
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[&drop_schema],
        &[
            &create_schema,
            &scratch_table(
                Engine::Postgres,
                "kerno_ui_apply_focus.items",
                "name VARCHAR(20)",
            ),
            "INSERT INTO kerno_ui_apply_focus.items (name) VALUES ('a')",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "kerno_ui_apply_focus", "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    app.type_in_cell(cx, 0, 2, "b", "enter");
    assert_eq!(app.pending(cx), 1);
    let grid = app.data_grid(cx);
    app.step(cx, |window, cx| {
        grid.read(cx).focus_handle(cx).focus(window, cx)
    });
    app.press(cx, &["secondary-s"]);
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the new page", |app, cx| {
        app.pending(cx) == 0 && app.data_grid(cx) != grid && app.data_rows(cx)[0][1] == "b"
    });
    // No click: the next key reaches the new grid (Alt+Insert adds a row).
    let fresh = app.data_grid(cx);
    assert!(app.step(cx, |window, cx| {
        fresh.read(cx).focus_handle(cx).is_focused(window)
    }));
    app.press(cx, &["alt-insert"]);
    assert_eq!(app.pending(cx), 1);
}

#[gpui_kit::test]
fn go_to_row_and_column_are_in_the_navigate_menu_of_a_cell(cx: &mut TestAppContext) {
    use gpui_kit::test::ClickOptions;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g as a, g as b from generate_series(1, 5) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["5 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    let right_click = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, cx| {
            window.click_with_options(
                "table-inner",
                ClickOptions::new()
                    .with_offset(gpui_kit::point(px(90.), px(44.)))
                    .with_button(gpui_kit::MouseButton::Right),
                cx,
            )
        })
    };
    right_click(&app, cx);
    click_menu_entry(&app, cx, "Navigate");
    click_menu_entry(&app, cx, "Go to row…");
    assert!(app.has(cx, "go-to-row"));
    app.type_text(cx, "4");
    app.press(cx, &["enter"]);
    assert_eq!(
        cx.update(|cx| grid.read(cx).selected_cell())
            .map(|(row, _)| row),
        Some(3)
    );
    right_click(&app, cx);
    click_menu_entry(&app, cx, "Navigate");
    click_menu_entry(&app, cx, "Go to column…");
    assert!(app.has(cx, "go-to-column"));
    app.press(cx, &["escape"]);
}

// G5: filters, order, counts and auto-refresh on results (docs/plans/PARITY-GAPS.md)

/// The fixture's orders and its 1,000,000-row table, on each engine.
fn g5_tables(engine: Engine) -> (&'static str, &'static str) {
    match engine {
        Engine::Postgres => ("shop.orders", "analytics.big_table"),
        Engine::MySql | Engine::MariaDb => ("kerno.orders", "kerno_analytics.big_table"),
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    }
}

/// How the result on screen of the active query tab is read again, and
/// its count.
fn g5_requery(
    app: &Fixture,
    cx: &mut TestAppContext,
) -> (Option<crate::query::tab::Requery>, Option<u64>) {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).result_requery())
}

#[gpui_kit::test]
fn a_select_result_is_filtered_ordered_and_counted_on_the_server(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let (orders, big) = g5_tables(engine);
        let condition = "status = 'shipped' AND id <= 400";
        let shipped = on_server(
            engine,
            &format!("select count(*) from {orders} where {condition}"),
        );
        let app = connected(cx, engine);
        // A line comment and `;` are no trouble to the derived table.
        app.run_in_new_tab(
            cx,
            &format!("select id, status -- every order\nfrom {orders};"),
            "secondary-enter",
        );
        app.wait_until(cx, "the orders", |app, cx| {
            app.outcomes(cx).len() == 1 && !app.running(cx)
        });
        let tab = app.tab(cx);
        app.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| tab.filter_result(condition, window, cx))
        });
        app.wait_until(cx, "the shipped orders", |app, cx| {
            app.outcomes(cx) == [format!("{shipped} rows")] && !app.running(cx)
        });
        let rows = app.result(cx, 0, 1000).1;
        assert!(
            rows.iter().all(|row| row[1].as_deref() == Some("shipped")),
            "{engine:?}"
        );
        let (requery, _) = g5_requery(&app, cx);
        let requery = requery.expect("a SELECT is read again");
        assert_eq!(requery.filter, condition, "{engine:?}");
        assert!(requery.statement.contains("-- every order\n"), "{engine:?}");
        // The filter is offered again by the history of the statement.
        app.wait_until(cx, "the history", |_, cx| {
            cx.update(|cx| tab.read(cx).result_filter_history()) == [condition]
        });
        assert!(app.has(cx, "result-filter-origin"), "{engine:?}");

        // A header sort on a result not wholly fetched is the server's: the
        // first page holds the last ids, and paging goes on.
        app.run_in_new_tab(
            cx,
            &format!("select id, bucket from {big}"),
            "secondary-enter",
        );
        app.wait_until(cx, "the first page", |app, cx| {
            app.outcomes(cx) == ["1000 rows, more"]
        });
        let tab = app.tab(cx);
        let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| {
                table
                    .delegate_mut()
                    .perform_sort(1, ColumnSort::Descending, window, cx)
            })
        });
        app.wait_until(cx, "the order of the server", |app, cx| {
            let (requery, _) = g5_requery(app, cx);
            requery.and_then(|requery| requery.sort) == Some(("id".into(), true))
                && app.outcomes(cx) == ["1000 rows, more"]
        });
        assert_eq!(
            app.result(cx, 0, 1).1[0][0].as_deref(),
            Some("1000000"),
            "{engine:?}"
        );
        let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
        assert_eq!(
            cx.update(|cx| grid.read(cx).delegate().ordered_by_server()),
            Some(true),
            "{engine:?}"
        );
        // Counted on a session of its own: the result keeps its pages.
        app.dispatch(cx, actions::CalculateRowCount);
        app.wait_until(cx, "the count", |app, cx| {
            g5_requery(app, cx).1 == Some(1_000_000)
        });
        app.click(cx, "fetch-more");
        app.wait_until(cx, "the next page", |app, cx| {
            app.outcomes(cx) == ["2000 rows, more"]
        });

        // A CTE with its own ORDER BY and LIMIT is read inside, as it is.
        app.run_in_new_tab(
            cx,
            &format!(
                "with t as (select id from {orders}) select * from t order by id desc limit 5"
            ),
            "secondary-enter",
        );
        app.wait_until(cx, "the last orders", |app, cx| {
            app.outcomes(cx) == ["5 rows"]
        });
        let tab = app.tab(cx);
        app.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| tab.filter_result("id > 0", window, cx))
        });
        app.wait_until(cx, "the filtered last orders", |app, cx| {
            g5_requery(app, cx)
                .0
                .is_some_and(|requery| requery.filter == "id > 0")
                && app.outcomes(cx) == ["5 rows"]
        });
        let last = on_server(engine, &format!("select max(id) from {orders}"));
        let mut ids: Vec<String> = app
            .result(cx, 0, 5)
            .1
            .into_iter()
            .filter_map(|row| row[0].clone())
            .collect();
        ids.sort_by_key(|id| std::cmp::Reverse(id.parse::<u64>().unwrap()));
        assert_eq!(ids[0], last, "{engine:?}");

        // MySQL and MariaDB refuse two columns of one name in a derived
        // table: nothing runs, and the result stays.
        if engine != Engine::Postgres {
            app.run_in_new_tab(cx, "select 1 as a, 2 as a", "secondary-enter");
            app.wait_until(cx, "the twins", |app, cx| app.outcomes(cx) == ["1 rows"]);
            let tab = app.tab(cx);
            app.step(cx, |window, cx| {
                tab.update(cx, |tab, cx| tab.filter_result("a = 1", window, cx))
            });
            cx.run_until_parked();
            assert_eq!(
                g5_requery(&app, cx).0.map(|requery| requery.filter),
                Some(String::new()),
                "{engine:?}"
            );
            assert!(!app.running(cx), "{engine:?}");
        }
    }
}

#[gpui_kit::test]
fn distinct_values_of_a_result_and_of_a_table_become_a_filter(cx: &mut TestAppContext) {
    use crate::query::grid::GridAction;

    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let (orders, _) = g5_tables(engine);
        let top = on_server(
            engine,
            &format!(
                "select status from {orders} where id <= 300 group by status \
                 order by count(*) desc, status limit 1"
            ),
        );
        let held = on_server(
            engine,
            &format!("select count(*) from {orders} where id <= 300 and status = '{top}'"),
        );
        let app = connected(cx, engine);
        app.run_in_new_tab(
            cx,
            &format!("select id, status from {orders} where id <= 300"),
            "secondary-enter",
        );
        app.wait_until(cx, "the orders", |app, cx| app.outcomes(cx) == ["300 rows"]);
        let tab = app.tab(cx);
        let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| table.set_selected_cell(0, 2, cx));
            Grid::perform(&grid, GridAction::DistinctValues, window, cx)
        });
        app.wait_until(cx, "the values", |app, cx| app.has(cx, "distinct-dialog"));
        // The most frequent value is listed first.
        app.click(cx, ("distinct-value", 0usize));
        app.click(cx, "distinct-apply");
        app.wait_until(cx, "the orders of the value", |app, cx| {
            app.outcomes(cx) == [format!("{held} rows")] && !app.running(cx)
        });
        let filter = g5_requery(&app, cx).0.expect("a requery").filter;
        assert!(filter.contains(&format!("IN ('{top}')")), "{filter}");

        // On a table's data: the picked values join the filter, NULL too.
        let schema = "kerno_ui_g5_distinct";
        let _scratch = filter_scratch(engine, schema);
        app.open_table(cx, schema, "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–4 of 4");
        let ids = |app: &Fixture, cx: &mut TestAppContext| -> Vec<String> {
            app.data_rows(cx)
                .into_iter()
                .map(|row| row[0].clone())
                .collect()
        };
        app.on_data(cx, |tab, window, cx| tab.distinct_values(2, window, cx));
        app.wait_until(cx, "the names", |app, cx| app.has(cx, "distinct-dialog"));
        // "b" is held by two rows: first.
        app.click(cx, ("distinct-value", 0usize));
        app.click(cx, "distinct-apply");
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(ids(&app, cx), ["2", "4"], "{engine:?}");
        app.on_data(cx, |tab, window, cx| {
            tab.set_filter(Filter::default(), window, cx)
        });
        app.wait_for_rows(cx, "Rows 1–4 of 4");
        // One row each: in order of value, NULL where the engine sorts it.
        app.on_data(cx, |tab, window, cx| tab.distinct_values(3, window, cx));
        app.wait_until(cx, "the quantities", |app, cx| {
            app.has(cx, "distinct-dialog")
        });
        let (three, null) = match engine {
            Engine::Postgres => (2usize, 3usize),
            _ => (3, 0),
        };
        app.click(cx, ("distinct-value", three));
        app.click(cx, ("distinct-value", null));
        app.click(cx, "distinct-apply");
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(ids(&app, cx), ["3", "4"], "{engine:?}");
    }
}

#[gpui_kit::test]
fn the_filters_applied_to_a_table_are_offered_again(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let schema = "kerno_ui_g5_history";
    let _scratch = filter_scratch(Engine::Postgres, schema);
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, schema, "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–4 of 4");
    let filter = |raw: &str, conditions: Vec<Condition>| Filter {
        conditions,
        raw: raw.into(),
    };
    app.on_data(cx, |tab, window, cx| {
        tab.set_filter(filter("qty > 1", Vec::new()), window, cx)
    });
    app.wait_for_rows(cx, "Rows 1–2 of 2");
    app.on_data(cx, |tab, window, cx| {
        tab.set_filter(filter("", vec![Condition::equals("name", "a")]), window, cx)
    });
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    let expected = ["\"name\" = 'a'", "qty > 1"];
    let history = |app: &Fixture, cx: &mut TestAppContext| -> Vec<String> {
        let tab = app.data(cx);
        cx.update(|cx| tab.read(cx).history().to_vec())
    };
    app.wait_until(cx, "the history", |app, cx| history(app, cx) == expected);

    // Opened again, the table offers both, the latest first.
    app.dispatch(cx, actions::CloseTab);
    app.open_table(cx, schema, "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–4 of 4");
    app.wait_until(cx, "the stored history", |app, cx| {
        history(app, cx) == expected
    });
    // A filter of the history is applied as its text.
    app.on_data(cx, |tab, window, cx| {
        tab.set_filter(filter(expected[1], Vec::new()), window, cx)
    });
    app.wait_for_rows(cx, "Rows 1–2 of 2");
}

#[gpui_kit::test]
fn a_large_table_shows_its_estimate_and_counts_on_demand(cx: &mut TestAppContext) {
    use crate::data::tab::Total;

    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let schema = match engine {
            Engine::Postgres => "analytics",
            _ => "kerno_analytics",
        };
        let app = connected(cx, engine);
        app.open_table(cx, schema, "big_table", RelationKind::Table);
        let total = |app: &Fixture, cx: &mut TestAppContext| {
            let tab = app.data(cx);
            cx.update(|cx| tab.read(cx).total())
        };
        app.wait_until(cx, "the count", |app, cx| total(app, cx).is_some());
        // Under the threshold of the planner's estimate, the table is
        // counted; above, the estimate is shown and "Count exactly" counts.
        app.on_data(cx, |tab, window, cx| {
            tab.set_estimate_above(1000);
            tab.refresh(window, cx);
        });
        app.wait_until(cx, "the estimate", |app, cx| {
            matches!(total(app, cx), Some(Total::About(_)))
        });
        // The estimate can be known before the rows are back: wait for
        // them (the state read "loading" once on a busy machine).
        app.wait_until(cx, "the rows", |app, cx| app.data_state(cx) != "loading");
        let shown = app.data_state(cx);
        assert!(shown.starts_with("Rows 1–1,000 of about "), "{shown}");
        app.click(cx, "data-count");
        app.wait_until(cx, "the exact count", |app, cx| {
            total(app, cx) == Some(Total::Exact(1_000_000))
        });
        assert_eq!(app.data_state(cx), "Rows 1–1,000 of 1,000,000");
        // A filtered table is counted, whatever its estimate.
        app.on_data(cx, |tab, window, cx| {
            tab.set_filter(
                Filter {
                    conditions: Vec::new(),
                    raw: "id <= 10".into(),
                },
                window,
                cx,
            )
        });
        app.wait_for_rows(cx, "Rows 1–10 of 10");
        // The command counts too.
        app.dispatch(cx, actions::CalculateRowCount);
        app.wait_until(cx, "the count again", |app, cx| {
            total(app, cx) == Some(Total::Exact(10))
        });
        app.dispatch(cx, actions::CloseTab);
    }
}

#[gpui_kit::test]
fn auto_refresh_reads_again_until_a_change_or_another_tab(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let schema = "kerno_ui_g5_auto";
    let _scratch = filter_scratch(engine, schema);
    let app = connected(cx, engine);
    app.open_table(cx, schema, "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–4 of 4");
    let every = Duration::from_secs(5);
    let data = app.data(cx);
    app.on_data(cx, |tab, window, cx| {
        tab.set_auto_refresh(Some(every), window, cx)
    });
    assert!(app.has(cx, "data-auto-refresh-on"));
    on_server(
        engine,
        &format!("INSERT INTO {schema}.items (name, qty) VALUES ('d', 4)"),
    );
    cx.executor().advance_clock(every);
    app.wait_for_rows(cx, "Rows 1–5 of 5");
    assert_eq!(cx.update(|cx| data.read(cx).auto_refresh()), Some(every));

    // A pending change stops it: a refresh would drop it.
    app.type_in_cell(cx, 0, 2, "z", "enter");
    assert_eq!(cx.update(|cx| data.read(cx).auto_refresh()), None);
    assert!(app.has(cx, "data-auto-refresh-stopped"));
    app.click(cx, "data-discard");

    // Another tab on screen stops it.
    app.on_data(cx, |tab, window, cx| {
        tab.set_auto_refresh(Some(every), window, cx)
    });
    app.run_in_new_tab(
        cx,
        &format!("select count(*) as n from {schema}.items"),
        "secondary-enter",
    );
    app.wait_until(cx, "the count", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert_eq!(cx.update(|cx| data.read(cx).auto_refresh()), None);

    // A result is read again in its place, until another run replaces it.
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| tab.set_auto_refresh(Some(every), window, cx))
    });
    assert!(app.has(cx, "result-auto-refresh"));
    on_server(
        engine,
        &format!("INSERT INTO {schema}.items (name, qty) VALUES ('e', 5)"),
    );
    cx.executor().advance_clock(every);
    app.wait_until(cx, "the new count", |app, cx| {
        app.result(cx, 0, 1).1 == [cells(&["6"])] && !app.running(cx)
    });
    assert_eq!(cx.update(|cx| tab.read(cx).auto_refresh()), Some(every));
    // What is being typed in the filter stays; the history keeps the first
    // run only.
    let storage = cx.update(|cx| AppState::get(cx).storage.clone());
    let runs = storage.history(None, "", 100).unwrap().len();
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| tab.type_result_filter("n > 0", window, cx))
    });
    let shown = |cx: &mut TestAppContext| {
        cx.update(|cx| tab.read(cx).grid_of(0).map(|grid| grid.entity_id()))
    };
    let before = shown(cx);
    cx.executor().advance_clock(every);
    app.wait_until(cx, "another refresh", |app, cx| {
        shown(cx) != before && !app.running(cx) && app.result(cx, 0, 1).1 == [cells(&["6"])]
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| tab.read(cx).typed_result_filter(cx)),
        "n > 0"
    );
    assert_eq!(storage.history(None, "", 100).unwrap().len(), runs);
    assert_eq!(cx.update(|cx| tab.read(cx).auto_refresh()), Some(every));
    app.retype(cx, "select 1");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the other run", |app, cx| {
        app.result(cx, 0, 1).1 == [cells(&["1"])]
    });
    cx.executor().advance_clock(every);
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| tab.read(cx).auto_refresh()), None);
    assert!(app.has(cx, "result-auto-refresh-stopped"));
}

// G8 (RES-51): the result of a query edited as rows of its table ---------------

impl Fixture {
    /// The grid of result `index` of the active query tab.
    fn result_grid(&self, cx: &mut TestAppContext, index: usize) -> Entity<TableState<Grid>> {
        let tab = self.tab(cx);
        cx.update(|cx| tab.read(cx).grid_of(index)).expect("a grid")
    }

    /// Waits until the active tab's first result is decided: editable, or
    /// read-only with the reason it says.
    fn result_verdict(&self, cx: &mut TestAppContext) -> Option<String> {
        self.wait_until(cx, "the result's table", |app, cx| {
            let tab = app.tab(cx);
            cx.update(|cx| {
                let tab = tab.read(cx);
                tab.result_read_only().is_some()
                    || tab
                        .grid_of(0)
                        .is_some_and(|grid| grid.read(cx).delegate().pending().is_some())
            })
        });
        let tab = self.tab(cx);
        cx.update(|cx| tab.read(cx).result_read_only())
    }

    /// Rows of the first result with changes waiting.
    fn result_pending(&self, cx: &mut TestAppContext) -> usize {
        let tab = self.tab(cx);
        cx.update(|cx| tab.read(cx).pending_count(cx))
    }

    /// What the first result draws; nothing while it is read again.
    fn result_rows(&self, cx: &mut TestAppContext) -> Vec<Vec<String>> {
        let tab = self.tab(cx);
        cx.update(|cx| {
            let grid = tab.read(cx).grid_of(0);
            grid.map_or_else(Vec::new, |grid| grid.read(cx).delegate().shown())
        })
    }

    /// Types `text` in a cell of the first result, in place of what it
    /// holds, and presses Enter.
    fn type_in_result(&self, cx: &mut TestAppContext, row: usize, column: usize, text: &str) {
        let grid = self.result_grid(cx, 0);
        self.step(cx, |window, cx| {
            crate::query::edit::edit_cell(&grid, row, column, window, cx)
        });
        self.press(cx, &["secondary-a", "backspace"]);
        self.type_text(cx, text);
        self.press(cx, &["enter"]);
    }
}

/// A scratch table `t` (id, name, qty) of four rows in `schema`.
fn g8_table(engine: Engine, schema: &str) -> Scratch {
    let (drop_schema, create_schema) = scratch_schema(engine, schema);
    Scratch::new(
        engine,
        &[&drop_schema],
        &[
            &create_schema,
            &format!("CREATE TABLE {schema}.t (id INT PRIMARY KEY, name VARCHAR(50), qty INT)"),
            &format!(
                "INSERT INTO {schema}.t VALUES (1, 'one', 1), (2, 'two', 2), (3, 'three', 3), \
                 (9, 'nine', 9)"
            ),
        ],
    )
}

/// The rows of `schema.t` on the server, `id|name|qty` by id.
fn g8_rows(engine: Engine, schema: &str) -> String {
    let concat = match engine {
        Engine::Postgres => format!(
            "SELECT string_agg(id || '|' || coalesce(name, 'NULL') || '|' || \
             coalesce(qty::text, 'NULL'), ',' ORDER BY id) FROM {schema}.t"
        ),
        Engine::MySql | Engine::MariaDb => format!(
            "SELECT GROUP_CONCAT(CONCAT(id, '|', COALESCE(name, 'NULL'), '|', \
             COALESCE(qty, 'NULL')) ORDER BY id SEPARATOR ',') FROM {schema}.t"
        ),
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    };
    on_server(engine, &concat)
}

#[gpui_kit::test]
fn a_select_result_of_one_table_is_edited_reviewed_and_applied(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_g8_edit";
        let _scratch = g8_table(engine, schema);
        let app = connected(cx, engine);
        // An alias is written back under the table's name; an expression
        // is shown, not written.
        app.run_in_new_tab(
            cx,
            &format!(
                "select id, name as n, qty * 2 as twice from {schema}.t where id < 5 order by id"
            ),
            "secondary-enter",
        );
        app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["3 rows"]);
        assert_eq!(app.result_verdict(cx), None, "{engine:?}");
        assert!(app.has(cx, "result-edit-bar") && !app.has(cx, "result-read-only"));
        let grid = app.result_grid(cx, 0);
        let (expression, key) = cx.update(|cx| {
            let grid = grid.read(cx).delegate();
            (grid.editable(0, 3), grid.editable(0, 1))
        });
        assert!(expression.is_err() && key.is_ok(), "{engine:?}");

        // A typed value, a NULL set with Delete, a deleted row, a new row.
        app.type_in_result(cx, 0, 2, "uno");
        select_cell(&app, cx, &grid, (1, 2));
        app.press(cx, &["delete"]);
        app.step(cx, |_, cx| {
            grid.update(cx, |table, cx| table.set_selected_row(2, cx))
        });
        app.click(cx, "result-delete");
        app.click(cx, "result-add-row");
        app.type_in_result(cx, 3, 1, "4");
        app.type_in_result(cx, 3, 2, "four");
        assert_eq!(app.result_pending(cx), 4, "{engine:?}");
        assert!(app.has(cx, "result-pending"));
        // Nothing is written yet; another run would drop the changes, so it
        // waits for them.
        assert_eq!(
            g8_rows(engine, schema),
            "1|one|1,2|two|2,3|three|3,9|nine|9"
        );
        app.press(cx, &["secondary-enter"]);
        assert_eq!(app.result_pending(cx), 4, "{engine:?}");

        app.click(cx, "result-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the applied changes", |app, cx| {
            app.result_pending(cx) == 0
                && app.outcomes(cx) == ["3 rows"]
                && app
                    .result_rows(cx)
                    .first()
                    .is_some_and(|row| row[1] == "uno")
        });
        // One transaction of its own (auto-commit): the server has it.
        assert_eq!(
            g8_rows(engine, schema),
            "1|uno|1,2|NULL|2,4|four|NULL,9|nine|9",
            "{engine:?}"
        );
        // The rows on screen are what the server now holds.
        assert_eq!(
            app.result_rows(cx),
            [["1", "uno", "2"], ["2", "NULL", "4"], ["4", "four", "NULL"]],
            "{engine:?}"
        );
        assert_eq!(app.result_verdict(cx), None, "{engine:?}");

        // An apply that fails changes nothing and keeps the changes: the
        // row was deleted elsewhere since it was read.
        app.type_in_result(cx, 0, 2, "first");
        app.type_in_result(cx, 1, 2, "second");
        on_server(engine, &format!("DELETE FROM {schema}.t WHERE id = 2"));
        app.click(cx, "result-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the failed apply", |app, cx| {
            let tab = app.tab(cx);
            cx.update(|cx| tab.read(cx).result_apply_error()).is_some()
        });
        assert_eq!(app.result_pending(cx), 2, "{engine:?}");
        assert!(app.has(cx, "result-apply-error"));
        assert_eq!(g8_rows(engine, schema), "1|uno|1,4|four|NULL,9|nine|9");
        app.click(cx, "result-discard");
        assert_eq!(app.result_pending(cx), 0);
    }
}

#[gpui_kit::test]
fn a_result_that_is_no_rows_of_one_table_says_why_it_is_read_only(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_g8_read_only";
        let _scratch = g8_table(engine, schema);
        on_server(
            engine,
            &format!("CREATE TABLE {schema}.u (id INT PRIMARY KEY, t_id INT)"),
        );
        on_server(engine, &format!("INSERT INTO {schema}.u VALUES (10, 1)"));
        let app = connected(cx, engine);
        for (sql, why) in [
            (
                format!("select t.id, u.id from {schema}.t t join {schema}.u u on u.t_id = t.id"),
                "Read-only result: its columns come from several tables.".to_string(),
            ),
            (
                format!("select name from {schema}.t"),
                format!(
                    "Read-only result: the key column `id` of `{schema}.t` is not in the result."
                ),
            ),
            (
                format!("select distinct id, name from {schema}.t"),
                "Read-only result: the statement uses DISTINCT.".to_string(),
            ),
        ] {
            app.run_in_new_tab(cx, &sql, "secondary-enter");
            assert_eq!(app.result_verdict(cx), Some(why), "{engine:?}: {sql}");
            assert!(app.has(cx, "result-read-only") && !app.has(cx, "result-edit-bar"));
            // Its keys change nothing.
            let grid = app.result_grid(cx, 0);
            select_cell(&app, cx, &grid, (0, 1));
            app.press(cx, &["delete"]);
            assert_eq!(app.result_pending(cx), 0, "{engine:?}: {sql}");
        }
    }
}

#[gpui_kit::test]
fn a_manual_commit_tab_applies_result_edits_inside_its_transaction(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_g8_manual";
        let _scratch = g8_table(engine, schema);
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        let tab = app.tab(cx);
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(true, cx)));
        app.type_text(cx, &format!("select id, name from {schema}.t order by id"));
        app.press(cx, &["secondary-enter"]);
        app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["4 rows"]);
        assert_eq!(app.result_verdict(cx), None, "{engine:?}");
        app.type_in_result(cx, 0, 2, "uno");
        app.click(cx, "result-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the applied changes", |app, cx| {
            app.result_pending(cx) == 0
                && app.outcomes(cx) == ["4 rows"]
                && app
                    .result_rows(cx)
                    .first()
                    .is_some_and(|row| row[1] == "uno")
        });
        // Inside the tab's transaction: nothing is committed, and the log
        // of the transaction holds the statement.
        assert_eq!(
            cx.update(|cx| tab.read(cx).tx_state()),
            kerno_core::TxState::Open
        );
        assert_eq!(
            g8_rows(engine, schema),
            "1|one|1,2|two|2,3|three|3,9|nine|9"
        );
        let logged = cx.update(|cx| {
            tab.read(cx)
                .transaction_log()
                .iter()
                .any(|entry| entry.sql.starts_with("UPDATE"))
        });
        assert!(logged, "{engine:?}");
        // Rolling the transaction back undoes the edit.
        app.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| tab.end_transaction(false, window, cx))
        });
        app.wait_until(cx, "the rollback", |_, cx| {
            cx.update(|cx| tab.read(cx).tx_state()) == kerno_core::TxState::Idle
        });
        assert_eq!(
            g8_rows(engine, schema),
            "1|one|1,2|two|2,3|three|3,9|nine|9"
        );
    }
}

#[gpui_kit::test]
fn a_result_filtered_or_ordered_by_the_server_stays_editable(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_g8_filtered";
        let _scratch = g8_table(engine, schema);
        let app = connected(cx, engine);
        app.run_in_new_tab(
            cx,
            &format!("select id, name from {schema}.t"),
            "secondary-enter",
        );
        app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["4 rows"]);
        assert_eq!(app.result_verdict(cx), None, "{engine:?}");
        // Read again in a derived table, which MariaDB names instead of the
        // table: the origins of the first run are kept.
        let tab = app.tab(cx);
        app.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| tab.filter_result("id > 1", window, cx))
        });
        app.wait_until(cx, "the filtered rows", |app, cx| {
            app.outcomes(cx) == ["3 rows"]
        });
        assert_eq!(app.result_verdict(cx), None, "{engine:?}");
        // A header orders an editable result on the server, never here: the
        // changes are kept by row as the server sent them.
        let grid = app.result_grid(cx, 0);
        app.step(cx, |window, cx| {
            Grid::perform(
                &grid,
                crate::query::grid::GridAction::Order(Some(true)),
                window,
                cx,
            )
        });
        app.wait_until(cx, "the rows by id descending", |app, cx| {
            app.result_rows(cx).first().is_some_and(|row| row[0] == "9")
        });
        let (requery, _) = cx.update(|cx| tab.read(cx).result_requery());
        assert_eq!(
            requery.and_then(|requery| requery.sort),
            Some(("id".to_string(), true)),
            "{engine:?}"
        );
        assert_eq!(app.result_verdict(cx), None, "{engine:?}");
        app.type_in_result(cx, 0, 2, "last");
        app.click(cx, "result-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the applied change", |app, cx| {
            app.result_pending(cx) == 0
        });
        app.wait_until(cx, "the rows read again", |app, cx| {
            app.result_rows(cx)
                .first()
                .is_some_and(|row| row[1] == "last")
        });
        assert_eq!(
            g8_rows(engine, schema),
            "1|one|1,2|two|2,3|three|3,9|last|9",
            "{engine:?}"
        );
    }
}

#[gpui_kit::test]
fn result_changes_stay_where_their_rows_were_read(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let engine = Engine::Postgres;
    let schema = "kerno_ui_g8_place";
    let _scratch = g8_table(engine, schema);
    let app = connected(cx, engine);
    app.run_in_new_tab(
        cx,
        &format!("select id, name from {schema}.t order by id"),
        "secondary-enter",
    );
    app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["4 rows"]);
    assert_eq!(app.result_verdict(cx), None);
    app.type_in_result(cx, 0, 2, "uno");
    // Keys with a modifier keep their own meaning on a selected cell:
    // Shift+Enter opens the value panel, Cmd+Enter (a run) waits for the
    // changes; neither types in the cell.
    let grid = app.result_grid(cx, 0);
    select_cell(&app, cx, &grid, (1, 2));
    app.press(cx, &["shift-enter"]);
    assert!(app.has(cx, "value-editor"));
    app.click(cx, "value-cancel");
    select_cell(&app, cx, &grid, (1, 2));
    app.press(cx, &["secondary-enter"]);
    assert_eq!(cx.update(|cx| grid.read(cx).delegate().editing()), None);
    assert_eq!(app.result_pending(cx), 1);
    // The tab moved to another database: its changes are not applied
    // there, nor here.
    let tab = app.tab(cx);
    let target = cx
        .update(|cx| tab.read(cx).target().cloned())
        .expect("a target");
    let other = Target {
        database: Some("kerno_other".into()),
        schema: None,
        ..target.clone()
    };
    cx.update(|cx| tab.update(cx, |tab, cx| tab.request_target(Some(other), cx)));
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| tab.review(window, cx))
    });
    assert!(!app.has(cx, "sql-confirm"));
    assert_eq!(app.result_pending(cx), 1);
    // Back where they were read, they apply.
    cx.update(|cx| tab.update(cx, |tab, cx| tab.request_target(Some(target), cx)));
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| tab.review(window, cx))
    });
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the applied change", |app, cx| {
        app.result_pending(cx) == 0
            && app
                .result_rows(cx)
                .first()
                .is_some_and(|row| row[1] == "uno")
    });
    assert_eq!(
        g8_rows(engine, schema),
        "1|uno|1,2|two|2,3|three|3,9|nine|9"
    );
}

// M10 Part 4, third list: the captures of the new screens -------------------

use gpui_kit::Pixels;

impl Fixture {
    /// The accessible label of the element with this id.
    fn label_of(&self, cx: &mut TestAppContext, id: impl Into<gpui_kit::ElementId>) -> String {
        let id = id.into();
        self.step(cx, |window, _| {
            window.find(id).label().unwrap_or_default().to_string()
        })
    }

    /// Name, type and drawn width of each column of the first result of
    /// the active tab, and the width its header and first rows need.
    fn result_header(&self, cx: &mut TestAppContext) -> Vec<(String, String, Pixels, Pixels)> {
        let tab = self.tab(cx);
        let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
        cx.update(|cx| {
            let table = grid.read(cx);
            let delegate = table.delegate();
            (1..=delegate.shown_columns().len())
                .map(|column| {
                    let source = delegate.source_column(column).unwrap();
                    let described = &delegate.store().columns()[source];
                    (
                        described.name.clone(),
                        described.type_name.clone(),
                        table.col_width(column).unwrap(),
                        delegate.content_width(source, cx),
                    )
                })
                .collect()
        })
    }

    /// Whether every column of the first result of the active tab offers
    /// its sort mark.
    fn sort_marks(&self, cx: &mut TestAppContext) -> bool {
        let tab = self.tab(cx);
        let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
        cx.update(|cx| {
            let delegate = grid.read(cx).delegate();
            (1..=delegate.shown_columns().len())
                .all(|column| delegate.column(column, cx).sort.is_some())
        })
    }
}

#[gpui_kit::test]
fn the_header_of_a_table_never_cuts_a_name_while_its_type_shows(cx: &mut TestAppContext) {
    if !testing::available(Engine::MySql) {
        return;
    }
    let app = connected(cx, Engine::MySql);
    app.open_table(cx, "kerno", "order_items", RelationKind::Table);
    app.wait_until(cx, "the first page", |app, cx| {
        app.data_state(cx).starts_with("Rows 1–")
    });
    let grid = app.data_grid(cx);
    let columns = cx.update(|cx| grid.read(cx).delegate().shown_columns());
    // The name is drawn whole (at least as wide as its text) beside its
    // type, the type in full too, in every column of the capture.
    for (index, name) in columns.iter().enumerate() {
        let column = index + 1;
        let (drawn, typed) = app.step(cx, |window, _| {
            (
                window.find(("column-name", column)).bounds().size.width,
                window.find(("column-type", column)).bounds().size.width,
            )
        });
        let (needed, type_needed) = cx.update(|cx| {
            let text = cx.text_system();
            let theme = cx.theme();
            let id = text.resolve_font(&gpui_kit::font(theme.font_family.clone()));
            let size = crate::query::grid::text_size(cx);
            let type_name = {
                let delegate = grid.read(cx).delegate();
                let source = delegate.source_column(column).unwrap();
                delegate.store().columns()[source].type_name.clone()
            };
            let width = |shown: &str, size: Pixels| -> Pixels {
                shown
                    .chars()
                    .map(|c| {
                        text.advance(id, size, c)
                            .map_or(px(0.), |advance| advance.width)
                    })
                    .sum()
            };
            (width(name, size), width(&type_name, size * 0.85))
        });
        assert!(drawn + px(1.) >= needed, "{name}: {drawn:?} < {needed:?}");
        assert!(
            typed + px(1.) >= type_needed,
            "{name}: type {typed:?} < {type_needed:?}"
        );
    }
}

#[gpui_kit::test]
fn a_result_header_shows_name_and_type_whole_and_keeps_them_when_read_again(
    cx: &mut TestAppContext,
) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let paid = on_server(
        Engine::Postgres,
        "select count(*) from shop.orders where status = 'paid'",
    );
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(cx, "select * from shop.orders", "secondary-enter");
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx) == ["1000 rows, more"]
    });
    // A page that only waits for Fetch more has nothing to stop; its
    // count is written as every count is.
    assert!(!app.has(cx, "tab-cancel"));
    assert_eq!(
        app.label_of(cx, "result-summary"),
        "1,000 rows fetched, more on the server"
    );
    // Each column is first drawn wide enough for its name, its type and
    // the values of the first page (a `timestamptz` whole).
    let before = app.result_header(cx);
    for (name, type_name, width, needed) in &before {
        assert!(!type_name.is_empty(), "{name}");
        assert!(
            *width >= (*needed).min(px(360.)),
            "{name}: {width:?} < {needed:?}"
        );
    }
    let placed = before
        .iter()
        .find(|column| column.0 == "placed_at")
        .unwrap();
    assert!(placed.2 < px(360.), "{placed:?}");
    // The first page in, the header orders (on the server) as it will
    // once read again.
    assert!(app.sort_marks(cx));

    // Filtered on the server: the same header, the same widths.
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.filter_result("status = 'paid'", window, cx)
        })
    });
    app.wait_until(cx, "the paid orders", |app, cx| {
        app.outcomes(cx) == [format!("{paid} rows")] && !app.running(cx)
    });
    let after = app.result_header(cx);
    let shape = |header: &[(String, String, Pixels, Pixels)]| -> Vec<(String, String, Pixels)> {
        header
            .iter()
            .map(|(name, type_name, width, _)| (name.clone(), type_name.clone(), *width))
            .collect()
    };
    assert_eq!(shape(&after), shape(&before));
    assert!(app.sort_marks(cx));
    assert_eq!(
        app.label_of(cx, "result-filter-origin"),
        "filtered on the server"
    );
    assert_eq!(app.label_of(cx, "result-summary"), format!("{paid} rows"));

    // Ordered too, from a header: still the same header.
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    app.step(cx, |window, cx| {
        grid.update(cx, |table, cx| {
            table
                .delegate_mut()
                .perform_sort(1, ColumnSort::Descending, window, cx)
        })
    });
    app.wait_until(cx, "the order of the server", |app, cx| {
        g5_requery(app, cx).0.and_then(|requery| requery.sort) == Some(("id".into(), true))
            && app.outcomes(cx) == [format!("{paid} rows")]
            && !app.running(cx)
    });
    assert_eq!(shape(&app.result_header(cx)), shape(&before));
    assert!(app.sort_marks(cx));
    assert_eq!(
        app.label_of(cx, "result-filter-origin"),
        "filtered and ordered on the server"
    );
}

#[gpui_kit::test]
fn the_connection_menu_ticks_read_only_when_it_is_on(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let read_only_entry = |app: &Fixture, cx: &mut TestAppContext| -> Option<bool> {
        use gpui_kit::test::TestQueryExt as _;
        let popup = gpui_kit::ElementId::Name("popup-menu".into());
        app.step(cx, |window, _| {
            window
                .find_all_by_label("Read-only")
                .into_iter()
                .find(|entry| entry.path().contains(&popup))
                .expect("a Read-only entry")
                .checked()
        })
    };
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    app.right_click(cx, ("connection", 0usize), None);
    assert_eq!(read_only_entry(&app, cx), Some(true));
    app.press(cx, &["escape"]);

    let app = connected(cx, Engine::Postgres);
    app.right_click(cx, ("connection", 0usize), None);
    assert_ne!(read_only_entry(&app, cx), Some(true));
    app.press(cx, &["escape"]);
}

#[gpui_kit::test]
fn cmd_comma_opens_the_settings_wherever_the_keyboard_is(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    // The keyboard nowhere (nothing focused yet, or what had it is gone).
    app.step(cx, |window, cx| window.blur(cx));
    app.press(cx, &["secondary-,"]);
    assert!(app.dialog_open(cx), "nothing focused");
    app.press(cx, &["escape"]);
    assert!(!app.dialog_open(cx));
    // In the editor of a query tab.
    app.press(cx, &["secondary-t"]);
    assert!(app.editor_has_keyboard(cx));
    app.press(cx, &["secondary-,"]);
    assert!(app.dialog_open(cx), "in the editor");
    app.press(cx, &["escape"]);
    assert!(!app.dialog_open(cx));
}

#[gpui_kit::test]
fn go_to_row_from_the_palette_acts_on_the_grid_of_the_active_tab(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    // A result under the editor, which keeps the keyboard.
    app.run_in_new_tab(
        cx,
        "select g from generate_series(1, 30) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["30 rows"]);
    assert!(app.editor_has_keyboard(cx));
    let go = |app: &Fixture, cx: &mut TestAppContext, row: &str| {
        app.press(cx, &["secondary-k"]);
        app.type_text(cx, "go to row");
        app.press(cx, &["enter"]);
        assert!(app.has(cx, "go-to-row"), "the Go to Row dialog");
        app.type_text(cx, row);
        app.press(cx, &["enter"]);
        assert!(!app.dialog_open(cx));
    };
    go(&app, cx, "12");
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    let row = |cx: &mut TestAppContext| {
        cx.update(|cx| crate::query::grid::current(grid.read(cx).selection()).0)
    };
    assert_eq!(row(cx), 11);
    // With the keyboard nowhere, the palette still reaches the grid.
    app.step(cx, |window, cx| window.blur(cx));
    go(&app, cx, "3");
    assert_eq!(row(cx), 2);

    // A table's rows, the keyboard in its filter field.
    app.open_table(cx, "shop", "customers", RelationKind::Table);
    app.wait_until(cx, "the first page", |app, cx| {
        app.data_state(cx).starts_with("Rows 1–")
    });
    app.click(cx, "filter-raw");
    go(&app, cx, "5");
    let grid = app.data_grid(cx);
    assert_eq!(
        cx.update(|cx| crate::query::grid::current(grid.read(cx).selection()).0),
        4
    );
}
