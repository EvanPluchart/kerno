//! Table DDL from a draft, run on PostgreSQL: a table created from a draft
//! reads back as that draft, and a draft edited from a table's structure
//! turns the table into what it says. Runs against the `kerno-postgres`
//! container; skipped with a message when it is down.

mod common;

use std::future::Future;
use std::sync::Arc;

use common::{exec_ok, run};
use kerno_core::Session;
use kerno_core::schema::TableRef;
use kerno_core::table_draft::{ColumnDraft, ForeignKeyDraft, IndexDraft, TableDraft, changes};

/// Runs the `cleanup` statements, then `body`, then `cleanup` again whether
/// `body` passed or panicked.
async fn cleaning_up(
    session: &Arc<dyn Session>,
    cleanup: &[String],
    body: impl Future<Output = ()> + Send + 'static,
) {
    for sql in cleanup {
        exec_ok(session, sql).await;
    }
    let outcome = tokio::spawn(body).await;
    let mut failed = Vec::new();
    for sql in cleanup {
        if let Err(error) = run(session, sql).await {
            failed.push(format!("{sql}: {error}"));
        }
    }
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
    assert!(failed.is_empty(), "cleanup failed: {failed:#?}");
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn column(name: &str, data_type: &str, nullable: bool) -> ColumnDraft {
    ColumnDraft {
        original: None,
        name: name.into(),
        data_type: data_type.into(),
        nullable,
        default: None,
        comment: None,
        auto_increment: false,
    }
}

/// `draft` as `from_structure` gives it once applied: every item tied to
/// its own name.
fn saved(mut draft: TableDraft) -> TableDraft {
    for column in &mut draft.columns {
        column.original = Some(column.name.clone());
    }
    for index in &mut draft.indexes {
        index.original = Some(index.name.clone());
    }
    for key in &mut draft.foreign_keys {
        key.original = Some(key.name.clone());
    }
    draft
}

/// Runs `statements` in order, in one transaction.
async fn apply(session: &Arc<dyn Session>, statements: &[String]) {
    exec_ok(session, "BEGIN").await;
    for sql in statements {
        exec_ok(session, sql).await;
    }
    exec_ok(session, "COMMIT").await;
}

/// The draft of `table` as it is on the server, checked to give no change
/// against itself.
async fn read_back(session: &Arc<dyn Session>, table: &TableRef) -> TableDraft {
    let structure = session.table_structure(table).await.unwrap();
    let draft = TableDraft::from_structure(table, &structure);
    assert_eq!(changes(table, &structure, &draft), vec![]);
    draft
}

#[tokio::test]
async fn a_table_created_from_a_draft_reads_back_as_the_draft() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("alter_create");
    let cleanup = [format!("DROP SCHEMA IF EXISTS {schema} CASCADE")];
    cleaning_up(&session, &cleanup, create(session.clone(), schema)).await;
}

async fn create(session: Arc<dyn Session>, schema: String) {
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(
        &session,
        &format!("CREATE TABLE {schema}.customers (id integer PRIMARY KEY)"),
    )
    .await;

    // Spellings as the catalog prints them, so the draft reads back equal.
    let table = TableRef::new(schema.clone(), "order items");
    let mut draft = TableDraft::new(table.clone());
    draft.comment = Some("Orders, it's said".into());
    draft.columns.extend([
        ColumnDraft {
            default: Some("'new'::character varying".into()),
            comment: Some("Where the order is".into()),
            ..column("status", "character varying(20)", false)
        },
        column("customer_id", "integer", true),
        ColumnDraft {
            default: Some("0".into()),
            ..column("total", "numeric(10,2)", false)
        },
        column("note", "text", true),
    ]);
    draft.indexes = vec![
        IndexDraft {
            original: None,
            name: "order items by customer".into(),
            columns: strings(&["customer_id", "status"]),
            unique: false,
        },
        IndexDraft {
            original: None,
            name: "order items unique note".into(),
            columns: strings(&["note"]),
            unique: true,
        },
    ];
    draft.foreign_keys = vec![ForeignKeyDraft {
        original: None,
        name: "order items customer".into(),
        columns: strings(&["customer_id"]),
        referenced_table: TableRef::new(schema.clone(), "customers"),
        referenced_columns: strings(&["id"]),
        on_update: "NO ACTION".into(),
        on_delete: "CASCADE".into(),
    }];
    assert_eq!(draft.validate(), Vec::<String>::new());

    let statements = session.dialect().create_table_sql(&draft).unwrap();
    assert!(statements[0].starts_with("CREATE TABLE"), "{statements:#?}");
    apply(&session, &statements).await;

    assert_eq!(read_back(&session, &table).await, saved(draft));
}

