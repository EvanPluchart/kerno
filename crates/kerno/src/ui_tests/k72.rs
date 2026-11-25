//! K72 (docs/plans/M12.md, D-141): "Generate test data…". The dialog opens from the tree, a
//! data tab's menu and the palette; the preview follows the settings; "Open script" opens a
//! query tab and runs nothing; "Insert" asks first, inserts in one transaction and the data tab
//! shows the rows; a read-only connection refuses it, production asks twice, Cancel inserts
//! nothing, a refused row keeps nothing; a SQLite file gets its rows too. Every test works in a
//! scratch schema `kerno_test_k72_ui_*` dropped at the end, or in a file of a temporary
//! directory.

use gpui_kit::component::WindowExt as _;

use super::*;
use crate::test_data_dialog::{self, TestDataDialog};
use kerno_core::test_data::Kind;

/// The texts drawn in the last frame.
fn drawn(app: &Fixture, cx: &mut TestAppContext) -> Vec<String> {
    app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .into_iter()
            .map(|text| text.text().to_string())
            .collect()
    })
}

/// A scratch schema with `t (id, name, price)` on PostgreSQL, the price held under 500 when
/// `checked`. Gives the schema.
fn scratch(tag: &str, checked: bool) -> (Scratch, String) {
    let schema = format!("kerno_test_k72_ui_{tag}");
    let check = if checked {
        " CONSTRAINT kerno_test_k72_cheap CHECK (price < 500)"
    } else {
        ""
    };
    let scratch = Scratch::new(
        Engine::Postgres,
        &[&format!("DROP SCHEMA IF EXISTS {schema} CASCADE")],
        &[
            &format!("CREATE SCHEMA {schema}"),
            &format!(
                "CREATE TABLE {schema}.t (id integer PRIMARY KEY, name text, \
                 price numeric(8,2) NOT NULL{check})"
            ),
        ],
    );
    (scratch, schema)
}

fn count(schema: &str) -> String {
    on_server(
        Engine::Postgres,
        &format!("SELECT count(*) FROM {schema}.t"),
    )
}

impl Fixture {
    /// Opens the dialog on `t` of `schema` (`main` and no database on SQLite) and waits for its
    /// preview.
    fn test_data(&self, cx: &mut TestAppContext, schema: &str) -> Entity<TestDataDialog> {
        let connection = cx.update(|cx| self.connections.read(cx).saved()[0].id.clone());
        let target = Target {
            connection,
            database: Some(if schema == "main" { "main" } else { "kerno" }.into()),
            schema: (schema != "main").then(|| schema.to_string()),
        };
        let (connections, workspace) = (self.connections.clone(), self.workspace.downgrade());
        let dialog = self
            .step(cx, |window, cx| {
                test_data_dialog::open(
                    connections,
                    workspace,
                    target,
                    TableRef::new(schema, "t"),
                    window,
                    cx,
                )
            })
            .expect("the dialog");
        self.wait_test_data(cx, &dialog, "ready");
        dialog
    }

    fn test_data_state(&self, cx: &mut TestAppContext, dialog: &Entity<TestDataDialog>) -> String {
        cx.update(|cx| dialog.read(cx).state())
    }

    fn wait_test_data(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<TestDataDialog>,
        state: &str,
    ) {
        self.wait_until(cx, state, |app, cx| {
            app.test_data_state(cx, dialog).starts_with(state)
        });
    }

    fn on_test_data<R>(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<TestDataDialog>,
        f: impl FnOnce(
            &mut TestDataDialog,
            &mut gpui_kit::Window,
            &mut gpui_kit::Context<TestDataDialog>,
        ) -> R,
    ) -> R {
        self.step(cx, |window, cx| {
            dialog.update(cx, |dialog, cx| f(dialog, window, cx))
        })
    }

