//! J14, one script on several connections (DBeaver #3605, #1804): one tab
//! per ticked connection with its results, stop or continue on error, each
//! target's own rules (production asks and a refusal skips only that
//! target, a read-only one refuses the writes and runs the reads), folders
//! that tick their connections, nothing sent to a connection left unticked,
//! and Cancel all.

use crate::multi_run::MultiRun;

use super::*;

/// The test server of `engine` saved as `name`, changed by `change`.
fn saved(
    engine: Engine,
    name: &str,
    change: impl FnOnce(&mut ConnectionConfig),
) -> ConnectionConfig {
    let mut config = testing::config(engine);
    config.name = name.into();
    change(&mut config);
    config
}

/// Opens the app with `configs` saved with their passwords, none connected,
/// and a query tab "Script" holding `sql`, on no connection, then a tab
/// "Other" after it; "Script" is the active tab.
fn with_connections(cx: &mut TestAppContext, configs: &[ConnectionConfig], sql: &str) -> Fixture {
    let storage = memory_storage();
    let ids: Vec<String> = configs
        .iter()
        .map(|config| storage.save_connection(None, config).unwrap())
        .collect();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    for id in &ids {
        kerno_core::secrets::save_secrets(secrets.as_ref(), id, &testing::secrets()).unwrap();
    }
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            let script = workspace.open_query("Script".into(), sql, None, window, cx);
            workspace.open_query("Other".into(), "", None, window, cx);
            workspace.show_query_tab(&script, window, cx);
        })
    });
    assert_eq!(app.active_query(cx).0, "Script");
    app
}

/// Opens "Run script on connections…" for the active tab.
fn open_dialog(app: &Fixture, cx: &mut TestAppContext) {
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::RunScriptOnConnections), cx)
    });
    assert!(app.has(cx, "multi-start"), "{}", app.situation(cx));
}

fn multi(app: &Fixture, cx: &mut TestAppContext) -> Entity<MultiRun> {
    cx.update(|cx| app.workspace.read(cx).multi_run().cloned())
        .expect("a run on several connections")
}

/// One line per target: `name: status · numbers · first error`.
fn summary(app: &Fixture, cx: &mut TestAppContext) -> Vec<String> {
    let run = multi(app, cx);
    cx.update(|cx| run.read(cx).summary(cx))
}

/// Waits until no target waits or runs.
fn wait_for_the_end(app: &Fixture, cx: &mut TestAppContext) {
    app.wait_until(cx, "every target to end", |app, cx| {
        let run = multi(app, cx);
        !cx.update(|cx| run.read(cx).in_progress())
    });
}

/// What each statement of each target's tab produced.
fn target_outcomes(app: &Fixture, cx: &mut TestAppContext) -> Vec<Vec<String>> {
    let run = multi(app, cx);
    cx.update(|cx| {
        run.read(cx)
            .target_tabs()
            .into_iter()
            .map(|tab| tab.map(|tab| tab.read(cx).outcomes(cx)).unwrap_or_default())
            .collect()
    })
}

fn status_of(app: &Fixture, cx: &mut TestAppContext, name: &str) -> Status {
    cx.update(|cx| {
        let connections = app.connections.read(cx);
        let saved = connections
            .saved()
            .iter()
            .find(|saved| saved.config.name == name)
            .expect("the connection");
        connections.status(&saved.id)
    })
}

