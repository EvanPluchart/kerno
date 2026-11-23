//! K38: a connection's SQL scripts ▸ in its right-click menu — its saved
//! queries by folder, then the `.sql` files last used on it — opened on it,
//! connected first, never twice; the same list from the palette.

use std::path::PathBuf;

use kerno_core::storage::SavedQuery;

use super::*;
use crate::sidebar::{Entry, SCRIPTS, Script};

/// The labels of `entries`, submenus as `label ▸ [...]`, as the sidebar's
/// own tests write them.
fn labels(entries: &[Entry]) -> Vec<String> {
    entries
        .iter()
        .map(|entry| match entry {
            Entry::Item(action, None) => action.label(),
            Entry::Item(action, Some(reason)) => format!("{} ({reason})", action.label()),
            Entry::Submenu(label, entries) => {
                format!("{label} ▸ [{}]", labels(entries).join(", "))
            }
            Entry::Note(text) => format!("{text} (disabled)"),
            Entry::Separator => "—".into(),
        })
        .collect()
}

/// The SQL scripts ▸ of the connection `id`, as its menu shows it now.
fn scripts_menu(app: &Fixture, cx: &mut TestAppContext, id: &str) -> String {
    let sidebar = cx.update(|cx| app.workspace.read(cx).sidebar());
    let entries = cx.update(|cx| {
        sidebar
            .read(cx)
            .menu_of(&Subject::Connection(id.into()), cx)
    });
    labels(&entries)
        .into_iter()
        .find(|label| label.starts_with(SCRIPTS))
        .expect("the connection's menu has SQL scripts ▸")
}

fn saved(connection: Option<&str>, folder: &str, name: &str) -> SavedQuery {
    SavedQuery {
        id: 0,
        connection_id: connection.map(str::to_string),
        folder: folder.into(),
        name: name.into(),
        sql: format!("select '{name}'"),
    }
}

/// A store with one PostgreSQL connection (not connected) and its id.
fn one_connection() -> (Arc<Storage>, String) {
    let storage = memory_storage();
    let id = storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    (storage, id)
}

/// Stores settings where the connection `id` used `files`, the latest first.
fn with_files(storage: &Storage, id: &str, files: &[PathBuf]) {
    let mut settings = Settings::default();
    for path in files.iter().rev() {
        settings.remember_script(path.clone(), Some(id.to_string()));
    }
    settings.save(storage).unwrap();
}

/// Waits for the saved queries to be read off the UI thread.
fn wait_for_saved(app: &Fixture, cx: &mut TestAppContext, count: usize) {
    app.wait_until(cx, "the saved queries", |_, cx| {
        cx.update(|cx| crate::library::SavedQueries::get(cx).len() == count)
    });
}

#[gpui_kit::test]
fn the_menu_lists_saved_queries_by_folder_then_files(cx: &mut TestAppContext) {
    let (storage, id) = one_connection();
    let other = storage
        .save_connection(None, &testing::config(Engine::MySql))
        .unwrap();
    for query in [
        saved(Some(&id), "reports", "Revenue"),
        saved(Some(&id), "reports", "Churn"),
        saved(Some(&id), "", "Top customers"),
        // Another connection's, and one for any connection: not this one's.
        saved(Some(&other), "", "Elsewhere"),
        saved(None, "", "Anywhere"),
    ] {
        storage.save_query(&query).unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let files = [
        a.join("orders.sql"),
        a.join("report.sql"),
        b.join("report.sql"),
    ];
    with_files(&storage, &id, &files);
    let app = open(cx, storage);
    wait_for_saved(&app, cx, 5);

    // Two files named alike say their folder.
    assert_eq!(
        scripts_menu(&app, cx, &id),
        format!(
            "SQL scripts ▸ [reports ▸ [Churn, Revenue], Top customers, —, orders.sql, \
             report.sql — {}, report.sql — {}, —, Open SQL file…, Saved queries…]",
            a.display(),
            b.display()
        )
    );
    // Beside New SQL editor, in the real menu of the row.
    app.right_click(cx, ("connection", 0usize), None);
    click_menu_entry(&app, cx, SCRIPTS);
    click_menu_entry(&app, cx, "reports");
    for label in ["Revenue", "Churn", "Top customers", "orders.sql"] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
    assert!(menu_entry(&app, cx, "Anywhere").is_none());
    assert!(menu_entry(&app, cx, "Elsewhere").is_none());
}

#[gpui_kit::test]
fn a_connection_without_scripts_says_so_and_offers_how_to_make_one(cx: &mut TestAppContext) {
    let (storage, id) = one_connection();
    let app = open(cx, storage);
    assert_eq!(
        scripts_menu(&app, cx, &id),
        "SQL scripts ▸ [No SQL scripts yet (disabled), —, Open SQL file…, Saved queries…]"
    );
    // "Saved queries…" opens the library on this connection's.
    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::SavedQueriesHere);
    assert!(app.shows_label(cx, "This connection only"));
}