    /// Types into a field of the dialog and waits for the preview again.
    fn type_test_data(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<TestDataDialog>,
        field: &str,
        column: &str,
        text: &str,
    ) {
        self.on_test_data(cx, dialog, |dialog, window, cx| {
            let index = if column.is_empty() {
                0
            } else {
                dialog.column(column)
            };
            dialog.type_into(field, index, text, window, cx)
        });
        self.wait_until(cx, "the preview", |app, cx| {
            !app.test_data_state(cx, dialog).starts_with("checking")
        });
    }

    fn test_data_preview(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<TestDataDialog>,
    ) -> (Vec<String>, Vec<Vec<String>>) {
        cx.update(|cx| dialog.read(cx).preview())
            .expect("a preview")
    }

    /// Whether the dialog of this table is on screen.
    fn shows_test_data(&self, cx: &mut TestAppContext, schema: &str) -> bool {
        self.has(cx, "test-data-dialog")
            && drawn(self, cx).contains(&format!("Generate test data for {schema}.t"))
    }

    fn close_dialogs(&self, cx: &mut TestAppContext) {
        self.step(cx, |window, cx| window.close_all_dialogs(cx));
    }
}

/// The tree's entry, the data tab's menu and the palette's command each open the dialog on the
/// table they were asked from.
#[gpui_kit::test]
fn generate_test_data_is_asked_from_the_tree_a_data_tab_and_the_palette(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, schema) = scratch("from", false);
    let app = connected(cx, Engine::Postgres);
    let path = ["kerno", schema.as_str(), "Tables", "t"];
    app.open_path(cx, &path);
    assert_eq!(app.why_not(cx, Action::GenerateTestData, &path), None);
    app.act(cx, Action::GenerateTestData, &path);
    assert!(app.shows_test_data(cx, &schema), "the tree's entry");
    app.close_dialogs(cx);

    app.open_table(cx, &schema, "t", RelationKind::Table);
    app.wait_for_rows(cx, "No rows");
    let index = app.active(cx).unwrap();
    app.right_click(cx, ("tab-title", index), None);
    click_menu_entry(&app, cx, "Fill with test data…");
    assert!(app.shows_test_data(cx, &schema), "the data tab's menu");
    app.close_dialogs(cx);

    app.dispatch(cx, actions::GenerateTestData);
    assert!(app.shows_test_data(cx, &schema), "the palette");
    app.close_dialogs(cx);
}

