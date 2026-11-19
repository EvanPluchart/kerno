//! M12, what the general critique of DBeaver shows (docs/plans/M12.md, the C points of
//! docs/COMPETITOR-ISSUES.md).

use gpui_kit::VisualTestContext;

use super::*;

/// The test's own table `<name>.t` on `engine`, dropped when the test ends.
fn quit_table(engine: Engine, name: &str) -> Scratch {
    let (drop, create) = scratch_schema(engine, name);
    let table = format!("CREATE TABLE {name}.t (id INT PRIMARY KEY)");
    Scratch::new(engine, &[&drop], &[&create, &table])
}

fn quit_requested(app: &Fixture, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| app.workspace.read(cx).quit_requested())
}

/// Ends the transaction a test left open, before its objects are dropped.
fn roll_back(app: &Fixture, cx: &mut TestAppContext) {
    app.dispatch(cx, actions::RollbackTransaction);
    app.wait_until(cx, "the rollback", |app, cx| {
        app.tx_state(cx) == kerno_core::TxState::Idle
    });
}

/// C15: Quit with a transaction open asks first, naming the tab; Cancel keeps the app and
/// the transaction as they were.
#[gpui_kit::test]
fn quitting_with_an_open_transaction_asks_and_cancel_keeps_it(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let _scratch = quit_table(engine, "kerno_ui_quit_cancel");
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        app.open_transaction(cx, "insert into kerno_ui_quit_cancel.t values (1)");

        app.dispatch(cx, actions::Quit);
        assert!(app.has(cx, "end-question"), "{engine:?}");
        for line in [
            "Quit Kerno?",
            "“Query 1”: a transaction is open.",
            "Roll back and continue",
        ] {
            assert!(app.shows_label(cx, line), "{engine:?}: {line}");
        }
        // Enter never answers it.
        app.press(cx, &["enter"]);
        assert!(app.has(cx, "end-question"), "{engine:?}");
        app.click(cx, "end-cancel");
        assert!(!app.dialog_open(cx), "{engine:?}");
        assert!(!quit_requested(&app, cx), "{engine:?}");
        assert_eq!(app.tx_state(cx), kerno_core::TxState::Open, "{engine:?}");
        roll_back(&app, cx);
    }
}

/// C15: with nothing to lose (no transaction, no pending change; unsaved text is kept by the
/// stored tabs), Quit quits at once, without a dialog.
#[gpui_kit::test]
fn quitting_with_nothing_to_lose_quits_at_once(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        app.type_text(cx, "select 1 as kerno_ui_quit_nothing");
        app.dispatch(cx, actions::Quit);
        assert!(!app.dialog_open(cx), "{engine:?}");
        assert!(quit_requested(&app, cx), "{engine:?}");
    }
}

/// C15: "Roll back and continue" sends ROLLBACK, then quits: the row is not on the server and
/// nothing holds its lock any more.
#[gpui_kit::test]
fn roll_back_and_quit_rolls_back(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let _scratch = quit_table(engine, "kerno_ui_quit_rb");
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        app.open_transaction(cx, "insert into kerno_ui_quit_rb.t values (1)");

        app.dispatch(cx, actions::Quit);
        app.click(cx, "end-rollback");
        app.wait_until(cx, "the rollback, then the quit", |app, cx| {
            quit_requested(app, cx)
        });
        assert_eq!(app.tx_state(cx), kerno_core::TxState::Idle, "{engine:?}");
        assert_eq!(
            on_server(engine, "SELECT count(*) FROM kerno_ui_quit_rb.t"),
            "0",
            "{engine:?}"
        );
        // The same key goes in from outside at once: no session holds the row the tab
        // inserted (an open transaction would make this wait, then fail).
        on_server(engine, "INSERT INTO kerno_ui_quit_rb.t VALUES (1)");
        assert_eq!(
            on_server(engine, "SELECT count(*) FROM kerno_ui_quit_rb.t"),
            "1",
            "{engine:?}"
        );
    }
}

/// C15: the window's close button takes the same path: the window stays, the question is
/// asked, Cancel keeps the transaction; with nothing left to lose it quits.
#[gpui_kit::test]
fn closing_the_window_asks_as_quit_does(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let _scratch = quit_table(engine, "kerno_ui_quit_close");
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        app.open_transaction(cx, "insert into kerno_ui_quit_close.t values (1)");

        let close = |app: &Fixture, cx: &mut TestAppContext| {
            let closed = VisualTestContext::from_window(app.window.into(), cx).simulate_close();
            app.step(cx, |_, _| ());
            closed
        };
        assert!(!close(&app, cx), "{engine:?}: the window stays");
        assert!(app.shows_label(cx, "Quit Kerno?"), "{engine:?}");
        assert!(
            app.shows_label(cx, "“Query 1”: a transaction is open."),
            "{engine:?}"
        );
        app.click(cx, "end-cancel");
        assert!(!quit_requested(&app, cx), "{engine:?}");
        assert_eq!(app.tx_state(cx), kerno_core::TxState::Open, "{engine:?}");

        roll_back(&app, cx);
        assert!(!close(&app, cx), "{engine:?}");
        assert!(!app.dialog_open(cx), "{engine:?}");
        assert!(quit_requested(&app, cx), "{engine:?}");
    }
}

/// C15: Quit while a dialog has the keyboard (the workspace is not on its path: the menu bar
/// or Cmd+Q reach the app-wide handler of `main.rs`) still asks first.
#[gpui_kit::test]
fn quitting_from_a_dialog_asks_too(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = quit_table(Engine::Postgres, "kerno_ui_quit_dialog");
    let app = connected(cx, Engine::Postgres);
    let (window, workspace) = (app.window.into(), app.workspace.downgrade());
    cx.update(|cx| crate::workspace::handle_quit(window, workspace, cx));
    app.press(cx, &["secondary-t"]);
    app.open_transaction(cx, "insert into kerno_ui_quit_dialog.t values (1)");

    app.dispatch(cx, actions::NewConnection);
    assert!(app.dialog_open(cx));
    app.dispatch(cx, actions::Quit);
    assert!(app.has(cx, "end-question"));
    assert!(app.shows_label(cx, "“Query 1”: a transaction is open."));
    // A second Quit does not ask twice: one Cancel closes the only question.
    app.dispatch(cx, actions::Quit);
    app.click(cx, "end-cancel");
    assert!(!app.has(cx, "end-question"));
    // Once cancelled, Quit asks again, with the other dialog still open.
    app.dispatch(cx, actions::Quit);
    assert!(app.has(cx, "end-question"));
    app.click(cx, "end-cancel");
    assert!(!app.has(cx, "end-question"));
    assert!(!quit_requested(&app, cx));
    assert_eq!(app.tx_state(cx), kerno_core::TxState::Open);
    // The connection dialog is still there, under the question.
    assert!(app.dialog_open(cx));
    app.press(cx, &["escape"]);
    assert!(!app.dialog_open(cx));
    roll_back(&app, cx);
}

// K2: every write can be cancelled; the first page of a table too (C3) -------------

/// A session of the test's own that holds a lock on `table` until dropped:
/// on PostgreSQL `LOCK TABLE … IN ACCESS EXCLUSIVE MODE` in a transaction,
/// on MySQL `LOCK TABLES … WRITE`. Any statement of the app on that table
/// waits behind it. Dropping it releases the lock and closes the session,
/// whether the test passed or not; a session that does not end in 15 s is
/// terminated (its own, and no other).
struct Locker {
    engine: Engine,
    id: String,
    release: Option<std::sync::mpsc::Sender<()>>,
    done: Option<std::thread::JoinHandle<()>>,
}

impl Locker {
    fn start(engine: Engine, table: &str) -> Self {
        use kerno_core::Driver as _;
        let (own_id, lock) = match engine {
            Engine::Postgres => (
                "SELECT pg_backend_pid()",
                vec![
                    "BEGIN".to_string(),
                    format!("LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE"),
                ],
            ),
            Engine::MySql | Engine::MariaDb => (
                "SELECT CONNECTION_ID()",
                vec![format!("LOCK TABLES {table} WRITE")],
            ),
        };
        let unlock = match engine {
            Engine::Postgres => "ROLLBACK",
            Engine::MySql | Engine::MariaDb => "UNLOCK TABLES",
        };
        let (locked, held) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let done = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let session = runtime.block_on(async {
                let params = testing::params(engine);
                let session = match engine {
                    Engine::Postgres => kerno_postgres::PostgresDriver.connect(&params).await,
                    Engine::MySql | Engine::MariaDb => {
                        kerno_mysql::MysqlDriver.connect(&params).await
                    }
                }
                .expect("the test server");
                let id = kerno_core::exec::collect(session.clone(), own_id)
                    .await
                    .unwrap()
                    .results[0]
                    .rows()[0][0]
                    .clone()
                    .unwrap();
                for sql in &lock {
                    kerno_core::exec::collect(session.clone(), sql.as_str())
                        .await
                        .unwrap_or_else(|error| panic!("{sql}: {error}"));
                }
                locked.send(id).unwrap();
                session
            });
            // Held until the test lets go (or its end drops the sender).
            let _ = released.recv();
            runtime.block_on(async {
                let _ = kerno_core::exec::collect(session.clone(), unlock).await;
                session.close().await;
            });
        });
        let id = held
            .recv_timeout(Duration::from_secs(30))
            .expect("the lock");
        Self {
            engine,
            id,
            release: Some(release),
            done: Some(done),
        }
    }

    /// Whether the lock is still held.
    fn holds(&self) -> bool {
        self.done.as_ref().is_some_and(|done| !done.is_finished())
    }

    /// Whether a statement of another session waits for this lock: the one
    /// a cancel is meant for. (Its text may not name the table: on
    /// PostgreSQL the first statement to wait can be one that reads the
    /// catalog by the table's oid.)
    fn blocks(&self) -> bool {
        let sql = match self.engine {
            Engine::Postgres => format!(
                "SELECT count(*) FROM pg_stat_activity WHERE {} = ANY(pg_blocking_pids(pid))",
                self.id
            ),
            Engine::MySql | Engine::MariaDb => format!(
                "SELECT count(*) FROM sys.schema_table_lock_waits WHERE blocking_pid = {}",
                self.id
            ),
        };
        on_server(self.engine, &sql) != "0"
    }
}

impl Drop for Locker {
    fn drop(&mut self) {
        drop(self.release.take());
        let Some(done) = self.done.take() else {
            return;
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while !done.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !done.is_finished() {
            let sql = match self.engine {
                Engine::Postgres => format!("SELECT pg_terminate_backend({})", self.id),
                Engine::MySql | Engine::MariaDb => format!("KILL CONNECTION {}", self.id),
            };
            let engine = self.engine;
            // A failure here must not abort a test that is already failing.
            let _ = std::thread::spawn(move || on_server(engine, &sql)).join();
        }
        let _ = done.join();
    }
}

/// The test's own table `<name>.t (id, v)` holding the row `(1, 'before')`,
/// dropped when the test ends.
fn k2_table(engine: Engine, name: &str) -> Scratch {
    let (drop, create) = scratch_schema(engine, name);
    let table = format!("CREATE TABLE {name}.t (id INT PRIMARY KEY, v VARCHAR(20))");
    let row = format!("INSERT INTO {name}.t VALUES (1, 'before')");
    Scratch::new(engine, &[&drop], &[&create, &table, &row])
}

/// The value of the row on the server.
fn k2_value(engine: Engine, name: &str) -> String {
    on_server(engine, &format!("SELECT v FROM {name}.t WHERE id = 1"))
}

/// The tree path of the table `<name>.t`.
fn k2_path(engine: Engine, name: &str) -> Vec<&str> {
    match engine {
        Engine::Postgres => vec!["kerno", name, "Tables", "t"],
        Engine::MySql | Engine::MariaDb => vec![name, "Tables", "t"],
    }
}

/// Waits for a statement of the app to wait behind `locker`.
fn wait_for_the_lock(app: &Fixture, cx: &mut TestAppContext, locker: &Locker) {
    app.wait_until(cx, "a statement to wait for the lock", |_, _| {
        locker.blocks()
    });
}

/// Waits for the statement that waited behind `locker` to be gone from the
/// server, the lock still held: it was stopped by the cancel, not by the
/// lock's end.
fn stopped_on_the_server(app: &Fixture, cx: &mut TestAppContext, locker: &Locker) {
    app.wait_until(cx, "the statement to be gone from the server", |_, _| {
        !locker.blocks()
    });
    assert!(
        locker.holds(),
        "stopped by the cancel, not by the lock's end"
    );
}

/// C3: Apply behind a lock held by another session has a Cancel (the
/// footer's, and Cmd+.): the UPDATE is stopped on the server, the one
/// transaction rolls back, and the pending change is kept.
#[gpui_kit::test]
fn a_data_apply_behind_a_lock_is_cancelled_and_keeps_the_edits(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k2_apply";
        let _scratch = k2_table(engine, name);
        let app = connected(cx, engine);
        app.open_table(cx, name, "t", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–1 of 1");
        let locker = Locker::start(engine, &format!("{name}.t"));
        app.type_in_cell(cx, 0, 2, "after", "enter");
        assert_eq!(app.pending(cx), 1, "{engine:?}");
        app.click(cx, "data-apply");
        app.click(cx, "sql-confirm");
        wait_for_the_lock(&app, cx, &locker);
        if engine == Engine::Postgres {
            app.click(cx, "data-cancel");
        } else {
            app.press(cx, &["secondary-."]);
        }
        app.wait_until(cx, "the cancelled apply", |app, cx| {
            let tab = app.data(cx);
            cx.update(|cx| tab.read(cx).apply_error().map(str::to_string))
                .is_some()
        });
        let tab = app.data(cx);
        assert_eq!(
            cx.update(|cx| tab.read(cx).apply_error().map(str::to_string)),
            Some("Cancelled: nothing was changed. The pending changes are kept.".into()),
            "{engine:?}"
        );
        stopped_on_the_server(&app, cx, &locker);
        assert_eq!(app.pending(cx), 1, "{engine:?}");
        drop(locker);
        assert_eq!(k2_value(engine, name), "before", "{engine:?}");
        // Nothing of the tab holds the row: the same write goes in at once.
        on_server(
            engine,
            &format!("UPDATE {name}.t SET v = 'outside' WHERE id = 1"),
        );
        assert_eq!(k2_value(engine, name), "outside", "{engine:?}");
        app.click(cx, "data-discard");
    }
}

/// C3: the table form's Apply behind a lock shows "Applying…" with Cancel,
/// and Cmd+. reaches it too: the ALTER is stopped on the server and the
/// form says nothing was changed.
#[gpui_kit::test]
fn a_structure_apply_behind_a_lock_is_cancelled(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k2_alter";
        let _scratch = k2_table(engine, name);
        let app = connected(cx, engine);
        k2_alter_behind_a_lock(&app, cx, engine, name);
        let locker = Locker::start(engine, &format!("{name}.t"));
        app.click(cx, "editor-apply");
        app.click(cx, "sql-confirm");
        wait_for_the_lock(&app, cx, &locker);
        assert!(app.has(cx, "editor-applying"), "{engine:?}");
        if engine == Engine::Postgres {
            // The form has the keyboard.
            let editor = app.editor(cx);
            app.step(cx, |window, cx| {
                editor.read(cx).focus_handle(cx).focus(window, cx)
            });
            app.press(cx, &["secondary-."]);
        } else {
            app.click(cx, "editor-cancel");
        }
        let editor = app.editor(cx);
        app.wait_until(cx, "the cancelled apply", |_, cx| {
            cx.update(|cx| editor.read(cx).error().is_some())
        });
        assert_eq!(
            cx.update(|cx| editor.read(cx).error().map(str::to_string)),
            Some("Cancelled: nothing was changed.".into()),
            "{engine:?}"
        );
        assert!(!app.has(cx, "editor-applying"), "{engine:?}");
        stopped_on_the_server(&app, cx, &locker);
        drop(locker);
        // The column is not there, and nothing holds the table.
        on_server(
            engine,
            &format!("ALTER TABLE {name}.t ADD COLUMN added INT"),
        );
    }
}

