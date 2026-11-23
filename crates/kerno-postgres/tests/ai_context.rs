//! What the AI assistant tells the model of the fixture schema `shop` (K50, D-119), read
//! only: the structures of the tables a statement names are read, nothing is created or
//! changed, no row is read. Skipped with a message when the `kerno-postgres` container is
//! down.

mod common;

use std::collections::HashMap;

use kerno_core::ai::{self, Action, Context};
use kerno_core::schema::TableRef;

/// The system text of `action` with the structures of its tables read.
async fn described(session: &std::sync::Arc<dyn kerno_core::Session>, action: &Action) -> String {
    let catalog = session.completion_catalog().await.unwrap();
    let mut context = Context::from_catalog(
        &catalog,
        "PostgreSQL",
        None,
        Some("kerno".into()),
        None,
        action.text(),
    );
    let read = ai::read_details(session.clone(), context.detail_tables()).await;
    let details: HashMap<_, _> = read
        .into_iter()
        .map(|(table, detail)| {
            (
                table.clone(),
                detail.unwrap_or_else(|why| panic!("{table:?}: {why}")),
            )
        })
        .collect();
    context.add_details(&details);
    ai::build_payload(action, &context, "m")["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tables_a_statement_names_are_described_with_their_keys_first() {
    let Some(session) = common::session().await else {
        return;
    };
    let action = Action::ExplainError {
        sql: "select * from shop.order_items i join shop.orders o on o.id = i.order_id".into(),
        error: "kerno test".into(),
    };
    let catalog = session.completion_catalog().await.unwrap();
    let context = Context::from_catalog(&catalog, "PostgreSQL", None, None, None, action.text());
    assert_eq!(
        context.detail_tables()[..2],
        [
            TableRef::new("shop", "order_items"),
            TableRef::new("shop", "orders")
        ]
    );
    let system = described(&session, &action).await;
    for line in [
        "- shop.order_items (table)\n  order_id bigint not null\n  line_no smallint not null",
        "\n  primary key (order_id, line_no)",
        "\n  foreign key (order_id) references shop.orders (id)",
        "\n  foreign key (product_id) references shop.products (id)",
        "\n  index (product_id)",
        "- shop.orders (table)\n  id bigint not null\n  customer_id bigint not null",
        "\n  status shop.order_status not null",
        "\n  invoice_number bigint\n",
        "\n  notes text",
        "\n  primary key (id)",
        "\n  foreign key (customer_id) references shop.customers (id)",
        "\n  index (customer_id)",
        "\n  index (status, placed_at",
    ] {
        assert!(system.contains(line), "{line:?} missing:\n{system}");
    }
    // The primary key's own index is the primary key line, not an index.
    assert!(!system.contains("unique (order_id, line_no)"), "{system}");
    // Never a default, a comment, a check or a value of a row.
    for never in [
        "nextval",
        "now()",
        "'pending'",
        "'EUR'",
        "quantity > 0",
        "People who can place orders",
        "@example.com",
    ] {
        assert!(!system.contains(never), "{never}:\n{system}");
    }
}

/// The system text of a context holding `table` alone, its structure read.
async fn described_alone(
    session: &std::sync::Arc<dyn kerno_core::Session>,
    table: TableRef,
) -> String {
    let mut context = Context {
        engine: "PostgreSQL".into(),
        tables: vec![ai::Table {
            schema: table.schema.clone(),
            name: table.name.clone(),
            view: false,
            columns: vec![("id".into(), "integer".into())],
            foreign_keys: Vec::new(),
            detail: None,
        }],
        ..Context::default()
    };
    let read = ai::read_details(session.clone(), vec![table]).await;
    // No panic before the caller drops its table: a failed read shows as a missing line.
    let details: HashMap<_, _> = read
        .into_iter()
        .filter_map(|(table, detail)| Some((table, detail.ok()?)))
        .collect();
    context.add_details(&details);
    let action = Action::ExplainQuery {
        sql: "select 1".into(),
    };
    ai::build_payload(&action, &context, "m")["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string()
}

/// An index on expressions and a partial index can hold values: only the parts that are column
/// names are sent, the rest as `(expression)`; an index of expressions only and a predicate are
/// left out. A scratch table, dropped before the assertions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expression_and_partial_indexes_send_no_value() {
    let Some(session) = common::session().await else {
        return;
    };
    let table = "public.kerno_test_ai_expressions";
    common::exec_ok(&session, &format!("DROP TABLE IF EXISTS {table}")).await;
    for sql in [
        format!("CREATE TABLE {table} (id integer PRIMARY KEY, code text, kind text, data jsonb)"),
        format!("CREATE INDEX kerno_test_ai_lower ON {table} (lower(code), kind DESC)"),
        format!(
            "CREATE UNIQUE INDEX kerno_test_ai_coalesce ON {table} \
             (coalesce(code, 'kerno-literal-1'))"
        ),
        format!(
            "CREATE INDEX kerno_test_ai_partial ON {table} (kind) \
             WHERE kind <> 'kerno-literal-2'"
        ),
        format!("CREATE INDEX kerno_test_ai_json ON {table} ((data ->> 'kerno-literal-3'))"),
    ] {
        common::exec_ok(&session, &sql).await;
    }
    let system = described_alone(
        &session,
        TableRef::new("public", "kerno_test_ai_expressions"),
    )
    .await;
    common::exec_ok(&session, &format!("DROP TABLE {table}")).await;
    assert!(
        system.contains("\n  index ((expression), kind)\n  index (kind)"),
        "{system}"
    );
    for never in ["kerno-literal", "lower(", "coalesce", "->>"] {
        assert!(!system.contains(never), "{never}:\n{system}");
    }
}

/// D-119's two numbers: the request of a sentence that names no table, on the whole `shop`
/// schema, before (names and types) and after (keys and indexes too). Printed; the test holds
/// that every table of `shop` is read and that the text stays within a few times the size.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_shop_schema_before_and_after() {
    let Some(session) = common::session().await else {
        return;
    };
    let action = Action::WriteSql {
        sentence: "how many rows are there".into(),
    };
    let catalog = session.completion_catalog().await.unwrap();
    let mut context = Context::from_catalog(
        &catalog,
        "PostgreSQL",
        None,
        Some("kerno".into()),
        Some("shop"),
        action.text(),
    );
    let before = ai::body(&ai::build_payload(&action, &context, "m")).len();
    let tables = context.detail_tables();
    assert_eq!(tables.len(), context.tables.len(), "every table of shop");
    let details: HashMap<_, _> = ai::read_details(session.clone(), tables)
        .await
        .into_iter()
        .map(|(table, detail)| (table, detail.unwrap()))
        .collect();
    context.add_details(&details);
    let after = ai::body(&ai::build_payload(&action, &context, "m")).len();
    eprintln!(
        "shop, {} tables: {before} bytes before, {after} after",
        context.tables.len()
    );
    assert!(after > before && after < before * 4, "{before} {after}");
}
