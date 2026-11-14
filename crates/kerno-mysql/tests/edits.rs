//! Pending edits on MySQL and MariaDB: the statements `kerno-core` builds
//! with this driver's dialect, applied in a transaction. Every type of
//! `kerno.all_types` is written from what the driver read and read back,
//! in a scratch database. Skipped with a message when the containers are
//! down.

mod common;

use std::sync::Arc;

use common::{engines, exec_ok, run, scalar};
use kerno_core::config::Engine;
use kerno_core::pending::Pending;
use kerno_core::schema::{ColumnInfo, TableRef, TableStructure};
use kerno_core::sql::{CellValue, RowChange, apply_edits, edit_statements};
use kerno_core::{ResultStore, Session, TxState, TypeKind};

/// One row as the driver delivered it: `None` for NULL, the cell's bytes
/// otherwise.
type RawRow = Vec<Option<Vec<u8>>>;

/// Every row of `table`, by key, exactly as the driver returns it, with
/// the names of the columns and whether each holds raw bytes.
async fn read(session: &Arc<dyn Session>, table: &TableRef) -> (Vec<(String, bool)>, Vec<RawRow>) {
    let sql = format!(
        "SELECT * FROM {} ORDER BY id",
        session.dialect().table_name(table)
    );
    let collected = run(session, &sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let result = &collected.results[0];
    let columns = result
        .columns
        .iter()
        .map(|column| (column.name.clone(), column.kind == TypeKind::Binary))
        .collect();
    let mut rows = Vec::new();
    for batch in &result.batches {
        for row in 0..batch.row_count() {
            rows.push(
                (0..batch.column_count())
                    .map(|column| batch.cell(row, column).map(<[u8]>::to_vec))
                    .collect(),
            );
        }
    }
    (columns, rows)
}

/// The cells of a row as values to write: what the driver read, handed
/// back as it came (bytes for binary columns, text for the rest).
fn values(columns: &[(String, bool)], row: &RawRow) -> Vec<(String, CellValue)> {
    columns
        .iter()
        .zip(row)
        .map(|((name, binary), cell)| {
            let value = match cell {
                None => CellValue::Null,
                Some(bytes) if *binary => CellValue::Bytes(bytes.clone()),
                Some(bytes) => {
                    CellValue::Text(String::from_utf8(bytes.clone()).expect("text cells are UTF-8"))
                }
            };
            (name.clone(), value)
        })
        .collect()
}

fn key(id: &str) -> Vec<(String, CellValue)> {
    vec![("id".into(), CellValue::Text(id.into()))]
}

fn without_id(values: Vec<(String, CellValue)>) -> Vec<(String, CellValue)> {
    values
        .into_iter()
        .filter(|(name, _)| name != "id")
        .collect()
}

/// Generates the statements for `changes` with the session's dialect and
/// applies them.
async fn apply(
    session: &Arc<dyn Session>,
    table: &TableRef,
    columns: &[ColumnInfo],
    changes: &[RowChange],
) -> Result<Vec<String>, kerno_core::sql::ApplyFailure> {
    let statements = edit_statements(session.dialect(), table, columns, changes)
        .expect("the changes name real columns");
    apply_edits(session, &statements).await?;
    Ok(statements)
}

#[tokio::test]
async fn every_type_round_trips_through_edit_statements() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "edits").await;
        // The same column types as the fixture table.
        exec_ok(
            &session,
            &format!("CREATE TABLE {database}.all_types LIKE kerno.all_types"),
        )
        .await;
        let table = TableRef::new(database.clone(), "all_types");
        let structure = session.table_structure(&table).await.unwrap();
        assert_eq!(structure.row_identity(), Some(vec!["id".to_string()]));
        assert_eq!(structure.columns.len(), 33);

        // The three fixture rows as the driver reads them: typical values,
        // all NULL, edge values.
        let (columns, originals) = read(&session, &TableRef::new("kerno", "all_types")).await;
        assert_eq!(originals.len(), 3);

        // Insert: every value written back from its text or bytes.
        let inserts: Vec<RowChange> = originals
            .iter()
            .map(|row| RowChange::Insert {
                values: values(&columns, row),
            })
            .collect();
        let statements = apply(&session, &table, &structure.columns, &inserts)
            .await
            .unwrap_or_else(|failure| panic!("insert: {failure}"));
        assert_eq!(statements.len(), 3);
        // The literals that are not plain strings or numbers.
        for literal in [
            "b'10101010'",
            "b'1'",
            "X'deadbeef'",
            "X'00ff10'",
            "'-838:59:59.000000'",
            "1.7976931348623157e308",
            "18446744073709551615",
        ] {
            assert!(
                statements.iter().any(|sql| sql.contains(literal)),
                "{literal} in {statements:#?}"
            );
        }
        // An all-NULL row, an empty string and empty bytes are three things.
        assert!(statements[1].contains("NULL, NULL"), "{}", statements[1]);
        assert!(statements[2].contains("X''"), "{}", statements[2]);
        assert_eq!(session.tx_state(), TxState::Idle);
        assert_eq!(read(&session, &table).await.1, originals, "after insert");

        // Update: the NULL row takes the values of the typical row, then of
        // the edge row, then becomes NULL again. One statement, one row.
        for source in [0, 2, 1] {
            let update = RowChange::Update {
                key: key("2"),
                set: without_id(values(&columns, &originals[source])),
            };
            apply(&session, &table, &structure.columns, &[update])
                .await
                .unwrap_or_else(|failure| panic!("update from row {source}: {failure}"));
            let (_, rows) = read(&session, &table).await;
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[1][0].as_deref(), Some(&b"2"[..]));
            assert_eq!(
                rows[1][1..],
                originals[source][1..],
                "row 2 after update from row {source}"
            );
            // The other rows were not touched.
            assert_eq!(rows[0], originals[0]);
            assert_eq!(rows[2], originals[2]);
        }

        // An update that changes nothing still found its row: the server
        // reports rows matched, so the apply goes through.
        let unchanged = RowChange::Update {
            key: key("1"),
            set: without_id(values(&columns, &originals[0])),
        };
        apply(&session, &table, &structure.columns, &[unchanged])
            .await
            .unwrap_or_else(|failure| panic!("an update to the same values: {failure}"));

        // Several changes at once, the key itself among them.
        let changes = [
            RowChange::Delete { key: key("3") },
            RowChange::Update {
                key: key("2"),
                set: vec![("id".into(), CellValue::Text("20".into()))],
            },
            RowChange::Insert {
                values: values(&columns, &originals[2]),
            },
        ];
        apply(&session, &table, &structure.columns, &changes)
            .await
            .unwrap_or_else(|failure| panic!("mixed changes: {failure}"));
        let (_, rows) = read(&session, &table).await;
        let ids: Vec<_> = rows.iter().map(|row| row[0].clone().unwrap()).collect();
        assert_eq!(ids, [b"1".to_vec(), b"3".to_vec(), b"20".to_vec()]);
        assert_eq!(rows[1], originals[2]);
        assert_eq!(rows[2][1..], originals[1][1..]);

        // The fixture itself was only read.
        assert_eq!(
            read(&session, &TableRef::new("kerno", "all_types")).await.1,
            originals
        );
        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

