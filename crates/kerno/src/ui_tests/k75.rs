//! K75 (docs/plans/M12.md, D-144): the schema tree kept the sessions of a connection that was
//! opened again (the read-only toggle, Reconnect, a connection's dialog saved with "connect", a
//! lost connection), closed by then: every schema not loaded yet said "connection closed: the
//! session was closed", and Reconnect did not mend it. Now the tree is read again on the new
//! sessions; a read of the tree whose session was lost opens a new one and reads once more.

use super::*;

/// What the tree's rows say when a read failed.
fn failures(app: &Fixture, cx: &mut TestAppContext) -> Vec<String> {
    app.tree(cx)
        .into_iter()
        .filter(|line| line.contains("Could not read"))
        .collect()
}

/// The connection's own session now: a new one after each reconnection.
fn meta(app: &Fixture, cx: &mut TestAppContext) -> Option<Arc<dyn kerno_core::Session>> {
    cx.update(|cx| {
        let connections = app.connections.read(cx);
        connections.meta_session(&connections.saved()[0].id)
    })
}

/// Waits until the connection is open on another session than `before`.
fn wait_reopened(
    app: &Fixture,
    cx: &mut TestAppContext,
    before: &Arc<dyn kerno_core::Session>,
    how: &str,
) {
    app.wait_until(cx, &format!("{how}: the connection again"), |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
            && meta(app, cx).is_some_and(|now| !Arc::ptr_eq(&now, before))
    });
}

/// The path of a part of the test server the tree has not read yet.
fn unread(engine: Engine) -> Vec<&'static str> {
    match engine {
        Engine::Postgres => vec!["kerno", "analytics"],
        _ => vec!["kerno_analytics"],
    }
}

/// Opens `path` and waits for its objects, or for its row to say why not.
fn read_unread(app: &Fixture, cx: &mut TestAppContext, engine: Engine, how: &str) {
    let path = unread(engine);
    app.open_path(cx, &path);
    if !app.is_open(cx, &path) {
        app.toggle(cx, &path);
    }
    let objects = [path.as_slice(), &["Tables"]].concat();
    app.wait_until(cx, &format!("{how}: {path:?} read"), |app, cx| {
        app.shows(cx, &objects) || !failures(app, cx).is_empty()
    });
    assert_eq!(
        failures(app, cx),
        Vec::<String>::new(),
        "{engine:?}, {how}: {:#?}",
        app.tree(cx)
    );
}

/// A statement that runs long enough to be cancelled.
fn slow(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "select pg_sleep(30) as kerno_test_k75_slow",
        _ => "select count(*) as kerno_test_k75_slow from kerno.orders where sleep(0.05) = 0",
    }
}

