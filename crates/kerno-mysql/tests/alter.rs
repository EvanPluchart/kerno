//! Table DDL from a draft, run on MySQL and MariaDB: a table created from a
//! draft reads back as that draft, and a draft edited from a table's
//! structure turns the table into what it says. Skipped with a message when
//! the containers are down.

mod common;

use std::future::Future;
use std::sync::Arc;

use common::{engines, exec_ok, run};
use kerno_core::schema::TableRef;
use kerno_core::table_draft::{ColumnDraft, ForeignKeyDraft, IndexDraft, TableDraft, changes};
use kerno_core::{Engine, Session};

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

/// An integer type as the engine prints it: MariaDB keeps a display width.
fn int(engine: Engine, name: &str) -> String {
    match (engine, name) {
        (Engine::MariaDb, "int") => "int(11)".into(),
        (Engine::MariaDb, "bigint") => "bigint(20)".into(),
        _ => name.into(),
    }
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

/// The draft of `table` as it is on the server, checked to give no change
/// against itself.
async fn read_back(session: &Arc<dyn Session>, table: &TableRef) -> TableDraft {
    let structure = session.table_structure(table).await.unwrap();
    let draft = TableDraft::from_structure(table, &structure);
    assert_eq!(changes(table, &structure, &draft), vec![]);
    draft
}

/// Runs `statements` in order (DDL commits each one on these engines).
async fn apply(session: &Arc<dyn Session>, statements: &[String]) {
    for sql in statements {
        exec_ok(session, sql).await;
    }
}

#[tokio::test]
async fn a_table_created_from_a_draft_reads_back_as_the_draft() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::scratch_database("alter_create");
        let cleanup = [format!("DROP DATABASE IF EXISTS {database}")];
        cleaning_up(
            &session,
            &cleanup,
            create(session.clone(), engine, database),
        )
        .await;
    }
}

async fn create(session: Arc<dyn Session>, engine: Engine, database: String) {
    exec_ok(&session, &format!("CREATE DATABASE {database}")).await;
    exec_ok(
        &session,
        &format!("CREATE TABLE {database}.customers (id int PRIMARY KEY)"),
    )
    .await;

    // Spellings as the catalog prints them, so the draft reads back equal.
    let table = TableRef::new(database.clone(), "order items");
    let mut draft = TableDraft::new(table.clone());
    draft.columns[0].data_type = int(engine, "bigint");
    draft.comment = Some("Orders, it's said".into());
    draft.columns.extend([
        ColumnDraft {
            default: Some("'new'".into()),
            comment: Some("Where the order is".into()),
            ..column("status", "varchar(20)", false)
        },
        column("customer_id", &int(engine, "int"), true),
        ColumnDraft {
            default: Some("0.00".into()),
            ..column("total", "decimal(10,2)", false)
        },
        column("note", "varchar(100)", true),
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
        referenced_table: TableRef::new(database.clone(), "customers"),
        referenced_columns: strings(&["id"]),
        on_update: "NO ACTION".into(),
        on_delete: "CASCADE".into(),
    }];
    assert_eq!(draft.validate(), Vec::<String>::new());

    let statements = session.dialect().create_table_sql(&draft).unwrap();
    assert_eq!(statements.len(), 1, "{statements:#?}");
    apply(&session, &statements).await;

    assert_eq!(read_back(&session, &table).await, saved(draft));
}

#[tokio::test]
async fn a_table_edited_from_its_structure_becomes_the_draft() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::scratch_database("alter_edit");
        let cleanup = [format!("DROP DATABASE IF EXISTS {database}")];
        cleaning_up(&session, &cleanup, edit(session.clone(), engine, database)).await;
    }
}

async fn edit(session: Arc<dyn Session>, engine: Engine, database: String) {
    for sql in [
        format!("CREATE DATABASE {database}"),
        format!("CREATE TABLE {database}.customers (id int PRIMARY KEY)"),
        // The foreign key has an index of its own, so none is made for it.
        format!(
            "CREATE TABLE {database}.items (\
               id int NOT NULL AUTO_INCREMENT PRIMARY KEY, \
               code varchar(20) NOT NULL, \
               qty int NOT NULL DEFAULT 1, \
               old_name varchar(40), \
               dropped text, \
               customer_id int, \
               email varchar(100) UNIQUE, \
               INDEX items_by_customer (customer_id), \
               INDEX items_by_code (code), \
               CONSTRAINT items_customer_fk FOREIGN KEY (customer_id) \
                 REFERENCES {database}.customers (id)) \
             COMMENT = 'before'"
        ),
    ] {
        exec_ok(&session, &sql).await;
    }
    let table = TableRef::new(database.clone(), "items");
    let structure = session.table_structure(&table).await.unwrap();
    let mut draft = TableDraft::from_structure(&table, &structure);
    // A unique key is an index here, and can be edited as one.
    assert_eq!(
        draft
            .indexes
            .iter()
            .map(|i| (i.name.as_str(), i.unique))
            .collect::<Vec<_>>(),
        [
            ("email", true),
            ("items_by_code", false),
            ("items_by_customer", false)
        ]
    );

    let at = |draft: &TableDraft, name: &str| {
        draft
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap_or_else(|| panic!("no column {name}"))
    };
    draft.table.name = "articles".into();
    draft.comment = Some("after".into());
    let code = at(&draft, "code");
    draft.columns[code].comment = Some("the code".into());
    let qty = at(&draft, "qty");
    draft.columns[qty].data_type = int(engine, "bigint");
    draft.columns[qty].default = Some("2".into());
    // Renamed and altered: one CHANGE COLUMN.
    let renamed = at(&draft, "old_name");
    draft.columns[renamed].name = "new_name".into();
    draft.columns[renamed].nullable = false;
    let dropped = at(&draft, "dropped");
    draft.columns.remove(dropped);
    draft.columns.push(ColumnDraft {
        comment: Some("new one".into()),
        ..column("added", "text", true)
    });
    // The auto-increment `id` stays a key throughout: one statement.
    draft.primary_key = strings(&["id", "code"]);
    draft.indexes.retain(|index| index.name != "items_by_code");
    draft.indexes.push(IndexDraft {
        original: None,
        name: "items_by_new_name".into(),
        columns: strings(&["new_name"]),
        unique: true,
    });
    draft.foreign_keys = vec![ForeignKeyDraft {
        original: None,
        name: "items_owner_fk".into(),
        on_delete: "SET NULL".into(),
        ..draft.foreign_keys[0].clone()
    }];
    assert_eq!(draft.validate(), Vec::<String>::new());

    let changes = changes(&table, &structure, &draft);
    let statements = session.dialect().alter_table_sql(&table, &changes).unwrap();
    assert_eq!(statements.len(), 2, "{statements:#?}");
    apply(&session, &statements).await;

    let articles = TableRef::new(database.clone(), "articles");
    assert_eq!(read_back(&session, &articles).await, saved(draft));
}
