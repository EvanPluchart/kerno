//! K28, defects of daily use: the window reopens where it was, a
//! notification covers no control of the tab, and "New SQL editor" on a
//! connection that is not open connects it first.

use kerno_core::storage::{WindowPlacement, WindowState};

use gpui_kit::component::WindowExt as _;

use super::*;
use crate::state::WINDOW_KEY;

fn stored_window(app: &Fixture) -> Option<WindowPlacement> {
    app.storage.setting(WINDOW_KEY).unwrap()
}

#[gpui_kit::test]
fn the_window_is_reopened_where_it_was(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.step(cx, |_, _| ());
    assert_eq!(stored_window(&app), None);

    // Resized: written a moment later, with the display it is on.
    cx.simulate_window_resize(app.window.into(), size(px(1000.), px(700.)));
    cx.executor().advance_clock(Duration::from_secs(1));
    app.wait_until(cx, "the stored window", |app, _| {
        stored_window(app).is_some()
    });
    let stored = stored_window(&app).unwrap();
    let display = cx.update(|cx| cx.primary_display().unwrap());
    assert_eq!(
        stored.display,
        Some(display.uuid().unwrap().to_string()),
        "{stored:?}"
    );
    assert_eq!((stored.width, stored.height), (1000., 700.));
    assert_eq!(stored.state, WindowState::Windowed);

    // The next start opens it there, on that display.
    let (bounds, on) = cx.update(|cx| crate::state::window_bounds(Some(&stored), cx));
    assert_eq!(on, Some(display.id()));
    assert_eq!(
        bounds,
        WindowBounds::Windowed(Bounds::new(
            Point::new(px(stored.x), px(stored.y)),
            size(px(1000.), px(700.))
        ))
    );
    let full = WindowPlacement {
        state: WindowState::Fullscreen,
        ..stored.clone()
    };
    let (bounds, _) = cx.update(|cx| crate::state::window_bounds(Some(&full), cx));
    assert!(matches!(bounds, WindowBounds::Fullscreen(_)), "{bounds:?}");

    // Off every display, or on one that is gone, or never stored: centred
    // on the main display, as before.
    let centred = cx.update(|cx| crate::state::window_bounds(None, cx));
    assert_eq!(centred.1, None);
    for gone in [
        WindowPlacement {
            x: 5000.,
            ..stored.clone()
        },
        WindowPlacement {
            display: Some("a display unplugged since".into()),
            ..stored.clone()
        },
    ] {
        let reopened = cx.update(|cx| crate::state::window_bounds(Some(&gone), cx));
        assert_eq!(reopened, centred, "{gone:?}");
    }
}

#[gpui_kit::test]
fn a_notification_covers_no_control_of_the_tab(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::NewQueryTab), cx)
    });
    assert_eq!(app.titles(cx).len(), 1);
    app.step(cx, |window, cx| {
        window.push_notification(
            crate::error_notification("Nothing ran.\nThe detail.".into()),
            cx,
        )
    });
    // Once it has slid in.
    cx.executor().advance_clock(Duration::from_secs(1));
    app.step(cx, |_, _| ());
    let (toast, controls) = app.step(cx, |window, _| {
        let toast = window.find("notification").bounds();
        let controls: Vec<(&str, Bounds<gpui_kit::Pixels>)> = [
            "main-toolbar",
            "tab-connection",
            "tab-commit-mode",
            "tab-history",
            "tab-saved",
            "tab-explain",
            "tab-run-all",
            "status-bar",
        ]
        .into_iter()
        .map(|id| (id, window.find(id).bounds()))
        .collect();
        (toast, controls)
    });
    assert!(toast.size.width > px(100.), "{toast:?}");
    for (id, bounds) in controls {
        assert!(
            !toast.intersects(&bounds),
            "the notification {toast:?} covers {id} {bounds:?}"
        );
    }
    // At the bottom right, as DBeaver's.
    let window_size = size(px(1200.), px(800.));
    assert!(toast.right() > window_size.width - px(40.), "{toast:?}");
    assert!(toast.bottom() > window_size.height / 2., "{toast:?}");
}