/// The preview follows the settings; "Open script" writes the statements into a query tab on
/// the table's connection and runs nothing; past the script's cap it is off.
#[gpui_kit::test]
fn the_preview_follows_the_settings_and_open_script_runs_nothing(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, schema) = scratch("script", false);
    let app = connected(cx, Engine::Postgres);
    let dialog = app.test_data(cx, &schema);
    let (names, rows) = app.test_data_preview(cx, &dialog);
    assert_eq!(names, ["id", "name", "price"]);
    assert_eq!(rows.len(), 10);
    let ids: Vec<&str> = rows.iter().map(|row| row[0].as_str()).collect();
    assert_eq!(ids, ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]);
    assert!(app.has(cx, "test-data-preview"));

    // A range of one price: every row shows it.
    app.type_test_data(cx, &dialog, "from", "price", "5");
    app.type_test_data(cx, &dialog, "to", "price", "5");
    let (_, rows) = app.test_data_preview(cx, &dialog);
    assert!(rows.iter().all(|row| row[2] == "5.00"), "{rows:?}");
    // Another seed, other names; the same seed again, the same names.
    let names_of = |rows: &[Vec<String>]| rows.iter().map(|row| row[1].clone()).collect::<Vec<_>>();
    app.type_test_data(cx, &dialog, "seed", "", "1");
    let first = names_of(&app.test_data_preview(cx, &dialog).1);
    app.type_test_data(cx, &dialog, "seed", "", "2");
    assert_ne!(names_of(&app.test_data_preview(cx, &dialog).1), first);
    app.type_test_data(cx, &dialog, "seed", "", "1");
    assert_eq!(names_of(&app.test_data_preview(cx, &dialog).1), first);
    // Another kind for the name: its values follow.
    app.on_test_data(cx, &dialog, |dialog, window, cx| {
        let index = dialog.column("name");
        dialog.set_kind(index, Kind::Null, window, cx)
    });
    app.wait_until(cx, "the NULL names", |app, cx| {
        app.test_data_preview(cx, &dialog)
            .1
            .iter()
            .all(|row| row[1] == "NULL")
    });
    // A setting the column cannot hold is said, and nothing can be made.
    app.type_test_data(cx, &dialog, "to", "price", "1000000");
    assert_eq!(
        app.test_data_state(cx, &dialog),
        "refused: “price” (numeric(8,2)) holds -999999.99 to 999999.99."
    );
    assert!(app.has(cx, "test-data-problem"));
    app.type_test_data(cx, &dialog, "to", "price", "5");

    // More rows than a script holds: Open script is off.
    app.type_test_data(cx, &dialog, "rows", "", "10,001");
    assert_eq!(app.test_data_state(cx, &dialog), "ready: 10001 rows");
    let tabs = app.titles(cx).len();
    app.click(cx, "test-data-open-script");
    assert_eq!(app.titles(cx).len(), tabs);
    app.type_test_data(cx, &dialog, "rows", "", "25");

    app.click(cx, "test-data-open-script");
    app.wait_until(cx, "the script's tab", |app, cx| {
        app.titles(cx).len() > tabs
    });
    assert!(!app.has(cx, "test-data-dialog"));
    let (title, sql) = app.active_query(cx);
    assert_eq!(title, "Test data for t");
    assert!(sql.starts_with("-- 25 rows of test data for "), "{sql}");
    assert!(
        sql.contains(&format!(
            "INSERT INTO \"{schema}\".\"t\" (\"id\", \"name\", \"price\") VALUES\n  (1, NULL, 5.00),"
        )),
        "{sql}"
    );
    assert_eq!(sql.matches("\n  (").count(), 25);
    let query = app.tab(cx);
    let target = cx.update(|cx| query.read(cx).target().cloned()).unwrap();
    assert_eq!(target.schema.as_deref(), Some(schema.as_str()));
    assert!(app.outcomes(cx).is_empty());
    assert_eq!(count(&schema), "0");
}

/// Insert on a local connection: the confirmation shows the SQL, once; the rows go in, and the
/// open data tab of the table shows them.
#[gpui_kit::test]
fn insert_on_a_local_connection_fills_the_table_and_its_data_tab(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, schema) = scratch("insert", false);
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, &schema, "t", RelationKind::Table);
    app.wait_for_rows(cx, "No rows");
    let dialog = app.test_data(cx, &schema);
    app.click(cx, "test-data-insert");
    assert!(app.has(cx, "sql-preview"));
    assert!(!app.has(cx, "sql-production"));
    app.click(cx, "sql-confirm");
    app.wait_test_data(cx, &dialog, "done");
    assert_eq!(app.test_data_state(cx, &dialog), "done: 100 rows");
    assert!(app.has(cx, "test-data-done"));
    assert_eq!(count(&schema), "100");
    app.close_dialogs(cx);
    app.wait_for_rows(cx, "Rows 1–100 of 100");
}