#[tokio::test]
async fn a_failed_apply_changes_nothing() {
    for engine in engines() {
        let session = common::session(engine).await;
        let observer = common::session(engine).await;
        let database = common::fresh_database(&session, "atomic").await;
        exec_ok(
            &session,
            &format!("CREATE TABLE {database}.all_types LIKE kerno.all_types"),
        )
        .await;
        exec_ok(
            &session,
            &format!("INSERT INTO {database}.all_types SELECT * FROM kerno.all_types"),
        )
        .await;
        let table = TableRef::new(database.clone(), "all_types");
        let structure = session.table_structure(&table).await.unwrap();
        let (columns, before) = read(&session, &table).await;
        assert_eq!(before.len(), 3);
        let set_int = |value: &str| vec![("c_int".to_string(), CellValue::Text(value.into()))];
        let new_row = |id: &str| RowChange::Insert {
            values: vec![
                ("id".into(), CellValue::Text(id.into())),
                ("c_varchar".into(), CellValue::Text("new".into())),
            ],
        };

        // The second statement hits a duplicate key: the first, which had
        // already run, is undone with it.
        let failure = apply(
            &session,
            &table,
            &structure.columns,
            &[new_row("10"), new_row("1")],
        )
        .await
        .expect_err("a duplicate key");
        assert_eq!(failure.statement, Some(1));
        assert_eq!(
            failure.error.db().and_then(|db| db.code.as_deref()),
            Some("1062 (23000)"),
            "{failure}"
        );
        assert_eq!(session.tx_state(), TxState::Idle);
        assert_eq!(read(&session, &table).await.1, before);
        // Another session never sees a trace of it.
        assert_eq!(read(&observer, &table).await.1, before);

        // The second statement addresses a row that is no longer there (as
        // if someone else had deleted it): nothing is changed either.
        let failure = apply(
            &session,
            &table,
            &structure.columns,
            &[
                RowChange::Update {
                    key: key("2"),
                    set: set_int("5"),
                },
                RowChange::Delete { key: key("999") },
            ],
        )
        .await
        .expect_err("a row that does not exist");
        assert_eq!(failure.statement, Some(1));
        assert!(
            failure.to_string().contains("no row was changed"),
            "{failure}"
        );
        assert_eq!(session.tx_state(), TxState::Idle);
        assert_eq!(read(&session, &table).await.1, before);

        // Same for an update of a missing row, as the first statement.
        let failure = apply(
            &session,
            &table,
            &structure.columns,
            &[
                RowChange::Update {
                    key: key("999"),
                    set: set_int("5"),
                },
                new_row("11"),
            ],
        )
        .await
        .expect_err("a row that does not exist");
        assert_eq!(failure.statement, Some(0));
        assert_eq!(session.tx_state(), TxState::Idle);
        assert_eq!(read(&session, &table).await.1, before);

        // A value the column refuses, in the last statement.
        let failure = apply(
            &session,
            &table,
            &structure.columns,
            &[
                RowChange::Delete { key: key("3") },
                RowChange::Update {
                    key: key("1"),
                    set: vec![("c_date".into(), CellValue::Text("not a date".into()))],
                },
            ],
        )
        .await
        .expect_err("an invalid date");
        assert_eq!(failure.statement, Some(1));
        assert_eq!(session.tx_state(), TxState::Idle);
        assert_eq!(read(&session, &table).await.1, before);
        assert_eq!(read(&observer, &table).await.1, before);

        // An apply is refused inside a transaction the user opened, and
        // leaves that transaction alone.
        exec_ok(&session, "START TRANSACTION").await;
        let failure = apply(&session, &table, &structure.columns, &[new_row("12")])
            .await
            .expect_err("a transaction is open");
        assert_eq!(failure.statement, None);
        assert_eq!(session.tx_state(), TxState::Open);
        exec_ok(&session, "ROLLBACK").await;

        // And the same changes go through once nothing is in the way.
        apply(
            &session,
            &table,
            &structure.columns,
            &[
                new_row("10"),
                RowChange::Update {
                    key: key("2"),
                    set: set_int("5"),
                },
                RowChange::Delete { key: key("3") },
            ],
        )
        .await
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(session.tx_state(), TxState::Idle);
        let (_, after) = read(&observer, &table).await;
        let ids: Vec<_> = after.iter().map(|row| row[0].clone().unwrap()).collect();
        assert_eq!(ids, [b"1".to_vec(), b"2".to_vec(), b"10".to_vec()]);
        let c_int = columns
            .iter()
            .position(|(name, _)| name == "c_int")
            .unwrap();
        assert_eq!(after[1][c_int].as_deref(), Some(&b"5"[..]));

        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

#[tokio::test]
async fn a_row_of_defaults_and_keys_that_need_care() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "edit_keys").await;
        exec_ok(
            &session,
            &format!(
                "CREATE TABLE {database}.t (\
                     id INT NOT NULL AUTO_INCREMENT PRIMARY KEY, \
                     label VARCHAR(40) NOT NULL DEFAULT 'none', \
                     flags BIT(4) NOT NULL DEFAULT b'1010', \
                     active TINYINT(1) NOT NULL DEFAULT 1)"
            ),
        )
        .await;
        let table = TableRef::new(database.clone(), "t");
        let structure = session.table_structure(&table).await.unwrap();
        let rows = || {
            let session = session.clone();
            let sql = format!("SELECT id, label, flags, active FROM {database}.t ORDER BY id");
            async move { common::rows(&session, &sql).await }
        };
        let text = |value: &str| CellValue::Text(value.into());
        let row = |cells: [&str; 4]| -> Vec<Option<String>> {
            cells.iter().map(|cell| Some(cell.to_string())).collect()
        };

        // A row with nothing given: neither engine has DEFAULT VALUES.
        let statements = apply(
            &session,
            &table,
            &structure.columns,
            &[
                RowChange::Insert { values: vec![] },
                RowChange::Insert {
                    values: vec![
                        ("label".into(), text(r"it's a \ 'test'")),
                        ("flags".into(), CellValue::Default),
                        ("active".into(), text("0")),
                    ],
                },
            ],
        )
        .await
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(
            statements[0],
            format!("INSERT INTO `{database}`.`t` () VALUES ()")
        );
        assert_eq!(
            rows().await,
            [
                row(["1", "none", "1010", "1"]),
                row(["2", r"it's a \ 'test'", "1010", "0"])
            ]
        );

        // Rows addressed by a text key with quotes and a backslash, by a
        // BIT value and by a boolean: the key literals must match what was
        // stored.
        exec_ok(
            &session,
            &format!("ALTER TABLE {database}.t DROP PRIMARY KEY, MODIFY id INT NOT NULL, ADD PRIMARY KEY (label, flags, active)"),
        )
        .await;
        let structure = session.table_structure(&table).await.unwrap();
        assert_eq!(
            structure.row_identity(),
            Some(vec![
                "label".to_string(),
                "flags".to_string(),
                "active".to_string()
            ])
        );
        let key = vec![
            ("label".to_string(), text(r"it's a \ 'test'")),
            ("flags".to_string(), text("1010")),
            ("active".to_string(), text("0")),
        ];
        apply(
            &session,
            &table,
            &structure.columns,
            &[RowChange::Update {
                key: key.clone(),
                set: vec![
                    ("flags".into(), text("0001")),
                    ("active".into(), text("true")),
                ],
            }],
        )
        .await
        .unwrap_or_else(|failure| panic!("{failure}"));
        assert_eq!(
            rows().await,
            [
                row(["1", "none", "1010", "1"]),
                row(["2", r"it's a \ 'test'", "0001", "1"])
            ]
        );
        // The old key no longer addresses anything.
        let failure = apply(
            &session,
            &table,
            &structure.columns,
            &[RowChange::Delete { key }],
        )
        .await
        .expect_err("the row moved");
        assert!(
            failure.to_string().contains("no row was changed"),
            "{failure}"
        );
        assert_eq!(
            scalar(&session, &format!("SELECT COUNT(*) FROM {database}.t")).await,
            "2"
        );

        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

/// Text with backslashes and quotes is stored exactly, even when the
/// session's backslash mode changed in a way the driver did not see.
#[tokio::test]
async fn backslashes_and_quotes_survive_any_backslash_mode() {
    for engine in engines() {
        if !kerno_core::testing::available(engine) {
            return;
        }
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "edit_backslash").await;
        let dialect = session.dialect();
        let table = TableRef::new(database.clone(), "t");
        exec_ok(
            &session,
            &format!(
                "CREATE TABLE {} (id INT PRIMARY KEY, v TEXT)",
                dialect.table_name(&table)
            ),
        )
        .await;
        let structure = session.table_structure(&table).await.unwrap();
        let awkward = [
            "plain",
            "it's",
            r"back\slash",
            r"ends with a backslash\",
            r"\' OR 1=1 -- ",
            r"\\' ; DROP TABLE t; -- ",
            "tab\tand\nnewline",
            r#"{"json": "a\nb"}"#,
            "",
        ];
        let changes = |offset: usize| -> Vec<RowChange> {
            awkward
                .iter()
                .enumerate()
                .map(|(index, text)| RowChange::Insert {
                    values: vec![
                        ("id".into(), CellValue::Text((offset + index).to_string())),
                        ("v".into(), CellValue::Text(text.to_string())),
                    ],
                })
                .collect()
        };
        let stored = |from: usize| {
            let session = session.clone();
            let name = dialect.table_name(&table);
            async move {
                common::rows(
                    &session,
                    &format!("SELECT v FROM {name} WHERE id >= {from} ORDER BY id"),
                )
                .await
                .into_iter()
                .take(awkward.len())
                .map(|row| row[0].clone().unwrap())
                .collect::<Vec<_>>()
            }
        };

        // Default mode.
        let statements = edit_statements(dialect, &table, &structure.columns, &changes(0)).unwrap();
        apply_edits(&session, &statements)
            .await
            .unwrap_or_else(|f| panic!("{engine:?}: {f}"));
        assert_eq!(stored(0).await, awkward, "{engine:?}, default mode");

        // Switch the mode through a prepared statement: the request does not
        // start with SET, so nothing tells the driver the mode changed.
        exec_ok(
            &session,
            "PREPARE kerno_mode FROM \"SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'\"",
        )
        .await;
        exec_ok(&session, "EXECUTE kerno_mode").await;
        assert!(
            scalar(&session, "SELECT @@session.sql_mode")
                .await
                .contains("NO_BACKSLASH_ESCAPES")
        );
        let statements =
            edit_statements(dialect, &table, &structure.columns, &changes(100)).unwrap();
        apply_edits(&session, &statements)
            .await
            .unwrap_or_else(|f| panic!("{engine:?}: {f}"));
        assert_eq!(
            stored(100).await,
            awkward,
            "{engine:?}, NO_BACKSLASH_ESCAPES behind the driver's back"
        );

        // And back, the same way.
        exec_ok(
            &session,
            "PREPARE kerno_mode FROM \"SET SESSION sql_mode = ''\"",
        )
        .await;
        exec_ok(&session, "EXECUTE kerno_mode").await;
        let statements =
            edit_statements(dialect, &table, &structure.columns, &changes(200)).unwrap();
        apply_edits(&session, &statements)
            .await
            .unwrap_or_else(|f| panic!("{engine:?}: {f}"));
        assert_eq!(
            stored(200).await,
            awkward,
            "{engine:?}, default mode restored behind the driver's back"
        );

        // Nothing in the values ran as SQL: the table is there, with every row.
        let count = scalar(
            &session,
            &format!("SELECT COUNT(*) FROM {}", dialect.table_name(&table)),
        )
        .await;
        assert_eq!(count, (awkward.len() * 3).to_string());
        exec_ok(
            &session,
            &format!("DROP DATABASE {}", dialect.quote_ident(&database)),
        )
        .await;
    }
}

