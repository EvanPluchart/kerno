//! J4, `kerno <url>` (Beekeeper #361, #2002): a URL opens an unsaved
//! connection that runs queries and is saved from its menu; a URL that
//! names a saved connection opens that one; URLs handed over through the
//! instance socket open in the same window.

use kerno_core::config::parse_connection_url;
use kerno_core::instance;

use crate::connections::manager::ConnectionsEvent;

use super::*;

/// The test server of `engine` as a URL, its password in it when `password`.
fn url(engine: Engine, password: bool) -> String {
    let scheme = match engine {
        Engine::Postgres => "postgres",
        Engine::MySql => "mysql",
        Engine::MariaDb => "mariadb",
    };
    let password = match password {
        true => format!(":{}", testing::DB_PASSWORD),
        false => String::new(),
    };
    format!(
        "{scheme}://kerno{password}@127.0.0.1:{}/kerno?sslmode=disable",
        testing::port(engine)
    )
}

impl Fixture {
    /// Opens a URL as `kerno <url>` does.
    fn open_url(&self, cx: &mut TestAppContext, url: &str) {
        let url = parse_connection_url(url).unwrap();
        let (workspace, connections) = (self.workspace.clone(), self.connections.clone());
        self.step(cx, |window, cx| {
            crate::open_url::open(&workspace, &connections, &url, window, cx)
        });
    }

    /// Ids of the listed connections.
    fn listed(&self, cx: &mut TestAppContext) -> Vec<String> {
        cx.update(|cx| {
            let connections = self.connections.read(cx);
            connections.saved().iter().map(|c| c.id.clone()).collect()
        })
    }

    fn unsaved(&self, cx: &mut TestAppContext, id: &str) -> bool {
        cx.update(|cx| self.connections.read(cx).is_unsaved(id))
    }

    fn stored_password(&self, cx: &mut TestAppContext, id: &str) -> Option<String> {
        let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
        kerno_core::secrets::load_secrets(secrets.as_ref(), id)
            .unwrap()
            .password
            .map(|password| password.expose().to_string())
    }

    fn wait_connected(&self, cx: &mut TestAppContext, index: usize) {
        self.wait_until(cx, "the connection", |app, cx| {
            let status = app.status(cx, index);
            assert!(!matches!(status, Status::Failed { .. }), "{status:?}");
            matches!(status, Status::Connected { .. })
        });
    }
}