/// Opens the form of `<name>.t` and adds the column `added`, not applied.
fn k2_alter_behind_a_lock(app: &Fixture, cx: &mut TestAppContext, engine: Engine, name: &str) {
    let connection = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let target = Target {
        connection,
        database: None,
        schema: None,
    };
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_table_editor(
                target,
                name.to_string(),
                Some(TableRef::new(name, "t")),
                window,
                cx,
            );
        })
    });
    app.wait_for_editor(cx);
    app.click(cx, "editor-add-column");
    app.fill_in(cx, ("column-name", 2usize), "added");
    app.fill_in(cx, ("column-type", 2usize), "int");
    let editor = app.editor(cx);
    let statements = cx
        .update(|cx| editor.read(cx).statements(cx))
        .map(|(statements, _)| statements)
        .unwrap_or_else(|problems| panic!("{engine:?}: {problems:?}"));
    assert_eq!(statements.len(), 1, "{engine:?}: {statements:?}");
}

/// C3: Truncate from the navigator (its tools dialog) behind a lock is
/// cancelled on the server; the rows stay.
#[gpui_kit::test]
fn a_navigator_truncate_behind_a_lock_is_cancelled(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k2_truncate";
        let _scratch = k2_table(engine, name);
        let app = connected(cx, engine);
        let path = k2_path(engine, name);
        app.open_path(cx, &path);
        let locker = Locker::start(engine, &format!("{name}.t"));
        let dialog = app
            .tools_dialog(
                cx,
                &path,
                Some(kerno_core::maintenance::Maintenance::Truncate(
                    Default::default(),
                )),
            )
            .expect("the dialog");
        app.click(cx, "maintenance-run");
        app.click(cx, "sql-confirm");
        wait_for_the_lock(&app, cx, &locker);
        app.click(cx, "maintenance-cancel");
        assert_eq!(app.wait_tool(cx, &dialog), "cancelled", "{engine:?}");
        stopped_on_the_server(&app, cx, &locker);
        drop(locker);
        assert_eq!(k2_value(engine, name), "before", "{engine:?}");
        app.click(cx, "maintenance-close");
    }
}

/// C3: Drop from the navigator behind a lock: the row says "Dropping…", its
/// menu and a notification offer Cancel, which stops the DROP on the
/// server; the table stays.
#[gpui_kit::test]
fn a_navigator_drop_behind_a_lock_is_cancelled(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k2_drop";
        let _scratch = k2_table(engine, name);
        let app = connected(cx, engine);
        let path = k2_path(engine, name);
        app.open_path(cx, &path);
        let locker = Locker::start(engine, &format!("{name}.t"));
        app.act(cx, Action::Drop, &path);
        app.click(cx, "sql-confirm");
        wait_for_the_lock(&app, cx, &locker);
        let navigator = cx.update(|cx| app.workspace.read(cx).navigator(cx));
        let writes = cx.update(|cx| navigator.read(cx).writes());
        assert_eq!(
            writes
                .iter()
                .map(|(_, doing)| doing.as_str())
                .collect::<Vec<_>>(),
            ["Dropping table t…"],
            "{engine:?}"
        );
        let row = app.tree_row(cx, &path);
        assert!(app.has(cx, ("node-doing", row)), "{engine:?}");
        if engine == Engine::Postgres {
            // The notification's Cancel, once it has slid in.
            cx.executor().advance_clock(Duration::from_secs(1));
            app.click(cx, ("navigator-write-cancel", writes[0].0 as usize));
        } else {
            // The row's menu.
            app.right_click(cx, ("node", row), None);
            click_menu_entry(&app, cx, "Cancel the running statement");
        }
        app.wait_until(cx, "the cancelled drop", |_, cx| {
            cx.update(|cx| navigator.read(cx).writes().is_empty())
        });
        assert!(!app.has(cx, ("node-doing", row)), "{engine:?}");
        stopped_on_the_server(&app, cx, &locker);
        drop(locker);
        assert_eq!(k2_value(engine, name), "before", "{engine:?}");
    }
}

/// C3: a table opened for the first time while another session locks it
/// shows "Loading rows…" with a Cancel that works: the read is stopped on
/// the server, the tab says "Cancelled" and Reload reads the rows.
#[gpui_kit::test]
fn the_first_page_of_a_table_behind_a_lock_is_cancelled(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k2_first";
        let _scratch = k2_table(engine, name);
        let app = connected(cx, engine);
        let locker = Locker::start(engine, &format!("{name}.t"));
        app.open_table(cx, name, "t", RelationKind::Table);
        wait_for_the_lock(&app, cx, &locker);
        assert_eq!(app.data_state(cx), "loading", "{engine:?}");
        app.click(cx, "data-cancel");
        app.wait_until(cx, "the cancelled read", |app, cx| {
            app.data_state(cx) == "cancelled"
        });
        assert!(app.has(cx, "data-cancelled"), "{engine:?}");
        stopped_on_the_server(&app, cx, &locker);
        drop(locker);
        app.click(cx, "data-retry");
        app.wait_for_rows(cx, "Rows 1–1 of 1");
    }
}

/// C3: closing a tab while its statement waits for a lock stops the
/// statement first: a table form that applies, a table's first page.
/// Nothing of a closed tab keeps running.
#[gpui_kit::test]
fn closing_a_tab_that_applies_stops_the_statement(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k2_close";
        let _scratch = k2_table(engine, name);
        let app = connected(cx, engine);
        k2_alter_behind_a_lock(&app, cx, engine, name);
        let locker = Locker::start(engine, &format!("{name}.t"));

        // The table form, applying.
        app.click(cx, "editor-apply");
        app.click(cx, "sql-confirm");
        wait_for_the_lock(&app, cx, &locker);
        app.dispatch(cx, actions::CloseTab);
        assert!(app.shows_label(cx, "Discard and close"), "{engine:?}");
        app.click(cx, "ok");
        assert!(app.titles(cx).is_empty(), "{engine:?}");
        stopped_on_the_server(&app, cx, &locker);

        // A table's first page, reading.
        app.open_table(cx, name, "t", RelationKind::Table);
        wait_for_the_lock(&app, cx, &locker);
        app.dispatch(cx, actions::CloseTab);
        assert!(app.titles(cx).is_empty(), "{engine:?}");
        stopped_on_the_server(&app, cx, &locker);

        drop(locker);
        // Neither ran: the column is not there, nothing holds the table.
        on_server(
            engine,
            &format!("ALTER TABLE {name}.t ADD COLUMN added INT"),
        );
    }
}

// K3: a query tab shows the database and schema its session is really in (C6) -----------------

/// What moves the session to `place` on `engine`, and what reads back where it is, with the
/// id of the server session that answers.
fn k3_sql(engine: Engine, place: &str) -> (String, &'static str) {
    match engine {
        Engine::Postgres => (
            format!("set search_path to {place}"),
            "select current_schema(), pg_backend_pid()::text",
        ),
        Engine::MySql | Engine::MariaDb => {
            (format!("use {place}"), "select database(), connection_id()")
        }
    }
}

/// Runs `sql` in the active tab, in place of its text, and gives what it produced.
fn k3_run(app: &Fixture, cx: &mut TestAppContext, sql: &str) -> Vec<String> {
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, sql);
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, sql, |app, cx| {
        !app.running(cx) && !app.outcomes(cx).is_empty()
    });
    app.outcomes(cx)
}

/// Where the active tab's session is, and its id on the server; after a lost connection, once
/// the connection is back.
fn k3_where(app: &Fixture, cx: &mut TestAppContext, engine: Engine) -> (String, String) {
    let (_, read) = k3_sql(engine, "");
    for _ in 0..3 {
        if k3_run(app, cx, read) == ["1 rows"] {
            let row = app.result(cx, 0, 1).1.remove(0);
            return (
                row[0].clone().unwrap_or_default(),
                row[1].clone().unwrap_or_default(),
            );
        }
        // The statement found the session gone: the connection opens again.
        app.wait_until(cx, "the connection to come back", |app, cx| {
            cx.executor().advance_clock(Duration::from_millis(500));
            let connections = app.connections.read_with(cx, |connections, _| {
                connections.reconnecting(&connections.saved()[0].id)
            });
            matches!(app.status(cx, 0), Status::Connected { .. }) && !connections
        });
    }
    panic!("{engine:?}: {read} never answered");
}

/// C6: a `USE` (a `SET search_path` on PostgreSQL) run in a tab moves the tab's picker, status
/// bar and stored target, says so, and leaves the other tabs alone; when the tab's server
/// session is killed, the next one opens where the user was, as the server itself answers.
fn k3_moves_and_survives_a_reconnect(cx: &mut TestAppContext, engine: Engine, place: &str) {
    let (drop, create) = scratch_schema(engine, place);
    let _scratch = Scratch::new(engine, &[&drop], &[&create]);
    let app = connected(cx, engine);
    app.press(cx, &["secondary-t"]);
    let other = app.tab(cx);
    // A session of its own, in the first place.
    let (home, _) = k3_where(&app, cx, engine);
    assert_ne!(home, place, "{engine:?}");
    let other_target = cx.update(|cx| other.read(cx).target().cloned());
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    let started = cx.update(|cx| tab.read(cx).target().cloned());
    let (_, first_id) = k3_where(&app, cx, engine);
    // A `SET` that moves nothing says nothing and changes nothing.
    let unmoved = match engine {
        Engine::Postgres => "set statement_timeout to 0",
        Engine::MySql | Engine::MariaDb => "set @kerno_ui_k3 = 1",
    };
    assert_eq!(k3_run(&app, cx, unmoved), ["done"], "{engine:?}");
    let messages = cx.update(|cx| tab.read(cx).messages());
    assert!(
        !messages.iter().any(|message| message.contains("Now in")),
        "{engine:?}: {messages:?}"
    );
    assert_eq!(cx.update(|cx| tab.read(cx).target().cloned()), started);

    let (moving, _) = k3_sql(engine, place);
    assert_eq!(k3_run(&app, cx, &moving), ["done"], "{engine:?}");
    let (what, how) = match engine {
        Engine::Postgres => ("schema", "search_path"),
        Engine::MySql | Engine::MariaDb => ("database", "USE"),
    };
    let messages = cx.update(|cx| tab.read(cx).messages());
    assert!(
        messages.contains(&format!("Info: Now in {what} {place} ({how}).")),
        "{engine:?}: {messages:?}"
    );
    let picker = cx.update(|cx| {
        app.workspace
            .read(cx)
            .picker_text(crate::workspace::toolbar::Item::Target, cx)
    });
    assert!(
        picker.starts_with(&format!("{place}@")),
        "{engine:?}: {picker}"
    );
    if engine != Engine::Postgres {
        let status = app.status_bar(cx);
        assert!(
            status.iter().any(|item| item.contains(place)),
            "{engine:?}: {status:?}"
        );
    }
    // The other tab did not move.
    assert_eq!(
        cx.update(|cx| other.read(cx).target().cloned()),
        other_target,
        "{engine:?}"
    );
    // The stored tab opens there after a restart (stored on a thread of its own).
    let stored_place = |app: &Fixture| {
        let tabs = app.storage.tabs::<TabSnapshot>().unwrap();
        tabs.into_iter().find_map(|tab| match tab {
            TabSnapshot::Query { title, target, .. } if title == "Query 2" => {
                target.and_then(|target| match engine {
                    Engine::Postgres => target.schema,
                    Engine::MySql | Engine::MariaDb => target.database,
                })
            }
            _ => None,
        })
    };
    app.wait_until(cx, "the tab to be stored", |app, cx| {
        cx.executor().advance_clock(Duration::from_millis(500));
        stored_place(app).as_deref() == Some(place)
    });

    if engine == Engine::Postgres {
        // A `SET LOCAL` lasts until the transaction ends: Roll back brings the tab back.
        let picker = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                app.workspace
                    .read(cx)
                    .picker_text(crate::workspace::toolbar::Item::Target, cx)
            })
        };
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(true, cx)));
        assert_eq!(
            k3_run(&app, cx, "set local search_path to public"),
            ["done"]
        );
        assert!(picker(cx).starts_with("public@"), "{}", picker(cx));
        roll_back(&app, cx);
        app.wait_until(cx, "the picker to come back", |_, cx| {
            picker(cx).starts_with(&format!("{place}@"))
        });
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(false, cx)));
    }

    // The server kills the tab's session: the next one opens where the tab was.
    let kill = match engine {
        Engine::Postgres => format!("SELECT pg_terminate_backend({first_id})"),
        Engine::MySql | Engine::MariaDb => format!("KILL {first_id}"),
    };
    on_server(engine, &kill);
    let (now, second_id) = k3_where(&app, cx, engine);
    assert_ne!(second_id, first_id, "{engine:?}: a new session");
    assert_eq!(now, place, "{engine:?}");

    // Choosing the first place in the picker goes back there.
    app.step(cx, |_, cx| {
        tab.update(cx, |tab, cx| tab.set_target(started, cx))
    });
    assert_eq!(k3_where(&app, cx, engine).0, home, "{engine:?}");
}

/// C6: `USE` on MySQL. (One engine per test: the stored tabs are the app's, and a workspace
/// left from another engine's run would store its own over them.)
#[gpui_kit::test]
fn use_in_a_tab_moves_its_picker_and_survives_a_reconnect(cx: &mut TestAppContext) {
    if !testing::available(Engine::MySql) {
        return;
    }
    k3_moves_and_survives_a_reconnect(cx, Engine::MySql, "kerno_ui_k3_b");
}

/// C6: `USE` on MariaDB.
#[gpui_kit::test]
fn use_in_a_mariadb_tab_moves_its_picker_and_survives_a_reconnect(cx: &mut TestAppContext) {
    if !testing::available(Engine::MariaDb) {
        return;
    }
    k3_moves_and_survives_a_reconnect(cx, Engine::MariaDb, "kerno_ui_k3_b");
}

/// C6: `SET search_path` on PostgreSQL.
#[gpui_kit::test]
fn set_search_path_in_a_tab_moves_its_picker_and_survives_a_reconnect(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    k3_moves_and_survives_a_reconnect(cx, Engine::Postgres, "kerno_ui_k3_other");
}

// K4: an open transaction is visible on every tab, with its age (C15, C21) ---------

/// Whether the window redraws the transactions' ages (a timer alive only while one is open).
fn ages_refreshing(app: &Fixture, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| app.workspace.read(cx).ages_refreshing())
}

/// What the transaction badge `id` says (its tooltip, for screen readers too); `None` when it
/// is not drawn.
fn badge(app: &Fixture, cx: &mut TestAppContext, id: (&'static str, usize)) -> Option<String> {
    app.step(cx, |window, _| {
        window
            .try_find(id)
            .map(|badge| badge.description().unwrap_or_default().to_string())
    })
}

/// The tooltip of a badge with the default shortcuts of Commit (⌃4) and Roll back (⌃8), as the
/// kit writes them on this system.
fn tip(state: kerno_core::TxState, age: Duration) -> String {
    let keys = |stroke: &'static str| {
        gpui_kit::component::kbd::Kbd::format(&gpui_kit::Keystroke::parse(stroke).unwrap())
    };
    crate::tabs::transaction_tip(
        state,
        Some(age),
        Some(&keys("ctrl-4")),
        Some(&keys("ctrl-8")),
    )
}