/// What the result of `sql` is, editable or why not, as a query tab decides
/// it: from the statement, the origins of its columns, the table.
async fn verdict(
    session: &Arc<dyn Session>,
    sql: &str,
) -> Result<Vec<Option<String>>, kerno_core::editable::ReadOnly> {
    use kerno_core::editable;
    let collected = run(session, sql).await.unwrap();
    let columns = collected.results[0].columns.clone();
    let source = editable::check(sql, session.dialect().lex_rules(), &columns)?;
    let (editable, _) = editable::lookup(session, &columns, &source).await?;
    Ok(editable.columns)
}

#[tokio::test]
async fn results_say_which_table_columns_they_show() {
    use kerno_core::ColumnOrigin;
    use kerno_core::editable::ReadOnly;
    use kerno_core::schema::RelationKind;
    for engine in engines() {
        let session = common::session(engine).await;
        let origins = |sql: &'static str| {
            let session = session.clone();
            async move {
                run(&session, sql).await.unwrap().results[0]
                    .columns
                    .iter()
                    .map(|column| column.origin.clone())
                    .collect::<Vec<_>>()
            }
        };
        let column = |table: &str, name: &str| {
            Some(ColumnOrigin::Named {
                schema: "kerno".into(),
                table: table.into(),
                column: name.into(),
            })
        };
        // Plain columns and aliases keep their table column; an expression
        // has none.
        assert_eq!(
            origins("SELECT id, full_name AS n, id + 1 AS e FROM customers").await,
            [
                column("customers", "id"),
                column("customers", "full_name"),
                None
            ],
            "{engine:?}"
        );
        // A view's columns are the view's.
        assert_eq!(
            origins("SELECT id FROM active_customers").await,
            [column("active_customers", "id")],
            "{engine:?}"
        );
        // MySQL names the table through a derived table; MariaDB names the
        // derived table, so a query tab keeps the origins of the first run.
        let derived = origins("SELECT * FROM (SELECT id FROM customers) kerno_q").await;
        let through = if engine == Engine::MariaDb {
            "kerno_q"
        } else {
            "customers"
        };
        assert_eq!(derived, [column(through, "id")], "{engine:?}");

        assert_eq!(
            verdict(
                &session,
                "SELECT id, full_name AS n, 1 AS e FROM customers WHERE id < 5"
            )
            .await,
            Ok(vec![Some("id".into()), Some("full_name".into()), None]),
            "{engine:?}"
        );
        let all = verdict(&session, "SELECT * FROM customers").await.unwrap();
        assert!(all.iter().all(Option::is_some), "{engine:?}");
        for (sql, why) in [
            (
                "SELECT c.id, o.id FROM customers c JOIN orders o ON o.customer_id = c.id",
                ReadOnly::SeveralTables,
            ),
            (
                "SELECT c.id, d.full_name FROM customers c JOIN customers d ON d.id = c.id",
                ReadOnly::Shape("joins tables"),
            ),
            ("SELECT 1 + 1", ReadOnly::NoTableColumn),
            (
                "SELECT full_name FROM customers",
                ReadOnly::KeyMissing {
                    column: "id".into(),
                    table: TableRef::new("kerno", "customers"),
                },
            ),
            (
                "SELECT * FROM active_customers",
                ReadOnly::View(
                    TableRef::new("kerno", "active_customers"),
                    RelationKind::View,
                ),
            ),
        ] {
            assert_eq!(verdict(&session, sql).await, Err(why), "{engine:?}: {sql}");
        }
    }
}