/// Pushes an error notification, lets it slide in, and says which of `ids`
/// it covers.
fn covered(app: &Fixture, cx: &mut TestAppContext, ids: &[&'static str]) -> Vec<&'static str> {
    app.step(cx, |window, cx| {
        window.push_notification(
            crate::error_notification("Nothing ran.\nThe detail.".into()),
            cx,
        )
    });
    cx.executor().advance_clock(Duration::from_secs(1));
    app.step(cx, |_, _| ());
    app.step(cx, |window, _| {
        let toast = window.find("notification").bounds();
        (ids.iter().copied())
            .filter(|id| toast.intersects(&window.find(*id).bounds()))
            .collect()
    })
}

#[gpui_kit::test]
fn a_notification_leaves_the_footers_of_a_table_and_a_result_alone(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    // A result with more rows to fetch: its footer's buttons.
    app.run_in_new_tab(
        cx,
        "SELECT g FROM generate_series(1, 5000) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the result", |app, cx| app.has(cx, "fetch-all"));
    let footer = ["fetch-more", "fetch-all", "result-export", "status-bar"];
    assert_eq!(covered(&app, cx, &footer), Vec::<&str>::new());
    app.step(cx, |window, cx| window.clear_notifications(cx));

    // A table's data: its pager at the bottom right.
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_until(cx, "the rows", |app, cx| app.has(cx, "data-next"));
    let pager = ["data-page", "data-previous", "data-next", "status-bar"];
    assert_eq!(covered(&app, cx, &pager), Vec::<&str>::new());
}

#[gpui_kit::test]
fn new_sql_editor_on_a_closed_connection_connects_it_first(cx: &mut TestAppContext) {
    let storage = memory_storage();
    // Nothing listens there: the attempt fails.
    let mut refused = testing::config(Engine::Postgres);
    refused.name = "refused".into();
    refused.port = 1;
    let refused = storage.save_connection(None, &refused).unwrap();
    let mut asking = testing::config(Engine::Postgres);
    asking.name = "with a password to type".into();
    asking.save_password = false;
    storage.save_connection(None, &asking).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &refused, &testing::secrets()).unwrap();
    let new_editor = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::NewQueryTab), cx)
        })
    };
    let bound = |app: &Fixture, cx: &mut TestAppContext| {
        let tab = app.tab(cx);
        cx.update(|cx| {
            tab.read(cx)
                .target()
                .map(|target| target.connection.clone())
        })
    };

    // No connection selected: a tab bound to none, as before.
    new_editor(&app, cx);
    assert_eq!(app.titles(cx).len(), 1);
    assert_eq!(bound(&app, cx), None);
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::CloseTab), cx)
    });
    assert_eq!(app.titles(cx).len(), 0);
    let names = cx.update(|cx| {
        (app.connections.read(cx).saved().iter())
            .map(|saved| saved.config.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(names, ["refused", "with a password to type"]);

    // A connection that cannot connect: it says why, no tab stays.
    let at = 0;
    app.click(cx, ("connection", at));
    new_editor(&app, cx);
    app.wait_until(cx, "the failure", |app, cx| {
        matches!(app.status(cx, at), Status::Failed { .. })
    });
    assert_eq!(app.titles(cx), Vec::<String>::new());

    // A password to type: cancelled, no tab; typed, the tab is on it.
    let at = 1;
    app.click(cx, ("connection", at));
    new_editor(&app, cx);
    app.wait_until(cx, "the password prompt", |app, cx| {
        app.has(cx, "secret-input")
    });
    assert_eq!(app.titles(cx), Vec::<String>::new());
    app.click(cx, "secret-cancel");
    assert_eq!(app.status(cx, at), Status::Disconnected);
    assert_eq!(app.titles(cx), Vec::<String>::new());
    if !testing::available(Engine::Postgres) {
        return;
    }
    new_editor(&app, cx);
    app.wait_until(cx, "the password prompt", |app, cx| {
        app.has(cx, "secret-input")
    });
    app.type_text(cx, testing::DB_PASSWORD);
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the tab", |app, cx| app.titles(cx).len() == 1);
    assert!(matches!(app.status(cx, at), Status::Connected { .. }));
    let id = cx.update(|cx| app.connections.read(cx).saved()[at].id.clone());
    assert_eq!(bound(&app, cx), Some(id));
}