/// DBeaver #3605, #1804: the same portable script on PostgreSQL, MySQL and
/// MariaDB opens three tabs after the source tab, each with its results;
/// the connections are opened on demand; a click on a target shows its tab.
#[gpui_kit::test]
fn a_portable_script_runs_on_three_engines_in_three_tabs(cx: &mut TestAppContext) {
    let engines = [Engine::Postgres, Engine::MySql, Engine::MariaDb];
    if !engines.into_iter().all(testing::available) {
        return;
    }
    let configs = engines.map(testing::config);
    let names = configs.clone().map(|config| config.name);
    let app = with_connections(cx, &configs, "SELECT 1 AS one;\nSELECT 'a' AS letter;");
    open_dialog(&app, cx);
    for index in 0..3usize {
        app.click(cx, ("multi-connection", index));
    }
    app.click(cx, "multi-start");
    wait_for_the_end(&app, cx);

    let lines = summary(&app, cx);
    for (line, name) in lines.iter().zip(&names) {
        assert!(
            line.starts_with(&format!("{name}: Done · 2 statements · 2 rows")),
            "{lines:#?}"
        );
    }
    assert_eq!(target_outcomes(&app, cx), vec![vec!["1 rows", "1 rows"]; 3]);
    let mut titles = vec!["Script".to_string()];
    titles.extend(names.iter().map(|name| format!("Script @ {name}")));
    titles.push("Other".into());
    assert_eq!(app.titles(cx), titles, "grouped after the source tab");
    for name in &names {
        assert!(
            matches!(status_of(&app, cx, name), Status::Connected { .. }),
            "{name}"
        );
    }
    // The source tab stays the active one until a target is clicked.
    assert_eq!(app.active(cx), Some(0));
    app.click(cx, ("multi-target", 1usize));
    assert!(!app.has(cx, "multi-targets"), "the dialog closes");
    assert_eq!(app.active(cx), Some(2));
    // The run can be shown again.
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::ShowMultiRun), cx)
    });
    assert!(app.has(cx, ("multi-target", 2usize)));
}

/// "Stop this connection" ends each target at its error; "Continue" runs
/// the statements after it.
#[gpui_kit::test]
fn each_connection_stops_at_its_error_or_continues(cx: &mut TestAppContext) {
    let engines = [Engine::Postgres, Engine::MySql];
    if !engines.into_iter().all(testing::available) {
        return;
    }
    let script = "SELECT 1 AS one;\nSELECT * FROM kerno_ui_multi_missing;\nSELECT 2 AS two;";
    let app = with_connections(cx, &engines.map(testing::config), script);

    open_dialog(&app, cx);
    app.click(cx, ("multi-connection", 0usize));
    app.click(cx, ("multi-connection", 1usize));
    app.click(cx, "multi-start");
    wait_for_the_end(&app, cx);
    let lines = summary(&app, cx);
    for line in &lines {
        assert!(
            line.contains(": Stopped at an error · 2 statements · 1 row")
                && line.contains("kerno_ui_multi_missing"),
            "{lines:#?}"
        );
    }
    for outcomes in target_outcomes(&app, cx) {
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        assert!(outcomes[1].starts_with("failed"), "{outcomes:?}");
    }

    // Again, going on after the error.
    app.click(cx, "multi-close");
    open_dialog(&app, cx);
    app.click(cx, ("multi-connection", 0usize));
    app.click(cx, ("multi-connection", 1usize));
    app.step(cx, |window, cx| {
        window.within("multi-on-error").click(2usize, cx)
    });
    app.click(cx, "multi-start");
    wait_for_the_end(&app, cx);
    let lines = summary(&app, cx);
    for line in &lines {
        assert!(
            line.contains(": Done, with errors · 3 statements · 2 rows")
                && line.contains("kerno_ui_multi_missing"),
            "{lines:#?}"
        );
    }
    for outcomes in target_outcomes(&app, cx) {
        assert_eq!(outcomes.len(), 3, "{outcomes:?}");
        assert!(outcomes[1].starts_with("failed"), "{outcomes:?}");
        assert_eq!(outcomes[2], "1 rows");
    }
}

