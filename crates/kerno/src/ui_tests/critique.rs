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