#[tokio::test]
async fn a_table_edited_from_its_structure_becomes_the_draft() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("alter_edit");
    let cleanup = [format!("DROP SCHEMA IF EXISTS {schema} CASCADE")];
    cleaning_up(&session, &cleanup, edit(session.clone(), schema)).await;
}

async fn edit(session: Arc<dyn Session>, schema: String) {
    for sql in [
        format!("CREATE SCHEMA {schema}"),
        format!("CREATE TABLE {schema}.customers (id integer PRIMARY KEY)"),
        format!(
            "CREATE TABLE {schema}.items (\
               id integer GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY, \
               code text NOT NULL, \
               qty integer NOT NULL DEFAULT 1, \
               old_name text, \
               dropped text, \
               customer_id integer, \
               email text UNIQUE, \
               serial_no serial, \
               CONSTRAINT items_customer_fk FOREIGN KEY (customer_id) \
                 REFERENCES {schema}.customers (id))"
        ),
        format!("CREATE INDEX items_by_code ON {schema}.items (code)"),
        format!("COMMENT ON TABLE {schema}.items IS 'before'"),
    ] {
        exec_ok(&session, &sql).await;
    }
    let table = TableRef::new(schema.clone(), "items");
    let structure = session.table_structure(&table).await.unwrap();
    let mut draft = TableDraft::from_structure(&table, &structure);
    // The unique constraint's index is the constraint's, not the draft's.
    assert_eq!(
        draft
            .indexes
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>(),
        ["items_by_code"]
    );
    assert!(draft.columns[7].auto_increment, "serial");

    let at = |draft: &mut TableDraft, name: &str| {
        draft
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap_or_else(|| panic!("no column {name}"))
    };
    draft.table.name = "articles".into();
    draft.comment = Some("after".into());
    let code = at(&mut draft, "code");
    draft.columns[code].comment = Some("the code".into());
    let qty = at(&mut draft, "qty");
    draft.columns[qty].data_type = "bigint".into();
    draft.columns[qty].default = Some("2".into());
    let renamed = at(&mut draft, "old_name");
    draft.columns[renamed].name = "new_name".into();
    draft.columns[renamed].nullable = false;
    let dropped = at(&mut draft, "dropped");
    draft.columns.remove(dropped);
    // A serial no longer: no default left.
    let serial = at(&mut draft, "serial_no");
    draft.columns[serial].auto_increment = false;
    draft.columns[serial].nullable = true;
    draft.columns.push(ColumnDraft {
        comment: Some("new one".into()),
        ..column("added", "text", true)
    });
    draft.primary_key = strings(&["id", "code"]);
    draft.indexes = vec![IndexDraft {
        original: None,
        name: "items_by_new_name".into(),
        columns: strings(&["new_name"]),
        unique: true,
    }];
    draft.foreign_keys = vec![ForeignKeyDraft {
        original: None,
        name: "items_owner_fk".into(),
        on_delete: "SET NULL".into(),
        ..draft.foreign_keys[0].clone()
    }];
    assert_eq!(draft.validate(), Vec::<String>::new());

    let changes = changes(&table, &structure, &draft);
    let statements = session.dialect().alter_table_sql(&table, &changes).unwrap();
    apply(&session, &statements).await;

    let articles = TableRef::new(schema.clone(), "articles");
    assert_eq!(read_back(&session, &articles).await, saved(draft));
}