/// A read-only connection opens the dialog and its script, and refuses Insert; production asks
/// twice, and declining inserts nothing; Cancel during an insert keeps nothing.
#[gpui_kit::test]
fn read_only_refuses_production_asks_twice_and_cancel_keeps_nothing(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, schema) = scratch("rules", false);

    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    let dialog = app.test_data(cx, &schema);
    assert!(app.has(cx, "test-data-read-only"));
    app.click(cx, "test-data-insert");
    app.on_test_data(cx, &dialog, |dialog, window, cx| dialog.confirm(window, cx));
    assert!(!app.has(cx, "sql-preview"));
    app.click(cx, "test-data-open-script");
    app.wait_until(cx, "the script's tab", |app, cx| {
        app.titles(cx).contains(&"Test data for t".to_string())
    });
    assert_eq!(count(&schema), "0");

    let mut config = testing::config(Engine::Postgres);
    config.environment = kerno_core::environment::PRODUCTION.into();
    let app = connected_as(cx, config);
    let dialog = app.test_data(cx, &schema);
    assert!(!app.has(cx, "test-data-read-only"));
    app.click(cx, "test-data-insert");
    assert!(app.has(cx, "sql-production"));
    // The first click only arms the button.
    app.click(cx, "sql-confirm");
    assert!(app.has(cx, "sql-preview"));
    assert_eq!(app.test_data_state(cx, &dialog), "ready: 100 rows");
    app.click(cx, "sql-cancel");
    assert!(!app.has(cx, "sql-preview"));
    assert_eq!(app.test_data_state(cx, &dialog), "ready: 100 rows");
    assert_eq!(count(&schema), "0");
    // Asked again, confirmed twice: the rows go in.
    app.click(cx, "test-data-insert");
    app.click(cx, "sql-confirm");
    app.click(cx, "sql-confirm");
    app.wait_test_data(cx, &dialog, "done");
    assert_eq!(count(&schema), "100");
    app.close_dialogs(cx);

    // Cancel while the rows go in: none is kept.
    on_server(Engine::Postgres, &format!("TRUNCATE {schema}.t"));
    let app = connected(cx, Engine::Postgres);
    let dialog = app.test_data(cx, &schema);
    app.type_test_data(cx, &dialog, "rows", "", "1000000");
    app.click(cx, "test-data-insert");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the first batches", |app, cx| {
        let state = app.test_data_state(cx, &dialog);
        assert!(state.starts_with("running"), "{state}");
        state != "running: 0"
    });
    app.click(cx, "test-data-cancel");
    app.wait_test_data(cx, &dialog, "cancelled");
    assert!(app.has(cx, "test-data-cancelled"));
    assert_eq!(count(&schema), "0");
}

/// A row the server refuses: the dialog names it with the server's words, and nothing is kept.
#[gpui_kit::test]
fn a_refused_row_keeps_nothing(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, schema) = scratch("refused", true);
    let app = connected(cx, Engine::Postgres);
    let dialog = app.test_data(cx, &schema);
    assert!(app.has(cx, "test-data-notes"));
    app.click(cx, "test-data-insert");
    app.click(cx, "sql-confirm");
    app.wait_test_data(cx, &dialog, "failed");
    let state = app.test_data_state(cx, &dialog);
    assert!(
        state.starts_with("failed: row ") && state.contains("kerno_test_k72_cheap"),
        "{state}"
    );
    assert!(app.has(cx, "test-data-failed"));
    assert_eq!(count(&schema), "0");
}

/// SQLite: the palette opens the dialog on the data tab's table of the file, and Insert fills it.
#[gpui_kit::test]
fn a_sqlite_table_gets_its_rows(cx: &mut TestAppContext) {
    use kerno_core::Driver as _;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("kerno_test_k72.db");
    assert!(kerno_sqlite::new_database_file(&file).unwrap());
    let config = super::sqlite::file_config(&file);
    let params = kerno_core::config::ConnectParams {
        engine: Engine::Sqlite,
        host: String::new(),
        port: 0,
        tunnel: None,
        user: String::new(),
        password: None,
        database: None,
        file: Some(file.clone()),
        tls: Default::default(),
        read_only: false,
        options: Default::default(),
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let session = kerno_sqlite::SqliteDriver.connect(&params).await.unwrap();
            kerno_core::exec::collect(
                session.clone(),
                "CREATE TABLE t (id INT PRIMARY KEY, email TEXT NOT NULL UNIQUE, at DATETIME)",
            )
            .await
            .unwrap();
            session.close().await;
        });
    let app = connected_as(cx, config);
    app.open_table(cx, "main", "t", RelationKind::Table);
    app.wait_for_rows(cx, "No rows");
    app.dispatch(cx, actions::GenerateTestData);
    assert!(app.shows_test_data(cx, "main"));
    app.close_dialogs(cx);
    let dialog = app.test_data(cx, "main");
    app.click(cx, "test-data-insert");
    app.click(cx, "sql-confirm");
    app.wait_test_data(cx, &dialog, "done");
    assert_eq!(app.test_data_state(cx, &dialog), "done: 100 rows");
    app.close_dialogs(cx);
    app.wait_for_rows(cx, "Rows 1–100 of 100");
}

