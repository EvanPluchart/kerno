//! K80 (docs/plans/M12.md, D-144): what the fourth pass in the real app found in K77's
//! dialogs. The "nothing to import" sentence ran out of the dialog's left edge, and the
//! DBeaver import's "To check" notes were drawn in the success colour.

use std::path::PathBuf;

use kerno_core::dbeaver;

use super::*;

/// The newest dialog's bounds and `id`'s, from one frame.
fn inside_dialog(app: &Fixture, cx: &mut TestAppContext, id: &'static str, at: &str) {
    let (surface, element) = app.step(cx, |window, _| {
        let surface = window
            .find_all("surface")
            .last()
            .expect("a dialog")
            .bounds();
        (surface, window.find(id).bounds())
    });
    assert!(
        element.left() >= surface.left() && element.right() <= surface.right(),
        "{at}: {id} {element:?} out of the dialog {surface:?}"
    );
}

/// Each width the dialog takes: its own 640 px, at the window's smallest width (720 px) too,
/// and narrower where the window clamps it.
const WIDTHS: [f32; 3] = [1280., 720., 600.];

#[gpui_kit::test]
fn the_dbeaver_nothing_to_import_sentence_wraps_inside_the_dialog(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("General.dbp");
    std::fs::write(&path, dbeaver::sample_project()).unwrap();
    let view = app.dbeaver_preview(cx, path.clone());
    app.dbeaver_import(cx, &view);
    app.click(cx, "dbeaver-close");
    app.dbeaver_preview(cx, path);
    assert_eq!(
        super::ai_endpoints::text_of(&app, cx, "dbeaver-nothing"),
        "Nothing to import: every connection Kerno can take from this file is already here."
    );
    for width in WIDTHS {
        cx.simulate_window_resize(app.window.into(), size(px(width), px(800.)));
        cx.run_until_parked();
        for id in ["dbeaver-nothing", "dbeaver-cancel", "dbeaver-import"] {
            inside_dialog(&app, cx, id, &format!("{width} px"));
        }
    }
}

#[gpui_kit::test]
fn the_connections_file_nothing_to_import_sentence_wraps_inside_the_dialog(
    cx: &mut TestAppContext,
) {
    let storage = memory_storage();
    storage
        .save_connection(None, &testing::config(Engine::MySql))
        .unwrap();
    let app = open(cx, storage);
    let dir = tempfile::tempdir().unwrap();
    let file: PathBuf = dir.path().join("connections.json");
    let (workspace, target) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.export_connections_to(target, window, cx)
        });
    });
    app.wait_until(cx, "the export", |_, _| file.exists());
    app.step(cx, |window, cx| {
        gpui_kit::component::WindowExt::close_dialog(window, cx)
    });
    // The same connection, renamed since, Replace unticked: its longest sentence.
    let name = format!("\"{}\"", testing::config(Engine::MySql).name);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains(&name), "{text}");
    std::fs::write(&file, text.replacen(&name, "\"kerno_test_k80 renamed\"", 1)).unwrap();
    let view = app.import_preview(cx, file);
    cx.update(|cx| view.update(cx, |view, cx| view.set_replace(false, cx)));
    assert_eq!(
        super::ai_endpoints::text_of(&app, cx, "import-nothing"),
        "Nothing to import: tick “Replace” to write the items with the same id."
    );
    for width in WIDTHS {
        cx.simulate_window_resize(app.window.into(), size(px(width), px(800.)));
        cx.run_until_parked();
        for id in ["import-nothing", "import-cancel", "import-next"] {
            inside_dialog(&app, cx, id, &format!("{width} px"));
        }
    }
}

/// The report's first line says what was written; the notes to check come below it, apart,
/// in the warning colour (the capture of K80 shows the colours).
#[gpui_kit::test]
fn the_dbeaver_report_keeps_its_notes_apart_from_what_was_imported(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data-sources.json");
    let json = serde_json::json!({
        "connections": {
            "a": {"provider": "postgresql", "driver": "postgres-jdbc", "name": "Hop",
                "configuration": {"host": "db.internal", "user": "u", "handlers": {
                    "postgre_ssl": {"enabled": true, "properties": {"sslMode": "require",
                        "ssl.ca.cert.value": "-----BEGIN"}}}}}
        }
    });
    std::fs::write(&path, json.to_string()).unwrap();
    let view = app.dbeaver_preview(cx, path);
    app.dbeaver_import(cx, &view);
    assert_eq!(
        super::ai_endpoints::text_of(&app, cx, "dbeaver-done"),
        "Imported 1 connection and 0 saved queries."
    );
    let notes = super::ai_endpoints::text_of(&app, cx, "dbeaver-to-check");
    assert!(
        notes.starts_with("To check:\n• Hop: certificate pasted"),
        "{notes}"
    );
}
