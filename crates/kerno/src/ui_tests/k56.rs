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
    // One connection gone since, for the import to write (K77: with nothing to write, Import is
    // off).
    let gone = app.storage.connections().unwrap().pop().unwrap().id;
    app.storage.delete_connection(&gone).unwrap();
    let list = app.connections.clone();
    cx.update(|cx| list.update(cx, |list, cx| list.reload(cx)));
    app.wait_until(cx, "the list", |app, cx| {
        cx.update(|cx| app.connections.read(cx).saved().len()) == 11
    });
    app.import_preview(cx, file);
    assert!(!app.has(cx, "import-nothing"));
    in_view(
        &app,
        cx,
        &["import-cancel", "import-next"],
        HEIGHT,
        "import preview",
    );
    // The connection added back asks about its password: Import, then the result has Close.
    app.click(cx, "import-next");
    in_view(
        &app,
        cx,
        &["import-back", "import-confirm"],
        HEIGHT,
        "passwords",
    );
    app.click(cx, "import-confirm");
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

/// The data tab's actions that need the server.
const SERVER_ACTIONS: [&str; 7] = [
    "data-refresh",
    "data-add-row",
    "data-duplicate",
    "data-delete",
    "data-set-null",
    "data-set-default",
    "data-export",
];

/// Whether each of `ids` is drawn greyed; `None` for one not drawn.
fn greyed(app: &Fixture, cx: &mut TestAppContext, ids: &[&'static str]) -> Vec<Option<bool>> {
    app.step(cx, |window, _| {
        ids.iter()
            .map(|id| {
                window
                    .try_find(*id)
                    .map(|found| found.disabled() == Some(true))
            })
            .collect()
    })
}

/// 3. A table tab whose connection closes keeps its rows readable, greys
/// what needs the server (fetching more, editing, filtering, exporting)
/// and refuses it from the keyboard too; Connect brings it all back.
#[gpui_kit::test]
fn a_table_tab_whose_connection_closed_offers_nothing_that_needs_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "shop", "customers", RelationKind::Table);
    app.wait_until(cx, "the rows", |app, cx| {
        app.data_state(cx).starts_with("Rows ")
    });
    let rows = app.data_rows(cx);
    app.click(cx, "table-cell-0:1");
    assert_eq!(greyed(&app, cx, &SERVER_ACTIONS), vec![Some(false); 7]);
    assert!(!app.has(cx, "data-offline"));

    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    cx.update(|cx| {
        app.connections
            .update(cx, |list, cx| list.disconnect(&id, cx))
    });
    app.wait_until(cx, "the closed connection", |app, cx| {
        app.status(cx, 0) == Status::Disconnected
    });
    // The rows stay, under a bar that says why and offers Connect.
    assert!(app.has(cx, "data-offline") && app.has(cx, "data-connect"));
    assert!(app.has(cx, "table-cell-0:1"));
    assert_eq!(app.data_rows(cx), rows);
    assert_eq!(greyed(&app, cx, &SERVER_ACTIONS), vec![Some(true); 7]);
    for id in ["filter-apply", "data-fetch-more"] {
        assert_eq!(greyed(&app, cx, &[id]), [Some(true)], "{id}");
    }
    assert!(!app.has(cx, "data-auto-refresh"));
    // The keyboard and the cell menu are refused as well: nothing becomes
    // pending.
    app.click(cx, "table-cell-0:1");
    app.type_text(cx, "x");
    app.press(cx, &["enter"]);
    let tab = app.data(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.duplicate_rows(None, window, cx);
            tab.set_selected(kerno_core::sql::CellValue::Null, window, cx);
        })
    });
    assert_eq!(app.pending(cx), 0);

    // Connect: the rows are read again and every action is back.
    app.click(cx, "data-connect");
    app.wait_until(cx, "the rows read again", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
            && app.data_state(cx).starts_with("Rows ")
    });
    assert!(!app.has(cx, "data-offline"));
    app.click(cx, "table-cell-0:1");
    assert_eq!(greyed(&app, cx, &SERVER_ACTIONS), vec![Some(false); 7]);
}