#[gpui_kit::test]
fn a_long_list_shows_the_last_saved_then_finds_the_others(cx: &mut TestAppContext) {
    let (storage, id) = one_connection();
    for n in 1..=crate::sidebar::MENU_SAVED_QUERIES + 2 {
        storage
            .save_query(&saved(Some(&id), "", &format!("q{n:02}")))
            .unwrap();
    }
    let app = open(cx, storage);
    wait_for_saved(&app, cx, crate::sidebar::MENU_SAVED_QUERIES + 2);
    // The last ten saved, by name, then the way to the others.
    assert_eq!(
        scripts_menu(&app, cx, &id),
        "SQL scripts ▸ [q03, q04, q05, q06, q07, q08, q09, q10, q11, q12, —, All scripts…, \
         Open SQL file…, Saved queries…]"
    );
    // All scripts… (and the palette's command) lists every one, to type.
    app.sidebar_does(
        cx,
        Subject::Connection(id.clone()),
        SidebarAction::AllScripts,
    );
    assert!(app.has(cx, "connection-scripts"));
    assert!(app.shows_label(cx, "q01"));
    if !testing::available(Engine::Postgres) {
        return;
    }
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    app.type_text(cx, "q01");
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the saved query's tab", |app, cx| {
        app.titles(cx) == ["q01"]
    });
    let tab = app.tab(cx);
    let target = cx.update(|cx| tab.read(cx).target().cloned());
    assert_eq!(target.map(|target| target.connection), Some(id));
}

#[gpui_kit::test]
fn a_click_connects_first_opens_on_the_connection_and_never_twice(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (storage, id) = one_connection();
    storage
        .save_query(&saved(Some(&id), "reports", "Revenue"))
        .unwrap();
    storage.save_query(&saved(Some(&id), "", "Churn")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kerno_test_scripts.sql");
    std::fs::write(&path, "select 42").unwrap();
    with_files(&storage, &id, std::slice::from_ref(&path));
    let app = open(cx, storage);
    wait_for_saved(&app, cx, 2);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    assert!(matches!(app.status(cx, 0), Status::Disconnected));

    // The file, from the real menu: the connection opens, then the tab.
    app.right_click(cx, ("connection", 0usize), None);
    click_menu_entry(&app, cx, SCRIPTS);
    click_menu_entry(&app, cx, "kerno_test_scripts.sql");
    app.wait_until(cx, "the file's tab", |app, cx| app.titles(cx).len() == 1);
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    let tab = app.tab(cx);
    let (target, file) = cx.update(|cx| {
        let tab = tab.read(cx);
        (tab.target().cloned(), tab.file().map(PathBuf::from))
    });
    assert_eq!(target.map(|target| target.connection), Some(id.clone()));
    assert_eq!(file, Some(path.clone()));
    assert_eq!(app.active_query(cx).1, "select 42");

    // The saved query, then each again: the tab that holds it is shown.
    let open_saved = |app: &Fixture, cx: &mut TestAppContext, name: &str| {
        let query = cx.update(|cx| {
            (crate::library::SavedQueries::get(cx).iter())
                .find(|query| query.name == name)
                .unwrap()
                .id
        });
        let script = Script::Saved {
            id: query,
            name: name.into(),
        };
        app.sidebar_does(
            cx,
            Subject::Connection(id.clone()),
            SidebarAction::OpenScript(script),
        );
    };
    open_saved(&app, cx, "Revenue");
    app.wait_until(cx, "the saved query's tab", |app, cx| {
        app.titles(cx).len() == 2
    });
    assert_eq!(app.active_query(cx).1, "select 'Revenue'");
    let file = Script::File {
        path: path.clone(),
        name: "kerno_test_scripts.sql".into(),
    };
    app.sidebar_does(
        cx,
        Subject::Connection(id.clone()),
        SidebarAction::OpenScript(file),
    );
    app.wait_until(cx, "the file's tab shown", |app, cx| {
        app.active(cx) == Some(0)
    });
    open_saved(&app, cx, "Revenue");
    assert_eq!(app.active(cx), Some(1));
    assert_eq!(
        app.titles(cx),
        ["kerno_test_scripts.sql", "Revenue"],
        "nothing opened twice"
    );

    // A tab saved as a query keeps its own title: its text finds it.
    let (workspace, target) = (
        app.workspace.clone(),
        Target {
            connection: id.clone(),
            database: None,
            schema: None,
        },
    );
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.open_query("Query 7".into(), "select 'Churn'", Some(target), window, cx)
        });
    });
    open_saved(&app, cx, "Churn");
    assert_eq!(app.active(cx), Some(2));
    assert_eq!(app.titles(cx).len(), 3, "the tab that holds it is shown");
}