/// The owner's sequence of the real-app pass, then every other way an open connection is opened
/// again: each time, a part of the tree not read yet reads on the new sessions, a data tab shows
/// its rows again and a query tab runs. Seen failing before the fix: the first way left the
/// tree with its closed sessions.
#[gpui_kit::test]
fn the_tree_reads_on_after_every_way_a_connection_is_opened_again(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
        let (schema, table) = match engine {
            Engine::Postgres => ("shop", "customers"),
            _ => ("kerno", "customers"),
        };
        if engine == Engine::Postgres {
            app.open_path(cx, &["kerno", "shop", "Tables"]);
        } else {
            app.open_path(cx, &["kerno", "Tables"]);
        }
        app.open_table(cx, schema, table, RelationKind::Table);
        let data = app.data(cx);
        let state = |cx: &mut TestAppContext| cx.update(|cx| data.read(cx).state(cx));
        app.wait_until(cx, "the rows", |_, cx| state(cx).contains(" of "));
        let rows = state(cx);

        for how in ["toolbar toggle", "menu toggle", "dialog", "reconnect"] {
            let old = kept(&app, cx, own_key(engine)).expect("the tree's session");
            match how {
                "toolbar toggle" => {
                    // Read-only on, a statement run and cancelled in a query tab, read-only off.
                    let before = meta(&app, cx).unwrap();
                    app.dispatch(cx, actions::ToggleReadOnly);
                    wait_reopened(&app, cx, &before, how);
                    assert!(app.first_stored().read_only, "{engine:?}");
                    app.run_in_new_tab(cx, slow(engine), "secondary-enter");
                    app.wait_until(cx, "the statement to start", |app, cx| {
                        app.running(cx) && running_on_server(engine, "kerno_test_k75_slow")
                    });
                    app.press(cx, &["secondary-."]);
                    app.wait_until(cx, "the cancellation", |app, cx| !app.running(cx));
                    assert_eq!(app.outcomes(cx), ["cancelled"], "{engine:?}");
                    app.press(cx, &["secondary-w"]);
                    let before = meta(&app, cx).unwrap();
                    app.dispatch(cx, actions::ToggleReadOnly);
                    wait_reopened(&app, cx, &before, how);
                    assert!(!app.first_stored().read_only, "{engine:?}");
                }
                "menu toggle" => {
                    for on in [true, false] {
                        let before = meta(&app, cx).unwrap();
                        let subject = Subject::Connection(id.clone());
                        app.sidebar_does(cx, subject, SidebarAction::ReadOnly(!on));
                        wait_reopened(&app, cx, &before, how);
                        assert_eq!(app.first_stored().read_only, on, "{engine:?}");
                    }
                }
                "dialog" => {
                    // Saved with "connect": the window opens it again with its new settings.
                    for on in [true, false] {
                        let before = meta(&app, cx).unwrap();
                        let config = ConnectionConfig {
                            read_only: on,
                            ..app.first_stored()
                        };
                        let saved = Some(id.clone());
                        cx.update(|cx| {
                            app.connections.update(cx, |connections, cx| {
                                connections.save(saved, config, testing::secrets(), None, true, cx)
                            })
                        });
                        wait_reopened(&app, cx, &before, how);
                        assert_eq!(app.first_stored().read_only, on, "{engine:?}");
                    }
                }
                _ => {
                    let before = meta(&app, cx).unwrap();
                    let subject = Subject::Connection(id.clone());
                    app.sidebar_does(cx, subject, SidebarAction::Reconnect);
                    wait_reopened(&app, cx, &before, how);
                }
            }
            // The tree is read again, on a new session, before anything is asked of it.
            app.wait_until(cx, &format!("{how}: the tree read again"), |app, cx| {
                kept(app, cx, own_key(engine)).is_some_and(|now| !Arc::ptr_eq(&now, &old))
            });
            read_unread(&app, cx, engine, how);

            // The data tab reads its rows again on a session of its own, and a query tab runs.
            app.step(cx, |window, cx| {
                data.update(cx, |tab, cx| tab.reload(window, cx));
            });
            app.wait_until(cx, &format!("{how}: the rows again"), |_, cx| {
                let now = state(cx);
                assert!(!now.starts_with("failed"), "{engine:?}, {how}: {now}");
                now == rows
            });
            app.run_in_new_tab(cx, "select 7 as seven", "secondary-enter");
            app.wait_until(cx, &format!("{how}: the query"), |app, cx| {
                app.outcomes(cx) == ["1 rows"]
            });
            app.press(cx, &["secondary-w"]);
        }
    }
}

/// The tree's session of the first connection under `key` (a database on PostgreSQL, `""` on
/// MySQL), when it keeps one.
fn kept(app: &Fixture, cx: &mut TestAppContext, key: &str) -> Option<Arc<dyn kerno_core::Session>> {
    cx.update(|cx| {
        let id = app.connections.read(cx).saved()[0].id.clone();
        let navigator = app.workspace.read(cx).navigator(cx);
        navigator.read(cx).tree_session(&id, key)
    })
}

/// The tree's key of the session that reads the test server's own database.
fn own_key(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "kerno",
        _ => "",
    }
}

/// The tree's session of the first connection under `key`, and its id on the server.
fn tree_session(
    app: &Fixture,
    cx: &mut TestAppContext,
    engine: Engine,
    key: &str,
) -> (Arc<dyn kerno_core::Session>, String) {
    let session = kept(app, cx, key).unwrap_or_else(|| panic!("no tree session {key:?}"));
    let sql = match engine {
        Engine::Postgres => "select pg_backend_pid()::text",
        _ => "select connection_id()",
    };
    let id = ask(app, cx, &session, sql).unwrap();
    (session, id)
}

