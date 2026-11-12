//! Pending edits on MySQL and MariaDB: the statements `kerno-core` builds
//! with this driver's dialect, applied in a transaction. Every type of
//! `kerno.all_types` is written from what the driver read and read back,
//! in a scratch database. Skipped with a message when the containers are
//! down.

mod common;

use std::sync::Arc;

use common::{engines, exec_ok, run, scalar};
use kerno_core::schema::{ColumnInfo, TableRef};
use kerno_core::sql::{CellValue, RowChange, apply_edits, edit_statements};
use kerno_core::{Session, TxState, TypeKind};

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