#[gpui_kit::test]
fn a_file_gone_since_is_said_leaves_the_lists_and_connects_nothing(cx: &mut TestAppContext) {
    let (storage, id) = one_connection();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gone.sql");
    with_files(&storage, &id, std::slice::from_ref(&path));
    let app = open(cx, storage);
    assert!(scripts_menu(&app, cx, &id).contains("gone.sql"));
    let script = Script::File {
        path: path.clone(),
        name: "gone.sql".into(),
    };
    app.sidebar_does(
        cx,
        Subject::Connection(id.clone()),
        SidebarAction::OpenScript(script),
    );
    app.wait_until(cx, "the missing file dropped", |_, cx| {
        !settings_now(cx).connection_scripts.contains_key(&id)
    });
    assert!(settings_now(cx).recent_scripts.is_empty());
    assert!(app.titles(cx).is_empty());
    assert!(matches!(app.status(cx, 0), Status::Disconnected));
    assert!(scripts_menu(&app, cx, &id).contains("No SQL scripts yet"));
}

#[gpui_kit::test]
fn a_copy_starts_without_scripts_and_a_deleted_connection_leaves_none(cx: &mut TestAppContext) {
    let (storage, id) = one_connection();
    storage
        .save_query(&saved(Some(&id), "", "Revenue"))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kerno_test_mine.sql");
    std::fs::write(&path, "select 1").unwrap();
    with_files(&storage, &id, std::slice::from_ref(&path));
    let app = open(cx, storage);
    wait_for_saved(&app, cx, 1);

    app.sidebar_does(
        cx,
        Subject::Connection(id.clone()),
        SidebarAction::Duplicate,
    );
    app.wait_until(cx, "the copy", |app, cx| {
        cx.update(|cx| app.connections.read(cx).saved().len() == 2)
    });
    let copy = cx.update(|cx| {
        (app.connections.read(cx).saved().iter())
            .find(|saved| saved.id != id)
            .unwrap()
            .id
            .clone()
    });
    assert!(scripts_menu(&app, cx, &copy).contains("No SQL scripts yet"));

    // An export holds no path of the user's (J15).
    let file = dir.path().join("connections.json");
    let (workspace, target) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.export_connections_to(target, window, cx)
        });
    });
    app.wait_until(cx, "the export", |_, _| file.exists());
    let exported = std::fs::read_to_string(&file).unwrap();
    assert!(!exported.contains("kerno_test_mine"), "{exported}");

    // Deleted: its list and its saved queries go; nothing shows elsewhere.
    let deleting = app.connections.clone();
    cx.update(|cx| deleting.update(cx, |connections, cx| connections.delete(&id, cx)));
    app.wait_until(cx, "the deletion", |app, cx| {
        cx.update(|cx| app.connections.read(cx).saved().len() == 1)
    });
    wait_for_saved(&app, cx, 0);
    let settings = settings_now(cx);
    assert!(!settings.connection_scripts.contains_key(&id));
    assert!(!settings.script_connections.values().any(|of| *of == id));
    assert!(scripts_menu(&app, cx, &copy).contains("No SQL scripts yet"));
}
