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

use crate::connections::manager::{Connections, Drivers, Status};
use crate::navigator::actions::{Action, Template};
use crate::settings::Settings;
use crate::sidebar::Sidebar;
use crate::state::AppState;
use crate::tabs::TabSnapshot;
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

    /// Clicks a row of the schema tree: opens or closes it.
    fn toggle(&self, cx: &mut TestAppContext, path: &[&str]) {
        let row = self.tree_row(cx, path);
        self.step(cx, |window, cx| {
            window.within("navigator").click(("node", row), cx)
        });
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
                sql: String::new()
            },
            TabSnapshot::Query {
                title: "Query 2".into(),
                sql: "select 'not saved anywhere'".into()
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
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
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

    // A double click closes it again.
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
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
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
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
    assert_eq!(
        app.active_query(cx),
        (
            "shop".to_string(),
            "SET search_path TO \"shop\";\n\n".to_string()
        )
    );

    app.act(cx, Action::CreateTable, &["kerno", "shop", "Tables"]);
    app.wait_until(cx, "the CREATE TABLE", |app, cx| app.titles(cx).len() == 6);
    let (title, sql) = app.active_query(cx);
    assert_eq!(title, "New table");
    assert!(
        sql.starts_with("CREATE TABLE \"shop\".\"new_table\" ("),
        "{sql}"
    );
    // Writing a statement is not running it.
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