/// Beekeeper #361: `kerno postgres://…` opens the connection without the
/// dialog. Unsaved, it is marked, written nowhere, runs a query, and
/// "Save connection…" stores it under the same id with its password in the
/// secret store.
#[gpui_kit::test]
fn a_url_opens_an_unsaved_connection_that_runs_a_query_and_is_saved(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let app = open(cx, memory_storage());
        app.open_url(cx, &url(engine, true));
        app.wait_connected(cx, 0);
        let id = app.listed(cx)[0].clone();
        let name = cx.update(|cx| app.connections.read(cx).saved()[0].config.name.clone());
        assert_eq!(
            name,
            format!("kerno@127.0.0.1:{}/kerno", testing::port(engine)),
            "{engine:?}"
        );
        assert!(app.unsaved(cx, &id));
        assert!(app.has(cx, ("connection-unsaved", 0usize)), "{engine:?}");
        assert!(app.storage.connections().unwrap().is_empty(), "{engine:?}");
        assert_eq!(app.stored_password(cx, &id), None, "{engine:?}");
        let menu = cx.update(|cx| {
            let sidebar = app.workspace.read(cx).sidebar();
            sidebar
                .read(cx)
                .menu_of(&Subject::Connection(id.clone()), cx)
        });
        assert_eq!(
            menu[0],
            crate::sidebar::Entry::Item(SidebarAction::SaveConnection, None),
            "{engine:?}"
        );

        app.run_in_new_tab(cx, "select 1 as one", "secondary-enter");
        app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
        let tab = app.tab(cx);
        let target = cx.update(|cx| tab.read(cx).target().cloned());
        assert_eq!(target.map(|target| target.connection), Some(id.clone()));

        // Reading the list again and renaming keep it, in memory only.
        app.sidebar_does(cx, Subject::List, SidebarAction::Refresh);
        app.wait_until(cx, "the list read again", |app, cx| {
            app.listed(cx) == [id.clone()]
                && app.unsaved(cx, &id)
                && matches!(app.status(cx, 0), Status::Connected { .. })
        });
        // A write of it to storage would fail and say so.
        let failures = std::rc::Rc::new(std::cell::Cell::new(0));
        let _watch = cx.update(|cx| {
            let failures = failures.clone();
            cx.subscribe(&app.connections, move |_, event, _| {
                if matches!(event, ConnectionsEvent::Failed { .. }) {
                    failures.set(failures.get() + 1);
                }
            })
        });
        let renamed = cx.update(|cx| {
            app.connections
                .update(cx, |c, cx| c.rename_connection(&id, "from a url", cx))
        });
        assert_eq!(renamed, Ok(()));
        cx.run_until_parked();
        let name = cx.update(|cx| app.connections.read(cx).saved()[0].config.name.clone());
        assert_eq!(name, "from a url");
        assert_eq!(failures.get(), 0, "{engine:?}");
        assert!(app.storage.connections().unwrap().is_empty(), "{engine:?}");
        app.sidebar_does(
            cx,
            Subject::Connection(id.clone()),
            SidebarAction::SaveConnection,
        );
        app.wait_until(cx, "the dialog", |app, cx| app.has(cx, "conn-save"));
        app.click(cx, "conn-save");
        app.wait_until(cx, "the saved connection", |app, cx| {
            !app.unsaved(cx, &id) && app.storage.connections().unwrap().len() == 1
        });
        let stored = app.storage.connections().unwrap();
        assert_eq!(stored[0].id, id, "saved under the id its tab uses");
        assert_eq!(stored[0].config.name, name);
        assert!(stored[0].config.save_password);
        assert_eq!(
            app.stored_password(cx, &id).as_deref(),
            Some(testing::DB_PASSWORD)
        );
        assert!(!app.has(cx, ("connection-unsaved", 0usize)));
        assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    }
}

/// Beekeeper #2002: a URL that names a saved connection (same engine, host,
/// port, user and database) opens that one, with its own password.
#[gpui_kit::test]
fn a_url_naming_a_saved_connection_connects_that_one(cx: &mut TestAppContext) {
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

    app.open_url(cx, &url(Engine::Postgres, false));
    app.wait_connected(cx, 0);
    assert_eq!(app.listed(cx), std::slice::from_ref(&id));
    assert!(!app.unsaved(cx, &id));
    assert!(!app.has(cx, ("connection-unsaved", 0usize)));
}

/// Beekeeper #361: a second `kerno <url>` hands its URL to the running
/// instance, which opens it in its window; the same URL again opens the
/// same connection, and a line that is not a URL is ignored.
#[gpui_kit::test]
fn urls_handed_over_through_the_socket_open_in_the_same_window(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    let app = open(cx, memory_storage());
    let socket = instance::socket_path(app._data_dir.path());
    let listener = instance::listen(&socket).unwrap().expect("nobody listens");
    let (workspace, connections) = (app.workspace.clone(), app.connections.clone());
    app.step(cx, |window, cx| {
        crate::open_url::listen(listener, &workspace, &connections, window, cx)
    });
    // A live instance's socket is never taken over.
    assert!(instance::listen(&socket).unwrap().is_none());

    instance::send(&socket, "not a url").unwrap();
    instance::send(&socket, &url(Engine::Postgres, true)).unwrap();
    app.wait_until(cx, "the first URL", |app, cx| app.listed(cx).len() == 1);
    app.wait_connected(cx, 0);

    instance::send(&socket, &url(Engine::Postgres, true)).unwrap();
    instance::send(&socket, &url(Engine::MySql, true)).unwrap();
    app.wait_until(cx, "the second URL", |app, cx| app.listed(cx).len() == 2);
    app.wait_connected(cx, 1);
    let engines = cx.update(|cx| {
        let connections = app.connections.read(cx);
        connections
            .saved()
            .iter()
            .map(|c| c.config.engine)
            .collect::<Vec<_>>()
    });
    assert_eq!(engines, [Engine::Postgres, Engine::MySql]);
    assert_eq!(cx.update(|cx| cx.windows().len()), 1);
    assert!(app.storage.connections().unwrap().is_empty());
}