/// What the badge `id` said in the last frame the window drew by itself (`badge` draws one
/// first): what the user sees when nothing but the app made the window draw.
fn drawn(app: &Fixture, cx: &mut TestAppContext, id: (&'static str, usize)) -> Option<String> {
    cx.update_window(app.window.into(), |_, window, _| {
        window
            .try_find(id)
            .map(|badge| badge.description().unwrap_or_default().to_string())
    })
    .unwrap()
}

/// Moves the executor's clock (the ages, the ticker), then draws a frame.
fn later(app: &Fixture, cx: &mut TestAppContext, seconds: u64) {
    cx.executor().advance_clock(Duration::from_secs(seconds));
    app.step(cx, |_, _| ());
}

/// C15: a transaction left open in a tab is seen from another tab: a badge on its tab in every
/// layout of the tabs and in the list of every tab, its age and the real shortcuts of Commit and
/// Roll back in its tooltip; the status bar of the tab on screen says nothing of it, the tab's
/// own says its age. The ages are redrawn only while a transaction is open.
#[gpui_kit::test]
fn a_transaction_is_shown_on_its_tab_while_another_is_active(cx: &mut TestAppContext) {
    use kerno_core::TxState::Open;
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        assert!(
            !ages_refreshing(&app, cx),
            "{engine:?}: no transaction, no timer"
        );
        assert_eq!(badge(&app, cx, ("tab-tx", 0)), None, "{engine:?}");
        app.open_transaction(cx, "select 1 as kerno_ui_k4_shown");
        assert_eq!(
            badge(&app, cx, ("tab-tx", 0)),
            Some(tip(Open, Duration::ZERO)),
            "{engine:?}"
        );
        assert!(ages_refreshing(&app, cx), "{engine:?}");

        // Another tab on screen: the first keeps its badge, the second has none, and the status
        // bar speaks of the second.
        app.press(cx, &["secondary-t"]);
        assert_eq!(app.active(cx), Some(1), "{engine:?}");
        later(&app, cx, 2 * 60 + 5);
        assert_eq!(
            badge(&app, cx, ("tab-tx", 0)),
            Some(tip(Open, Duration::from_secs(125))),
            "{engine:?}"
        );
        assert_eq!(badge(&app, cx, ("tab-tx", 1)), None, "{engine:?}");
        assert!(!app.status_says(cx, "transaction"), "{engine:?}");
        for layout in [
            &actions::TabsOnSeveralRows as &dyn gpui_kit::Action,
            &actions::TabsAtTheSide,
            &actions::TabsOnOneRow,
        ] {
            app.step(cx, |window, cx| {
                window.dispatch_action(layout.boxed_clone(), cx)
            });
            assert!(app.has(cx, ("tab-tx", 0usize)), "{engine:?}: {layout:?}");
            assert!(!app.has(cx, ("tab-tx", 1usize)), "{engine:?}: {layout:?}");
        }
        app.click(cx, "tabs-overflow");
        assert_eq!(
            badge(&app, cx, ("all-tabs-tx", 0)),
            Some(tip(Open, Duration::from_secs(125))),
            "{engine:?}"
        );
        assert_eq!(badge(&app, cx, ("all-tabs-tx", 1)), None, "{engine:?}");
        app.press(cx, &["escape"]);

        // Back on the first tab: its status bar says for how long.
        app.dispatch(cx, actions::PreviousTab);
        assert_eq!(app.active(cx), Some(0), "{engine:?}");
        assert!(
            app.status_bar(cx)
                .contains(&"transaction open · 2 min".to_string()),
            "{engine:?}: {:?}",
            app.status_bar(cx)
        );
        // Without any input, the timer draws the window again with the new age.
        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        assert_eq!(
            drawn(&app, cx, ("tab-tx", 0)),
            Some(tip(Open, Duration::from_secs(185))),
            "{engine:?}"
        );

        roll_back(&app, cx);
        assert_eq!(badge(&app, cx, ("tab-tx", 0)), None, "{engine:?}");
        assert!(
            !ages_refreshing(&app, cx),
            "{engine:?}: the timer went with it"
        );
        assert!(!app.status_says(cx, "transaction"), "{engine:?}");
    }
}

/// C15: the badge, the age and the timer go however the transaction ends: Commit, a `COMMIT` or
/// `ROLLBACK` typed after a `BEGIN` typed in auto-commit (the age starting at the `BEGIN`),
/// closing the tab, disconnecting.
#[gpui_kit::test]
fn the_transaction_badge_goes_when_it_ends(cx: &mut TestAppContext) {
    use kerno_core::TxState::{Idle, Open};
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let gone = |app: &Fixture, cx: &mut TestAppContext, how: &str| {
            assert_eq!(app.tx_state(cx), Idle, "{engine:?}: {how}");
            assert_eq!(badge(app, cx, ("tab-tx", 0)), None, "{engine:?}: {how}");
            assert!(!ages_refreshing(app, cx), "{engine:?}: {how}");
            assert!(!app.status_says(cx, "transaction"), "{engine:?}: {how}");
        };
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);

        // Commit.
        app.open_transaction(cx, "select 1 as kerno_ui_k4_commit");
        later(&app, cx, 30);
        app.dispatch(cx, actions::CommitTransaction);
        app.wait_until(cx, "the commit", |app, cx| app.tx_state(cx) == Idle);
        gone(&app, cx, "Commit");

        // Typed in auto-commit: `BEGIN` opens it, its age starts there; `ROLLBACK` ends it.
        let tab = app.tab(cx);
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(false, cx)));
        let begin = match engine {
            Engine::Postgres => "begin",
            Engine::MySql | Engine::MariaDb => "start transaction",
        };
        assert_eq!(k3_run(&app, cx, begin), ["done"], "{engine:?}");
        assert_eq!(app.tx_state(cx), Open, "{engine:?}");
        assert!(ages_refreshing(&app, cx), "{engine:?}");
        later(&app, cx, 42);
        assert!(
            app.status_bar(cx)
                .contains(&"transaction open · 42 s".to_string()),
            "{engine:?}: {:?}",
            app.status_bar(cx)
        );
        assert_eq!(k3_run(&app, cx, "select 1"), ["1 rows"], "{engine:?}");
        assert_eq!(
            badge(&app, cx, ("tab-tx", 0)),
            Some(tip(Open, Duration::from_secs(42))),
            "{engine:?}: a statement does not restart it"
        );
        assert_eq!(k3_run(&app, cx, "rollback"), ["done"], "{engine:?}");
        gone(&app, cx, "ROLLBACK typed");
        // The next one starts its own age.
        assert_eq!(k3_run(&app, cx, begin), ["done"], "{engine:?}");
        assert_eq!(
            badge(&app, cx, ("tab-tx", 0)),
            Some(tip(Open, Duration::ZERO)),
            "{engine:?}"
        );
        assert_eq!(k3_run(&app, cx, "commit"), ["done"], "{engine:?}");
        gone(&app, cx, "COMMIT typed");

        // Moved off its session by the setter itself (what follows the question of the picker
        // and of "Set as default", K5): the session is closed with its transaction, and the
        // next one is dated anew.
        app.open_transaction(cx, "select 1 as kerno_ui_k4_moved");
        later(&app, cx, 30);
        let tab = app.tab(cx);
        let target = cx.update(|cx| tab.read(cx).target().cloned());
        app.step(cx, |_, cx| {
            tab.update(cx, |tab, cx| tab.set_target(None, cx))
        });
        gone(&app, cx, "moved");
        app.step(cx, |_, cx| {
            tab.update(cx, |tab, cx| tab.set_target(target, cx))
        });
        app.open_transaction(cx, "select 1 as kerno_ui_k4_moved_again");
        assert_eq!(
            badge(&app, cx, ("tab-tx", 0)),
            Some(tip(Open, Duration::ZERO)),
            "{engine:?}"
        );
        roll_back(&app, cx);

        // Disconnecting asks, then rolls back.
        app.open_transaction(cx, "select 1 as kerno_ui_k4_disconnect");
        assert!(app.has(cx, ("tab-tx", 0usize)), "{engine:?}");
        app.dispatch(cx, actions::DisconnectTabConnection);
        app.click(cx, "end-rollback");
        app.wait_until(cx, "the disconnection", |app, cx| {
            app.status(cx, 0) == Status::Disconnected
        });
        gone(&app, cx, "disconnected");

        // Closing the tab asks, then rolls back: no timer is left for a tab that is gone.
        app.step(cx, |window, cx| {
            window.double_click(("connection", 0usize), cx)
        });
        app.wait_until(cx, "the connection", |app, cx| {
            matches!(app.status(cx, 0), Status::Connected { .. })
        });
        // A new tab, which has the keyboard (the sidebar took it).
        app.press(cx, &["secondary-t"]);
        app.open_transaction(cx, "select 1 as kerno_ui_k4_close");
        assert!(app.has(cx, ("tab-tx", 1usize)), "{engine:?}");
        assert!(ages_refreshing(&app, cx), "{engine:?}");
        app.press(cx, &["secondary-w"]);
        assert!(app.shows_label(cx, "Roll back and close"), "{engine:?}");
        app.click(cx, "ok");
        assert_eq!(app.titles(cx), ["Query 1"], "{engine:?}");
        assert!(!ages_refreshing(&app, cx), "{engine:?}: closed");
    }
}

/// C15: a transaction that ends in a tab not on screen (its script commits once the user went
/// to another tab) leaves the strip as it ends, without anything else drawing the window.
#[gpui_kit::test]
fn a_transaction_ended_behind_another_tab_leaves_the_strip(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        app.open_transaction(cx, "select 1 as kerno_ui_k4_behind");
        let first = app.tab(cx);
        let sleep = match engine {
            Engine::Postgres => "select pg_sleep(1) as kerno_ui_k4_behind;\ncommit",
            Engine::MySql | Engine::MariaDb => "select sleep(1) as kerno_ui_k4_behind;\ncommit",
        };
        app.press(cx, &["secondary-a", "backspace"]);
        app.type_text(cx, sleep);
        app.press(cx, &["secondary-shift-enter"]);
        app.press(cx, &["secondary-t"]);
        assert_eq!(app.active(cx), Some(1), "{engine:?}");
        assert!(drawn(&app, cx, ("tab-tx", 0)).is_some(), "{engine:?}");
        app.wait_until(cx, "the commit behind the other tab", |app, cx| {
            cx.update(|cx| first.read(cx).tx_state()) == kerno_core::TxState::Idle
                && drawn(app, cx, ("tab-tx", 0)).is_none()
        });
        assert!(!ages_refreshing(&app, cx), "{engine:?}");
    }
}

/// C15: a transaction is dated from its `BEGIN`, not from the end of the run that opened it: the
/// clock moved while a later statement ran counts. A `BEGIN` typed at the head of a script, and
/// the one manual commit sends before the first statement.
#[gpui_kit::test]
fn a_transaction_begun_in_a_script_is_dated_from_its_begin(cx: &mut TestAppContext) {
    use kerno_core::TxState::Open;
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        let (begin, sleep) = match engine {
            Engine::Postgres => ("begin", "select pg_sleep(2) as kerno_ui_k4_script"),
            Engine::MySql | Engine::MariaDb => {
                ("start transaction", "select sleep(2) as kerno_ui_k4_script")
            }
        };
        for manual in [false, true] {
            app.press(cx, &["secondary-t"]);
            let tab = app.tab(cx);
            cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(manual, cx)));
            let script = if manual {
                sleep.to_string()
            } else {
                format!("{begin};\n{sleep}")
            };
            app.type_text(cx, &script);
            app.press(cx, &["secondary-shift-enter"]);
            // The tab has dated the transaction, its last statement still runs.
            app.wait_until(cx, "the transaction dated, the run going on", |app, cx| {
                cx.update(|cx| tab.read(cx).transaction_age(cx)).is_some() && app.running(cx)
            });
            cx.executor().advance_clock(Duration::from_secs(20));
            app.wait_until(cx, "the end of the run", |app, cx| !app.running(cx));
            let index = app.active(cx).unwrap();
            assert_eq!(
                badge(&app, cx, ("tab-tx", index)),
                Some(tip(Open, Duration::from_secs(20))),
                "{engine:?}, manual commit: {manual}"
            );
            roll_back(&app, cx);
        }
    }
}

/// C15: a transaction a statement made fail (PostgreSQL) keeps its badge, which says so, and the
/// age of the transaction it was, back to a savepoint too; Roll back ends it.
#[gpui_kit::test]
fn a_failed_transaction_keeps_its_badge_and_its_age(cx: &mut TestAppContext) {
    use kerno_core::TxState::Failed;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    app.open_transaction(cx, "select 1 as kerno_ui_k4_failed");
    assert_eq!(k3_run(&app, cx, "savepoint kerno_ui_k4_sp"), ["done"]);
    later(&app, cx, 90);
    assert_eq!(k3_run(&app, cx, "select 1/0"), ["failed [22012]"]);
    assert_eq!(app.tx_state(cx), Failed);
    assert_eq!(
        badge(&app, cx, ("tab-tx", 0)),
        Some(tip(Failed, Duration::from_secs(90)))
    );
    assert!(
        app.status_bar(cx)
            .contains(&"transaction failed: roll back · 1 min".to_string()),
        "{:?}",
        app.status_bar(cx)
    );
    // Back to the savepoint: open again, the same transaction, the same age.
    assert_eq!(
        k3_run(&app, cx, "rollback to savepoint kerno_ui_k4_sp"),
        ["done"]
    );
    assert_eq!(app.tx_state(cx), kerno_core::TxState::Open);
    assert_eq!(
        badge(&app, cx, ("tab-tx", 0)),
        Some(tip(kerno_core::TxState::Open, Duration::from_secs(90)))
    );
    roll_back(&app, cx);
    assert_eq!(badge(&app, cx, ("tab-tx", 0)), None);
    assert!(!ages_refreshing(&app, cx));
}

/// C15: a connection lost under a tab's transaction (the server rolled it back) takes its badge
/// away, as soon as the tab learns it.
#[gpui_kit::test]
fn a_lost_connection_takes_the_badge_away(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        let own = match engine {
            Engine::Postgres => "select pg_backend_pid()::text",
            Engine::MySql | Engine::MariaDb => "select connection_id()",
        };
        app.open_transaction(cx, own);
        let id = app.result(cx, 0, 1).1[0][0].clone().unwrap();
        assert!(app.has(cx, ("tab-tx", 0usize)), "{engine:?}");
        on_server(
            engine,
            &match engine {
                Engine::Postgres => format!("SELECT pg_terminate_backend({id})"),
                Engine::MySql | Engine::MariaDb => format!("KILL {id}"),
            },
        );
        k3_run(&app, cx, "select 1");
        app.wait_until(cx, "the transaction to be reported lost", |app, cx| {
            app.tx_state(cx) == kerno_core::TxState::Idle && !app.has(cx, ("tab-tx", 0usize))
        });
        assert!(!ages_refreshing(&app, cx), "{engine:?}");
        // The connection comes back; the tab's next transaction starts its own age.
        app.wait_until(cx, "the connection to come back", |app, cx| {
            cx.executor().advance_clock(Duration::from_millis(500));
            matches!(app.status(cx, 0), Status::Connected { .. })
        });
        later(&app, cx, 30);
        app.open_transaction(cx, "select 1 as kerno_ui_k4_lost");
        assert_eq!(
            badge(&app, cx, ("tab-tx", 0)),
            Some(tip(kerno_core::TxState::Open, Duration::ZERO)),
            "{engine:?}"
        );
        roll_back(&app, cx);
    }
}