/// The first cell `sql` gives on `session`, run on the app's runtime; the error's text when it
/// fails.
fn ask(
    app: &Fixture,
    cx: &mut TestAppContext,
    session: &Arc<dyn kerno_core::Session>,
    sql: &'static str,
) -> Result<String, String> {
    let slot: Arc<std::sync::Mutex<Option<Result<String, String>>>> = Arc::default();
    let (asking, filled) = (session.clone(), slot.clone());
    cx.update(|cx| {
        bridge::spawn(cx, async move {
            let read = kerno_core::exec::collect(asking, sql).await;
            *filled.lock().unwrap() = Some(match read {
                Ok(read) => Ok(read.results[0].rows()[0][0].clone().unwrap_or_default()),
                Err(error) => Err(error.to_string()),
            });
        })
        .detach()
    });
    app.wait_until(cx, sql, |_, _| slot.lock().unwrap().is_some());
    slot.lock().unwrap().take().unwrap()
}

/// A data tab reading its rows at the moment its connection is opened again, read-only now,
/// reads them again on a session of the new connection, which is read-only on the server: it
/// kept the session it was opening on the old one. Seen failing without the tab dropping its late
/// read: the rows never came back (the session kept was closed).
#[gpui_kit::test]
fn a_data_tab_reading_while_its_connection_opens_again_moves_to_the_new_one(
    cx: &mut TestAppContext,
) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
        let schema = match engine {
            Engine::Postgres => "shop",
            _ => "kerno",
        };
        app.open_table(cx, schema, "customers", RelationKind::Table);
        let data = app.data(cx);
        let state = |cx: &mut TestAppContext| cx.update(|cx| data.read(cx).state(cx));
        app.wait_until(cx, "the rows", |_, cx| state(cx).contains(" of "));
        let rows = state(cx);

        let before = meta(&app, cx).unwrap();
        let sidebar = cx.update(|cx| app.workspace.read(cx).sidebar());
        app.step(cx, |window, cx| {
            data.update(cx, |tab, cx| tab.reload(window, cx));
            sidebar.update(cx, |sidebar, cx| {
                let subject = Subject::Connection(id.clone());
                sidebar.perform(&subject, SidebarAction::ReadOnly(false), window, cx)
            });
        });
        wait_reopened(&app, cx, &before, "read-only on");
        app.wait_until(cx, "the rows again", |_, cx| state(cx) == rows);
        let session = cx.update(|cx| data.read(cx).held_session()).unwrap();
        let sql = match engine {
            Engine::Postgres => "show default_transaction_read_only",
            _ => "select @@session.transaction_read_only",
        };
        let read_only = ask(&app, cx, &session, sql);
        let expected = match engine {
            Engine::Postgres => "on",
            _ => "1",
        };
        assert_eq!(read_only, Ok(expected.to_string()), "{engine:?}");
    }
}

/// Ends the server session `id` from outside the app, and waits until it is gone.
fn end_on_server(app: &Fixture, cx: &mut TestAppContext, engine: Engine, id: &str) {
    let (end, count) = match engine {
        Engine::Postgres => (
            format!("SELECT pg_terminate_backend({id})"),
            format!("SELECT count(*) FROM pg_stat_activity WHERE pid = {id}"),
        ),
        _ => (
            format!("KILL {id}"),
            format!("SELECT count(*) FROM information_schema.PROCESSLIST WHERE ID = {id}"),
        ),
    };
    on_server(engine, &end);
    app.wait_until(cx, "the session to go", |_, _| {
        on_server(engine, &count) == "0"
    });
}

