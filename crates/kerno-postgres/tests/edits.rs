//! Pending-edit SQL on PostgreSQL: every type written back from the text
//! the driver returned, and atomic apply. Runs against the `kerno-postgres`
//! container; skipped with a message when it is down.

mod common;

use std::sync::Arc;

use common::{exec_ok, run, scalar};
use kerno_core::pending::Pending;
use kerno_core::schema::{TableRef, TableStructure};
use kerno_core::sql::{CellValue, RowChange, apply_edits, edit_statements};
use kerno_core::{ResultStore, Session, TxState, TypeKind};

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

// Keys of every kind, JSON, SQL expressions (CLAUDE.md §4 J5) ---------------
//
// These go through the data tab's own path: the rows as the driver read
// them in a `ResultStore`, the edits in a `Pending`, its `changes` written
// by `edit_statements` and run by `apply_edits`, then read again.

/// Runs `body` on a session, then drops `cleanup` whether it passed or
/// failed (a panic in the body is raised again afterwards).
async fn cleaned<F>(session: Arc<dyn Session>, cleanup: String, body: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let outcome = tokio::spawn(body).await;
    exec_ok(&session, &cleanup).await;
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

    // Two new rows.
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

    // A cell that is not the key, found by the key.
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

    // The key itself: the row is found by the key it was read with.
    apply_pending(session, table, &structure, &store, |pending, store| {
        for (name, value) in keyed.key.iter().zip(&keyed.values[2]) {
            pending.set(store, 0, at(store, name), value.clone());
        }
    })
    .await;
    let store = page(session, table, "label").await;
    assert_eq!(rows(&store), expect(&[("a2", 2), ("b", 1)]), "{what}: key");

    // A new row, a copy of b with a key of its own, and b deleted.
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
/// uuid, an enum, two and three columns of mixed types, bytes, and text
/// with quotes, backslashes and Unicode take every edit. A unique key on a
/// column that takes NULL addresses no row.
#[tokio::test]
async fn rows_keyed_by_every_kind_of_key_take_every_edit() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("edit_key_kinds");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    let cleanup = format!("DROP SCHEMA {schema} CASCADE");
    let body_session = session.clone();
    cleaned(session, cleanup, async move {
        let session = body_session;
        exec_ok(
            &session,
            &format!("CREATE TYPE {schema}.mood AS ENUM ('sad', 'ok', 'happy', 'it''s', 'ça va')"),
        )
        .await;
        let keyed = |name: &str, columns: &str, key: Vec<&'static str>, values| Keyed {
            table: TableRef::new(schema.clone(), name),
            ddl: format!("CREATE TABLE {schema}.{name} ({columns}, label text NOT NULL)"),
            key,
            values,
        };
        let bytes = |value: &[u8]| CellValue::Bytes(value.to_vec());
        let tables = [
            keyed(
                "by_uuid",
                "id uuid PRIMARY KEY",
                vec!["id"],
                [
                    "6f1c3a52-0f7e-4c1d-9a3b-2b8e5d4c7a10",
                    "00000000-0000-0000-0000-000000000000",
                    "ffffffff-ffff-ffff-ffff-ffffffffffff",
                    "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
                    "123e4567-e89b-12d3-a456-426614174000",
                ]
                .map(|value| texts([value])),
            ),
            keyed(
                "by_enum",
                &format!("id {schema}.mood PRIMARY KEY"),
                vec!["id"],
                ["sad", "ok", "it's", "ça va", "happy"].map(|value| texts([value])),
            ),
            keyed(
                "by_two",
                "a int, b text, PRIMARY KEY (a, b)",
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
                "d date, n numeric(10,2), t timestamptz, PRIMARY KEY (d, n, t)",
                vec!["d", "n", "t"],
                [
                    texts(["2024-02-29", "1.50", "2024-02-29 10:11:12.345+00"]),
                    texts(["2024-02-29", "1.50", "2024-02-29 10:11:12.346+00"]),
                    texts(["1999-12-31", "-0.01", "2000-01-01 00:00:00+00"]),
                    texts(["2024-02-29", "99999999.99", "2024-02-29 10:11:12.345+00"]),
                    texts(["2024-03-01", "0.00", "1970-01-01 00:00:00+00"]),
                ],
            ),
            keyed(
                "by_bytes",
                "id bytea PRIMARY KEY",
                vec!["id"],
                [
                    vec![bytes(&[0x00])],
                    vec![bytes(&[0xff, 0x00, 0x27])],
                    vec![bytes(b"\\'\"")],
                    vec![bytes(&[])],
                    vec![bytes(&[0xde, 0xad, 0xbe, 0xef])],
                ],
            ),
            keyed(
                "by_text",
                "id text PRIMARY KEY",
                vec!["id"],
                [
                    "it's",
                    "back\\slash \\",
                    "caf\u{e9} \u{2603} \u{1F600}",
                    "\"dq\" $$ :p $1 ?",
                    "  spaced  ",
                ]
                .map(|value| texts([value])),
            ),
        ];
        for keyed in &tables {
            every_edit_by_key(&session, keyed).await;
        }

        // A unique column that takes NULL cannot tell rows apart: such a
        // table has no row identity, and only new rows can be written.
        let table = TableRef::new(schema.clone(), "by_nullable");
        exec_ok(
            &session,
            &format!("CREATE TABLE {schema}.by_nullable (x int UNIQUE, label text)"),
        )
        .await;
        exec_ok(
            &session,
            &format!("INSERT INTO {schema}.by_nullable VALUES (NULL, 'a'), (NULL, 'b')"),
        )
        .await;
        let structure = session.table_structure(&table).await.unwrap();
        assert_eq!(structure.row_identity(), None);
        let store = page(&session, &table, "label").await;
        let mut pending = Pending::default();
        pending.set(&store, 0, at(&store, "label"), text("z"));
        assert!(pending.changes(&store, &[]).is_err());
    })
    .await;
}