/// C15: the environment's "close idle transactions" rule rolls back a transaction of a tab not
/// on screen: its badge goes from the strip at once, and the timer with it.
#[gpui_kit::test]
fn the_idle_rule_takes_the_badge_off_a_tab_not_shown(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let _scratch = quit_table(engine, "kerno_ui_k4_idle");
        let config = ConnectionConfig {
            environment: kerno_core::environment::PRODUCTION.into(),
            ..testing::config(engine)
        };
        let app = connected_as(cx, config);
        app.press(cx, &["secondary-t"]);
        app.type_text(cx, "insert into kerno_ui_k4_idle.t values (1)");
        app.press(cx, &["secondary-enter"]);
        // Production asks twice before a write.
        app.click(cx, "sql-confirm");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the insert", |app, cx| {
            app.outcomes(cx) == ["done, 1 affected"] && !app.running(cx)
        });
        assert!(app.has(cx, ("tab-tx", 0usize)), "{engine:?}");
        app.press(cx, &["secondary-t"]);
        later(&app, cx, 590);
        assert!(app.has(cx, ("tab-tx", 0usize)), "{engine:?}");
        later(&app, cx, 11);
        app.wait_until(cx, "the idle rollback", |app, cx| {
            !app.has(cx, ("tab-tx", 0usize))
        });
        assert_eq!(app.active(cx), Some(1), "{engine:?}: never shown");
        assert!(!ages_refreshing(&app, cx), "{engine:?}");
        assert_eq!(
            on_server(engine, "SELECT count(*) FROM kerno_ui_k4_idle.t"),
            "0",
            "{engine:?}"
        );
    }
}

/// C15: the badge and the unsaved mark of one tab are two things, and the badge never pushes
/// the close button away nor over the title's row, in each layout.
#[gpui_kit::test]
fn the_badge_and_the_unsaved_mark_stay_apart_from_the_close_button(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    cx.update(|cx| {
        tab.update(cx, |tab, cx| {
            tab.set_file("/kerno_ui_k4_a_long_file_name.sql".into(), true, cx)
        })
    });
    app.open_transaction(cx, "select 1 as kerno_ui_k4_unsaved");
    app.press(cx, &["secondary-t"]);
    let labels = cx.update(|cx| app.workspace.read(cx).tab_labels(cx));
    assert_eq!(labels[0], "kerno_ui_k4_a_long_file_name.sql •");
    for layout in [
        &actions::TabsOnOneRow as &dyn gpui_kit::Action,
        &actions::TabsOnSeveralRows,
        &actions::TabsAtTheSide,
    ] {
        app.step(cx, |window, cx| {
            window.dispatch_action(layout.boxed_clone(), cx)
        });
        let (title, badge, close) = app.step(cx, |window, _| {
            (
                window.find(("tab-title", 0usize)).bounds(),
                window.find(("tab-tx", 0usize)).bounds(),
                window.find(("close-tab", 0usize)).bounds(),
            )
        });
        assert!(
            badge.size.width > gpui_kit::px(0.) && badge.left() >= title.left(),
            "{layout:?}: {badge:?} in {title:?}"
        );
        assert!(
            badge.right() <= close.left(),
            "{layout:?}: the badge {badge:?} before the close button {close:?}"
        );
        // Centred on the title's line.
        assert!(
            badge.top() >= title.top() && badge.bottom() <= title.bottom(),
            "{layout:?}: {badge:?} in {title:?}"
        );
    }
    // At the side the title is cut, the badge and the close button stay in the row.
    let (row, badge, close) = app.step(cx, |window, _| {
        (
            window.find(("tab-row", 0usize)).bounds(),
            window.find(("tab-tx", 0usize)).bounds(),
            window.find(("close-tab", 0usize)).bounds(),
        )
    });
    assert!(badge.right() <= row.right() && close.right() <= row.right());
    app.dispatch(cx, actions::TabsOnOneRow);
    app.dispatch(cx, actions::PreviousTab);
    roll_back(&app, cx);
}

/// C15: tabs restored after a restart have no session: no badge, no timer, even on a connected
/// connection.
#[gpui_kit::test]
fn restored_tabs_show_no_transaction(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let storage = memory_storage();
    let id = storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let tabs: Vec<TabSnapshot> = (0..2)
        .map(|index| TabSnapshot::Query {
            title: format!("Query {}", index + 1),
            sql: "select 1 as kerno_ui_k4_restored".into(),
            target: Some(Target {
                connection: id.clone(),
                database: None,
                schema: None,
            }),
            file: None,
            unsaved: false,
            source: None,
        })
        .collect();
    storage.save_tabs(&tabs).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    assert_eq!(app.titles(cx), ["Query 1", "Query 2"]);
    for index in 0..2usize {
        assert!(!app.has(cx, ("tab-tx", index)));
    }
    assert!(!ages_refreshing(&app, cx));
    assert!(!app.status_says(cx, "transaction"));
}

// K5: switching to auto-commit or "Set as default" ends the transaction with a question (C15) --

/// Runs `sql` in the active tab, in place of its text, answering the environment's
/// confirmations (production asks before every execution, twice before a write), and gives
/// what it produced.
fn k5_run(app: &Fixture, cx: &mut TestAppContext, sql: &str) -> Vec<String> {
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, sql);
    app.press(cx, &["secondary-enter"]);
    while app.has(cx, "sql-confirm") {
        app.click(cx, "sql-confirm");
    }
    app.wait_until(cx, sql, |app, cx| {
        !app.running(cx) && !app.outcomes(cx).is_empty()
    });
    app.outcomes(cx)
}

fn manual_commit(app: &Fixture, cx: &mut TestAppContext) -> bool {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).manual_commit())
}

/// What the engine's `BEGIN` is.
fn k5_begin(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "begin",
        Engine::MySql | Engine::MariaDb => "start transaction",
    }
}

/// C15: switching a tab to auto-commit with its transaction open asks how to end it (the
/// palette's command, its key and the main toolbar's menu, and the tab's own menu): Enter and
/// Cancel keep the mode and the transaction; Commit commits, then switches, and the next
/// statement is committed as it runs.
#[gpui_kit::test]
fn switching_to_auto_commit_with_a_transaction_asks(cx: &mut TestAppContext) {
    use kerno_core::TxState::{Idle, Open};
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let _scratch = quit_table(engine, "kerno_ui_k5_auto");
        let count = || on_server(engine, "SELECT count(*) FROM kerno_ui_k5_auto.t");
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        app.open_transaction(cx, "insert into kerno_ui_k5_auto.t values (1)");

        app.dispatch(cx, actions::ToggleCommitMode);
        for line in [
            "Switch “Query 1” to auto-commit?",
            "“Query 1”: a transaction is open.",
        ] {
            assert!(app.shows_label(cx, line), "{engine:?}: {line}");
        }
        app.press(cx, &["enter"]);
        assert!(
            app.has(cx, "end-question"),
            "{engine:?}: Enter never answers"
        );
        app.click(cx, "end-cancel");
        assert!(!app.dialog_open(cx), "{engine:?}");
        assert!(manual_commit(&app, cx), "{engine:?}: the mode is kept");
        assert_eq!(app.tx_state(cx), Open, "{engine:?}: the transaction too");
        assert_eq!(count(), "0", "{engine:?}");

        // The tab's own menu goes the same way.
        let tab = app.tab(cx);
        app.step(cx, |_, cx| {
            tab.update(cx, |tab, cx| tab.request_manual_commit(false, cx))
        });
        assert!(app.has(cx, "end-question"), "{engine:?}: the tab's menu");
        app.click(cx, "end-commit");
        app.wait_until(cx, "the commit, then the switch", |app, cx| {
            app.tx_state(cx) == Idle && !manual_commit(app, cx)
        });
        assert_eq!(count(), "1", "{engine:?}");
        // Auto-commit from now on: the next statement is committed as it runs.
        assert_eq!(
            k5_run(&app, cx, "insert into kerno_ui_k5_auto.t values (2)"),
            ["done, 1 affected"],
            "{engine:?}"
        );
        assert_eq!(app.tx_state(cx), Idle, "{engine:?}");
        assert_eq!(count(), "2", "{engine:?}");
        // Without a transaction, no question.
        app.dispatch(cx, actions::ToggleCommitMode);
        assert!(manual_commit(&app, cx), "{engine:?}");
        app.dispatch(cx, actions::ToggleCommitMode);
        assert!(!app.dialog_open(cx), "{engine:?}");
        assert!(!manual_commit(&app, cx), "{engine:?}");
    }
}

/// C15: switching to auto-commit ends the transaction only. The session stays, so the edits
/// waiting in a result are neither asked about nor discarded.
#[gpui_kit::test]
fn switching_to_auto_commit_keeps_the_edits_waiting_in_a_result(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = g8_table(Engine::Postgres, "kerno_ui_k5_edits");
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    app.open_transaction(
        cx,
        "select id, name, qty from kerno_ui_k5_edits.t order by id",
    );
    assert_eq!(app.result_verdict(cx), None);
    app.type_in_result(cx, 0, 2, "uno");
    assert_eq!(app.result_pending(cx), 1);

    app.dispatch(cx, actions::ToggleCommitMode);
    assert!(app.shows_label(cx, "“Query 1”: a transaction is open."));
    let waiting = "“Query 1”: 1 pending change is discarded.";
    assert!(!app.shows_label(cx, waiting), "only the transaction");
    app.click(cx, "end-rollback");
    app.wait_until(cx, "the rollback, then the switch", |app, cx| {
        app.tx_state(cx) == kerno_core::TxState::Idle && !manual_commit(app, cx)
    });
    assert_eq!(app.result_pending(cx), 1, "the edit is still waiting");
}

/// C15: a failed transaction can only be rolled back: the question offers no Commit that
/// works, and Roll back ends it, then switches.
#[gpui_kit::test]
fn switching_to_auto_commit_with_a_failed_transaction_only_rolls_back(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    app.open_transaction(cx, "select 1 as kerno_ui_k5_failed");
    assert_eq!(k5_run(&app, cx, "select 1/0"), ["failed [22012]"]);
    assert_eq!(app.tx_state(cx), kerno_core::TxState::Failed);
    app.dispatch(cx, actions::ToggleCommitMode);
    assert!(app.shows_label(
        cx,
        "“Query 1”: a statement failed in its transaction, which can only be rolled back."
    ));
    // Commit is greyed: a click does nothing.
    app.click(cx, "end-commit");
    assert!(app.has(cx, "end-question"));
    assert!(manual_commit(&app, cx));
    app.click(cx, "end-rollback");
    app.wait_until(cx, "the rollback, then the switch", |app, cx| {
        app.tx_state(cx) == kerno_core::TxState::Idle && !manual_commit(app, cx)
    });
}

/// C15: a transaction begun by a typed `BEGIN` in auto-commit is not lost by switching to manual
/// commit: no question, the transaction, its badge and its log go on.
#[gpui_kit::test]
fn switching_to_manual_commit_keeps_a_typed_begin(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let _scratch = quit_table(engine, "kerno_ui_k5_manual");
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        let tab = app.tab(cx);
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(false, cx)));
        assert_eq!(k5_run(&app, cx, k5_begin(engine)), ["done"], "{engine:?}");
        let insert = "insert into kerno_ui_k5_manual.t values (1)";
        assert_eq!(k5_run(&app, cx, insert), ["done, 1 affected"], "{engine:?}");

        app.dispatch(cx, actions::ToggleCommitMode);
        assert!(!app.dialog_open(cx), "{engine:?}");
        assert!(manual_commit(&app, cx), "{engine:?}");
        assert_eq!(app.tx_state(cx), kerno_core::TxState::Open, "{engine:?}");
        assert!(app.has(cx, ("tab-tx", 0usize)), "{engine:?}");
        assert!(app.status_says(cx, "transaction open"), "{engine:?}");
        let log = cx.update(|cx| {
            tab.read(cx)
                .transaction_log()
                .iter()
                .map(|entry| entry.sql.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(log, [k5_begin(engine), insert], "{engine:?}");
        assert_eq!(
            on_server(engine, "SELECT count(*) FROM kerno_ui_k5_manual.t"),
            "0",
            "{engine:?}"
        );
        roll_back(&app, cx);
    }
}

/// C15: the schema tree's "Set as default" moves the active tab as the toolbar's picker does:
/// at once without a transaction; with one, after the question (Cancel keeps the place and
/// the transaction; Commit commits, then moves).
#[gpui_kit::test]
fn set_as_default_asks_before_rolling_back(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let (first, second) = ("kerno_ui_k5_default", "kerno_ui_k5_default_b");
        let (drop_first, create_first) = scratch_schema(engine, first);
        let (drop_second, create_second) = scratch_schema(engine, second);
        let _scratch = Scratch::new(
            engine,
            &[&drop_first, &drop_second],
            &[
                &create_first,
                &create_second,
                "CREATE TABLE kerno_ui_k5_default.t (id INT PRIMARY KEY)",
            ],
        );
        let count = || on_server(engine, "SELECT count(*) FROM kerno_ui_k5_default.t");
        let (path, other, kind): (Vec<&str>, Vec<&str>, &str) = match engine {
            Engine::Postgres => (vec!["kerno", first], vec!["kerno", second], "schema"),
            Engine::MySql | Engine::MariaDb => (vec![first], vec![second], "database"),
        };
        let place = |app: &Fixture, cx: &mut TestAppContext| {
            let tab = app.tab(cx);
            cx.update(|cx| {
                tab.read(cx).target().and_then(|target| match engine {
                    Engine::Postgres => target.schema.clone(),
                    Engine::MySql | Engine::MariaDb => target.database.clone(),
                })
            })
        };
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        app.open_path(cx, &other);
        app.open_path(cx, &path);

        // No transaction: no question, the tab moves.
        app.act(cx, Action::SetDefault, &path);
        assert!(!app.dialog_open(cx), "{engine:?}");
        assert_eq!(place(&app, cx).as_deref(), Some(first), "{engine:?}");

        app.open_transaction(cx, "insert into kerno_ui_k5_default.t values (1)");
        app.act(cx, Action::SetDefault, &other);
        for line in [
            format!("Switch “Query 1” to the {kind} “{second}”?"),
            "“Query 1”: a transaction is open.".to_string(),
        ] {
            assert!(app.shows_label(cx, &line), "{engine:?}: {line}");
        }
        app.click(cx, "end-cancel");
        assert_eq!(place(&app, cx).as_deref(), Some(first), "{engine:?}");
        assert_eq!(
            app.tx_state(cx),
            kerno_core::TxState::Open,
            "{engine:?}: kept"
        );

        app.act(cx, Action::SetDefault, &other);
        app.click(cx, "end-commit");
        app.wait_until(cx, "the commit, then the move", |app, cx| {
            app.tx_state(cx) == kerno_core::TxState::Idle
                && place(app, cx).as_deref() == Some(second)
        });
        assert_eq!(count(), "1", "{engine:?}");
    }
}