/// One answer of a query on the SQLite file, on a session of its own.
fn on_file(file: &std::path::Path, sql: &str) -> String {
    use kerno_core::Driver as _;
    let params = kerno_core::config::ConnectParams {
        engine: Engine::Sqlite,
        host: String::new(),
        port: 0,
        tunnel: None,
        user: String::new(),
        password: None,
        database: None,
        file: Some(file.to_path_buf()),
        tls: Default::default(),
        read_only: false,
        options: Default::default(),
    };
    let sql = sql.to_string();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let session = kerno_sqlite::SqliteDriver.connect(&params).await.unwrap();
            let outcome = kerno_core::exec::collect(session.clone(), sql)
                .await
                .unwrap();
            session.close().await;
            outcome
                .results
                .into_iter()
                .next()
                .and_then(|result| result.rows().into_iter().next())
                .and_then(|row| row.into_iter().next().flatten())
                .unwrap_or_default()
        })
}

impl Fixture {
    /// Insert, confirmed once, until done.
    fn insert_test_data(&self, cx: &mut TestAppContext, dialog: &Entity<TestDataDialog>) {
        self.click(cx, "test-data-insert");
        self.click(cx, "sql-confirm");
        self.wait_until(cx, "the insert", |app, cx| {
            !app.test_data_state(cx, dialog).starts_with("running")
        });
        assert!(
            self.test_data_state(cx, dialog).starts_with("done"),
            "{}",
            self.test_data_state(cx, dialog)
        );
    }
}

