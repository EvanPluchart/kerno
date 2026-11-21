//! SQLite (K29): a connection that is a database file, made in the dialog,
//! browsed, queried, edited and altered, on files these tests create in
//! temporary directories, never a file of the user's.

use std::path::PathBuf;

use kerno_core::Driver as _;
use kerno_core::config::Secrets;
use kerno_core::exec;

use super::tool_wizards::paints;
use super::*;

/// The fixture schema of the driver's tests, in a new file of `dir`.
fn fixture_file(dir: &Path) -> PathBuf {
    let file = dir.join("shop.db");
    assert!(kerno_sqlite::new_database_file(&file).unwrap());
    let script = std::fs::read_to_string(testing::fixtures().join("sqlite/schema.sql")).unwrap();
    let path = file.clone();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let session = kerno_sqlite::SqliteDriver
                .connect(&kerno_core::config::ConnectParams {
                    engine: Engine::Sqlite,
                    host: String::new(),
                    port: 0,
                    tunnel: None,
                    user: String::new(),
                    password: None,
                    database: None,
                    file: Some(path),
                    tls: Default::default(),
                    read_only: false,
                    options: Default::default(),
                })
                .await
                .unwrap();
            exec::collect(session.clone(), script).await.unwrap();
            session.close().await;
        });
    file
}

fn file_config(file: &Path) -> ConnectionConfig {
    ConnectionConfig {
        name: "Shop file".into(),
        file: Some(file.to_path_buf()),
        ..ConnectionConfig::new(Engine::Sqlite)
    }
}

/// Acceptance 1: the dialog shows the file and none of a server's fields;
/// the test opens the file and names SQLite's version; saving keeps the
/// path and no secret.
#[gpui_kit::test]
fn a_database_file_connection_is_made_tested_and_opened(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("new.db");
    assert!(kerno_sqlite::new_database_file(&file).unwrap());
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-n"]);
    app.click(cx, "engine-sqlite");
    assert!(app.has(cx, "conn-file") && app.has(cx, "conn-file-new"));
    for hidden in [
        "conn-url",
        "conn-host",
        "conn-port",
        "conn-user",
        "conn-password",
    ] {
        assert!(!app.has(cx, hidden), "{hidden} is hidden for a file");
    }
    assert!(!paints(&app, cx, "SSL / TLS") && !paints(&app, cx, "SSH tunnel"));

    // A missing file says so in plain words, SQLite's text below.
    app.click(cx, "conn-file");
    app.type_text(cx, &dir.path().join("missing.db").display().to_string());
    app.click(cx, "conn-test");
    app.wait_until(cx, "the failed test", |app, cx| {
        app.has(cx, "conn-test-failed")
    });
    assert!(paints(&app, cx, "does not exist."));
    assert!(!dir.path().join("missing.db").exists(), "never created");

    app.click(cx, "conn-file");
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, &file.display().to_string());
    app.click(cx, "conn-test");
    app.wait_until(cx, "the test", |app, cx| app.has(cx, "conn-test-passed"));
    assert!(paints(&app, cx, "SQLite 3."));
    app.click(cx, "conn-save-connect");
    app.wait_until(cx, "the connection", |app, cx| {
        let listed = cx.update(|cx| !app.connections.read(cx).saved().is_empty());
        listed && matches!(app.status(cx, 0), Status::Connected { .. })
    });
    let stored = app.storage.connections().unwrap();
    assert_eq!(stored[0].config.engine, Engine::Sqlite);
    assert_eq!(stored[0].config.file.as_deref(), Some(file.as_path()));
    assert_eq!(stored[0].config.name, "new.db", "named after its file");
    let store = cx.update(|cx| AppState::get(cx).secrets.clone());
    assert_eq!(
        kerno_core::secrets::load_secrets(store.as_ref(), &stored[0].id).unwrap(),
        Secrets::default(),
        "no secret is kept for a file"
    );
}

