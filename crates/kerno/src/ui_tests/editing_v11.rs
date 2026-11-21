//! J5 (M11 Part 3), on PostgreSQL, MySQL and MariaDB: rows keyed by a uuid
//! and by two columns edited in a table's data, a cell given a SQL
//! expression (`now()`, `DEFAULT`, a new uuid) that the review shows
//! unquoted, and the row read again with what the server made of it.
//! Demand: DBeaver #9964 and #10048, Beekeeper #2313 and #3029.

use kerno_core::sql::CellValue;

use super::*;

/// The current time on `engine`, as a user types it.
fn now(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "now()",
        Engine::MySql | Engine::MariaDb => "NOW()",
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    }
}

/// A new uuid on `engine`.
fn new_uuid(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "gen_random_uuid()",
        Engine::MySql | Engine::MariaDb => "UUID()",
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    }
}

/// A uuid column on `engine`.
fn uuid_type(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "uuid",
        Engine::MySql | Engine::MariaDb => "CHAR(36)",
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    }
}

impl Fixture {
    /// The pending value of a cell of the active data tab (`source`: the
    /// column of the result, without the row numbers).
    fn pending_at(&self, cx: &mut TestAppContext, row: usize, source: usize) -> Option<CellValue> {
        let grid = self.data_grid(cx);
        cx.update(|cx| {
            let grid = grid.read(cx).delegate();
            grid.pending()?
                .value(grid.row_count(), row, source)
                .cloned()
        })
    }

    /// The statements "Review SQL" shows for the active data tab.
    fn review_sql(&self, cx: &mut TestAppContext) -> Vec<String> {
        let tab = self.data(cx);
        cx.update(|cx| tab.read(cx).pending_sql(cx)).unwrap().1
    }

    /// Opens a cell of the active data tab, replaces its text with `sql`
    /// and makes it an expression with ⌘E, then Enter.
    fn type_expression(&self, cx: &mut TestAppContext, row: usize, column: usize, sql: &str) {
        self.on_data(cx, |tab, window, cx| tab.edit_cell(row, column, window, cx));
        self.press(cx, &["secondary-a", "backspace"]);
        self.type_text(cx, sql);
        self.press(cx, &["secondary-e", "enter"]);
    }

    /// Reviews and applies the pending changes of the active data tab,
    /// and waits until they are written and the rows read again.
    fn apply_and_wait(&self, cx: &mut TestAppContext, rows: &str) {
        self.click(cx, "data-apply");
        assert!(self.has(cx, "sql-preview"));
        self.click(cx, "sql-confirm");
        self.wait_until(cx, "the applied changes", |app, cx| {
            let tab = app.data(cx);
            let error = cx.update(|cx| tab.read(cx).apply_error().map(str::to_string));
            assert_eq!(error, None);
            app.pending(cx) == 0 && app.data_state(cx) == rows
        });
    }
}