/// C15: the environment's idle rule watches a transaction begun by a `BEGIN` typed in an
/// auto-commit tab, as it does in manual commit: one that only read is left alone, one that
/// wrote is rolled back once idle for the period (production: 10 minutes), on the simulated
/// clock.
#[gpui_kit::test]
fn an_explicit_begin_in_auto_commit_is_rolled_back_when_idle(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let _scratch = quit_table(engine, "kerno_ui_k5_idle");
        let config = ConnectionConfig {
            environment: kerno_core::environment::PRODUCTION.into(),
            ..testing::config(engine)
        };
        let app = connected_as(cx, config);
        app.press(cx, &["secondary-t"]);
        let tab = app.tab(cx);
        // Production starts in manual commit.
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(false, cx)));
        let rolled_back = |cx: &mut TestAppContext| {
            cx.update(|cx| tab.read(cx).messages())
                .iter()
                .any(|message| message.contains("left idle for 10 minutes: it was rolled back"))
        };

        // One that only read is left alone.
        assert_eq!(k5_run(&app, cx, k5_begin(engine)), ["done"], "{engine:?}");
        assert_eq!(
            k5_run(&app, cx, "select 1 as kerno_ui_k5_read"),
            ["1 rows"],
            "{engine:?}"
        );
        later(&app, cx, 601);
        assert_eq!(app.tx_state(cx), kerno_core::TxState::Open, "{engine:?}");
        assert!(!rolled_back(cx), "{engine:?}");

        // One that wrote is rolled back once idle for the period.
        assert_eq!(
            k5_run(&app, cx, "insert into kerno_ui_k5_idle.t values (1)"),
            ["done, 1 affected"],
            "{engine:?}"
        );
        later(&app, cx, 590);
        assert_eq!(app.tx_state(cx), kerno_core::TxState::Open, "{engine:?}");
        later(&app, cx, 11);
        app.wait_until(cx, "the idle rollback", |app, cx| {
            app.tx_state(cx) == kerno_core::TxState::Idle && rolled_back(cx)
        });
        assert!(!app.has(cx, ("tab-tx", 0usize)), "{engine:?}");
        assert!(!manual_commit(&app, cx), "{engine:?}: still auto-commit");
        assert_eq!(
            on_server(engine, "SELECT count(*) FROM kerno_ui_k5_idle.t"),
            "0",
            "{engine:?}"
        );
    }
}

/// C15: a transaction begun by a `BEGIN` typed in auto-commit has a log of its own: nothing of
/// one that ended out of sight (its connection closed under it, with no statement of the tab
/// to see it) stays in it, so the idle rule never counts an earlier transaction's writes.
#[gpui_kit::test]
fn a_typed_begin_after_a_closed_connection_starts_its_own_log(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let _scratch = quit_table(engine, "kerno_ui_k5_lost");
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        let tab = app.tab(cx);
        cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(false, cx)));
        let log = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                tab.read(cx)
                    .transaction_log()
                    .iter()
                    .map(|entry| entry.sql.clone())
                    .collect::<Vec<_>>()
            })
        };
        let begin = k5_begin(engine);
        assert_eq!(k5_run(&app, cx, begin), ["done"], "{engine:?}");
        let insert = "insert into kerno_ui_k5_lost.t values (1)";
        assert_eq!(k5_run(&app, cx, insert), ["done, 1 affected"], "{engine:?}");
        assert_eq!(log(cx), [begin, insert], "{engine:?}");
        // Closed by something that could not ask (a lost connection does the same).
        let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
        app.step(cx, |_, cx| {
            app.connections
                .update(cx, |connections, cx| connections.disconnect(&id, cx))
        });
        app.wait_until(cx, "the transaction to be reported lost", |app, cx| {
            app.tx_state(cx) == kerno_core::TxState::Idle
                && app.status(cx, 0) == Status::Disconnected
        });
        app.step(cx, |_, cx| {
            app.connections
                .update(cx, |connections, cx| connections.connect(&id, cx))
        });
        app.wait_until(cx, "the connection", |app, cx| {
            matches!(app.status(cx, 0), Status::Connected { .. })
        });
        assert_eq!(k5_run(&app, cx, begin), ["done"], "{engine:?}");
        let read = "select 1 as kerno_ui_k5_lost";
        assert_eq!(k5_run(&app, cx, read), ["1 rows"], "{engine:?}");
        assert_eq!(log(cx), [begin, read], "{engine:?}");
        roll_back(&app, cx);
    }
}

// K6: a script file never binds to a non-local connection silently; a deleted connection's tabs
// (C6).

/// Two saved connections to `engine`, named and in environments as given (the sidebar lists
/// them in this order), with their passwords; none connected. Their ids, in that order.
fn k6_app(
    cx: &mut TestAppContext,
    engine: Engine,
    connections: [(&str, &str); 2],
) -> (Fixture, [String; 2]) {
    let storage = memory_storage();
    let ids = connections.map(|(name, environment)| {
        let config = ConnectionConfig {
            name: name.into(),
            environment: environment.into(),
            ..testing::config(engine)
        };
        storage.save_connection(None, &config).unwrap()
    });
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    for id in &ids {
        kerno_core::secrets::save_secrets(secrets.as_ref(), id, &testing::secrets()).unwrap();
    }
    (app, ids)
}

/// Connects the connection listed at `index` (it becomes the one the navigator browses).
fn k6_connect(app: &Fixture, cx: &mut TestAppContext, index: usize) {
    app.step(cx, |window, cx| {
        window.double_click(("connection", index), cx)
    });
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, index), Status::Connected { .. })
    });
}

/// Makes `id` the connection the navigator browses.
fn k6_browse(app: &Fixture, cx: &mut TestAppContext, id: &str) {
    let navigator = cx.update(|cx| app.workspace.read(cx).navigator(cx));
    cx.update(|cx| navigator.update(cx, |navigator, cx| navigator.select_connection(id, cx)));
}

/// Opens the file at `path` as File ▸ Open does, and returns its tab.
fn k6_open_file(
    app: &Fixture,
    cx: &mut TestAppContext,
    path: &Path,
) -> Entity<crate::query::tab::QueryTab> {
    let opening = path.to_path_buf();
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_sql_file_from(opening, window, cx)
        })
    });
    app.wait_until(cx, "the file's tab", |app, cx| {
        let file = cx.update(|cx| {
            (app.workspace.read(cx).active_query_tab())
                .and_then(|tab| tab.read(cx).file().map(Path::to_path_buf))
        });
        file.as_deref() == Some(path)
    });
    app.tab(cx)
}

fn k6_connection(
    tab: &Entity<crate::query::tab::QueryTab>,
    cx: &mut TestAppContext,
) -> Option<String> {
    cx.update(|cx| tab.read(cx).connection_id().map(str::to_string))
}

fn k6_ran_on(cx: &mut TestAppContext, path: &Path) -> Option<String> {
    cx.update(|cx| {
        AppState::get(cx)
            .settings
            .script_connections
            .get(path)
            .cloned()
    })
}

/// The vertical place of the entry of the open menu with this label.
fn k6_menu_y(app: &Fixture, cx: &mut TestAppContext, label: &str) -> gpui_kit::Pixels {
    menu_entry(app, cx, label)
        .unwrap_or_else(|| panic!("no menu entry {label:?}"))
        .y
}

/// C6: a `.sql` file opened beside a production connection opens unbound: its bar says so, Run,
/// Run all and Explain run nothing and point at it, and its picker lists that connection first.
/// Picked, the tab runs there with production's own confirmations; opened again beside a local
/// connection, the file proposes the one it ran on first, still without binding.
#[gpui_kit::test]
fn a_script_opened_beside_a_production_connection_is_not_bound(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let name = "kerno_ui_k6_prod";
    let _scratch = quit_table(engine, name);
    let (app, [local, production]) = k6_app(
        cx,
        engine,
        [
            ("kerno_ui_k6 a local", kerno_core::environment::LOCAL),
            (
                "kerno_ui_k6 b production",
                kerno_core::environment::PRODUCTION,
            ),
        ],
    );
    k6_connect(&app, cx, 1);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kerno_ui_k6_prod.sql");
    std::fs::write(&path, format!("insert into {name}.t values (1)")).unwrap();

    let tab = k6_open_file(&app, cx, &path);
    assert_eq!(k6_connection(&tab, cx), None);
    assert_eq!(
        cx.update(|cx| tab.read(cx).proposed().map(str::to_string)),
        Some(production.clone())
    );
    assert!(app.shows_label(cx, "Pick a connection to run this script."));
    assert_eq!(k6_ran_on(cx, &path), None);
    app.press(cx, &["secondary-enter", "secondary-shift-enter"]);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| tab.explain(false, window, cx))
    });
    assert!(
        !app.dialog_open(cx),
        "no confirmation: nothing was about to run"
    );
    assert_eq!(app.outcomes(cx), Vec::<String>::new());
    assert_eq!(
        cx.update(|cx| tab.read(cx).messages()),
        ["Warning: Nothing ran. Pick a connection to run this script."]
    );
    assert_eq!(
        on_server(engine, &format!("SELECT count(*) FROM {name}.t")),
        "0"
    );

    // The picker is one click away, the proposed connection first (it is second in the list).
    app.click(cx, "unbound-pick");
    assert!(
        k6_menu_y(&app, cx, "kerno_ui_k6 b production")
            < k6_menu_y(&app, cx, "kerno_ui_k6 a local (not connected)")
    );
    click_menu_entry(&app, cx, "kerno_ui_k6 b production");
    assert_eq!(k6_connection(&tab, cx), Some(production.clone()));
    assert!(!app.has(cx, "unbound-bar"));
    assert_eq!(k6_ran_on(cx, &path), Some(production.clone()));

    // Production's own confirmations apply to it, as to any tab of it.
    app.press(cx, &["secondary-enter"]);
    app.click(cx, "sql-confirm");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the insert", |app, cx| {
        app.outcomes(cx) == ["done, 1 affected"] && !app.running(cx)
    });
    app.dispatch(cx, actions::CommitTransaction);
    app.wait_until(cx, "the commit", |app, cx| {
        app.tx_state(cx) == kerno_core::TxState::Idle
    });
    assert_eq!(
        on_server(engine, &format!("SELECT count(*) FROM {name}.t")),
        "1"
    );

    // Opened again while a local connection is browsed: production is proposed first, the
    // tab is not bound, to it or to the local one.
    app.press(cx, &["secondary-w"]);
    assert!(app.titles(cx).is_empty());
    k6_connect(&app, cx, 0);
    k6_browse(&app, cx, &local);
    let tab = k6_open_file(&app, cx, &path);
    assert_eq!(k6_connection(&tab, cx), None);
    assert_eq!(
        cx.update(|cx| tab.read(cx).proposed().map(str::to_string)),
        Some(production)
    );
    assert!(app.has(cx, "unbound-bar"));
    app.click(cx, "tab-connection");
    assert!(
        k6_menu_y(&app, cx, "kerno_ui_k6 b production")
            < k6_menu_y(&app, cx, "kerno_ui_k6 a local")
    );
    app.press(cx, &["escape"]);

    // A connection that is not open can be picked (D-078): the first run opens it.
    app.step(cx, |_, cx| {
        app.connections
            .update(cx, |connections, cx| connections.disconnect(&local, cx))
    });
    app.wait_until(cx, "the local connection closed", |app, cx| {
        app.status(cx, 0) == Status::Disconnected
    });
    app.click(cx, "unbound-pick");
    click_menu_entry(&app, cx, "kerno_ui_k6 a local (not connected)");
    assert_eq!(k6_connection(&tab, cx), Some(local.clone()));
    assert!(!app.has(cx, "unbound-bar"));
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "select 1 as kerno_ui_k6_picked");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the row from the local connection", |app, cx| {
        app.outcomes(cx) == ["1 rows"] && !app.running(cx)
    });
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
}

/// C6: beside a local connection a file opens bound to it, as before, and remembers it. The rules
/// decide, not the environment's name: an environment of the user's that confirms writes twice
/// opens the file unbound; the same environment without that rule binds it again.
#[gpui_kit::test]
fn a_script_opened_beside_a_local_connection_is_bound(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let app = connected(cx, engine);
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, "select 1 as kerno_ui_k6_local").unwrap();
        path
    };

    let first = file("kerno_ui_k6_a.sql");
    let tab = k6_open_file(&app, cx, &first);
    assert_eq!(k6_connection(&tab, cx), Some(id.clone()));
    assert!(!app.has(cx, "unbound-bar"));
    assert_eq!(k6_ran_on(cx, &first), Some(id.clone()));
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);

    // An environment of the user's, named nothing like production.
    let own = "kerno-ui-k6-careful";
    let rules = |careful: bool| kerno_core::environment::EnvironmentRules {
        confirm_writes_twice: careful,
        ..Default::default()
    };
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            settings
                .environments
                .push(kerno_core::environment::EnvironmentDef {
                    id: own.into(),
                    name: "Careful".into(),
                    color_light: None,
                    color_dark: None,
                    tint: kerno_core::environment::Tint::None,
                    rules: rules(true),
                })
        })
    });
    let config = ConnectionConfig {
        environment: own.into(),
        ..app.first_stored()
    };
    let saving = (id.clone(), config);
    cx.update(|cx| {
        app.connections.update(cx, |connections, cx| {
            connections.save(
                Some(saving.0),
                saving.1,
                testing::secrets(),
                None,
                false,
                cx,
            )
        })
    });
    app.wait_until(cx, "the new environment", |app, cx| {
        cx.update(|cx| app.connections.read(cx).saved()[0].config.environment == own)
    });
    let second = file("kerno_ui_k6_b.sql");
    let tab = k6_open_file(&app, cx, &second);
    assert_eq!(k6_connection(&tab, cx), None);
    assert!(app.has(cx, "unbound-bar"));

    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            for environment in &mut settings.environments {
                if environment.id == own {
                    environment.rules = rules(false);
                }
            }
        })
    });
    let third = file("kerno_ui_k6_c.sql");
    let tab = k6_open_file(&app, cx, &third);
    assert_eq!(k6_connection(&tab, cx), Some(id));
    assert!(!app.has(cx, "unbound-bar"));
}