/// Acceptance 2 to 5: the tree lists the file's tables, a query returns
/// rows, a long one is cancelled, a cell is edited and applied, and a table
/// without a key says why it is read-only.
#[gpui_kit::test]
fn the_fixture_file_is_browsed_queried_and_edited(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = fixture_file(dir.path());
    let app = connected_as(cx, file_config(&file));
    // The one database, `main`, opens on its folders.
    app.wait_until(cx, "the folders", |app, cx| {
        app.tree(cx)
            .iter()
            .any(|row| row.trim().starts_with("Tables"))
    });
    assert_eq!(app.tree(cx)[0], "main", "{:#?}", app.tree(cx));
    // A database file cannot make another database.
    assert!(app.why_not(cx, Action::CreateDatabase, &["main"]).is_some());
    // The compare has no SQLite side yet: its entry says so (it did nothing).
    assert!(app.why_not(cx, Action::CompareWith, &["main"]).is_some());

    app.run_in_new_tab(
        cx,
        "SELECT email, vip FROM customers ORDER BY id",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["3 rows"]);
    let (_, rows) = app.result(cx, 0, 5);
    assert_eq!(rows[0], cells(&["ada@example.com", "1"]));

    let long = "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n \
                WHERE x < 5000000000) SELECT count(*) FROM n";
    app.run_in_new_tab(cx, long, "secondary-enter");
    // The window keeps working while the tab's session opens and the
    // statement starts on it (a file has no server to ask whether it runs).
    let started = std::time::Instant::now();
    app.wait_until(cx, "the statement to start", |app, cx| {
        app.running(cx) && started.elapsed() > Duration::from_millis(700)
    });
    app.press(cx, &["secondary-."]);
    app.wait_until(cx, "the cancellation", |app, cx| !app.running(cx));
    assert_eq!(app.outcomes(cx), ["cancelled"]);

    app.open_table(cx, "main", "customers", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–3 of 3");
    // Columns: the row number, id, email, name, created_at, vip.
    app.type_in_cell(cx, 1, 3, "Bob", "enter");
    assert_eq!(app.pending(cx), 1);
    app.click(cx, "data-apply");
    assert!(app.has(cx, "sql-preview"));
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the applied change", |app, cx| {
        let tab = app.data(cx);
        let error = cx.update(|cx| tab.read(cx).apply_error().map(str::to_string));
        assert_eq!(error, None);
        app.pending(cx) == 0 && app.data_state(cx) == "Rows 1–3 of 3"
    });
    // What the grid draws starts at id: no row number there.
    assert_eq!(app.data_rows(cx)[1][2], "Bob");

    app.open_table(cx, "main", "events", RelationKind::Table);
    app.wait_until(cx, "the keyless table", |app, cx| {
        app.data_state(cx).starts_with("Rows")
    });
    let tab = app.data(cx);
    let reason = cx.update(|cx| tab.read(cx).read_only_reason(cx));
    assert!(
        reason.is_some_and(|reason| reason.contains("no primary key")),
        "a table without a key is read-only, and says why"
    );
}

/// Acceptance 6: what SQLite's ALTER TABLE cannot do is refused in plain
/// words before anything runs; moving a column is refused too.
#[gpui_kit::test]
fn an_alter_sqlite_cannot_do_is_refused_with_its_reason(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = fixture_file(dir.path());
    let app = connected_as(cx, file_config(&file));
    app.open_structure(cx, "main", "orders", RelationKind::Table);
    app.wait_for_structure(cx);
    app.click(cx, "structure-edit");
    app.wait_for_editor(cx);
    assert!(paints(&app, cx, "SQLite cannot move a column"));
    // SQLite has no comments: the form shows no field for one.
    assert!(app.has(cx, "editor-name") && app.has(cx, ("column-name", 0usize)));
    assert!(!app.has(cx, "editor-comment") && !app.has(cx, ("column-comment", 0usize)));
    // Columns: 0 id, 1 customer_id, 2 placed_on, 3 note.
    app.fill_in(cx, ("column-type", 3usize), "INTEGER");
    let editor = app.editor(cx);
    let problems = cx
        .update(|cx| editor.read(cx).statements(cx))
        .expect_err("a type change is refused");
    assert!(
        problems
            .iter()
            .any(|problem| problem.contains("SQLite cannot change the type of column “note”")),
        "{problems:?}"
    );
}