/// K76: the dialog says what the seed does, and it is true: the same seed on the empty table
/// twice gives the same rows; on the table those rows went into, the values that must be unique
/// continue after them, so the same settings insert again. The dialog keeps the table's last
/// settings, seed included, for the run of the app; New seed changes it.
#[gpui_kit::test]
fn the_same_seed_gives_the_same_rows_and_the_dialog_keeps_its_settings(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let schema = "kerno_test_k76_ui_same";
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[&format!("DROP SCHEMA IF EXISTS {schema} CASCADE")],
        &[
            &format!("CREATE SCHEMA {schema}"),
            &format!(
                "CREATE TABLE {schema}.t (id uuid PRIMARY KEY, n integer NOT NULL UNIQUE, \
                 code text NOT NULL UNIQUE, name text, qty integer NOT NULL)"
            ),
        ],
    );
    let rows = || {
        on_server(
            Engine::Postgres,
            &format!(
                "SELECT string_agg(concat_ws('|', id, n, code, coalesce(name, 'NULL'), qty), ',' \
                 ORDER BY n) FROM {schema}.t"
            ),
        )
    };
    let distinct = |column: &str| {
        on_server(
            Engine::Postgres,
            &format!("SELECT count(DISTINCT {column}) FROM {schema}.t"),
        )
    };
    let app = connected(cx, Engine::Postgres);
    let dialog = app.test_data(cx, schema);
    let intro = drawn(&app, cx)
        .into_iter()
        .find(|text| text.starts_with("Rows made on this computer"))
        .expect("the dialog's first words");
    assert!(
        intro.contains(
            "The same settings give the same rows, except values that must be unique, which \
             continue after the rows already in the table."
        ),
        "{intro}"
    );
    app.type_test_data(cx, &dialog, "rows", "", "20");
    app.type_test_data(cx, &dialog, "seed", "", "4242");
    app.type_test_data(cx, &dialog, "from", "qty", "1");
    app.type_test_data(cx, &dialog, "to", "qty", "9");
    let kinds = cx.update(|cx| dialog.read(cx).typed(cx)).2;
    app.insert_test_data(cx, &dialog);
    let first = rows();
    assert_eq!(first.split(',').count(), 20);
    app.close_dialogs(cx);

    // Opened again on the emptied table: the same settings, the same rows.
    on_server(Engine::Postgres, &format!("TRUNCATE {schema}.t"));
    let dialog = app.test_data(cx, schema);
    assert_eq!(
        cx.update(|cx| dialog.read(cx).typed(cx)),
        ("20".to_string(), "4242".to_string(), kinds)
    );
    let (names, preview) = app.test_data_preview(cx, &dialog);
    let qty = names
        .iter()
        .position(|n| n == "qty")
        .expect("the qty column");
    for row in preview {
        assert!(
            (1..=9).contains(&row[qty].parse::<i32>().unwrap()),
            "{row:?}"
        );
    }
    app.insert_test_data(cx, &dialog);
    assert_eq!(rows(), first);
    app.close_dialogs(cx);

    // Again on the table those rows are in: the keys continue after them, the sequence from
    // the largest number there now (its start was left as guessed).
    let dialog = app.test_data(cx, schema);
    app.insert_test_data(cx, &dialog);
    for column in ["id", "n", "code"] {
        assert_eq!(distinct(column), "40", "{column}");
    }
    assert_eq!(
        on_server(Engine::Postgres, &format!("SELECT max(n) FROM {schema}.t")),
        "40"
    );
    app.close_dialogs(cx);
    // The tree's entry finds the same settings.
    let path = ["kerno", schema, "Tables", "t"];
    app.open_path(cx, &path);
    app.act(cx, Action::GenerateTestData, &path);
    assert!(app.shows_test_data(cx, schema));
    let seed = app.step(cx, |window, _| {
        window.find("test-data-seed").value().map(str::to_string)
    });
    assert_eq!(seed.as_deref(), Some("4242"));
    app.close_dialogs(cx);
    // New seed: other rows.
    let dialog = app.test_data(cx, schema);
    assert_eq!(cx.update(|cx| dialog.read(cx).typed(cx)).1, "4242");
    app.click(cx, "test-data-new-seed");
    assert_ne!(cx.update(|cx| dialog.read(cx).typed(cx)).1, "4242");
}

/// K76: a SQLite text column that holds a date: left to its default when it has one, else a
/// date by its name; the rows go in with the server's timestamps and the dates drawn.
#[gpui_kit::test]
fn a_sqlite_text_column_of_dates_gets_dates_or_its_default(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("kerno_test_k76.db");
    assert!(kerno_sqlite::new_database_file(&file).unwrap());
    on_file(
        &file,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, created_at TEXT DEFAULT CURRENT_TIMESTAMP, \
         due_date TEXT, note TEXT)",
    );
    let app = connected_as(cx, super::sqlite::file_config(&file));
    let dialog = app.test_data(cx, "main");
    assert_eq!(
        cx.update(|cx| dialog.read(cx).typed(cx)).2,
        [Kind::Server, Kind::Default, Kind::Date, Kind::Words]
    );
    assert_eq!(app.test_data_preview(cx, &dialog).0, ["due_date", "note"]);
    app.insert_test_data(cx, &dialog);
    assert_eq!(
        on_file(
            &file,
            "SELECT count(*) FROM t WHERE created_at GLOB \
             '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9] [0-9][0-9]:[0-9][0-9]:[0-9][0-9]'"
        ),
        "100"
    );
    assert_eq!(
        on_file(
            &file,
            "SELECT count(*) FROM t WHERE due_date IS NULL OR due_date GLOB \
             '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'"
        ),
        "100"
    );
    assert_ne!(
        on_file(&file, "SELECT count(DISTINCT due_date) FROM t"),
        "1"
    );
}