/// CLAUDE.md §4 A5: the server ends the tree's session (an administrator, a timeout, a
/// restart): the next read of the tree opens a new session and reads again, without a word to
/// the user; the connection and its own session stay. Seen failing with the second read taken
/// out: the row said the connection was lost.
#[gpui_kit::test]
fn a_tree_session_ended_by_the_server_is_replaced_and_read_again(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        let key = own_key(engine);
        app.wait_until(cx, "the tree", |app, cx| !app.tree(cx).is_empty());
        let path = unread(engine);
        app.open_path(cx, &path[..path.len() - 1]);
        if engine == Engine::Postgres {
            app.wait_until(cx, "the schemas", |app, cx| app.shows(cx, &path));
        }
        let before = meta(&app, cx).unwrap();
        let (session, id) = tree_session(&app, cx, engine, key);
        end_on_server(&app, cx, engine, &id);

        read_unread(&app, cx, engine, "ended by the server");
        let (now, _) = tree_session(&app, cx, engine, key);
        assert!(!Arc::ptr_eq(&now, &session), "{engine:?}: a new session");
        assert!(
            meta(&app, cx).is_some_and(|meta| Arc::ptr_eq(&meta, &before)),
            "{engine:?}: the connection stays"
        );
        assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    }
}

/// CLAUDE.md §4 A5: the server goes away while the tree is open. A read of the tree finds its
/// session lost and cannot open another: the connection is found lost and opened again, again
/// and again, until the server is back; then the tree is read again on new sessions and the
/// part not read yet reads. Seen failing with the tree kept after the reconnection.
#[gpui_kit::test]
fn a_tree_read_when_the_server_went_away_mends_once_it_is_back(cx: &mut TestAppContext) {
    use kerno_core::testing::{Proxy, ProxyMode};
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let proxy = Proxy::start(testing::port(engine));
        let config = ConnectionConfig {
            port: proxy.port(),
            ..testing::config(engine)
        };
        let app = connected_as(cx, config);
        let path = unread(engine);
        app.open_path(cx, &path);
        let old = kept(&app, cx, own_key(engine)).expect("the tree's session");

        proxy.set(ProxyMode::Drop);
        app.toggle(cx, &path);
        app.wait_until(cx, "the loss", |app, cx| {
            cx.update(|cx| {
                let connections = app.connections.read(cx);
                connections.reconnecting(&connections.saved()[0].id)
            })
        });

        proxy.set(ProxyMode::Forward);
        app.wait_until(cx, "the connection and the tree again", |app, cx| {
            cx.executor().advance_clock(Duration::from_millis(500));
            matches!(app.status(cx, 0), Status::Connected { .. })
                && kept(app, cx, own_key(engine)).is_some_and(|now| !Arc::ptr_eq(&now, &old))
        });
        read_unread(&app, cx, engine, "back");
    }
}

/// The SQLite twin of the reconnections: a database file's tree reads on, after Reconnect and
/// the read-only toggle, on new sessions. Seen failing with the tree kept after them.
#[gpui_kit::test]
fn a_database_file_tree_reads_on_after_it_is_opened_again(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = super::sqlite::fixture_file(dir.path());
    let app = connected_as(cx, super::sqlite::file_config(&file));
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    app.open_path(cx, &["main", "Tables"]);
    for how in ["read-only on", "reconnect", "read-only off"] {
        let (old, before) = (kept(&app, cx, "").unwrap(), meta(&app, cx).unwrap());
        let action = match how {
            "reconnect" => SidebarAction::Reconnect,
            _ => SidebarAction::ReadOnly(how == "read-only off"),
        };
        app.sidebar_does(cx, Subject::Connection(id.clone()), action);
        wait_reopened(&app, cx, &before, how);
        app.wait_until(cx, &format!("{how}: the tree read again"), |app, cx| {
            kept(app, cx, "").is_some_and(|now| !Arc::ptr_eq(&now, &old))
        });
        let table = ["main", "Tables", "customers"];
        app.open_path(cx, &table);
        app.toggle(cx, &table);
        app.wait_until(cx, &format!("{how}: the columns"), |app, cx| {
            app.shows(cx, &["main", "Tables", "customers", "Columns"])
                || !failures(app, cx).is_empty()
        });
        assert_eq!(failures(&app, cx), Vec::<String>::new(), "{how}");
    }
}