/// citext keys match without regard to case on the server; the extension
/// lives in a scratch database of this test, not in the fixture's.
#[tokio::test]
async fn rows_keyed_by_citext_take_every_edit() {
    let Some(session) = common::session().await else {
        return;
    };
    let database = common::scratch_schema("edit_citext");
    let drop = format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)");
    exec_ok(&session, &drop).await;
    exec_ok(&session, &format!("CREATE DATABASE {database}")).await;
    let body = {
        let database = database.clone();
        async move {
            let mut params = kerno_core::testing::params(common::ENGINE);
            params.database = Some(database);
            let session = common::connect(&params).await.unwrap();
            exec_ok(&session, "CREATE EXTENSION citext").await;
            let keyed = Keyed {
                table: TableRef::new("public", "by_citext"),
                ddl: "CREATE TABLE public.by_citext (id citext PRIMARY KEY, label text NOT NULL)"
                    .into(),
                key: vec!["id"],
                values: ["Ann", "BOB", "Ann-Marie", "c\u{e9}cile", "Dee's"]
                    .map(|value| texts([value])),
            };
            every_edit_by_key(&session, &keyed).await;
        }
    };
    cleaned(session, drop, body).await;
}

/// DBeaver #10048: a JSON document with nested objects, arrays, escapes,
/// large numbers and null survives edit, apply and read: byte for byte in
/// `json`, as the same value in `jsonb`; and what was read writes back.
#[tokio::test]
async fn json_documents_survive_edit_apply_and_read() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("edit_json");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    let cleanup = format!("DROP SCHEMA {schema} CASCADE");
    let body_session = session.clone();
    cleaned(session, cleanup, async move {
        let session = body_session;
        let table = TableRef::new(schema.clone(), "docs");
        exec_ok(
            &session,
            &format!("CREATE TABLE {schema}.docs (id int PRIMARY KEY, j json, jb jsonb)"),
        )
        .await;
        let structure = session.table_structure(&table).await.unwrap();
        let doc = r#"{"a": {"b": [1, 2.50, -3e2, null, true, {}], "c": []}, "s": "quote \" backslash \\ nl \n tab \t \u00e9 é ☃ 😀 'single'", "big": 123456789012345678901234567890, "neg": -9007199254740993, "n": null}"#;
        let other = r#"[{"k":"v"},"\\\\",0.1]"#;
        let quoted = |value: &str| session.dialect().quote_string(value);
        let count = |condition: String| {
            let session = session.clone();
            let sql = format!("SELECT count(*) FROM {schema}.docs WHERE {condition}");
            async move { scalar(&session, &sql).await }
        };

        let store = page(&session, &table, "id").await;
        apply_pending(&session, &table, &structure, &store, |pending, store| {
            let row = pending.add_row(store);
            pending.set(store, row, at(store, "id"), text("1"));
            pending.set(store, row, at(store, "j"), text(doc));
            pending.set(store, row, at(store, "jb"), text(doc));
        })
        .await;
        let store = page(&session, &table, "id").await;
        assert_eq!(cell(&store, 0, "j"), text(doc), "json is stored as typed");
        assert_eq!(count(format!("jb = {}::jsonb", quoted(doc))).await, "1");

        // What was read is written back unchanged to a new row, while the
        // first takes another document.
        let (j, jb) = (cell(&store, 0, "j"), cell(&store, 0, "jb"));
        apply_pending(&session, &table, &structure, &store, |pending, store| {
            let row = pending.add_row(store);
            pending.set(store, row, at(store, "id"), text("2"));
            pending.set(store, row, at(store, "j"), j);
            pending.set(store, row, at(store, "jb"), jb);
            pending.set(store, 0, at(store, "j"), text(other));
            pending.set(store, 0, at(store, "jb"), text(other));
        })
        .await;
        let store = page(&session, &table, "id").await;
        assert_eq!(cell(&store, 0, "j"), text(other));
        assert_eq!(cell(&store, 1, "j"), text(doc));
        assert_eq!(
            count(format!("id = 1 AND jb = {}::jsonb", quoted(other))).await,
            "1"
        );
        assert_eq!(
            count(format!("id = 2 AND jb = {}::jsonb", quoted(doc))).await,
            "1"
        );
    })
    .await;
}