/// 3. The same tab restored after a restart: Connect, nothing that needs
/// the server offered; Connect reads the rows and offers them.
#[gpui_kit::test]
fn a_table_tab_restored_closed_offers_connect_only(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let storage = memory_storage();
    let id = storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let target = Target {
        connection: id.clone(),
        database: None,
        schema: None,
    };
    storage
        .save_tabs(&[TabSnapshot::Data {
            target,
            schema: "shop".into(),
            table: "customers".into(),
            view: false,
        }])
        .unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    assert!(app.has(cx, "data-waiting") && app.has(cx, "data-connect"));
    let shown = greyed(&app, cx, &SERVER_ACTIONS);
    assert!(shown.iter().all(|state| state != &Some(false)), "{shown:?}");
    app.click(cx, "data-connect");
    app.wait_until(cx, "the rows", |app, cx| {
        app.data_state(cx).starts_with("Rows ")
    });
    app.click(cx, "table-cell-0:1");
    assert_eq!(greyed(&app, cx, &SERVER_ACTIONS), vec![Some(false); 7]);
}

/// The connection of the active query tab.
fn tab_connection(app: &Fixture, cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| {
        let tab = app.workspace.read(cx).active_query_tab()?.read(cx);
        tab.target().map(|target| target.connection.clone())
    })
}

/// What the toolbar's "New SQL editor" says it opens on.
fn editor_tooltip(app: &Fixture, cx: &mut TestAppContext) -> String {
    app.step(cx, |window, _| {
        window
            .find("toolbar-new-editor")
            .description()
            .unwrap_or_default()
            .to_string()
    })
}

/// 4. The toolbar's "New SQL editor" opens on the connection selected in
/// the tree when the keyboard was last there (connecting it first), on the
/// active tab's when it was last in the tab; its tooltip says which.
#[gpui_kit::test]
fn the_toolbar_opens_an_editor_on_the_connection_selected_in_the_tree(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let storage = memory_storage();
    let ids: Vec<String> = ["k56 one", "k56 two"]
        .into_iter()
        .map(|name| {
            let config = ConnectionConfig {
                name: name.into(),
                ..testing::config(Engine::Postgres)
            };
            storage.save_connection(None, &config).unwrap()
        })
        .collect();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    for id in &ids {
        kerno_core::secrets::save_secrets(secrets.as_ref(), id, &testing::secrets()).unwrap();
    }
    let row = |app: &Fixture, cx: &mut TestAppContext, id: &str| {
        cx.update(|cx| {
            let connections = app.connections.read(cx);
            connections.saved().iter().position(|saved| saved.id == id)
        })
        .unwrap()
    };
    // A query tab on the first connection, the keyboard in its editor.
    let first = row(&app, cx, &ids[0]);
    app.click(cx, ("connection", first));
    app.press(cx, &["secondary-t"]);
    app.wait_until(cx, "the first tab", |app, cx| {
        tab_connection(app, cx).as_deref() == Some(ids[0].as_str())
    });
    app.click(cx, "query-editor");
    assert_eq!(editor_tooltip(&app, cx), "Open a SQL editor on “k56 one”");

    // The second selected in the tree: the toolbar opens there, connected
    // first.
    let second = row(&app, cx, &ids[1]);
    app.click(cx, ("connection", second));
    assert_eq!(editor_tooltip(&app, cx), "Open a SQL editor on “k56 two”");
    app.click(cx, "toolbar-new-editor");
    app.wait_until(cx, "the tab on the second", |app, cx| {
        tab_connection(app, cx).as_deref() == Some(ids[1].as_str())
    });
    assert_eq!(app.titles(cx).len(), 2);

    // The first selected in the tree, then the keyboard back in the tab:
    // the active tab's connection (the second) wins.
    app.click(cx, ("connection", first));
    app.click(cx, "query-editor");
    assert_eq!(editor_tooltip(&app, cx), "Open a SQL editor on “k56 two”");
    app.click(cx, "toolbar-new-editor");
    app.wait_until(cx, "a third tab", |app, cx| app.titles(cx).len() == 3);
    assert_eq!(tab_connection(&app, cx).as_deref(), Some(ids[1].as_str()));
}

/// Runs the dialog's test, which passes; the line it says of the password.
fn tested_password(app: &Fixture, cx: &mut TestAppContext) -> String {
    app.click(cx, "conn-test");
    app.wait_until(cx, "the passed test", |app, cx| {
        assert!(!app.has(cx, "conn-test-failed"));
        app.has(cx, "conn-test-passed")
    });
    let lines = [
        crate::connections::manager::TYPED_HERE,
        "Password from Kerno's file",
        "Password asked",
        "Password remembered until Kerno quits",
    ];
    lines
        .into_iter()
        .find(|line| super::tool_wizards::paints(app, cx, line))
        .unwrap_or_default()
        .to_string()
}