#[gpui_kit::test]
fn rows_keyed_by_a_uuid_or_by_two_columns_are_edited_and_applied(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let server = |sql: &str| on_server(engine, sql);
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_keys_v11");
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &format!(
                    "CREATE TABLE kerno_ui_keys_v11.by_uuid (id {} PRIMARY KEY, name VARCHAR(20))",
                    uuid_type(engine)
                ),
                "INSERT INTO kerno_ui_keys_v11.by_uuid VALUES \
                 ('00000000-0000-0000-0000-000000000001', 'ann'), \
                 ('6f1c3a52-0f7e-4c1d-9a3b-2b8e5d4c7a10', 'bob')",
                "CREATE TABLE kerno_ui_keys_v11.by_pair (a INT, b VARCHAR(20), name VARCHAR(20), \
                 PRIMARY KEY (a, b))",
                "INSERT INTO kerno_ui_keys_v11.by_pair VALUES (1, 'x', 'ann'), (1, 'y', 'bob')",
            ],
        );
        let app = connected(cx, engine);

        // A row found by its uuid: a cell, and the key of the other row.
        app.open_table(cx, "kerno_ui_keys_v11", "by_uuid", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(
            app.data_rows(cx)[0],
            ["00000000-0000-0000-0000-000000000001", "ann"]
        );
        app.type_in_cell(cx, 0, 2, "anna", "enter");
        app.type_in_cell(cx, 1, 1, "ffffffff-ffff-ffff-ffff-ffffffffffff", "enter");
        assert_eq!(app.pending(cx), 2);
        app.apply_and_wait(cx, "Rows 1–2 of 2");
        assert_eq!(
            server(
                "SELECT count(*) FROM kerno_ui_keys_v11.by_uuid WHERE \
                 (id = '00000000-0000-0000-0000-000000000001' AND name = 'anna') \
                 OR (id = 'ffffffff-ffff-ffff-ffff-ffffffffffff' AND name = 'bob')"
            ),
            "2",
            "{engine:?}"
        );

        // A row found by two columns: a cell, and one column of the key.
        app.open_table(cx, "kerno_ui_keys_v11", "by_pair", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(app.data_rows(cx)[1], ["1", "y", "bob"]);
        app.type_in_cell(cx, 1, 3, "bobby", "enter");
        app.type_in_cell(cx, 0, 2, "z", "enter");
        let statements = app.review_sql(cx);
        assert_eq!(statements.len(), 2, "{statements:?}");
        assert!(
            statements.iter().all(|sql| sql.contains("WHERE")),
            "{statements:?}"
        );
        app.apply_and_wait(cx, "Rows 1–2 of 2");
        assert_eq!(
            server(
                "SELECT count(*) FROM kerno_ui_keys_v11.by_pair WHERE \
                 (a = 1 AND b = 'z' AND name = 'ann') OR (a = 1 AND b = 'y' AND name = 'bobby')"
            ),
            "2",
            "{engine:?}"
        );
    }
}

#[gpui_kit::test]
fn a_cell_takes_a_sql_expression_and_shows_what_the_server_made_of_it(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let server = |sql: &str| on_server(engine, sql);
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_expr_v11");
        let stamp = match engine {
            Engine::Postgres => "timestamp(6)",
            Engine::MySql | Engine::MariaDb => "DATETIME(6)",
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
        };
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &scratch_table(
                    engine,
                    "kerno_ui_expr_v11.events",
                    &format!("name VARCHAR(20) NOT NULL DEFAULT 'none', stamp {stamp} NULL"),
                ),
                "INSERT INTO kerno_ui_expr_v11.events (name) VALUES ('a')",
            ],
        );
        let app = connected(cx, engine);
        app.open_table(cx, "kerno_ui_expr_v11", "events", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–1 of 1");

        // ⌘E makes what is typed an expression: written unquoted.
        app.type_expression(cx, 0, 3, now(engine));
        assert_eq!(
            app.pending_at(cx, 0, 2),
            Some(CellValue::Expression(now(engine).into()))
        );
        let statements = app.review_sql(cx);
        assert!(
            statements[0].contains(&format!(" = {}", now(engine))),
            "{statements:?}"
        );
        assert!(
            !statements[0].contains(&format!("'{}'", now(engine))),
            "{statements:?}"
        );
        // Opened again, the cell is still the expression: Enter changes
        // nothing.
        app.on_data(cx, |tab, window, cx| tab.edit_cell(0, 3, window, cx));
        app.press(cx, &["enter"]);
        assert_eq!(
            app.pending_at(cx, 0, 2),
            Some(CellValue::Expression(now(engine).into()))
        );
        // Review SQL shows it, then applies it.
        app.click(cx, "data-review");
        assert!(app.has(cx, "sql-preview"));
        app.click(cx, "sql-confirm");
        // The row is read again: the cell holds the server's time.
        app.wait_until(cx, "the stored time", |app, cx| {
            app.pending(cx) == 0 && app.data_rows(cx)[0][2].starts_with("20")
        });
        let recent = match engine {
            Engine::Postgres => "stamp > now() - interval '1 minute'",
            Engine::MySql | Engine::MariaDb => "stamp > NOW() - INTERVAL 1 MINUTE",
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
        };
        assert_eq!(
            server(&format!(
                "SELECT count(*) FROM kerno_ui_expr_v11.events WHERE {recent}"
            )),
            "1",
            "{engine:?}"
        );

        // The button does what ⌘E does; an empty expression is refused and
        // the cell stays open.
        app.on_data(cx, |tab, window, cx| tab.edit_cell(0, 2, window, cx));
        app.press(cx, &["secondary-a", "backspace"]);
        app.click(cx, "cell-expression");
        app.press(cx, &["enter"]);
        let grid = app.data_grid(cx);
        assert_eq!(
            cx.update(|cx| grid.read(cx).delegate().editing()),
            Some((0, 1)),
            "{engine:?}: the empty expression keeps the cell open"
        );
        assert_eq!(app.pending(cx), 0);
        app.type_text(cx, "upper('it''s')");
        app.press(cx, &["enter"]);
        assert_eq!(
            app.pending_at(cx, 0, 1),
            Some(CellValue::Expression("upper('it''s')".into()))
        );
        assert!(
            app.review_sql(cx)[0].contains("= upper('it''s') WHERE"),
            "{:?}",
            app.review_sql(cx)
        );
        // Switched back to a value, the same text is a quoted literal.
        app.on_data(cx, |tab, window, cx| tab.edit_cell(0, 2, window, cx));
        app.click(cx, "cell-expression");
        app.press(cx, &["enter"]);
        assert_eq!(
            app.pending_at(cx, 0, 1),
            Some(CellValue::Text("upper('it''s')".into()))
        );
        app.click(cx, "data-discard");
        assert_eq!(app.pending(cx), 0);
    }
}

