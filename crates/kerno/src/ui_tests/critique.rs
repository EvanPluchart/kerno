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
