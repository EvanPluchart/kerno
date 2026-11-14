//! A connection that drops or stops answering comes back by itself
//! (CLAUDE.md §4 A5, J9; DBeaver #6903): each test talks to a shared test
//! server through a proxy of its own ([`testing::Proxy`]), which it breaks
//! and mends without disturbing the other tests.

use kerno_core::testing::{Proxy, ProxyMode};

use super::*;

/// The app connected to `engine` through a proxy of the test's own.
fn through_proxy(cx: &mut TestAppContext, engine: Engine) -> (Fixture, Proxy) {
    let proxy = Proxy::start(testing::port(engine));
    let config = ConnectionConfig {
        port: proxy.port(),
        ..testing::config(engine)
    };
    (connected_as(cx, config), proxy)
}

impl Fixture {
    /// Whether the active tab says a statement was cut off by the loss.
    fn says_lost(&self, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .active_query_tab()
                .is_some_and(|tab| {
                    tab.read(cx)
                        .failures()
                        .iter()
                        .any(|message| message == crate::query::tab::LOST_WHILE_RUNNING)
                })
        })
    }

    fn reconnecting(&self, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            let connections = self.connections.read(cx);
            connections.reconnecting(&connections.saved()[0].id)
        })
    }

    /// Lets the simulated clock run (keep-alive, retry waits) until the
    /// connection is open again.
    fn wait_reconnected(&self, cx: &mut TestAppContext) {
        self.wait_until(cx, "the connection to come back", |app, cx| {
            cx.executor().advance_clock(Duration::from_millis(500));
            matches!(app.status(cx, 0), Status::Connected { .. }) && !app.reconnecting(cx)
        });
    }

    /// Runs the active tab's text, confirming a write if asked.
    fn run_tab(&self, cx: &mut TestAppContext) {
        self.press(cx, &["secondary-enter"]);
        if self.has(cx, "sql-confirm") {
            self.click(cx, "sql-confirm");
        }
    }
}

#[gpui_kit::test]
fn a_dropped_connection_comes_back_by_itself(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let (app, proxy) = through_proxy(cx, engine);
        app.run_in_new_tab(cx, "SELECT 1 AS one", "secondary-enter");
        app.wait_until(cx, "the first row", |app, cx| {
            app.outcomes(cx) == ["1 rows"]
        });

        // The server goes away: the statement fails in plain words, and the
        // connection is opened again, and again, while it is away.
        proxy.set(ProxyMode::Drop);
        app.run_tab(cx);
        app.wait_until(cx, "the loss", |app, cx| app.reconnecting(cx));
        assert!(app.says_lost(cx), "{engine:?}: {}", app.situation(cx));
        for _ in 0..4 {
            cx.executor().advance_clock(Duration::from_secs(2));
            app.step(cx, |_, _| ());
        }
        assert!(app.reconnecting(cx), "{engine:?}: still trying");
        assert_eq!(app.status(cx, 0), Status::Connecting, "{engine:?}");

        // Back: usable without anything done by the user.
        proxy.set(ProxyMode::Forward);
        app.wait_reconnected(cx);
        app.run_tab(cx);
        app.wait_until(cx, "the row again", |app, cx| {
            app.outcomes(cx) == ["1 rows"]
        });
        app.press(cx, &["secondary-w"]);
    }
}

#[gpui_kit::test]
fn a_connection_that_stops_answering_is_found_out_and_comes_back(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let (app, proxy) = through_proxy(cx, engine);
        app.run_in_new_tab(cx, "SELECT 1 AS one", "secondary-enter");
        app.wait_until(cx, "the first row", |app, cx| {
            app.outcomes(cx) == ["1 rows"]
        });

        // Nothing closes, nothing answers: the keep-alive check times out.
        proxy.set(ProxyMode::BlackHole);
        cx.executor().advance_clock(Duration::from_secs(5));
        app.wait_until(cx, "the check to time out", |app, cx| app.reconnecting(cx));

        proxy.set(ProxyMode::Forward);
        app.wait_reconnected(cx);
        app.run_tab(cx);
        app.wait_until(cx, "the row again", |app, cx| {
            app.outcomes(cx) == ["1 rows"]
        });
        app.press(cx, &["secondary-w"]);
    }
}

#[gpui_kit::test]
fn disconnect_stops_the_retries(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let (app, proxy) = through_proxy(cx, engine);
    proxy.set(ProxyMode::Drop);
    cx.executor().advance_clock(Duration::from_secs(5));
    app.wait_until(cx, "the loss", |app, cx| app.reconnecting(cx));
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    cx.update(|cx| {
        app.connections
            .update(cx, |connections, cx| connections.disconnect(&id, cx))
    });
    assert!(!app.reconnecting(cx));
    assert_eq!(app.status(cx, 0), Status::Disconnected);
    // The server back, nothing reconnects: the user said so.
    proxy.set(ProxyMode::Forward);
    for _ in 0..10 {
        cx.executor().advance_clock(Duration::from_secs(5));
        app.step(cx, |_, _| ());
    }
    std::thread::sleep(Duration::from_millis(300));
    cx.run_until_parked();
    assert_eq!(app.status(cx, 0), Status::Disconnected);
}

/// A write cut off by the loss is reported, never sent again: the server
/// has it once at most (DBeaver #6903).
#[gpui_kit::test]
fn a_write_in_flight_is_not_run_again(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_reconnect_write";
        let (drop_schema, create_schema) = scratch_schema(engine, schema);
        let table = format!("CREATE TABLE {schema}.hits (n int)");
        let _scratch = Scratch::new(engine, &[&drop_schema], &[&create_schema, &table]);
        let (app, proxy) = through_proxy(cx, engine);
        let sleep = match engine {
            Engine::Postgres => "pg_sleep(2)",
            _ => "SLEEP(2)",
        };
        let marker = "kerno_ui_reconnect_marker";
        app.press(cx, &["secondary-t"]);
        app.type_text(
            cx,
            &format!("INSERT INTO {schema}.hits SELECT 1 FROM (SELECT {sleep} AS {marker}) s"),
        );
        app.run_tab(cx);
        app.wait_until(cx, "the insert on the server", |_, _| {
            running_on_server(engine, marker)
        });
        proxy.set(ProxyMode::Drop);
        app.wait_until(cx, "the loss", |app, cx| app.reconnecting(cx));
        assert!(app.says_lost(cx), "{engine:?}: {}", app.situation(cx));
        proxy.set(ProxyMode::Forward);
        app.wait_reconnected(cx);
        // Long enough for a replay to have reached the server.
        app.wait_until(cx, "the statement to end on the server", |_, _| {
            !running_on_server(engine, marker)
        });
        for _ in 0..6 {
            cx.executor().advance_clock(Duration::from_secs(1));
            app.step(cx, |_, _| ());
        }
        std::thread::sleep(Duration::from_secs(1));
        cx.run_until_parked();
        let count: u32 = on_server(engine, &format!("SELECT count(*) FROM {schema}.hits"))
            .parse()
            .unwrap();
        assert!(count <= 1, "{engine:?}: the insert ran {count} times");
        assert!(!running_on_server(engine, marker), "{engine:?}");
        app.press(cx, &["secondary-w"]);
        if app.has(cx, "sql-confirm") {
            app.click(cx, "sql-confirm");
        }
    }
}
