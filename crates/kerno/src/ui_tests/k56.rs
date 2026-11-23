//! K56 (docs/plans/M12.md): seven small defects of the dialogs and the tabs
//! found by the passes in the real app.

use gpui_kit::{Bounds, Pixels};

use super::*;

fn bounds(
    app: &Fixture,
    cx: &mut TestAppContext,
    id: impl Into<gpui_kit::ElementId>,
) -> Bounds<Pixels> {
    let id = id.into();
    app.step(cx, |window, _| window.find(id).bounds())
}

/// `buttons` lie inside the dialog, and the dialog inside a window
/// `height` tall; all read from one frame.
fn in_view(
    app: &Fixture,
    cx: &mut TestAppContext,
    buttons: &[&'static str],
    height: f32,
    at: &str,
) {
    let (surface, found) = app.step(cx, |window, _| {
        // The newest dialog's: one closed just before may still be leaving.
        let surface = window
            .find_all("surface")
            .last()
            .expect("a dialog")
            .bounds();
        let found: Vec<_> = buttons.iter().map(|id| window.find(*id).bounds()).collect();
        (surface, found)
    });
    assert!(
        surface.top() >= px(0.) && surface.bottom() <= px(height),
        "{at}: {surface:?} in a window {height} px tall"
    );
    for (id, button) in buttons.iter().zip(found) {
        assert!(
            button.top() >= surface.top() && button.bottom() <= surface.bottom(),
            "{at}: {id} {button:?} out of {surface:?}"
        );
    }
}

const CONNECTION_BUTTONS: [&str; 4] =
    ["conn-test", "conn-cancel", "conn-save", "conn-save-connect"];

/// 1. In a short window, a failed test leaves the connection dialog's
/// buttons in view on every tab; its fields scroll, and so does a long
/// result in its own box.
#[gpui_kit::test]
fn the_connection_dialog_keeps_its_buttons_in_a_short_window(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    for height in [560., 700.] {
        cx.simulate_window_resize(app.window.into(), size(px(1200.), px(height)));
        app.click(cx, "empty-new-connection");
        // Nothing listens on port 1: refused at once, no server needed.
        app.click(cx, "conn-url");
        app.type_text(cx, "postgres://kerno:pw@127.0.0.1:1/kerno?sslmode=disable");
        app.press(cx, &["enter"]);
        in_view(&app, cx, &CONNECTION_BUTTONS, height, "before the test");
        app.click(cx, "conn-test");
        app.wait_until(cx, "the failed test", |app, cx| {
            app.has(cx, "conn-test-failed")
        });
        for tab in ["General", "SSL / TLS", "SSH tunnel", "Advanced"] {
            app.click_label(cx, tab);
            in_view(
                &app,
                cx,
                &CONNECTION_BUTTONS,
                height,
                &format!("{tab} at {height}"),
            );
            // The result is between the fields and the buttons.
            let failed = bounds(&app, cx, "conn-test-failed");
            let test = bounds(&app, cx, "conn-test");
            assert!(failed.top() < test.top(), "{tab} at {height}");
            // Right above them, without a blank between.
            assert!(
                test.top() - failed.bottom() < px(40.),
                "{tab} at {height}: {failed:?} {test:?}"
            );
        }
        // The fields past the bottom are reached by scrolling.
        app.click_label(cx, "General");
        let dialog = bounds(&app, cx, "conn-dialog");
        let last = bounds(&app, cx, "conn-read-only");
        if height < 600. {
            assert!(
                last.top() > bounds(&app, cx, "conn-test-failed").top(),
                "{last:?}"
            );
        }
        for _ in 0..10 {
            app.step(cx, |window, cx| {
                window.scroll(
                    "conn-dialog",
                    gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-200.))),
                    cx,
                )
            });
        }
        let last = bounds(&app, cx, "conn-read-only");
        assert!(
            last.bottom() <= bounds(&app, cx, "conn-test-failed").top()
                && last.top() >= dialog.top(),
            "scrolled into view at {height}: {last:?}"
        );
        app.click(cx, "conn-cancel");
        assert!(!app.dialog_open(cx));
    }
}

