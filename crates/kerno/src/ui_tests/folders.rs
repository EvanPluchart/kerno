//! Folders have a colour and an icon (CLAUDE.md §3, owner's test of
//! 2026-11-13, item 3): set in the New folder dialog and in Edit folder…
//! (F4), drawn on the folder's row, kept in the settings, exported without
//! their picture.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kerno_core::config::FolderLook;

use super::*;
use crate::folder_dialog::{self, FolderDialog};

/// The app with one PostgreSQL connection saved in the folder `Work`.
fn with_work_folder(cx: &mut TestAppContext) -> Fixture {
    let storage = memory_storage();
    let config = ConnectionConfig {
        group: Some("Work".into()),
        ..testing::config(Engine::Postgres)
    };
    storage.save_connection(None, &config).unwrap();
    open(cx, storage)
}

fn look(color: Option<&str>, glyph: Option<&str>, image: Option<&str>) -> FolderLook {
    FolderLook {
        color: color.map(str::to_string),
        glyph: glyph.map(str::to_string),
        image: image.map(str::to_string),
    }
}

impl Fixture {
    fn looks(&self, cx: &mut TestAppContext) -> BTreeMap<String, FolderLook> {
        cx.update(|cx| AppState::get(cx).settings.folder_looks.clone())
    }

    fn folder_icons(&self, cx: &mut TestAppContext) -> PathBuf {
        cx.update(|cx| AppState::get(cx).paths.folder_icons())
    }

    /// Opens Edit folder… on `path`, as its menu entry does.
    fn edit_folder(&self, cx: &mut TestAppContext, path: &str) -> Entity<FolderDialog> {
        let (connections, path) = (self.connections.clone(), path.to_string());
        self.step(cx, move |window, cx| {
            folder_dialog::open(connections, folder_dialog::Target::Edit(path), window, cx)
        })
    }

    /// Hands `path` to the dialog as the picker does, and waits for it.
    fn pick_picture(&self, cx: &mut TestAppContext, dialog: &Entity<FolderDialog>, path: PathBuf) {
        let picking = dialog.clone();
        self.step(cx, move |_, cx| {
            picking.update(cx, |dialog, cx| dialog.load_picture(path, cx))
        });
        self.wait_until(cx, "the picture to be read", |_, cx| {
            cx.update(|cx| !dialog.read(cx).reading())
        });
    }

    fn name_shown(&self, cx: &mut TestAppContext) -> Option<String> {
        self.step(cx, |window, _| {
            window
                .find("name-input")
                .value()
                .map(|value| value.to_string())
        })
    }
}

#[gpui_kit::test]
fn a_new_folder_gets_a_colour_and_a_glyph(cx: &mut TestAppContext) {
    let storage = memory_storage();
    storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let app = open(cx, storage);

    app.right_click_below_the_tree(cx);
    app.click_label(cx, "New folder…");
    assert!(app.has(cx, "folder-dialog"));
    app.type_text(cx, "Clients");
    app.click(cx, "folder-color-blue");
    app.click(cx, "folder-glyph-briefcase");
    app.click(cx, "name-ok");
    assert!(!app.has(cx, "folder-dialog"));

    let expected = BTreeMap::from([(
        "Clients".to_string(),
        look(Some("blue"), Some("briefcase"), None),
    )]);
    assert_eq!(app.looks(cx), expected);
    // The row draws the glyph in the colour; no dot, no bar beside it.
    assert!(app.has(cx, ("conn-folder", 0usize)));
    assert!(app.has(cx, ("folder-glyph", 0usize)));
    assert!(!app.has(cx, ("folder-dot", 0usize)));
    assert!(!app.has(cx, ("folder-picture", 0usize)));
    // Stored: the next run draws it the same.
    app.wait_until(cx, "the look to be stored", |app, _| {
        Settings::load(&app.storage).folder_looks == expected
    });

    // The plain folder glyph and no colour make the folder plain again.
    let dialog = app.edit_folder(cx, "Clients");
    assert_eq!(
        cx.update(|cx| dialog.read(cx).look().clone()),
        look(Some("blue"), Some("briefcase"), None)
    );
    app.click(cx, "folder-color-none");
    app.click(cx, "folder-glyph-folder");
    app.click(cx, "name-ok");
    assert_eq!(app.looks(cx), BTreeMap::new());
}