/// C6: deleting a connection asks once about everything its tabs would lose (a transaction,
/// pending edits, a table form's changes); Cancel deletes nothing. Confirmed, its query tabs stay,
/// unbound, with their text, file and unsaved mark, a running statement stopped on the server, and
/// say why; its data, structure and form tabs close; the other connection's tab keeps its own;
/// nothing is bound in their place; a restart finds the same tabs.
#[gpui_kit::test]
fn deleting_a_connection_unbinds_its_query_tabs_and_closes_the_others(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k6_delete";
        let marker = "kerno_ui_k6_delete_running";
        let _scratch = k2_table(engine, name);
        let (app, [deleted, kept]) = k6_app(
            cx,
            engine,
            [
                ("kerno_ui_k6 a deleted", kerno_core::environment::LOCAL),
                ("kerno_ui_k6 b kept", kerno_core::environment::LOCAL),
            ],
        );
        k6_connect(&app, cx, 1);
        k6_connect(&app, cx, 0);
        k6_browse(&app, cx, &deleted);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kerno_ui_k6_delete.sql");
        std::fs::write(&path, "select 1").unwrap();

        // On the connection deleted: a file's tab with unsaved text and a transaction, a tab
        // running a statement, a table's rows with a change not applied, its structure, a form.
        let script = k6_open_file(&app, cx, &path);
        let insert = format!("insert into {name}.t values (2, 'two')");
        app.open_transaction(cx, &insert);
        assert!(cx.update(|cx| script.read(cx).unsaved()), "{engine:?}");
        app.press(cx, &["secondary-t"]);
        let running = app.tab(cx);
        let sleep = match engine {
            Engine::Postgres => format!("select '{marker}', pg_sleep(60)"),
            Engine::MySql | Engine::MariaDb => format!("select '{marker}', sleep(60)"),
        };
        app.type_text(cx, &sleep);
        app.press(cx, &["secondary-enter"]);
        app.wait_until(cx, "the statement on the server", |_, _| {
            running_on_server(engine, marker)
        });
        app.open_table(cx, name, "t", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–1 of 1");
        app.type_in_cell(cx, 0, 2, "after", "enter");
        assert_eq!(app.pending(cx), 1, "{engine:?}");
        app.open_structure(cx, name, "t", RelationKind::Table);
        k2_alter_behind_a_lock(&app, cx, engine, name);
        // A tab of it closed before: it can be reopened, unbound.
        app.press(cx, &["secondary-t"]);
        app.type_text(cx, "select 'kerno_ui_k6_closed'");
        app.press(cx, &["secondary-w"]);
        // On the other one: a tab, active.
        k6_browse(&app, cx, &kept);
        app.press(cx, &["secondary-t"]);
        let other = app.tab(cx);
        assert_eq!(k6_connection(&other, cx), Some(kept.clone()), "{engine:?}");
        assert_eq!(app.titles(cx).len(), 6, "{engine:?}");

        let delete = |app: &Fixture, cx: &mut TestAppContext| {
            app.right_click(cx, ("connection", 0usize), None);
            app.click_label(cx, "Delete…");
            app.click(cx, "ok");
        };
        delete(&app, cx);
        assert!(app.has(cx, "end-question"), "{engine:?}");
        assert!(
            app.shows_label(cx, "Delete “kerno_ui_k6 a deleted”?"),
            "{engine:?}"
        );
        let mut lines = Vec::new();
        for index in 0..8usize {
            if app.has(cx, ("end-line", index)) {
                lines.push(app.label_of(cx, ("end-line", index)));
            }
        }
        for line in [
            "“kerno_ui_k6_delete.sql”: a transaction is open.",
            "1 pending change is discarded.",
            "the changes not applied to the table are lost.",
        ] {
            assert!(
                lines.iter().any(|shown| shown.contains(line)),
                "{engine:?}: {line:?} in {lines:?}"
            );
        }
        app.click(cx, "end-cancel");
        assert!(!app.dialog_open(cx), "{engine:?}");
        assert_eq!(app.storage.connections().unwrap().len(), 2, "{engine:?}");
        assert_eq!(app.titles(cx).len(), 6, "{engine:?}");
        assert_eq!(
            k6_connection(&script, cx),
            Some(deleted.clone()),
            "{engine:?}"
        );
        assert_eq!(
            cx.update(|cx| script.read(cx).tx_state()),
            kerno_core::TxState::Open,
            "{engine:?}"
        );

        delete(&app, cx);
        app.click(cx, "end-rollback");
        app.wait_until(cx, "the deletion", |app, cx| {
            cx.update(|cx| app.connections.read(cx).get(&deleted).is_none())
        });
        let titles = app.titles(cx);
        assert_eq!(titles.len(), 3, "{engine:?}: {titles:?}");
        assert_eq!(
            app.active(cx),
            Some(2),
            "{engine:?}: the active tab stays active"
        );
        for tab in [&script, &running] {
            assert_eq!(
                k6_connection(tab, cx),
                None,
                "{engine:?}: nothing bound in its place"
            );
        }
        assert_eq!(k6_connection(&other, cx), Some(kept.clone()), "{engine:?}");
        let (file, unsaved, sql) = cx.update(|cx| {
            let tab = script.read(cx);
            (
                tab.file().map(Path::to_path_buf),
                tab.unsaved(),
                tab.sql(cx),
            )
        });
        assert_eq!(file.as_deref(), Some(path.as_path()), "{engine:?}");
        assert!(unsaved, "{engine:?}");
        assert_eq!(sql, insert, "{engine:?}");
        app.wait_until(cx, "the statement stopped on the server", |_, _| {
            !running_on_server(engine, marker)
        });
        assert_eq!(
            on_server(engine, &format!("SELECT count(*) FROM {name}.t")),
            "1"
        );
        assert_eq!(k2_value(engine, name), "before", "{engine:?}");

        // The statement stopped is not reported lost (the connection is not opened again).
        app.press(cx, &["ctrl-tab", "ctrl-tab"]);
        assert_eq!(app.active(cx), Some(1), "{engine:?}");
        assert_eq!(app.outcomes(cx), Vec::<String>::new(), "{engine:?}");
        app.press(cx, &["ctrl-tab"]);
        // Its tab says why it runs nowhere, and a run says so too.
        app.press(cx, &["ctrl-tab"]);
        assert_eq!(app.active(cx), Some(0), "{engine:?}");
        assert!(
            app.shows_label(
                cx,
                "Its connection was deleted: pick another one to run this script."
            ),
            "{engine:?}"
        );
        app.press(cx, &["secondary-enter"]);
        assert!(
            cx.update(|cx| script.read(cx).messages()).contains(
                &"Warning: Nothing ran. Its connection was deleted: pick another one to run this script."
                    .to_string()
            ),
            "{engine:?}"
        );
        assert_eq!(k6_connection(&script, cx), None, "{engine:?}");

        // The tab closed before comes back unbound, and goes again.
        app.dispatch(cx, actions::ReopenClosedTab);
        let reopened = app.tab(cx);
        assert_eq!(app.titles(cx).len(), 4, "{engine:?}");
        assert_eq!(
            cx.update(|cx| reopened.read(cx).sql(cx)),
            "select 'kerno_ui_k6_closed'",
            "{engine:?}"
        );
        assert_eq!(k6_connection(&reopened, cx), None, "{engine:?}");
        app.press(cx, &["secondary-w"]);

        // A restart opens the same three tabs.
        app.wait_until(cx, "the tabs to be stored", |app, cx| {
            cx.executor().advance_clock(Duration::from_millis(500));
            let stored = app.storage.tabs::<TabSnapshot>().unwrap();
            stored.len() == 3
                && matches!(
                    &stored[..2],
                    [
                        TabSnapshot::Query { target: None, file: Some(_), unsaved: true, .. },
                        TabSnapshot::Query { target: None, file: None, .. },
                    ]
                )
                && matches!(&stored[2], TabSnapshot::Query { target: Some(target), .. } if target.connection == kept)
        });
    }
}

/// C21: on MySQL 8.4 a `WHERE` in a comment for a newer version (`/*!90000 … */`) is skipped
/// by the server, so the statement deletes every row: on a local connection it asks, as a
/// `DELETE` without a `WHERE` does, and Cancel leaves the table as it was on the server. The
/// comment sits before `ORDER BY`: the editor drops a comment that ends a statement from what
/// it sends (and classifies), so there the question was already asked.
#[gpui_kit::test]
fn a_versioned_comment_where_still_asks(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) {
        return;
    }
    let name = "kerno_ui_k7";
    let (drop, create) = scratch_schema(engine, name);
    let table = format!("CREATE TABLE {name}.t (id INT PRIMARY KEY)");
    let rows = format!("INSERT INTO {name}.t VALUES (1), (2)");
    let _scratch = Scratch::new(engine, &[&drop], &[&create, &table, &rows]);
    let app = connected(cx, engine);

    app.run_in_new_tab(
        cx,
        &format!("DELETE FROM {name}.t /*!90000 WHERE id = 1 */ ORDER BY id"),
        "secondary-enter",
    );
    assert!(app.has(cx, "sql-preview"));
    assert_eq!(app.label_of(cx, "sql-title"), "Run this statement?");
    app.click(cx, "sql-cancel");
    assert!(!app.has(cx, "sql-preview"));
    assert!(!app.running(cx));
    assert_eq!(app.outcomes(cx), Vec::<String>::new());
    assert_eq!(
        on_server(engine, &format!("SELECT count(*) FROM {name}.t")),
        "2"
    );

    // A comment the server runs is sent with its statement, where it ends it too: the server
    // applies the WHERE, and one row goes, not all of them.
    assert_eq!(
        k5_run(
            &app,
            cx,
            &format!("DELETE FROM {name}.t /*! WHERE id = 1 */")
        ),
        ["done, 1 affected"]
    );
    assert_eq!(on_server(engine, &format!("SELECT id FROM {name}.t")), "2");
}

// K8 -----------------------------------------------------------------------------------------

/// The start-failure window `error` opens on the data under `paths`.
fn k8_failure_window(
    cx: &mut TestAppContext,
    paths: &AppPaths,
    error: &anyhow::Error,
) -> gpui_kit::AnyWindowHandle {
    cx.update(|cx| {
        let before = cx.windows().len();
        crate::start_failure::open(error, Some(paths.clone()), None, cx);
        let windows = cx.windows();
        assert_eq!(windows.len(), before + 1, "the window opened");
        *windows.last().unwrap()
    })
}

/// Runs `look` in `window` after a frame.
fn k8_in<R>(
    cx: &mut TestAppContext,
    window: gpui_kit::AnyWindowHandle,
    look: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R,
) -> R {
    let seen = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            look(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    seen
}

/// The ids of the start-failure window's buttons that are shown.
fn k8_buttons(cx: &mut TestAppContext, window: gpui_kit::AnyWindowHandle) -> Vec<&'static str> {
    k8_in(cx, window, |window, _| {
        [
            "start-failure-folder",
            "start-failure-fresh",
            "start-failure-quit",
        ]
        .into_iter()
        .filter(|id| window.try_find(*id).is_some())
        .collect()
    })
}

/// A data directory whose store was written by a newer Kerno (schema 999), and why it does
/// not open.
fn k8_newer_store() -> (tempfile::TempDir, AppPaths, anyhow::Error) {
    let data = tempfile::tempdir().unwrap();
    let paths = AppPaths::under(data.path().to_path_buf());
    let storage = Storage::open(&paths.database()).unwrap();
    storage
        .save_connection(None, &ConnectionConfig::new(Engine::Postgres))
        .unwrap();
    drop(storage);
    kerno_core::storage::set_schema_for_tests(&paths.database(), 999).unwrap();
    let error = crate::load(&paths)
        .err()
        .expect("a newer store does not open");
    (data, paths, error)
}

/// C16: a store written by a newer Kerno opens the start-failure window instead of nothing:
/// what happened, what to do, the raw error, and its three buttons. "Start fresh" moves the
/// store aside (renamed, with the schema it had) and starts Kerno on a new one in the same run.
#[gpui_kit::test]
fn a_start_failure_opens_a_window_that_says_what_happened(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (data, paths, error) = k8_newer_store();
    let window = k8_failure_window(cx, &paths, &error);
    let failure = crate::start_failure::start_failure(&error);
    assert!(failure.headline.contains("newer Kerno"), "{failure:?}");
    k8_in(cx, window, |window, _| {
        use gpui_kit::test::TestQueryExt as _;
        for text in [&failure.headline, &failure.next_step, &failure.raw] {
            assert!(window.try_find_by_label(text).is_some(), "{text}");
        }
        assert!(failure.raw.contains("schema 999"), "{failure:?}");
    });
    assert_eq!(
        k8_buttons(cx, window),
        [
            "start-failure-folder",
            "start-failure-fresh",
            "start-failure-quit"
        ]
    );

    k8_in(cx, window, |window, cx| {
        window.click("start-failure-fresh", cx)
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while cx.update(|cx| !cx.has_global::<crate::start_failure::tests::Started>()) {
        assert!(
            std::time::Instant::now() < deadline,
            "Kerno never started fresh"
        );
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    let (connections, database) = cx.update(|cx| {
        let started = cx.global::<crate::start_failure::tests::Started>();
        (started.connections, started.database.clone())
    });
    assert_eq!((connections, database), (0, paths.database()));
    assert!(
        cx.update(|cx| cx.windows().is_empty()),
        "the failure window closed"
    );
    let moved: Vec<String> = std::fs::read_dir(data.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.starts_with("kerno-moved-"))
        .collect();
    // The store, and its write-ahead log and index when there are, under one new name.
    let stores: Vec<&String> = moved
        .iter()
        .filter(|name| name.ends_with(".sqlite3"))
        .collect();
    assert_eq!(stores.len(), 1, "{moved:?}");
    assert!(stores[0].ends_with("-schema999.sqlite3"), "{moved:?}");
    assert!(
        moved
            .iter()
            .all(|name| name.starts_with(stores[0].as_str())),
        "{moved:?}"
    );
    // Nothing was deleted: back at the schema this build reads (4, which the store had before
    // it was marked 999), the moved store still holds its connection.
    let moved_store = data.path().join(stores[0]);
    kerno_core::storage::set_schema_for_tests(&moved_store, 4).unwrap();
    assert_eq!(
        Storage::open(&moved_store)
            .unwrap()
            .connections()
            .unwrap()
            .len(),
        1
    );
}

/// C16: while another Kerno has the data open, "Start fresh" is not offered: moving the store
/// would pull it from under that one.
#[gpui_kit::test]
fn start_fresh_is_not_offered_while_another_kerno_has_the_data(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let (data, paths, error) = k8_newer_store();
    let socket = kerno_core::instance::socket_path(&paths.data_dir);
    let in_use = |cx: &mut TestAppContext, window| {
        k8_in(cx, window, |window, _| {
            use gpui_kit::test::TestQueryExt as _;
            window
                .try_find_by_label(
                    "Another Kerno is using this data: quit it before starting fresh.",
                )
                .is_some()
        })
    };
    // Another Kerno opens the data while the window is up: the click moves nothing.
    let window = k8_failure_window(cx, &paths, &error);
    assert!(k8_buttons(cx, window).contains(&"start-failure-fresh"));
    let other = kerno_core::instance::listen(&socket)
        .unwrap()
        .expect("the socket is free");
    k8_in(cx, window, |window, cx| {
        window.click("start-failure-fresh", cx)
    });
    assert_eq!(
        k8_buttons(cx, window),
        ["start-failure-folder", "start-failure-quit"]
    );
    assert!(in_use(cx, window));
    assert!(paths.database().exists());
    assert!(std::fs::read_dir(data.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("kerno-moved-")
    }));

    // Running already when the window opens: not offered at all.
    let window = k8_failure_window(cx, &paths, &error);
    assert_eq!(
        k8_buttons(cx, window),
        ["start-failure-folder", "start-failure-quit"]
    );
    assert!(in_use(cx, window));
    drop(other);
}

/// C16: a settings save that fails shows one notification, however many saves fail after it
/// (a slider dragged); the next save that succeeds takes it away.
#[gpui_kit::test]
fn a_settings_save_failure_is_shown(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    storage.refuse_writes(true).unwrap();
    let before = notification_count(&app, cx);
    for size in 0..20 {
        cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 100 + size));
    }
    app.wait_until(cx, "the failed save is shown", |app, cx| {
        notification_count(app, cx) == before + 1
    });
    fn shown(app: &Fixture, cx: &mut TestAppContext) -> Vec<gpui_kit::EntityId> {
        use gpui_kit::component::WindowExt as _;
        app.step(cx, |window, cx| {
            window
                .notifications(cx)
                .iter()
                .map(|note| note.entity_id())
                .collect()
        })
    }
    let first = shown(&app, cx);
    for size in 0..20 {
        cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 200 + size));
        app.wait_until(cx, "the save ended", |_, cx| {
            cx.update(crate::state::settings_idle)
        });
    }
    // One notification for all of them, the same one, not shown again at each save (the kit's
    // notifications carry no label to read their text by: the wording is in
    // `state::show_save_outcome`).
    assert_eq!(shown(&app, cx), first);
    assert_eq!(notification_count(&app, cx), before + 1);
    assert_ne!(
        Settings::load(&storage).page_size,
        219,
        "nothing was written"
    );

    storage.refuse_writes(false).unwrap();
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 300));
    app.wait_until(cx, "the save that worked", |_, _| {
        Settings::load(&storage).page_size == 300
    });
    app.wait_until(cx, "the notification goes", |app, cx| {
        cx.executor().advance_clock(Duration::from_secs(1));
        notification_count(app, cx) == before
    });
}

