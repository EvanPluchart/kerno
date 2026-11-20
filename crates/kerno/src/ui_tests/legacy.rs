//! K22, legacy MySQL servers in the app (docs/plans/M12.md): the server named
//! with its product, a read-only connection to a server without read-only
//! sessions warned about, and a plan the server cannot give said before
//! anything runs. On `kerno-mysql55` (`scripts/test-env.sh up --legacy`).

use super::*;

/// K22: on MySQL 5.5.62 the status says "MySQL 5.5.62"; a read-only
/// connection opens with one warning (the server has no read-only sessions
/// before 5.6.5); Explain is refused with a notification and runs nothing
/// (no JSON plan before 5.6.5). Mutations: the `Warned` event not emitted
/// (no notification arrives), the guard of `QueryTab::explain` removed (the
/// statement runs and fails on the server).
#[gpui_kit::test]
fn a_legacy_server_is_named_warned_about_and_spared_a_plan_it_lacks(cx: &mut TestAppContext) {
    let Some((_, params)) = testing::legacy_mysql()
        .into_iter()
        .find(|(name, _)| *name == "kerno-mysql55")
    else {
        return;
    };
    let app = connected_as(
        cx,
        ConnectionConfig {
            port: params.port,
            read_only: true,
            ..testing::config(Engine::MySql)
        },
    );
    match app.status(cx, 0) {
        Status::Connected { version } => assert_eq!(version, "MySQL 5.5.62"),
        other => panic!("{other:?}"),
    }
    app.wait_until(cx, "the read-only warning", |app, cx| {
        notification_count(app, cx) == 1
    });

    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "SELECT * FROM kerno.orders");
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::ExplainStatement), cx)
    });
    assert_eq!(notification_count(&app, cx), 2);
    assert!(!app.running(cx));
    assert!(app.outcomes(cx).is_empty(), "{:?}", app.outcomes(cx));

    // Asked while the connection is closed: the tab connects first, then
    // says the same and runs nothing.
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    app.step(cx, |_, cx| {
        app.connections
            .update(cx, |connections, cx| connections.disconnect(&id, cx))
    });
    app.wait_until(cx, "the connection to close", |app, cx| {
        matches!(app.status(cx, 0), Status::Disconnected)
    });
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::ExplainAnalyze), cx)
    });
    app.wait_until(cx, "the connection and the notice", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. }) && notification_count(app, cx) >= 4
    });
    assert!(!app.running(cx));
    assert!(app.outcomes(cx).is_empty(), "{:?}", app.outcomes(cx));
}

/// The real-app pass of 2026-11-20: on MySQL 5.1 the Locks list said "not supported by this
/// database: the locks of this server The account may lack the privilege…": two sentences run
/// together, and the account blamed for what the release does not have. It now says the server
/// has no such list, names the server, and offers nothing to retry. Fails with `failed` keeping
/// an unsupported list as a failure.
#[gpui_kit::test]
fn a_list_an_old_server_does_not_have_is_said_as_missing(cx: &mut TestAppContext) {
    let Some((_, params)) = testing::legacy_mysql()
        .into_iter()
        .find(|(name, _)| *name == "kerno-mysql51")
    else {
        return;
    };
    let app = connected_as(
        cx,
        ConnectionConfig {
            port: params.port,
            ..testing::config(Engine::MySql)
        },
    );
    app.dispatch(cx, actions::OpenLockManager);
    app.wait_until(cx, "the locks answered", |app, cx| {
        let tab = app.admin(cx);
        cx.update(|cx| tab.read(cx).state(crate::admin::tab::Pane::Locks)) == "missing"
    });
    assert!(app.has(
        cx,
        ("admin-missing", crate::admin::tab::Pane::Locks as usize)
    ));
    assert!(!app.has(cx, ("admin-error", crate::admin::tab::Pane::Locks as usize)));
}