/// 5. A password typed in the connection dialog is said typed there; one
/// read from Kerno's file, untouched, is said to come from it; typed over
/// (even with the same text), it is typed again.
#[gpui_kit::test]
fn a_tested_password_says_where_it_really_came_from(cx: &mut TestAppContext) {
    use kerno_core::secrets::FileSecretStore;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let app = open(cx, memory_storage());
    let store: Arc<dyn kerno_core::secrets::SecretStore> =
        Arc::new(FileSecretStore::new(dir.path()));
    cx.update(|cx| cx.global_mut::<AppState>().secrets = store.clone());
    let port = testing::port(Engine::Postgres);

    // A new connection, its password typed (here through its URL).
    app.click(cx, "empty-new-connection");
    app.click(cx, "conn-url");
    app.type_text(
        cx,
        &format!(
            "postgres://kerno:{}@127.0.0.1:{port}/kerno?sslmode=disable",
            testing::DB_PASSWORD
        ),
    );
    app.press(cx, &["enter"]);
    assert_eq!(tested_password(&app, cx), "Password typed here");
    app.click(cx, "conn-name");
    app.type_text(cx, "kerno_ui k56");
    app.click(cx, "conn-save");
    app.wait_until(cx, "the saved connection", |app, _| {
        app.storage.connections().unwrap().len() == 1
    });
    let id = app.storage.connections().unwrap()[0].id.clone();
    app.wait_until(cx, "the saved password", |_, _| {
        kerno_core::secrets::load_secrets(store.as_ref(), &id)
            .is_ok_and(|secrets| secrets.password.is_some())
    });

    // Edited: the password read from Kerno's file, untouched.
    app.wait_until(cx, "the dialog closed", |app, cx| !app.dialog_open(cx));
    app.sidebar_does(
        cx,
        crate::sidebar::Subject::Connection(id.clone()),
        crate::sidebar::SidebarAction::Edit,
    );
    app.wait_until(cx, "the dialog", |app, cx| app.has(cx, "conn-test"));
    assert_eq!(tested_password(&app, cx), "Password from Kerno's file");

    // Typed over with the same text: typed here.
    app.click(cx, "conn-password");
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, testing::DB_PASSWORD);
    assert_eq!(tested_password(&app, cx), "Password typed here");
}

/// 6. Each kind of save panel opens in the folder it last saved in, its
/// own: the home folder first, then the folder chosen, the home folder
/// again once that folder is gone; another kind keeps to its own.
#[gpui_kit::test]
fn each_save_panel_opens_where_its_kind_last_saved(cx: &mut TestAppContext) {
    let home = kerno_core::home::dir().unwrap_or_default();
    let dir = tempfile::tempdir().unwrap();
    let folder = std::fs::canonicalize(dir.path()).unwrap().join("exports");
    std::fs::create_dir(&folder).unwrap();
    let app = open(cx, memory_storage());
    // The panel of the connections' export, answered with `file`; the
    // folder it opened in.
    let export = |app: &Fixture, cx: &mut TestAppContext, file: Option<std::path::PathBuf>| {
        let connections = app.connections.clone();
        app.step(cx, |window, cx| {
            crate::portability::open_export(connections, window, cx);
        });
        app.click(cx, "portable-choose-file");
        let opened = std::rc::Rc::new(std::cell::RefCell::new(std::path::PathBuf::new()));
        let seen = opened.clone();
        cx.simulate_new_path_selection(move |at| {
            *seen.borrow_mut() = at.to_path_buf();
            file
        });
        app.step(cx, |_, _| ());
        app.press(cx, &["escape"]);
        opened.take()
    };
    assert_eq!(export(&app, cx, Some(folder.join("one.json"))), home);
    app.wait_until(cx, "the folder kept", |_, cx| {
        cx.update(|cx| {
            AppState::get(cx)
                .settings
                .save_folders
                .get("connections")
                .cloned()
        })
        .as_ref()
            == Some(&folder)
    });
    assert_eq!(export(&app, cx, None), folder);
    // A query tab's "Save as…" has its own memory: the home folder.
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SaveFileAs);
    let opened = std::rc::Rc::new(std::cell::RefCell::new(std::path::PathBuf::new()));
    let seen = opened.clone();
    cx.simulate_new_path_selection(move |at| {
        *seen.borrow_mut() = at.to_path_buf();
        None
    });
    app.step(cx, |_, _| ());
    assert_eq!(opened.take(), home);
    // The folder gone: the home folder again.
    std::fs::remove_dir_all(&folder).unwrap();
    assert_eq!(export(&app, cx, None), home);
}