// K9: the server's detail and hint on every error surface (C14) ----------------------------

/// The error notifications this test's window has shown, oldest first.
fn errors_shown() -> Vec<String> {
    crate::ERRORS_SHOWN.with(|shown| shown.borrow().clone())
}

/// The test's own table `<name>.t (id, v)` holding the rows `(1, 'a')` and `(2, 'b')`,
/// dropped when the test ends.
fn k9_table(engine: Engine, name: &str) -> Scratch {
    let (drop, create) = scratch_schema(engine, name);
    let table = format!("CREATE TABLE {name}.t (id INT PRIMARY KEY, v VARCHAR(20))");
    let rows = format!("INSERT INTO {name}.t VALUES (1, 'a'), (2, 'b')");
    Scratch::new(engine, &[&drop], &[&create, &table, &rows])
}

/// C14: an Apply the server refuses for a duplicate key says what happened first, then the
/// server's message with its code and, on PostgreSQL, its detail naming the key; MySQL gives
/// no detail or hint: its message and its error number are kept, nothing else is invented.
#[gpui_kit::test]
fn a_duplicate_key_on_apply_shows_the_server_detail(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k9_dup";
        let _scratch = k9_table(engine, name);
        let app = connected(cx, engine);
        app.open_table(cx, name, "t", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        // The second row takes the key of the first.
        app.type_in_cell(cx, 1, 1, "1", "enter");
        assert_eq!(app.pending(cx), 1, "{engine:?}");
        app.click(cx, "data-apply");
        app.click(cx, "sql-confirm");
        let tab = app.data(cx);
        app.wait_until(cx, "the refused apply", |_, cx| {
            cx.update(|cx| tab.read(cx).apply_error().is_some())
        });
        let error = cx
            .update(|cx| tab.read(cx).apply_error().map(str::to_string))
            .unwrap();
        let lines: Vec<&str> = error.lines().collect();
        assert_eq!(
            lines[0], "Nothing was changed: statement 1 failed. The pending changes are kept.",
            "{engine:?}: {error}"
        );
        match engine {
            Engine::Postgres => {
                assert!(
                    lines[1].starts_with("duplicate key value violates unique constraint")
                        && lines[1].ends_with("[23505]"),
                    "{error}"
                );
                assert_eq!(lines[2], "Detail: Key (id)=(1) already exists.", "{error}");
                assert_eq!(lines.len(), 3, "{error}");
            }
            _ => {
                assert!(
                    lines[1].starts_with("Duplicate entry '1'")
                        && lines[1].ends_with("[1062 (23000)]"),
                    "{engine:?}: {error}"
                );
                assert_eq!(lines.len(), 2, "{engine:?}: {error}");
            }
        }
        assert_eq!(app.pending(cx), 1, "{engine:?}");
        assert_eq!(
            on_server(
                engine,
                &format!("SELECT count(*) FROM {name}.t WHERE id = 1")
            ),
            "1"
        );
        app.click(cx, "data-discard");
    }
}

/// C14: Drop on a table a view depends on: the notification says the table was not dropped,
/// then PostgreSQL's message, its detail (the view) and its hint, which holds the fix
/// (`DROP … CASCADE`). It stays until closed. (MySQL drops a table under a view without an
/// error: no twin.)
#[gpui_kit::test]
fn dropping_a_table_with_dependents_shows_the_hint(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let name = "kerno_ui_k9_drop";
    let (drop, create) = scratch_schema(engine, name);
    let _scratch = Scratch::new(
        engine,
        &[&drop],
        &[
            &create,
            &format!("CREATE TABLE {name}.t (id INT PRIMARY KEY)"),
            &format!("CREATE VIEW {name}.v AS SELECT id FROM {name}.t"),
        ],
    );
    let app = connected(cx, engine);
    let path = k2_path(engine, name);
    app.open_path(cx, &path);
    let before = errors_shown().len();
    app.act(cx, Action::Drop, &path);
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the refused drop", |_, _| errors_shown().len() > before);
    let shown = errors_shown();
    let text = shown.last().unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "The table “t” was not dropped.", "{text}");
    assert!(
        lines[1].starts_with("cannot drop table") && lines[1].ends_with("[2BP01]"),
        "{text}"
    );
    assert!(
        lines[2].starts_with("Detail: view kerno_ui_k9_drop.v depends on table"),
        "{text}"
    );
    assert!(lines[3].starts_with("Hint: Use DROP ... CASCADE"), "{text}");
    assert_eq!(
        on_server(
            engine,
            &format!("SELECT count(*) FROM pg_tables WHERE schemaname = '{name}'")
        ),
        "1"
    );
}

/// C14, C15: a COMMIT the server refuses (a deferred unique constraint checked at commit)
/// says first what came of the transaction, read from the session after the failure: rolled
/// back, nothing saved; then the server's message and detail. The tab's badge and state
/// agree: no transaction is open any more.
#[gpui_kit::test]
fn a_failed_commit_says_what_was_kept(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let name = "kerno_ui_k9_commit";
    let (drop, create) = scratch_schema(engine, name);
    let _scratch = Scratch::new(
        engine,
        &[&drop],
        &[
            &create,
            &format!(
                "CREATE TABLE {name}.t (id INT, CONSTRAINT one_id UNIQUE (id) DEFERRABLE \
                 INITIALLY DEFERRED)"
            ),
            &format!("INSERT INTO {name}.t VALUES (1)"),
        ],
    );
    let app = connected(cx, engine);
    app.press(cx, &["secondary-t"]);
    app.open_transaction(cx, &format!("insert into {name}.t values (1)"));
    let before = errors_shown().len();
    app.dispatch(cx, actions::CommitTransaction);
    app.wait_until(cx, "the refused commit", |_, _| {
        errors_shown().len() > before
    });
    let shown = errors_shown();
    let text = shown.last().unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "The commit failed: the transaction was rolled back by the server; nothing it changed \
         was saved.",
        "{text}"
    );
    assert!(
        lines[1].starts_with("duplicate key value violates unique constraint \"one_id\"")
            && lines[1].ends_with("[23505]"),
        "{text}"
    );
    assert_eq!(lines[2], "Detail: Key (id)=(1) already exists.", "{text}");
    assert_eq!(app.tx_state(cx), kerno_core::TxState::Idle);
    assert!(!app.has(cx, ("tab-tx", 0usize)));
    assert_eq!(
        on_server(engine, &format!("SELECT count(*) FROM {name}.t")),
        "1"
    );
}

/// C14, C15: each outcome of a failed COMMIT or ROLLBACK, as the session reports it after the
/// failure, on both engines' states; a connection lost during a COMMIT never claims to know.
#[test]
fn a_failed_end_of_transaction_says_its_outcome() {
    use crate::query::tab::ending_failed;
    use kerno_core::{DbError, Error, TxState};
    let refused = Error::from(DbError {
        message: "duplicate key value violates unique constraint \"u\"".into(),
        code: Some("23505".into()),
        detail: Some("Key (id)=(1) already exists.".into()),
        ..DbError::default()
    });
    let first = |commit, error: &Error, after| {
        ending_failed(commit, error, after)
            .lines()
            .next()
            .unwrap()
            .to_string()
    };
    assert!(first(true, &refused, TxState::Idle).contains("rolled back by the server"));
    for after in [TxState::Open, TxState::Failed] {
        assert_eq!(
            first(true, &refused, after),
            "The commit failed; the transaction is still open."
        );
        assert_eq!(
            first(false, &refused, after),
            "The rollback failed; the transaction is still open."
        );
    }
    assert!(first(false, &refused, TxState::Idle).contains("no longer open"));
    let lost = Error::Closed("broken pipe".into());
    assert!(first(true, &lost, TxState::Idle).contains("is not known"));
    // The server's words follow, with the detail.
    let text = ending_failed(true, &refused, TxState::Idle);
    assert_eq!(
        text.lines().skip(1).collect::<Vec<_>>(),
        [
            "duplicate key value violates unique constraint \"u\" [23505]",
            "Detail: Key (id)=(1) already exists."
        ]
    );
}

/// A secret store that refuses every read, as the macOS keychain does when its prompt is
/// answered Deny (the words of `secrets.rs`'s `secret_error`).
struct Refusing;

impl kerno_core::secrets::SecretStore for Refusing {
    fn get(&self, _: &str) -> kerno_core::Result<Option<Secret>> {
        Err(kerno_core::Error::Secrets(
            "could not read from the system secret store: Platform failure: User canceled the \
             operation."
                .into(),
        ))
    }
    fn set(&self, _: &str, _: &Secret) -> kerno_core::Result<()> {
        Ok(())
    }
    fn delete(&self, _: &str) -> kerno_core::Result<()> {
        Ok(())
    }
    fn is_persistent(&self) -> bool {
        true
    }
}

/// C14: a connect whose saved password the secret store refuses says so in plain words with
/// the next step (Allow, or "Ask each time"), never the generic "The connection failed.", and
/// never a secret.
#[gpui_kit::test]
fn a_refused_keychain_read_says_what_to_do(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let config = testing::config(Engine::Postgres);
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    cx.update(|cx| cx.global_mut::<AppState>().secrets = Arc::new(Refusing));
    cx.update(|cx| {
        app.connections
            .update(cx, |connections, cx| connections.connect(&id, cx))
    });
    app.wait_until(cx, "the refused read", |app, cx| {
        matches!(app.status(cx, 0), Status::Failed { .. })
    });
    let Status::Failed { message } = app.status(cx, 0) else {
        unreachable!()
    };
    assert!(message.starts_with("secrets: "), "{message}");
    let explained = kerno_core::error::explain(&message, &config);
    assert_eq!(
        explained.kind,
        kerno_core::error::FailureKind::SecretStore,
        "{message}"
    );
    assert!(
        explained
            .next_step
            .ends_with("or set Password to “Ask each time” in the connection's settings."),
        "{explained:?}"
    );
    let password = testing::secrets().password.unwrap();
    assert!(!message.contains(password.expose()), "{message}");
}

// K10: pin what works but nothing tests (C3, C6, C21) ---------------------------------------

/// A statement whose first page arrives at once and whose other 3,000 rows each take 20 ms on
/// the server: "Fetch all" is still running when it is cancelled, nothing large is read, and
/// one a failed run leaves behind ends within a minute.
fn k10_slow_rows(engine: Engine, marker: &str) -> String {
    match engine {
        Engine::Postgres => format!(
            "select g as {marker} from generate_series(1, 4000) g \
             where g <= 1000 or (select true from pg_sleep(0.02 + g * 0))"
        ),
        Engine::MySql | Engine::MariaDb => format!(
            "select id as {marker}, username from kerno_analytics.big_table \
             where id <= 4000 and if(id <= 1000, 0, sleep(0.02)) = 0"
        ),
    }
}

/// A name for the statements of this test process alone: one a failed run left on the
/// server (a MySQL `SLEEP` runs on after its client is gone) is not taken for this one.
fn k10_marker(what: &str) -> String {
    format!("kerno_ui_k10_{what}_{}", std::process::id())
}

/// Cancels the statement of the active tab once it runs on the server, waits for it to be
/// gone there, and checks the same tab runs the next statement.
fn k10_cancel(app: &Fixture, cx: &mut TestAppContext, engine: Engine, marker: &str) {
    app.wait_until(cx, "the statement on the server", |app, cx| {
        app.running(cx) && running_on_server(engine, marker)
    });
    app.press(cx, &["secondary-."]);
    app.wait_until(cx, "the cancellation", |app, cx| !app.running(cx));
    app.wait_until(cx, "the statement to be gone from the server", |_, _| {
        !running_on_server(engine, marker)
    });
}

/// C3: "Fetch all" in a query tab is cancelled on the server (Cmd+.): the statement is gone
/// there, the rows that arrived are kept (marked incomplete, the server's cancellation after
/// them), and the tab runs the next statement.
#[gpui_kit::test]
fn fetch_all_is_cancelled_on_the_server(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let marker = k10_marker("fetch");
        let app = connected(cx, engine);
        app.run_in_new_tab(cx, &k10_slow_rows(engine, &marker), "secondary-enter");
        app.wait_until(cx, "the first page", |app, cx| {
            app.outcomes(cx) == ["1000 rows, more"] && !app.running(cx)
        });
        app.click(cx, "fetch-all");
        k10_cancel(&app, cx, engine, &marker);
        // The rows that arrived are kept, marked incomplete; the server's cancellation follows.
        let outcomes = app.outcomes(cx);
        assert_eq!(outcomes.len(), 2, "{engine:?}: {outcomes:?}");
        assert_eq!(outcomes[1], "cancelled", "{engine:?}");
        let rows: usize = outcomes[0]
            .strip_suffix(" rows, incomplete")
            .and_then(|count| count.parse().ok())
            .unwrap_or_else(|| panic!("{engine:?}: {outcomes:?}"));
        assert!(rows >= 1000, "{engine:?}: the rows that arrived are kept");
        assert_eq!(
            k3_run(&app, cx, "select 7 as seven"),
            ["1 rows"],
            "{engine:?}"
        );
    }
}

/// C3: EXPLAIN ANALYZE runs the statement, so it is cancelled on the server as a run is
/// (MariaDB's form is `ANALYZE FORMAT=JSON`), and the tab runs the next statement.
#[gpui_kit::test]
fn an_explain_analyze_is_cancelled(cx: &mut TestAppContext) {
    let marker = k10_marker("explain");
    // 5,000 rows of 10 ms: 50 s, gone soon after a failed run.
    let mysql = format!("select count(*) as {marker} from kerno.orders where sleep(0.01) = 0");
    for (engine, slow) in [
        (Engine::Postgres, format!("select pg_sleep(60) as {marker}")),
        (Engine::MySql, mysql.clone()),
        (Engine::MariaDb, mysql),
    ] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        app.run_in_new_tab(cx, &slow, "secondary-shift-e");
        k10_cancel(&app, cx, engine, &marker);
        // MySQL may send the plan of what ran before the kill, then the kill's error.
        let outcomes = app.outcomes(cx);
        let (last, before) = outcomes.split_last().expect("an outcome");
        assert_eq!(last, "cancelled", "{engine:?}: {outcomes:?}");
        assert!(
            before.iter().all(|outcome| outcome.starts_with("plan, ")),
            "{engine:?}: {outcomes:?}"
        );
        assert_eq!(
            k3_run(&app, cx, "select 7 as seven"),
            ["1 rows"],
            "{engine:?}"
        );
    }
}