/// 1. The other tall dialogs, in the same short window: the folder dialog,
/// the connections' export and import (a file of a dozen connections), the
/// CSV import (thirty columns) and the simple backup and restore with
/// their wizards.
#[gpui_kit::test]
fn the_other_tall_dialogs_keep_their_buttons_in_a_short_window(cx: &mut TestAppContext) {
    const HEIGHT: f32 = 560.;
    let storage = memory_storage();
    for index in 0..12 {
        let mut config = testing::config(Engine::Postgres);
        config.name = format!("kerno_ui k56 {index}");
        storage.save_connection(None, &config).unwrap();
    }
    let app = open(cx, storage);
    cx.simulate_window_resize(app.window.into(), size(px(1200.), px(HEIGHT)));

    let connections = app.connections.clone();
    app.step(cx, |window, cx| {
        crate::folder_dialog::open(
            connections,
            crate::folder_dialog::Target::New(None),
            window,
            cx,
        );
    });
    in_view(&app, cx, &["name-cancel", "name-ok"], HEIGHT, "folder");
    app.press(cx, &["escape"]);

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("kerno-connections.json");
    let connections = app.connections.clone();
    let export = app.step(cx, |window, cx| {
        crate::portability::open_export(connections, window, cx)
    });
    in_view(
        &app,
        cx,
        &["portable-close", "portable-choose-file"],
        HEIGHT,
        "export",
    );
    let target = file.clone();
    app.step(cx, |window, cx| {
        export.update(cx, |dialog, cx| dialog.export_to(target, window, cx))
    });
    app.wait_until(cx, "the export", |app, cx| app.has(cx, "portable-done"));
    app.click(cx, "portable-close");
    app.import_preview(cx, file);
    in_view(
        &app,
        cx,
        &["import-cancel", "import-next"],
        HEIGHT,
        "import preview",
    );
    // Without a password to give, Next imports: the result has Close.
    app.click(cx, "import-next");
    app.wait_until(cx, "the import", |app, cx| app.has(cx, "import-done"));
    in_view(&app, cx, &["import-close"], HEIGHT, "import done");
    app.click(cx, "import-close");
    assert!(!app.dialog_open(cx));

    if !testing::available(Engine::Postgres) {
        return;
    }
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    cx.update(|cx| app.connections.update(cx, |list, cx| list.connect(&id, cx)));
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    let csv = dir.path().join("wide.csv");
    let header: Vec<String> = (0..30).map(|column| format!("column_{column}")).collect();
    let row: Vec<String> = (0..30).map(|column| column.to_string()).collect();
    std::fs::write(&csv, format!("{}\n{}\n", header.join(","), row.join(","))).unwrap();
    let import = app.import_dialog(cx, "public", None).expect("the import");
    app.import_file(cx, &import, &csv);
    in_view(
        &app,
        cx,
        &["import-close", "import-next"],
        HEIGHT,
        "CSV options",
    );
    app.wait_until(cx, "the mapping", |app, cx| {
        app.click(cx, "import-next");
        cx.update(|cx| import.read(cx).state(cx)) == "mapping"
    });
    in_view(
        &app,
        cx,
        &["import-back", "import-start"],
        HEIGHT,
        "CSV mapping",
    );
    app.press(cx, &["escape"]);

    for restore in [false, true] {
        let dialog = app.simple_dialog(cx, "kerno", restore).expect("the dialog");
        app.wait_until(cx, "the clients probed", |app, cx| {
            let state = app.wizard_state(cx, &dialog);
            !state.starts_with("prob") && !state.starts_with("prep")
        });
        let (at, start) = match restore {
            false => ("simple backup", "backup-start"),
            true => ("simple restore", "restore-start"),
        };
        in_view(
            &app,
            cx,
            &["backup-advanced", "backup-close", start],
            HEIGHT,
            at,
        );
        app.tap(cx, "backup-advanced");
        let next = if app.has(cx, "wizard-next") {
            "wizard-next"
        } else {
            start
        };
        in_view(
            &app,
            cx,
            &["backup-close", next],
            HEIGHT,
            &format!("{at}'s wizard"),
        );
        app.press(cx, &["escape"]);
    }
}

/// 2. A path typed in the restore's File field is taken once typing
/// pauses, without Enter and with the focus still in the field: a path
/// that names nothing says so, a real one makes the restore ready.
#[gpui_kit::test]
fn a_restore_takes_a_typed_path_without_enter(cx: &mut TestAppContext) {
    use kerno_core::tools::ClientFamily;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = super::tool_wizards::fake_client(
        &root,
        "pg",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        "exit 0",
    );
    let script = root.join("script.sql");
    std::fs::write(&script, "SELECT 1;\n").unwrap();
    let app = connected(cx, Engine::Postgres);
    app.use_client(cx, ClientFamily::Postgres, &client);
    let dialog = app.simple_dialog(cx, "kerno", true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    let pause = |app: &Fixture, cx: &mut TestAppContext| {
        cx.executor().advance_clock(Duration::from_millis(500));
        app.step(cx, |_, _| ());
    };

    app.click(cx, "restore-file");
    app.type_text(cx, &root.join("missing.sql").display().to_string());
    pause(&app, cx);
    let state = app.wait_backup(cx, &dialog, &["file:"]);
    assert!(state.contains("No such file: "), "{state}");
    assert!(app.has(cx, "restore-problem"));
    let disabled = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, _| window.find("restore-start").disabled()) == Some(true)
    };
    assert!(disabled(&app, cx));

    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, &script.display().to_string());
    pause(&app, cx);
    let state = app.wait_backup(cx, &dialog, &["ready", "file", "no tool", "refused"]);
    assert!(state.starts_with("ready: "), "{state}");
    assert!(!disabled(&app, cx));
    // The field kept the keyboard and its text, as typed.
    let field = cx.update(|cx| dialog.read(cx).file().map(Path::to_path_buf));
    assert_eq!(field.as_deref(), Some(script.as_path()));
    app.type_text(cx, "x");
    pause(&app, cx);
    let state = app.wait_backup(cx, &dialog, &["file:"]);
    assert!(state.contains("script.sqlx"), "{state}");
}
