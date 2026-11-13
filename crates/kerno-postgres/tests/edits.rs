//! Pending-edit SQL on PostgreSQL: every type written back from the text
//! the driver returned, and atomic apply. Runs against the `kerno-postgres`
//! container; skipped with a message when it is down.

mod common;

use std::sync::Arc;

use common::{exec_ok, run, scalar};
use kerno_core::schema::{TableRef, TableStructure};
use kerno_core::sql::{CellValue, RowChange, apply_edits, edit_statements};
use kerno_core::{Session, TxState, TypeKind};

/// Every row of `table` as edit values, keyed by column name.
async fn read(
    session: &Arc<dyn Session>,
    table: &str,
    structure: &TableStructure,
) -> Vec<Vec<(String, CellValue)>> {
    let collected = run(session, &format!("SELECT * FROM {table} ORDER BY id"))
        .await
        .unwrap();
    let result = &collected.results[0];
    let mut rows = Vec::new();
    for batch in &result.batches {
        for row in 0..batch.row_count() {
            let mut values = Vec::new();
            for (index, column) in structure.columns.iter().enumerate() {
                let value = match batch.cell(row, index) {
                    None => CellValue::Null,
                    Some(bytes) if column.kind == TypeKind::Binary => {
                        CellValue::Bytes(bytes.to_vec())
                    }
                    Some(bytes) => CellValue::Text(String::from_utf8(bytes.to_vec()).unwrap()),
                };
                values.push((column.name.clone(), value));
            }
            rows.push(values);
        }
    }
    rows
}