#[tokio::test]
async fn edits_join_an_open_transaction_without_ending_it() {
    use kerno_core::sql::apply_edits_in_transaction;
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "edit_in_transaction").await;
        exec_ok(
            &session,
            &format!("CREATE TABLE {database}.t (id INT PRIMARY KEY, name TEXT)"),
        )
        .await;
        exec_ok(
            &session,
            &format!("INSERT INTO {database}.t VALUES (1, 'one'), (2, 'two')"),
        )
        .await;
        let table = TableRef::new(database.clone(), "t");
        let structure = session.table_structure(&table).await.unwrap();
        let generate = |changes: &[RowChange]| {
            edit_statements(session.dialect(), &table, &structure.columns, changes).unwrap()
        };
        let rename = |id: &str, name: &str| RowChange::Update {
            key: vec![("id".into(), CellValue::Text(id.into()))],
            set: vec![("name".into(), CellValue::Text(name.into()))],
        };
        let names = || {
            let session = session.clone();
            let sql = format!("SELECT GROUP_CONCAT(name ORDER BY id) FROM {database}.t");
            async move { scalar(&session, &sql).await }
        };

        let statements = generate(&[rename("1", "uno")]);
        assert!(
            apply_edits_in_transaction(&session, &statements)
                .await
                .is_err()
        );

        exec_ok(&session, "START TRANSACTION").await;
        exec_ok(
            &session,
            &format!("INSERT INTO {database}.t VALUES (3, 'three')"),
        )
        .await;
        apply_edits_in_transaction(&session, &statements)
            .await
            .unwrap_or_else(|f| panic!("{engine:?}: {f}"));
        assert_eq!(session.tx_state(), TxState::Open, "{engine:?}");
        assert_eq!(names().await, "uno,two,three", "{engine:?}");

        let failing = generate(&[rename("2", "dos"), rename("404", "none")]);
        let failure = apply_edits_in_transaction(&session, &failing)
            .await
            .unwrap_err();
        assert_eq!(failure.statement, Some(1), "{engine:?}");
        assert_eq!(session.tx_state(), TxState::Open, "{engine:?}");
        assert_eq!(names().await, "uno,two,three", "{engine:?}");

        exec_ok(&session, "ROLLBACK").await;
        assert_eq!(names().await, "one,two", "{engine:?}");
        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