#[gpui_kit::test]
fn edit_folder_sets_a_custom_image(cx: &mut TestAppContext) {
    let app = with_work_folder(cx);
    // The folder's menu offers it.
    app.right_click(cx, ("conn-folder", 0usize), None);
    assert!(app.shows_label(cx, "Edit folder…"));
    app.click_label(cx, "Edit folder…");
    assert_eq!(app.name_shown(cx).as_deref(), Some("Work"));
    app.click(cx, "name-cancel");

    let source = tempfile::tempdir().unwrap();
    let logo = source.path().join("logo.png");
    image::RgbaImage::from_pixel(1000, 1000, image::Rgba([200, 240, 60, 255]))
        .save_with_format(&logo, image::ImageFormat::Png)
        .unwrap();
    let dialog = app.edit_folder(cx, "Work");
    app.pick_picture(cx, &dialog, logo);
    let file = cx
        .update(|cx| dialog.read(cx).look().image.clone())
        .expect("the picture is taken");
    assert!(app.has(cx, "folder-picture"), "the dialog shows it");
    let stored = app.folder_icons(cx).join(&file);
    assert_eq!(
        image::open(&stored).unwrap().into_rgba8().dimensions(),
        (32, 32)
    );

    // A file that is not a picture says so and changes nothing.
    let fake = source.path().join("fake.svg");
    std::fs::write(&fake, "not an svg").unwrap();
    app.pick_picture(cx, &dialog, fake);
    assert!(app.has(cx, "folder-picture-error"));
    assert_eq!(
        cx.update(|cx| dialog.read(cx).look().image.clone())
            .as_deref(),
        Some(file.as_str())
    );

    // With a colour, the row draws the picture and a dot.
    app.click(cx, "folder-color-green");
    app.click(cx, "name-ok");
    assert_eq!(
        app.looks(cx),
        BTreeMap::from([("Work".to_string(), look(Some("green"), None, Some(&file)))])
    );
    assert!(app.has(cx, ("folder-picture", 0usize)));
    assert!(app.has(cx, ("folder-dot", 0usize)));
    assert!(!app.has(cx, ("folder-glyph", 0usize)));

    // Renamed, it keeps its look; without its picture, the file goes.
    let dialog = app.edit_folder(cx, "Work");
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "Job");
    app.click(cx, "folder-picture-remove");
    assert!(!app.has(cx, "folder-picture"));
    app.click(cx, "name-ok");
    drop(dialog);
    assert_eq!(
        cx.update(|cx| app.connections.read(cx).folders(cx)),
        ["Job"]
    );
    assert_eq!(
        app.looks(cx),
        BTreeMap::from([("Job".to_string(), look(Some("green"), None, None))])
    );
    app.wait_until(cx, "the unused picture to be removed", |_, _| {
        !stored.exists()
    });
}

#[gpui_kit::test]
fn f4_on_a_folder_opens_edit_folder(cx: &mut TestAppContext) {
    let app = with_work_folder(cx);
    app.click(cx, ("conn-folder", 0usize));
    app.press(cx, &["f4"]);
    assert!(app.has(cx, "folder-dialog"), "F4 opens Edit folder…");
    assert_eq!(app.name_shown(cx).as_deref(), Some("Work"));
    app.click(cx, "name-cancel");

    // F2 still renames it, by name only.
    app.click(cx, ("conn-folder", 0usize));
    app.press(cx, &["f2"]);
    assert!(app.has(cx, "name-input") && !app.has(cx, "folder-dialog"));
    app.click(cx, "name-cancel");
}

#[gpui_kit::test]
fn folder_looks_travel_with_exported_connections(cx: &mut TestAppContext) {
    let app = with_work_folder(cx);
    let picture = app.folder_icons(cx).join("00000000000000aa.png");
    std::fs::create_dir_all(picture.parent().unwrap()).unwrap();
    std::fs::write(&picture, b"picture").unwrap();
    let set = |app: &Fixture, cx: &mut TestAppContext, look: FolderLook| {
        cx.update(|cx| {
            app.connections
                .update(cx, |c, cx| c.set_folder_look("Work", look, cx))
        })
    };
    set(
        &app,
        cx,
        look(Some("blue"), Some("server"), Some("00000000000000aa.png")),
    );

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("connections.json");
    let (workspace, target) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.export_connections_to(target, window, cx)
        });
    });
    app.wait_until(cx, "the export", |_, _| file.exists());
    let exported = std::fs::read_to_string(&file).unwrap();
    assert!(
        exported.contains("\"server\"") && !exported.contains(".png"),
        "{exported}"
    );

    let import = |app: &Fixture, cx: &mut TestAppContext, count: usize| {
        let (workspace, source) = (app.workspace.clone(), file.clone());
        app.step(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.import_connections_from(source, window, cx)
            });
        });
        app.wait_until(cx, "the import", |app, cx| {
            cx.update(|cx| app.connections.read(cx).saved().len() == count)
        });
    };
    // A folder without a look takes the file's: colour and glyph.
    set(&app, cx, FolderLook::default());
    import(&app, cx, 2);
    assert_eq!(
        app.looks(cx),
        BTreeMap::from([("Work".to_string(), look(Some("blue"), Some("server"), None))])
    );
    // One that has a look keeps it.
    set(&app, cx, look(Some("red"), None, None));
    import(&app, cx, 3);
    assert_eq!(
        app.looks(cx),
        BTreeMap::from([("Work".to_string(), look(Some("red"), None, None))])
    );
}