/// A production target asks its own confirmation, naming it, one at a time;
/// a refusal skips that target only, and the next one asks in turn.
#[gpui_kit::test]
fn a_production_target_asks_and_a_refusal_skips_only_that_target(cx: &mut TestAppContext) {
    if ![Engine::Postgres, Engine::MySql]
        .into_iter()
        .all(testing::available)
    {
        return;
    }
    let production = |config: &mut ConnectionConfig| {
        config.environment = kerno_core::environment::PRODUCTION.into();
    };
    let configs = [
        saved(Engine::Postgres, "local PG", |_| {}),
        saved(Engine::Postgres, "prod PG", production),
        saved(Engine::MySql, "prod MySQL", production),
    ];
    let app = with_connections(cx, &configs, "SELECT 1 AS one");
    open_dialog(&app, cx);
    for index in 0..3usize {
        app.click(cx, ("multi-connection", index));
    }
    app.click(cx, "multi-start");

    // The local one runs without a question; the first production one asks,
    // the other waits for that answer.
    app.wait_until(cx, "the first confirmation", |app, cx| {
        app.has(cx, "sql-confirm")
    });
    assert!(app.shows_label(cx, "Run this statement on Production: prod PG?"));
    let lines = summary(&app, cx);
    assert!(
        lines[1].ends_with("Waiting for your confirmation"),
        "{lines:#?}"
    );
    assert_eq!(lines[2], "prod MySQL: Waiting", "{lines:#?}");
    app.click(cx, "sql-cancel");

    app.wait_until(cx, "the second confirmation", |app, cx| {
        app.shows_label(cx, "Run this statement on Production: prod MySQL?")
    });
    app.click(cx, "sql-confirm");
    wait_for_the_end(&app, cx);

    let lines = summary(&app, cx);
    assert!(
        lines[0].starts_with("local PG: Done · 1 statement"),
        "{lines:#?}"
    );
    assert_eq!(lines[1], "prod PG: Not run: confirmation declined");
    assert!(
        lines[2].starts_with("prod MySQL: Done · 1 statement · 1 row")
            && lines[2].contains("transaction open"),
        "production starts in manual commit: {lines:#?}"
    );
    let outcomes = target_outcomes(&app, cx);
    assert_eq!(outcomes[1], Vec::<String>::new(), "nothing ran on prod PG");
    assert_eq!(outcomes[2], ["1 rows"]);
    let declined = cx.update(|cx| app.connections.read(cx).saved()[1].id.clone());
    assert!(
        app.storage
            .history(Some(&declined), "", 10)
            .unwrap()
            .is_empty()
    );
}

/// A read-only target refuses the INSERT in its place, says so, and runs
/// the SELECTs; the other target inserts. Proven on the server.
#[gpui_kit::test]
fn a_read_only_target_refuses_the_insert_and_runs_the_selects(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_multi_ro CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_multi_ro",
            "CREATE TABLE kerno_ui_multi_ro.t (id INT)",
        ],
    );
    let configs = [
        saved(Engine::Postgres, "writer", |_| {}),
        saved(Engine::Postgres, "reader", |config| config.read_only = true),
    ];
    let script = "SELECT 1 AS one;\nINSERT INTO kerno_ui_multi_ro.t VALUES (1);\nSELECT 2 AS two;";
    let app = with_connections(cx, &configs, script);
    open_dialog(&app, cx);
    app.click(cx, ("multi-connection", 0usize));
    app.click(cx, ("multi-connection", 1usize));
    app.click(cx, "multi-start");
    wait_for_the_end(&app, cx);

    assert!(!app.has(cx, "sql-preview"), "nothing was asked");
    let lines = summary(&app, cx);
    assert!(
        lines[0].starts_with("writer: Done · 3 statements · 3 rows"),
        "{lines:#?}"
    );
    assert!(
        lines[1].starts_with("reader: Done · 2 statements · 2 rows")
            && lines[1].ends_with("1 write refused (read-only)"),
        "{lines:#?}"
    );
    assert_eq!(
        target_outcomes(&app, cx),
        [
            vec!["1 rows", "done, 1 affected", "1 rows"],
            vec!["1 rows", "failed []", "1 rows"],
        ]
    );
    assert_eq!(
        on_server(Engine::Postgres, "SELECT count(*) FROM kerno_ui_multi_ro.t"),
        "1"
    );
}