#[gpui_kit::test]
fn a_new_row_takes_default_and_a_new_uuid(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let server = |sql: &str| on_server(engine, sql);
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_insert_v11");
        let id_default = match engine {
            Engine::Postgres => "DEFAULT gen_random_uuid()",
            Engine::MySql | Engine::MariaDb => "DEFAULT (UUID())",
            kerno_core::Engine::Sqlite => {
                unreachable!("SQLite has no test server: these tests run on the server engines")
            }
        };
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &format!(
                    "CREATE TABLE kerno_ui_insert_v11.items (id {} PRIMARY KEY {id_default}, \
                     name VARCHAR(20) NOT NULL DEFAULT 'none', qty INT DEFAULT 7)",
                    uuid_type(engine)
                ),
            ],
        );
        let app = connected(cx, engine);
        app.open_table(cx, "kerno_ui_insert_v11", "items", RelationKind::Table);
        app.wait_for_rows(cx, "No rows");

        // A new row: a new uuid, the name's default, a quantity.
        app.click(cx, "data-add-row");
        app.type_expression(cx, 0, 1, new_uuid(engine));
        let grid = app.data_grid(cx);
        app.step(cx, |_, cx| {
            grid.update(cx, |table, cx| table.set_selected_cell(0, 2, cx))
        });
        app.click(cx, "data-set-default");
        assert_eq!(
            app.pending_at(cx, 0, 1),
            Some(CellValue::Expression("DEFAULT".into()))
        );
        app.type_in_cell(cx, 0, 3, "3", "enter");
        let statements = app.review_sql(cx);
        assert!(
            statements[0].ends_with(&format!("VALUES ({}, DEFAULT, 3)", new_uuid(engine))),
            "{statements:?}"
        );
        app.apply_and_wait(cx, "Rows 1–1 of 1");
        assert_eq!(app.data_rows(cx)[0][0].len(), 36, "{engine:?}: a uuid");
        assert_eq!(app.data_rows(cx)[0][1..], ["none", "3"]);

        // "Set default" on a fetched cell writes DEFAULT, unquoted.
        let grid = app.data_grid(cx);
        app.step(cx, |_, cx| {
            grid.update(cx, |table, cx| table.set_selected_cell(0, 3, cx))
        });
        app.click(cx, "data-set-default");
        let statements = app.review_sql(cx);
        assert!(statements[0].contains(" = DEFAULT WHERE"), "{statements:?}");
        app.apply_and_wait(cx, "Rows 1–1 of 1");
        assert_eq!(app.data_rows(cx)[0][2], "7");
        assert_eq!(
            server(
                "SELECT count(*) FROM kerno_ui_insert_v11.items WHERE name = 'none' AND qty = 7"
            ),
            "1",
            "{engine:?}"
        );
    }
}