// Keys of every kind, JSON, SQL expressions (CLAUDE.md §4 J5) ---------------
//
// These go through the data tab's own path: the rows as the driver read
// them in a `ResultStore`, the edits in a `Pending`, its `changes` written
// by `edit_statements` and run by `apply_edits`, then read again.

/// Runs `body`, then drops `database` whether it passed or failed (a panic
/// in the body is raised again afterwards).
async fn cleaned<F>(session: &Arc<dyn Session>, database: &str, body: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(body).await;
    exec_ok(session, &format!("DROP DATABASE IF EXISTS {database}")).await;
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
}

/// The rows of `table` ordered by `order`, as the data tab holds them.
async fn page(session: &Arc<dyn Session>, table: &TableRef, order: &str) -> ResultStore {
    let sql = format!(
        "SELECT * FROM {} ORDER BY {order}",
        session.dialect().table_name(table)
    );
    let mut result = run(session, &sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .results
        .remove(0);
    let mut store = ResultStore::new(result.columns.clone());
    for batch in result.batches.drain(..) {
        store.push(batch);
    }
    store
}

/// The column of `store` called `name`.
fn at(store: &ResultStore, name: &str) -> usize {
    store
        .columns()
        .iter()
        .position(|column| column.name == name)
        .unwrap_or_else(|| panic!("no column {name}"))
}

/// A fetched cell as the data tab compares and copies it.
fn cell(store: &ResultStore, row: usize, name: &str) -> CellValue {
    let column = at(store, name);
    match store.cell(row, column) {
        None => CellValue::Null,
        Some(bytes) if store.columns()[column].kind == TypeKind::Binary => {
            CellValue::Bytes(bytes.to_vec())
        }
        Some(bytes) => CellValue::Text(String::from_utf8(bytes.to_vec()).unwrap()),
    }
}

/// Makes the pending changes `edit` gives on `store` and applies them as
/// the data tab does. Returns the statements that ran.
async fn apply_pending(
    session: &Arc<dyn Session>,
    table: &TableRef,
    structure: &TableStructure,
    store: &ResultStore,
    edit: impl FnOnce(&mut Pending, &ResultStore),
) -> Vec<String> {
    let mut pending = Pending::default();
    edit(&mut pending, store);
    let identity = structure.row_identity().unwrap_or_default();
    let changes = pending.changes(store, &identity).unwrap();
    let statements =
        edit_statements(session.dialect(), table, &structure.columns, &changes).unwrap();
    apply_edits(session, &statements)
        .await
        .unwrap_or_else(|failure| panic!("{failure}\n{}", statements.join(";\n")));
    statements
}

/// A table whose key is `key`, with a `label` text column after it.
struct Keyed {
    table: TableRef,
    /// The `CREATE TABLE` statement.
    ddl: String,
    key: Vec<&'static str>,
    /// Key values of rows a and b, of a once its key is changed, of a new
    /// row c and of d, a copy of b.
    values: [Vec<CellValue>; 5],
}

/// Gives a row its key values and its label.
fn fill(
    pending: &mut Pending,
    store: &ResultStore,
    row: usize,
    keyed: &Keyed,
    key: usize,
    label: &str,
) {
    for (name, value) in keyed.key.iter().zip(&keyed.values[key]) {
        pending.set(store, row, at(store, name), value.clone());
    }
    pending.set(
        store,
        row,
        at(store, "label"),
        CellValue::Text(label.into()),
    );
}

/// Every edit of the data tab on a table keyed by `keyed.key`: insert,
/// a cell, the key itself, a copy and a deletion in one apply, each read
/// back as the server holds it.
async fn every_edit_by_key(session: &Arc<dyn Session>, keyed: &Keyed) {
    let what = &keyed.ddl;
    exec_ok(session, what).await;
    let table = &keyed.table;
    let structure = session.table_structure(table).await.unwrap();
    assert_eq!(
        structure.row_identity(),
        Some(keyed.key.iter().map(|name| name.to_string()).collect()),
        "{what}"
    );
    let rows = |store: &ResultStore| -> Vec<(CellValue, Vec<CellValue>)> {
        (0..store.row_count())
            .map(|row| {
                let key = keyed
                    .key
                    .iter()
                    .map(|name| cell(store, row, name))
                    .collect();
                (cell(store, row, "label"), key)
            })
            .collect()
    };
    let expect = |rows: &[(&str, usize)]| -> Vec<(CellValue, Vec<CellValue>)> {
        rows.iter()
            .map(|(label, key)| {
                (
                    CellValue::Text(label.to_string()),
                    keyed.values[*key].clone(),
                )
            })
            .collect()
    };

    let store = page(session, table, "label").await;
    apply_pending(session, table, &structure, &store, |pending, store| {
        for (key, label) in [(0, "a"), (1, "b")] {
            let row = pending.add_row(store);
            fill(pending, store, row, keyed, key, label);
        }
    })
    .await;
    let store = page(session, table, "label").await;
    assert_eq!(
        rows(&store),
        expect(&[("a", 0), ("b", 1)]),
        "{what}: insert"
    );

    apply_pending(session, table, &structure, &store, |pending, store| {
        pending.set(store, 0, at(store, "label"), CellValue::Text("a2".into()))
    })
    .await;
    let store = page(session, table, "label").await;
    assert_eq!(
        rows(&store),
        expect(&[("a2", 0), ("b", 1)]),
        "{what}: update"
    );

    apply_pending(session, table, &structure, &store, |pending, store| {
        for (name, value) in keyed.key.iter().zip(&keyed.values[2]) {
            pending.set(store, 0, at(store, name), value.clone());
        }
    })
    .await;
    let store = page(session, table, "label").await;
    assert_eq!(rows(&store), expect(&[("a2", 2), ("b", 1)]), "{what}: key");

    apply_pending(session, table, &structure, &store, |pending, store| {
        let row = pending.add_row(store);
        fill(pending, store, row, keyed, 3, "c");
        let copy = pending.duplicate(store, 1, &[]).unwrap();
        fill(pending, store, copy, keyed, 4, "d");
        pending.delete(store.row_count(), [1]);
    })
    .await;
    let store = page(session, table, "label").await;
    assert_eq!(
        rows(&store),
        expect(&[("a2", 2), ("c", 3), ("d", 4)]),
        "{what}: copy and delete"
    );
}

fn text(value: &str) -> CellValue {
    CellValue::Text(value.into())
}

fn texts<const N: usize>(values: [&str; N]) -> Vec<CellValue> {
    values.into_iter().map(text).collect()
}

/// DBeaver #9964 and #10048, Beekeeper #2313 and #3029: rows keyed by a
/// uuid (`CHAR(36)`, MariaDB's `UUID`), an `ENUM`, two and three columns of
/// mixed types, `BINARY(16)` and `VARBINARY`, and text with quotes,
/// backslashes and Unicode in two collations and in latin1 take every
/// edit. A unique key on a column that takes NULL addresses no row.
#[tokio::test]
async fn rows_keyed_by_every_kind_of_key_take_every_edit() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "edit_key_kinds").await;
        let body = {
            let (session, database) = (session.clone(), database.clone());
            async move {
                let keyed = |name: &str, columns: &str, key: Vec<&'static str>, values| Keyed {
                    table: TableRef::new(database.clone(), name),
                    ddl: format!(
                        "CREATE TABLE {database}.{name} ({columns}, label VARCHAR(20) NOT NULL)"
                    ),
                    key,
                    values,
                };
                let uuids = [
                    "6f1c3a52-0f7e-4c1d-9a3b-2b8e5d4c7a10",
                    "00000000-0000-0000-0000-000000000000",
                    "ffffffff-ffff-ffff-ffff-ffffffffffff",
                    "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
                    "123e4567-e89b-12d3-a456-426614174000",
                ]
                .map(|value| texts([value]));
                let sixteen = |fill: u8, first: &[u8]| {
                    let mut bytes = vec![fill; 16];
                    bytes[..first.len()].copy_from_slice(first);
                    vec![CellValue::Bytes(bytes)]
                };
                let bytes = |value: &[u8]| vec![CellValue::Bytes(value.to_vec())];
                let awkward = [
                    "it's",
                    "back\\slash \\",
                    "caf\u{e9} \u{2603} \u{1F600}",
                    "\\' OR 1=1 -- \"dq\"",
                    "  spaced",
                ]
                .map(|value| texts([value]));
                let mut tables = vec![
                    keyed(
                        "by_char_uuid",
                        "id CHAR(36) PRIMARY KEY",
                        vec!["id"],
                        uuids.clone(),
                    ),
                    keyed(
                        "by_enum",
                        "id ENUM('sad', 'ok', 'happy', 'it''s', 'ça va') PRIMARY KEY",
                        vec!["id"],
                        ["sad", "ok", "it's", "ça va", "happy"].map(|value| texts([value])),
                    ),
                    keyed(
                        "by_two",
                        "a INT, b VARCHAR(20), PRIMARY KEY (a, b)",
                        vec!["a", "b"],
                        [
                            texts(["1", "x"]),
                            texts(["1", "y"]),
                            texts(["2", "x"]),
                            texts(["-7", ""]),
                            texts(["1", "x"]),
                        ],
                    ),
                    keyed(
                        "by_three",
                        "d DATE, n DECIMAL(10,2), t DATETIME(3), PRIMARY KEY (d, n, t)",
                        vec!["d", "n", "t"],
                        [
                            texts(["2024-02-29", "1.50", "2024-02-29 10:11:12.345"]),
                            texts(["2024-02-29", "1.50", "2024-02-29 10:11:12.346"]),
                            texts(["1999-12-31", "-0.01", "2000-01-01 00:00:00.000"]),
                            texts(["2024-02-29", "99999999.99", "2024-02-29 10:11:12.345"]),
                            texts(["2024-03-01", "0.00", "1970-01-02 00:00:00.000"]),
                        ],
                    ),
                    keyed(
                        "by_binary16",
                        "id BINARY(16) PRIMARY KEY",
                        vec!["id"],
                        [
                            sixteen(0x00, &[]),
                            sixteen(0xff, &[0x00, 0x27]),
                            sixteen(0x5c, b"\\'\""),
                            sixteen(0x01, &[0xde, 0xad]),
                            sixteen(0x27, &[0x00]),
                        ],
                    ),
                    keyed(
                        "by_varbinary",
                        "id VARBINARY(32) PRIMARY KEY",
                        vec!["id"],
                        [
                            bytes(&[0x00]),
                            bytes(&[0xff, 0x00, 0x27]),
                            bytes(b"\\'\""),
                            bytes(&[]),
                            bytes(&[0xde, 0xad, 0xbe, 0xef]),
                        ],
                    ),
                    keyed(
                        "by_text",
                        "id VARCHAR(50) PRIMARY KEY",
                        vec!["id"],
                        awkward.clone(),
                    ),
                    keyed(
                        "by_text_bin",
                        "id VARCHAR(50) COLLATE utf8mb4_bin PRIMARY KEY",
                        vec!["id"],
                        awkward,
                    ),
                    keyed(
                        "by_latin1",
                        "id VARCHAR(50) CHARACTER SET latin1 PRIMARY KEY",
                        vec!["id"],
                        [
                            "it's",
                            "back\\slash \\",
                            "caf\u{e9}",
                            "na\u{ef}ve \\'",
                            "  spaced",
                        ]
                        .map(|value| texts([value])),
                    ),
                ];
                if engine == Engine::MariaDb {
                    tables.push(keyed("by_uuid", "id UUID PRIMARY KEY", vec!["id"], uuids));
                }
                for keyed in &tables {
                    every_edit_by_key(&session, keyed).await;
                }

                // A unique column that takes NULL cannot tell rows apart:
                // such a table has no row identity, and only new rows can
                // be written.
                let table = TableRef::new(database.clone(), "by_nullable");
                exec_ok(
                    &session,
                    &format!(
                        "CREATE TABLE {database}.by_nullable (x INT UNIQUE, label VARCHAR(20))"
                    ),
                )
                .await;
                exec_ok(
                    &session,
                    &format!("INSERT INTO {database}.by_nullable VALUES (NULL, 'a'), (NULL, 'b')"),
                )
                .await;
                let structure = session.table_structure(&table).await.unwrap();
                assert_eq!(structure.row_identity(), None);
                let store = page(&session, &table, "label").await;
                let mut pending = Pending::default();
                pending.set(&store, 0, at(&store, "label"), text("z"));
                assert!(pending.changes(&store, &[]).is_err());
            }
        };
        cleaned(&session, &database, body).await;
    }
}