/// A folder ticks its connections; the connection left unticked gets
/// nothing, not even a connection. Proven on the three servers.
#[gpui_kit::test]
fn a_folder_ticks_its_connections_and_an_unticked_one_receives_nothing(cx: &mut TestAppContext) {
    let engines = [Engine::Postgres, Engine::MySql, Engine::MariaDb];
    if !engines.into_iter().all(testing::available) {
        return;
    }
    let _postgres = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_multi_untick CASCADE"],
        &["CREATE SCHEMA kerno_ui_multi_untick"],
    );
    let _mysql = Scratch::new(
        Engine::MySql,
        &["DROP DATABASE IF EXISTS kerno_ui_multi_untick"],
        &["CREATE DATABASE kerno_ui_multi_untick"],
    );
    let _mariadb = Scratch::new(
        Engine::MariaDb,
        &["DROP DATABASE IF EXISTS kerno_ui_multi_untick"],
        &["CREATE DATABASE kerno_ui_multi_untick"],
    );
    let in_folder = |config: &mut ConnectionConfig| config.group = Some("multi".into());
    let configs = [
        saved(Engine::Postgres, "pg", in_folder),
        saved(Engine::MySql, "mysql", |_| {}),
        saved(Engine::MariaDb, "mariadb", in_folder),
    ];
    let script = "CREATE TABLE kerno_ui_multi_untick.t (id INT);\n\
                  INSERT INTO kerno_ui_multi_untick.t VALUES (1);";
    let app = with_connections(cx, &configs, script);
    open_dialog(&app, cx);
    // Rows: the folder, its two connections, then mysql at the top level.
    app.click(cx, ("multi-folder", 0usize));
    app.click(cx, "multi-start");
    wait_for_the_end(&app, cx);

    let lines = summary(&app, cx);
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(
        lines[0].starts_with("pg: Done · 2 statements"),
        "{lines:#?}"
    );
    assert!(
        lines[1].starts_with("mariadb: Done · 2 statements"),
        "{lines:#?}"
    );
    assert!(
        !app.titles(cx).contains(&"Script @ mysql".to_string()),
        "{:?}",
        app.titles(cx)
    );
    assert_eq!(status_of(&app, cx, "mysql"), Status::Disconnected);
    for engine in [Engine::Postgres, Engine::MariaDb] {
        assert_eq!(
            on_server(engine, "SELECT count(*) FROM kerno_ui_multi_untick.t"),
            "1",
            "{engine:?}"
        );
    }
    assert_eq!(
        on_server(
            Engine::MySql,
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = 'kerno_ui_multi_untick'"
        ),
        "0",
        "nothing reached the unticked connection"
    );
}

/// Cancel all stops the statements running on every target.
#[gpui_kit::test]
fn cancel_all_stops_every_running_target(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let configs = [
        saved(Engine::Postgres, "slow A", |_| {}),
        saved(Engine::Postgres, "slow B", |_| {}),
    ];
    let app = with_connections(cx, &configs, "SELECT pg_sleep(60) AS kerno_ui_multi_sleep");
    open_dialog(&app, cx);
    app.click(cx, ("multi-connection", 0usize));
    app.click(cx, ("multi-connection", 1usize));
    app.click(cx, "multi-start");
    app.wait_until(cx, "both statements on the server", |_, _| {
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM pg_stat_activity WHERE state = 'active' \
             AND query LIKE '%kerno_ui_multi_sleep%' AND pid <> pg_backend_pid()",
        ) == "2"
    });
    app.click(cx, "multi-cancel-all");
    wait_for_the_end(&app, cx);
    let lines = summary(&app, cx);
    assert!(
        lines.iter().all(|line| line.contains(": Cancelled")),
        "{lines:#?}"
    );
    assert_eq!(target_outcomes(&app, cx), vec![vec!["cancelled"]; 2]);
}
