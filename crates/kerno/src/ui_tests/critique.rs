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