async fn scratch(session: &Arc<dyn Session>, test: &str) -> (String, TableRef, TableStructure) {
    let schema = common::scratch_schema(test);
    exec_ok(session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(session, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(
        session,
        &format!("CREATE TABLE {schema}.copy (LIKE public.all_types INCLUDING ALL)"),
    )
    .await;
    let table = TableRef::new(schema.clone(), "copy");
    let structure = session.table_structure(&table).await.unwrap();
    (schema, table, structure)
}

#[tokio::test]
async fn every_type_survives_a_write_from_its_text_form() {
    let Some(session) = common::session().await else {
        return;
    };
    let (schema, table, structure) = scratch(&session, "edit_types").await;
    let source = session
        .table_structure(&TableRef::new("public", "all_types"))
        .await
        .unwrap();
    let originals = read(&session, "public.all_types", &source).await;
    assert_eq!(originals.len(), 3);
    assert_eq!(structure.row_identity(), Some(vec!["id".to_string()]));

    // Insert the three fixture rows (typical, all NULL, edge values).
    let inserts: Vec<_> = originals
        .iter()
        .map(|values| RowChange::Insert {
            values: values.clone(),
        })
        .collect();
    let statements =
        edit_statements(session.dialect(), &table, &structure.columns, &inserts).unwrap();
    assert_eq!(statements.len(), 3);
    apply_edits(&session, &statements)
        .await
        .unwrap_or_else(|failure| panic!("{failure}\n{}", statements.join(";\n")));
    assert_eq!(session.tx_state(), TxState::Idle);
    let qualified = format!("{schema}.copy");
    assert_eq!(read(&session, &qualified, &structure).await, originals);

    // Update: give row 2 (all NULL) every value of row 3, then row 3 the
    // values of row 1, cell by cell.
    let key = |id: &str| vec![("id".to_string(), CellValue::Text(id.into()))];
    let without_id = |row: &[(String, CellValue)]| -> Vec<(String, CellValue)> {
        row.iter()
            .filter(|(name, _)| name != "id")
            .cloned()
            .collect()
    };
    let updates = [
        RowChange::Update {
            key: key("2"),
            set: without_id(&originals[2]),
        },
        RowChange::Update {
            key: key("3"),
            set: without_id(&originals[0]),
        },
        // Back to NULL everywhere for row 1.
        RowChange::Update {
            key: key("1"),
            set: without_id(&originals[1]),
        },
    ];
    let statements =
        edit_statements(session.dialect(), &table, &structure.columns, &updates).unwrap();
    apply_edits(&session, &statements)
        .await
        .unwrap_or_else(|failure| panic!("{failure}\n{}", statements.join(";\n")));

    let after = read(&session, &qualified, &structure).await;
    assert_eq!(
        without_id(&after[0]),
        without_id(&originals[1]),
        "row 1 is all NULL now"
    );
    assert_eq!(
        without_id(&after[1]),
        without_id(&originals[2]),
        "row 2 holds the edge values"
    );
    assert_eq!(
        without_id(&after[2]),
        without_id(&originals[0]),
        "row 3 holds the typical values"
    );

    // Delete.
    let deletes = [RowChange::Delete { key: key("2") }];
    let statements =
        edit_statements(session.dialect(), &table, &structure.columns, &deletes).unwrap();
    apply_edits(&session, &statements).await.unwrap();
    assert_eq!(
        scalar(
            &session,
            &format!("SELECT string_agg(id::text, ',' ORDER BY id) FROM {qualified}")
        )
        .await,
        "1,3"
    );

    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;
}

#[tokio::test]
async fn awkward_text_is_stored_exactly() {
    let Some(session) = common::session().await else {
        return;
    };
    let (schema, table, structure) = scratch(&session, "edit_text").await;
    let awkward = [
        "it's",
        "back\\slash and 'quote'",
        "\\",
        "'; DROP TABLE copy; --",
        "line1\nline2\r\n\ttabbed",
        "$$ dollar $$ :param $1 ?",
        "caf\u{e9} \u{2603} \u{1F600} \u{0}",
        "",
        " leading and trailing ",
        "NULL",
    ];
    let mut expected = Vec::new();
    let mut changes = Vec::new();
    for (index, text) in awkward.iter().enumerate() {
        // PostgreSQL text cannot hold a NUL byte: the server must refuse
        // it, not store something else.
        if text.contains('\u{0}') {
            continue;
        }
        expected.push((index.to_string(), text.to_string()));
        changes.push(RowChange::Insert {
            values: vec![
                ("id".into(), CellValue::Text(index.to_string())),
                ("c_text".into(), CellValue::Text(text.to_string())),
            ],
        });
    }
    let statements =
        edit_statements(session.dialect(), &table, &structure.columns, &changes).unwrap();
    apply_edits(&session, &statements).await.unwrap();

    let stored = common::rows(
        &session,
        &format!("SELECT id::text, c_text FROM {schema}.copy ORDER BY id"),
    )
    .await;
    let stored: Vec<(String, String)> = stored
        .into_iter()
        .map(|row| (row[0].clone().unwrap(), row[1].clone().unwrap()))
        .collect();
    assert_eq!(stored, expected);
    // The table is still there: nothing in the values was executed.
    assert_eq!(
        scalar(&session, &format!("SELECT count(*) FROM {schema}.copy")).await,
        expected.len().to_string()
    );

    let nul = [RowChange::Insert {
        values: vec![
            ("id".into(), CellValue::Text("99".into())),
            ("c_text".into(), CellValue::Text("a\u{0}b".into())),
        ],
    }];
    let statements = edit_statements(session.dialect(), &table, &structure.columns, &nul).unwrap();
    assert!(apply_edits(&session, &statements).await.is_err());
    assert_eq!(
        scalar(
            &session,
            &format!("SELECT count(*) FROM {schema}.copy WHERE id = 99")
        )
        .await,
        "0"
    );

    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;
}

#[tokio::test]
async fn a_failed_apply_changes_nothing() {
    let Some(session) = common::session().await else {
        return;
    };
    let (schema, table, structure) = scratch(&session, "edit_atomic").await;
    let qualified = format!("{schema}.copy");
    exec_ok(
        &session,
        &format!("INSERT INTO {qualified} (id, c_text) VALUES (1, 'one'), (2, 'two')"),
    )
    .await;
    let snapshot = || async {
        common::rows(
            &session,
            &format!("SELECT id::text, c_text FROM {qualified} ORDER BY id"),
        )
        .await
    };
    let before = snapshot().await;
    let key = |id: &str| vec![("id".to_string(), CellValue::Text(id.into()))];
    let set_text = |value: &str| vec![("c_text".to_string(), CellValue::Text(value.into()))];
    let generate = |changes: &[RowChange]| {
        edit_statements(session.dialect(), &table, &structure.columns, changes).unwrap()
    };

    // The second statement violates the primary key: the first is undone.
    let statements = generate(&[
        RowChange::Update {
            key: key("1"),
            set: set_text("changed"),
        },
        RowChange::Insert {
            values: vec![("id".into(), CellValue::Text("2".into()))],
        },
    ]);
    let failure = apply_edits(&session, &statements).await.unwrap_err();
    assert_eq!(failure.statement, Some(1));
    assert_eq!(
        failure.error.db().and_then(|db| db.code.as_deref()),
        Some("23505")
    );
    assert_eq!(session.tx_state(), TxState::Idle);
    assert_eq!(snapshot().await, before);

    // A row that no longer exists: someone else deleted it.
    let statements = generate(&[
        RowChange::Update {
            key: key("1"),
            set: set_text("changed"),
        },
        RowChange::Update {
            key: key("404"),
            set: set_text("ghost"),
        },
    ]);
    let failure = apply_edits(&session, &statements).await.unwrap_err();
    assert_eq!(failure.statement, Some(1));
    assert!(
        failure
            .to_string()
            .contains("modified or deleted by someone else"),
        "{failure}"
    );
    assert_eq!(session.tx_state(), TxState::Idle);
    assert_eq!(snapshot().await, before);

    // Same for a delete of a vanished row.
    let statements = generate(&[RowChange::Delete { key: key("404") }]);
    assert!(apply_edits(&session, &statements).await.is_err());
    assert_eq!(snapshot().await, before);

    // An update that sets a value to what it already is still counts as
    // changing its row.
    let statements = generate(&[RowChange::Update {
        key: key("1"),
        set: set_text("one"),
    }]);
    apply_edits(&session, &statements).await.unwrap();

    // Refused when a transaction is already open; that transaction is untouched.
    exec_ok(&session, "BEGIN").await;
    let statements = generate(&[RowChange::Delete { key: key("1") }]);
    let failure = apply_edits(&session, &statements).await.unwrap_err();
    assert_eq!(failure.statement, None);
    assert_eq!(session.tx_state(), TxState::Open);
    exec_ok(&session, "ROLLBACK").await;
    assert_eq!(snapshot().await, before);

    // Nothing to do is not an error and opens no transaction.
    apply_edits(&session, &[]).await.unwrap();
    assert_eq!(session.tx_state(), TxState::Idle);

    // A good apply does commit: another session sees it.
    let statements = generate(&[
        RowChange::Delete { key: key("2") },
        RowChange::Update {
            key: key("1"),
            set: set_text("uno"),
        },
        RowChange::Insert {
            values: vec![("id".into(), CellValue::Text("3".into()))],
        },
    ]);
    apply_edits(&session, &statements).await.unwrap();
    let other = common::session().await.unwrap();
    assert_eq!(
        common::rows(
            &other,
            &format!("SELECT id::text, c_text FROM {qualified} ORDER BY id")
        )
        .await,
        vec![
            vec![Some("1".to_string()), Some("uno".to_string())],
            vec![Some("3".to_string()), None],
        ]
    );

    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;
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
    let Some(session) = common::session().await else {
        return;
    };
    let oid: u32 = scalar(&session, "SELECT 'shop.customers'::regclass::oid")
        .await
        .parse()
        .unwrap();
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
    let column = |number| {
        Some(ColumnOrigin::Oid {
            table: oid,
            column: number,
        })
    };
    // Plain columns and aliases keep their table column; an expression
    // has none.
    assert_eq!(
        origins("SELECT id, full_name AS n, id + 1 AS e FROM shop.customers").await,
        [column(1), column(3), None]
    );
    // Through a derived table, as a filtered result is read again.
    assert_eq!(
        origins("SELECT * FROM (SELECT id, full_name FROM shop.customers) kerno_q").await,
        [column(1), column(3)]
    );
    // A view's columns are the view's.
    let view: u32 = scalar(&session, "SELECT 'shop.order_totals'::regclass::oid")
        .await
        .parse()
        .unwrap();
    assert!(
        origins("SELECT * FROM shop.order_totals").await.iter().all(
            |origin| matches!(origin, Some(ColumnOrigin::Oid { table, .. }) if *table == view)
        )
    );

    // Editable: one table, its key in the result, aliases written back
    // under the table's names.
    assert_eq!(
        verdict(
            &session,
            "SELECT id, full_name AS n, id + 1 AS e FROM shop.customers WHERE id < 5"
        )
        .await,
        Ok(vec![Some("id".into()), Some("full_name".into()), None])
    );
    let all = verdict(&session, "SELECT * FROM shop.customers")
        .await
        .unwrap();
    assert!(all.iter().all(Option::is_some) && all[0].as_deref() == Some("id"));
    // Read-only, and why.
    let customers = TableRef::new("shop", "customers");
    for (sql, why) in [
        (
            "SELECT c.id, o.id FROM shop.customers c JOIN shop.orders o ON o.customer_id = c.id",
            ReadOnly::SeveralTables,
        ),
        (
            "SELECT c.id, c.full_name FROM shop.customers c JOIN shop.customers d ON d.id = c.id",
            ReadOnly::Shape("joins tables"),
        ),
        ("SELECT 1 + 1", ReadOnly::NoTableColumn),
        (
            "SELECT full_name FROM shop.customers",
            ReadOnly::KeyMissing {
                column: "id".into(),
                table: customers.clone(),
            },
        ),
        (
            "SELECT * FROM shop.order_totals",
            ReadOnly::View(TableRef::new("shop", "order_totals"), RelationKind::View),
        ),
        (
            "SELECT * FROM shop.product_sales",
            ReadOnly::View(
                TableRef::new("shop", "product_sales"),
                RelationKind::MaterializedView,
            ),
        ),
    ] {
        assert_eq!(verdict(&session, sql).await, Err(why), "{sql}");
    }
}

#[tokio::test]
async fn edits_join_an_open_transaction_without_ending_it() {
    use kerno_core::sql::apply_edits_in_transaction;
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("edit_in_transaction");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(
        &session,
        &format!("CREATE TABLE {schema}.t (id int PRIMARY KEY, name text)"),
    )
    .await;
    exec_ok(
        &session,
        &format!("INSERT INTO {schema}.t VALUES (1, 'one'), (2, 'two')"),
    )
    .await;
    let table = TableRef::new(schema.clone(), "t");
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
        let sql = format!("SELECT string_agg(name, ',' ORDER BY id) FROM {schema}.t");
        async move { scalar(&session, &sql).await }
    };

    // Refused outside a transaction.
    let statements = generate(&[rename("1", "uno")]);
    assert!(
        apply_edits_in_transaction(&session, &statements)
            .await
            .is_err()
    );

    exec_ok(&session, "BEGIN").await;
    exec_ok(
        &session,
        &format!("INSERT INTO {schema}.t VALUES (3, 'three')"),
    )
    .await;
    apply_edits_in_transaction(&session, &statements)
        .await
        .unwrap();
    assert_eq!(session.tx_state(), TxState::Open);
    assert_eq!(names().await, "uno,two,three");

    // A failed apply takes back its own statements only.
    let failing = generate(&[rename("2", "dos"), rename("404", "none")]);
    let failure = apply_edits_in_transaction(&session, &failing)
        .await
        .unwrap_err();
    assert_eq!(failure.statement, Some(1));
    assert_eq!(session.tx_state(), TxState::Open);
    assert_eq!(names().await, "uno,two,three");

    // The user's rollback undoes the applied edits with the rest.
    exec_ok(&session, "ROLLBACK").await;
    assert_eq!(names().await, "one,two");

    // A failed transaction is refused: it must be rolled back first.
    exec_ok(&session, "BEGIN").await;
    assert!(run(&session, "SELECT 1 / 0").await.is_err());
    assert_eq!(session.tx_state(), TxState::Failed);
    assert!(
        apply_edits_in_transaction(&session, &statements)
            .await
            .is_err()
    );
    exec_ok(&session, "ROLLBACK").await;
    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;
}