/// C6: renaming a connection from the sidebar renames it wherever its tab shows it: the
/// tab's connection picker (its menu), the main toolbar's pickers and the status bar.
#[gpui_kit::test]
fn renaming_a_connection_renames_its_tabs_picker(cx: &mut TestAppContext) {
    use crate::workspace::toolbar::Item;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let shown = |app: &Fixture, cx: &mut TestAppContext, name: &str| {
        // The tab's own picker: its menu lists the connections, built when it opens.
        app.click(cx, "tab-connection");
        let listed = menu_entry(app, cx, name).is_some();
        app.press(cx, &["escape"]);
        let (connection, target) = cx.update(|cx| {
            let workspace = app.workspace.read(cx);
            (
                workspace.picker_text(Item::Connection, cx),
                workspace.picker_text(Item::Target, cx),
            )
        });
        (
            listed,
            connection == name,
            target.ends_with(&format!("@{name}")),
            app.status_bar(cx).iter().any(|item| item == name),
        )
    };
    let before = cx.update(|cx| app.connections.read(cx).saved()[0].config.name.clone());
    assert_eq!(shown(&app, cx, &before), (true, true, true, true));

    app.right_click(cx, ("connection", 0usize), None);
    app.click_label(cx, "Rename…");
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "kerno_ui_k10 renamed");
    app.click(cx, "name-ok");
    assert_eq!(
        shown(&app, cx, "kerno_ui_k10 renamed"),
        (true, true, true, true)
    );
    assert_eq!(shown(&app, cx, &before), (false, false, false, false));
}

/// The test's own table `<name>.t` holding the rows 1 and 2, dropped when the test ends.
fn k10_table(engine: Engine, name: &str) -> Scratch {
    let (drop, create) = scratch_schema(engine, name);
    let table = format!("CREATE TABLE {name}.t (id INT PRIMARY KEY, v VARCHAR(20))");
    let rows = format!("INSERT INTO {name}.t VALUES (1, 'one'), (2, 'two')");
    Scratch::new(engine, &[&drop], &[&create, &table, &rows])
}

fn k10_count(engine: Engine, name: &str) -> String {
    on_server(engine, &format!("SELECT count(*) FROM {name}.t"))
}

/// C21: EXPLAIN ANALYZE runs its statement, so on a local, writable connection the plan of a
/// `DELETE` without a `WHERE` asks first, as running it does; Cancel runs nothing.
#[gpui_kit::test]
fn explain_analyze_of_a_delete_without_where_asks(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k10_plan_delete";
        let _scratch = k10_table(engine, name);
        let app = connected(cx, engine);
        app.run_in_new_tab(cx, &format!("delete from {name}.t"), "secondary-shift-e");
        assert!(app.has(cx, "sql-preview"), "{engine:?}");
        assert!(!app.has(cx, "sql-production"), "{engine:?}");
        assert_eq!(app.label_of(cx, "sql-title"), "Run this statement?");
        app.click(cx, "sql-cancel");
        assert!(!app.has(cx, "sql-preview"), "{engine:?}");
        assert!(!app.running(cx), "{engine:?}");
        assert_eq!(app.outcomes(cx), Vec::<String>::new(), "{engine:?}");
        assert_eq!(k10_count(engine, name), "2", "{engine:?}");
        // The plan of a statement that only reads runs without a question.
        app.press(cx, &["secondary-a", "backspace"]);
        app.type_text(cx, &format!("select * from {name}.t"));
        app.press(cx, &["secondary-shift-e"]);
        assert!(!app.has(cx, "sql-preview"), "{engine:?}");
        app.wait_until(cx, "the plan", |app, cx| {
            !app.running(cx) && !app.outcomes(cx).is_empty()
        });
    }
}

/// C21: a run on several connections asks each local, writable target before a `DELETE`
/// without a `WHERE`, one at a time: Cancel skips that target only (its rows are all on the
/// server), the other one, confirmed, deletes.
#[gpui_kit::test]
fn a_local_target_of_a_multi_run_asks_before_a_delete_without_where(cx: &mut TestAppContext) {
    use super::multi_run::{open_dialog, saved, summary, wait_for_the_end, with_connections};
    if ![Engine::Postgres, Engine::MySql]
        .into_iter()
        .all(testing::available)
    {
        return;
    }
    let name = "kerno_ui_k10_multi";
    let _pg = k10_table(Engine::Postgres, name);
    let _my = k10_table(Engine::MySql, name);
    let configs = [
        saved(Engine::Postgres, "local PG", |_| {}),
        saved(Engine::MySql, "local MySQL", |_| {}),
    ];
    let app = with_connections(cx, &configs, &format!("DELETE FROM {name}.t"));
    open_dialog(&app, cx);
    app.click(cx, ("multi-connection", 0usize));
    app.click(cx, ("multi-connection", 1usize));
    app.click(cx, "multi-start");

    app.wait_until(cx, "the first question", |app, cx| {
        app.has(cx, "sql-preview")
    });
    assert_eq!(app.label_of(cx, "sql-title"), "Run this statement?");
    assert!(!app.has(cx, "sql-production"));
    let lines = summary(&app, cx);
    assert!(
        lines[0].ends_with("Waiting for your confirmation"),
        "{lines:#?}"
    );
    assert_eq!(k10_count(Engine::Postgres, name), "2");
    app.click(cx, "sql-cancel");

    app.wait_until(cx, "the second question", |app, cx| {
        summary(app, cx)[1].ends_with("Waiting for your confirmation")
    });
    assert!(app.has(cx, "sql-preview"));
    app.click(cx, "sql-confirm");
    wait_for_the_end(&app, cx);

    let lines = summary(&app, cx);
    assert_eq!(
        lines[0], "local PG: Not run: confirmation declined",
        "{lines:#?}"
    );
    assert!(lines[1].starts_with("local MySQL: Done"), "{lines:#?}");
    assert_eq!(k10_count(Engine::Postgres, name), "2");
    assert_eq!(k10_count(Engine::MySql, name), "0");
}

/// C21: Apply of a table's pending edits on a production connection asks twice, in order:
/// the first confirmation only arms the second ("Run on Production"), refusing the second
/// runs nothing and keeps the edit; confirming both applies it.
#[gpui_kit::test]
fn a_data_apply_on_production_asks_twice(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k10_prod";
        let _scratch = k10_table(engine, name);
        let value = || on_server(engine, &format!("SELECT v FROM {name}.t WHERE id = 1"));
        let mut config = testing::config(engine);
        config.environment = kerno_core::environment::PRODUCTION.into();
        let app = connected_as(cx, config);
        app.open_table(cx, name, "t", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        app.type_in_cell(cx, 0, 2, "changed", "enter");
        assert_eq!(app.pending(cx), 1, "{engine:?}");

        app.click(cx, "data-apply");
        assert!(app.has(cx, "sql-production"), "{engine:?}");
        assert_eq!(app.label_of(cx, "sql-confirm"), "Apply", "{engine:?}");
        app.click(cx, "sql-confirm");
        assert!(
            app.has(cx, "sql-preview"),
            "{engine:?}: one click is not enough"
        );
        assert_eq!(
            app.label_of(cx, "sql-confirm"),
            "Run on Production",
            "{engine:?}"
        );
        assert_eq!(value(), "one", "{engine:?}");
        app.click(cx, "sql-cancel");
        assert!(!app.has(cx, "sql-preview"), "{engine:?}");
        assert_eq!(value(), "one", "{engine:?}");
        assert_eq!(app.pending(cx), 1, "{engine:?}: the edit is kept");

        app.click(cx, "data-apply");
        app.click(cx, "sql-confirm");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the apply", |app, cx| app.pending(cx) == 0);
        assert_eq!(value(), "changed", "{engine:?}");
    }
}

// K10: what K1-K9 left read, not run ---------------------------------------------------------

/// C6 (K3): the read-only toggle rebuilds the connection's sessions; a tab moved by `USE` (a
/// `SET search_path` on PostgreSQL) opens its new session where it was moved, and a write is
/// refused.
#[gpui_kit::test]
fn the_read_only_toggle_keeps_a_tab_where_it_was_moved(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let place = "kerno_ui_k10_moved";
        let (drop, create) = scratch_schema(engine, place);
        let _scratch = Scratch::new(engine, &[&drop], &[&create]);
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        let (moving, _) = k3_sql(engine, place);
        assert_eq!(k3_run(&app, cx, &moving), ["done"], "{engine:?}");
        let (_, first_id) = k3_where(&app, cx, engine);

        app.dispatch(cx, actions::ToggleReadOnly);
        app.wait_until(cx, "the reconnection", |app, cx| {
            app.first_stored().read_only && matches!(app.status(cx, 0), Status::Connected { .. })
        });
        let (now, second_id) = k3_where(&app, cx, engine);
        assert_ne!(second_id, first_id, "{engine:?}: a new session");
        assert_eq!(now, place, "{engine:?}");
        let write = format!("create table {place}.t (id int)");
        assert_eq!(k3_run(&app, cx, &write), ["failed []"], "{engine:?}");
        assert_eq!(
            on_server(
                engine,
                &format!(
                    "SELECT count(*) FROM information_schema.tables WHERE table_schema = '{place}'"
                )
            ),
            "0",
            "{engine:?}"
        );
    }
}

/// C14 (K9): the table form's Apply failure shows the server's hint: a column changed from
/// text to `int` cannot be cast without `USING`, and PostgreSQL says how.
#[gpui_kit::test]
fn a_failed_table_form_apply_shows_the_server_hint(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let name = "kerno_ui_k10_form";
    let _scratch = k10_table(engine, name);
    let app = connected(cx, engine);
    let connection = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let target = Target {
        connection,
        database: None,
        schema: None,
    };
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_table_editor(
                target,
                name.to_string(),
                Some(TableRef::new(name, "t")),
                window,
                cx,
            );
        })
    });
    app.wait_for_editor(cx);
    app.fill_in(cx, ("column-type", 1usize), "int");
    app.click(cx, "editor-apply");
    app.click(cx, "sql-confirm");
    let editor = app.editor(cx);
    app.wait_until(cx, "the failed apply", |_, cx| {
        cx.update(|cx| editor.read(cx).error().is_some())
    });
    let error = cx.update(|cx| editor.read(cx).error().map(str::to_string).unwrap());
    assert!(error.contains("[42804]"), "{error}");
    assert!(
        error.contains("Hint: You might need to specify \"USING v::integer\"."),
        "{error}"
    );
    assert_eq!(
        on_server(
            engine,
            &format!(
                "SELECT data_type FROM information_schema.columns \
                 WHERE table_schema = '{name}' AND column_name = 'v'"
            )
        ),
        "character varying"
    );
}

/// C8 (K11): typing a CTE in a query tab, `x.` lists the CTE's columns and Enter writes the
/// first one. One engine per test: in the app opened second in one test context, Enter
/// accepted the item and then also wrote a newline (seen, not explained).
fn k11_cte_columns_in_the_editor(cx: &mut TestAppContext, engine: Engine, table: &str) {
    if !testing::available(engine) {
        return;
    }
    let app = connected(cx, engine);
    new_tab_with_catalog(&app, cx);
    let typed = format!("with x as (select id, status from {table}) select x.");
    app.type_text(cx, &typed);
    let (open, items) = completion_menu(&app, cx);
    assert!(open, "{items:?}");
    assert_eq!(items, ["id", "status"]);
    app.press(cx, &["enter"]);
    assert_eq!(app.active_query(cx).1, format!("{typed}id"));
}

#[gpui_kit::test]
fn a_cte_typed_in_the_editor_offers_its_columns(cx: &mut TestAppContext) {
    k11_cte_columns_in_the_editor(cx, Engine::Postgres, "shop.orders");
}

#[gpui_kit::test]
fn a_cte_typed_in_a_mysql_editor_offers_its_columns(cx: &mut TestAppContext) {
    k11_cte_columns_in_the_editor(cx, Engine::MySql, "orders");
}

// K12 ----------------------------------------------------------------------------------------

/// C9 (K12): a routine whose body holds several statements, typed in a query tab without
/// `DELIMITER` (`BEGIN ATOMIC … END` on PostgreSQL, `BEGIN … END` on MySQL and MariaDB), is
/// sent whole. Run as a script it is one statement before `SELECT 3`, and its `DELETE` is
/// stored, not run on its own (nothing asks, the row stays); run from the cursor on a line of
/// its body, the whole `CREATE` runs again. Each time the server holds the routine with both
/// statements of its body.
fn k12_a_routine_typed_without_delimiter(cx: &mut TestAppContext, engine: Engine) {
    if !testing::available(engine) {
        return;
    }
    let name = "kerno_ui_k12";
    let (drop, create) = scratch_schema(engine, name);
    let table = format!("CREATE TABLE {name}.t (id INT PRIMARY KEY)");
    let row = format!("INSERT INTO {name}.t VALUES (1)");
    let _scratch = Scratch::new(engine, &[&drop], &[&create, &table, &row]);
    let (routine, definition, drop_routine) = match engine {
        Engine::Postgres => (
            format!("CREATE FUNCTION {name}.f() RETURNS int LANGUAGE sql\nBEGIN ATOMIC"),
            format!("SELECT pg_get_functiondef('{name}.f()'::regprocedure)"),
            format!("DROP FUNCTION {name}.f()"),
        ),
        Engine::MySql | Engine::MariaDb => (
            format!("CREATE PROCEDURE {name}.p()\nBEGIN"),
            format!(
                "SELECT ROUTINE_DEFINITION FROM information_schema.ROUTINES \
                 WHERE ROUTINE_SCHEMA = '{name}' AND ROUTINE_NAME = 'p'"
            ),
            format!("DROP PROCEDURE {name}.p"),
        ),
    };
    let script = format!("{routine}\n  DELETE FROM {name}.t;\n  SELECT 2;\nEND;\nSELECT 3;");
    let stored = |what: &str| {
        let body = on_server(engine, &definition);
        assert!(
            body.contains(&format!("DELETE FROM {name}.t")) && body.contains("SELECT 2"),
            "{engine:?} {what}: {body}"
        );
        assert_eq!(
            on_server(engine, &format!("SELECT count(*) FROM {name}.t")),
            "1",
            "{engine:?} {what}"
        );
    };
    let app = connected(cx, engine);

    app.run_in_new_tab(cx, &script, "secondary-shift-enter");
    assert!(
        !app.has(cx, "sql-preview"),
        "{engine:?}: nothing to confirm"
    );
    app.wait_until(cx, "the script", |app, cx| {
        !app.running(cx) && app.outcomes(cx).len() == 2
    });
    assert_eq!(app.outcomes(cx), ["done", "1 rows"], "{engine:?}");
    stored("run as a script");

    on_server(engine, &drop_routine);
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.editor().update(cx, |editor, cx| {
                // On `  SELECT 2;`, inside the body.
                editor.set_cursor_position(
                    gpui_kit::component::input::Position::new(3, 4),
                    window,
                    cx,
                )
            })
        })
    });
    app.press(cx, &["secondary-enter"]);
    assert!(
        !app.has(cx, "sql-preview"),
        "{engine:?}: nothing to confirm"
    );
    app.wait_until(cx, "the statement at the cursor", |app, cx| {
        !app.running(cx) && app.outcomes(cx) == ["done"]
    });
    stored("run from the cursor");
}

#[gpui_kit::test]
fn a_begin_atomic_function_typed_without_delimiter_is_one_statement(cx: &mut TestAppContext) {
    k12_a_routine_typed_without_delimiter(cx, Engine::Postgres);
}

#[gpui_kit::test]
fn a_mysql_procedure_typed_without_delimiter_is_one_statement(cx: &mut TestAppContext) {
    k12_a_routine_typed_without_delimiter(cx, Engine::MySql);
}

#[gpui_kit::test]
fn a_mariadb_procedure_typed_without_delimiter_is_one_statement(cx: &mut TestAppContext) {
    k12_a_routine_typed_without_delimiter(cx, Engine::MariaDb);
}