/// DBeaver #9964, Beekeeper #2313: a pending value can be SQL, written
/// into the statement unquoted and run by the server; the row read again
/// holds what the server made of it.
#[tokio::test]
async fn sql_expressions_are_run_by_the_server() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("edit_expressions");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    let cleanup = format!("DROP SCHEMA {schema} CASCADE");
    let body_session = session.clone();
    cleaned(session, cleanup, async move {
        let session = body_session;
        let table = TableRef::new(schema.clone(), "t");
        exec_ok(
            &session,
            &format!(
                "CREATE TABLE {schema}.t (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), \
                 label text NOT NULL DEFAULT 'none', stamp timestamptz, n int DEFAULT 7)"
            ),
        )
        .await;
        exec_ok(
            &session,
            &format!(
                "INSERT INTO {schema}.t VALUES ('00000000-0000-0000-0000-000000000001', 'a', NULL, 1)"
            ),
        )
        .await;
        let structure = session.table_structure(&table).await.unwrap();
        let expression = |sql: &str| CellValue::Expression(sql.into());
        let store = page(&session, &table, "label").await;
        let statements = apply_pending(&session, &table, &structure, &store, |pending, store| {
            pending.set(store, 0, at(store, "stamp"), expression("now()"));
            pending.set(store, 0, at(store, "n"), expression("DEFAULT"));
            let row = pending.add_row(store);
            pending.set(store, row, at(store, "id"), expression("gen_random_uuid()"));
            pending.set(store, row, at(store, "label"), expression("DEFAULT"));
            pending.set(
                store,
                row,
                at(store, "stamp"),
                expression("now() - interval '1 day'"),
            );
        })
        .await;
        assert_eq!(
            statements,
            [
                format!(
                    "UPDATE \"{schema}\".\"t\" SET \"stamp\" = now(), \"n\" = DEFAULT \
                     WHERE \"id\" = '00000000-0000-0000-0000-000000000001'"
                ),
                format!(
                    "INSERT INTO \"{schema}\".\"t\" (\"id\", \"label\", \"stamp\") \
                     VALUES (gen_random_uuid(), DEFAULT, now() - interval '1 day')"
                ),
            ]
        );
        let count = |condition: &str| {
            let session = session.clone();
            let sql = format!("SELECT count(*) FROM {schema}.t WHERE {condition}");
            async move { scalar(&session, &sql).await }
        };
        assert_eq!(
            count("label = 'a' AND n = 7 AND stamp > now() - interval '1 minute'").await,
            "1"
        );
        assert_eq!(
            count(
                "label = 'none' AND id <> '00000000-0000-0000-0000-000000000001' \
                 AND stamp < now() - interval '23 hours'"
            )
            .await,
            "1"
        );
        // Read again, the cells hold the server's values.
        let store = page(&session, &table, "label").await;
        assert!(matches!(cell(&store, 0, "stamp"), CellValue::Text(stamp) if stamp.starts_with("20")));

        // An expression the server refuses changes nothing.
        let mut pending = Pending::default();
        pending.set(&store, 0, at(&store, "n"), expression("no_such_function()"));
        pending.set(&store, 1, at(&store, "label"), text("changed"));
        let changes = pending
            .changes(&store, &structure.row_identity().unwrap())
            .unwrap();
        let statements =
            edit_statements(session.dialect(), &table, &structure.columns, &changes).unwrap();
        assert!(apply_edits(&session, &statements).await.is_err());
        assert_eq!(count("label = 'changed'").await, "0");
    })
    .await;
}