/// DBeaver #10048: a JSON document with nested objects, arrays, escapes,
/// large numbers and null survives edit, apply and read: as the same value
/// in MySQL's `JSON`, byte for byte in MariaDB's (a `LONGTEXT`); and what
/// was read writes back.
#[tokio::test]
async fn json_documents_survive_edit_apply_and_read() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "edit_json").await;
        let body = {
            let (session, database) = (session.clone(), database.clone());
            async move {
                let table = TableRef::new(database.clone(), "docs");
                exec_ok(
                    &session,
                    &format!("CREATE TABLE {database}.docs (id INT PRIMARY KEY, j JSON)"),
                )
                .await;
                let structure = session.table_structure(&table).await.unwrap();
                // Integers past 2^53 but within 64 bits: MySQL keeps them
                // exact (beyond 64 bits it stores a double, by design).
                let doc = r#"{"a": {"b": [1, 2.5, -300, null, true, {}], "c": []}, "s": "quote \" backslash \\ nl \n tab \t \u00e9 é ☃ 😀 'single'", "big": 9007199254740993, "neg": -9007199254740993, "n": null}"#;
                let other = r#"[{"k": "v"}, "\\\\", 0.1]"#;
                // The same value: MySQL compares JSON documents, MariaDB
                // (whose JSON is text) has JSON_EQUALS.
                let same = |row: &str, value: &str| {
                    let literal = session.dialect().quote_string(value);
                    let equal = match engine {
                        Engine::MariaDb => format!("JSON_EQUALS(j, {literal})"),
                        _ => format!("j = CAST({literal} AS JSON)"),
                    };
                    let sql = format!(
                        "SELECT COUNT(*) FROM {database}.docs WHERE id = {row} AND {equal}"
                    );
                    let session = session.clone();
                    async move { scalar(&session, &sql).await }
                };

                let store = page(&session, &table, "id").await;
                apply_pending(&session, &table, &structure, &store, |pending, store| {
                    let row = pending.add_row(store);
                    pending.set(store, row, at(store, "id"), text("1"));
                    pending.set(store, row, at(store, "j"), text(doc));
                })
                .await;
                let store = page(&session, &table, "id").await;
                assert_eq!(same("1", doc).await, "1", "{engine:?}");
                if engine == Engine::MariaDb {
                    assert_eq!(cell(&store, 0, "j"), text(doc), "stored as typed");
                }

                // What was read is written back unchanged to a new row,
                // while the first takes another document.
                let j = cell(&store, 0, "j");
                apply_pending(&session, &table, &structure, &store, |pending, store| {
                    let row = pending.add_row(store);
                    pending.set(store, row, at(store, "id"), text("2"));
                    pending.set(store, row, at(store, "j"), j.clone());
                    pending.set(store, 0, at(store, "j"), text(other));
                })
                .await;
                let store = page(&session, &table, "id").await;
                assert_eq!(cell(&store, 1, "j"), j, "{engine:?}: read back as read");
                assert_eq!(same("2", doc).await, "1", "{engine:?}");
                assert_eq!(same("1", other).await, "1", "{engine:?}");
            }
        };
        cleaned(&session, &database, body).await;
    }
}

/// DBeaver #9964, Beekeeper #2313: a pending value can be SQL, written
/// into the statement unquoted and run by the server; the row read again
/// holds what the server made of it.
#[tokio::test]
async fn sql_expressions_are_run_by_the_server() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "edit_expressions").await;
        let body = {
            let (session, database) = (session.clone(), database.clone());
            async move {
                let table = TableRef::new(database.clone(), "t");
                exec_ok(
                    &session,
                    &format!(
                        "CREATE TABLE {database}.t (id CHAR(36) PRIMARY KEY DEFAULT (UUID()), \
                         label VARCHAR(20) NOT NULL DEFAULT 'none', stamp DATETIME(6) NULL, \
                         n INT DEFAULT 7)"
                    ),
                )
                .await;
                exec_ok(
                    &session,
                    &format!(
                        "INSERT INTO {database}.t VALUES \
                         ('00000000-0000-0000-0000-000000000001', 'a', NULL, 1)"
                    ),
                )
                .await;
                let structure = session.table_structure(&table).await.unwrap();
                let expression = |sql: &str| CellValue::Expression(sql.into());
                let store = page(&session, &table, "label").await;
                let statements =
                    apply_pending(&session, &table, &structure, &store, |pending, store| {
                        pending.set(store, 0, at(store, "stamp"), expression("NOW(6)"));
                        pending.set(store, 0, at(store, "n"), expression("DEFAULT"));
                        let row = pending.add_row(store);
                        pending.set(store, row, at(store, "id"), expression("UUID()"));
                        pending.set(store, row, at(store, "label"), expression("DEFAULT"));
                        pending.set(
                            store,
                            row,
                            at(store, "stamp"),
                            expression("NOW() - INTERVAL 1 DAY"),
                        );
                    })
                    .await;
                assert_eq!(
                    statements,
                    [
                        format!(
                            "UPDATE `{database}`.`t` SET `stamp` = NOW(6), `n` = DEFAULT \
                             WHERE `id` = '00000000-0000-0000-0000-000000000001'"
                        ),
                        format!(
                            "INSERT INTO `{database}`.`t` (`id`, `label`, `stamp`) \
                             VALUES (UUID(), DEFAULT, NOW() - INTERVAL 1 DAY)"
                        ),
                    ]
                );
                let count = |condition: &str| {
                    let session = session.clone();
                    let sql = format!("SELECT COUNT(*) FROM {database}.t WHERE {condition}");
                    async move { scalar(&session, &sql).await }
                };
                assert_eq!(
                    count("label = 'a' AND n = 7 AND stamp > NOW() - INTERVAL 1 MINUTE").await,
                    "1",
                    "{engine:?}"
                );
                assert_eq!(
                    count(
                        "label = 'none' AND id <> '00000000-0000-0000-0000-000000000001' \
                         AND LENGTH(id) = 36 AND stamp < NOW() - INTERVAL 23 HOUR"
                    )
                    .await,
                    "1",
                    "{engine:?}"
                );
                let store = page(&session, &table, "label").await;
                assert!(
                    matches!(cell(&store, 0, "stamp"), CellValue::Text(stamp) if stamp.starts_with("20"))
                );

                // An expression the server refuses changes nothing.
                let mut pending = Pending::default();
                pending.set(&store, 0, at(&store, "n"), expression("no_such_function()"));
                pending.set(&store, 1, at(&store, "label"), text("changed"));
                let changes = pending
                    .changes(&store, &structure.row_identity().unwrap())
                    .unwrap();
                let statements =
                    edit_statements(session.dialect(), &table, &structure.columns, &changes)
                        .unwrap();
                assert!(apply_edits(&session, &statements).await.is_err());
                assert_eq!(count("label = 'changed'").await, "0");
            }
        };
        cleaned(&session, &database, body).await;
    }
}
