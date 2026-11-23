//! What the AI assistant tells the model of the fixture database `kerno` (K50, D-119), on
//! MySQL and MariaDB, read only: the structures of the tables a statement names are read,
//! nothing is created or changed, no row is read. Each engine whose container is down is
//! skipped with a message.

mod common;

use std::collections::HashMap;

use kerno_core::ai::{self, Action, Context};
use kerno_core::schema::TableRef;
use kerno_core::sql::Catalog;

/// A functional index (MySQL 8; MariaDB has none) can hold values: only the parts that are
/// column names are sent, the rest as `(expression)`; an index of expressions only is left
/// out. A scratch database, dropped before the assertions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expression_indexes_send_no_value() {
    use kerno_core::config::Engine;
    for engine in common::engines() {
        if engine != Engine::MySql {
            continue;
        }
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "ai_expressions").await;
        let table = format!("{database}.items");
        for sql in [
            format!(
                "CREATE TABLE {table} (id INT PRIMARY KEY, code VARCHAR(40), kind VARCHAR(20))"
            ),
            format!("CREATE INDEX items_lower ON {table} ((lower(code)), kind)"),
            format!("CREATE INDEX items_coalesce ON {table} ((coalesce(code, 'kerno-literal-1')))"),
        ] {
            common::exec_ok(&session, &sql).await;
        }
        let mut context = Context {
            engine: engine.label().into(),
            tables: vec![ai::Table {
                schema: database.clone(),
                name: "items".into(),
                view: false,
                columns: vec![("id".into(), "int".into())],
                foreign_keys: Vec::new(),
                detail: None,
            }],
            ..Context::default()
        };
        let read = ai::read_details(session.clone(), context.detail_tables()).await;
        let details: HashMap<_, _> = read
            .into_iter()
            .filter_map(|(table, detail)| Some((table, detail.ok()?)))
            .collect();
        context.add_details(&details);
        let action = Action::ExplainQuery {
            sql: "select 1".into(),
        };
        let system = ai::build_payload(&action, &context, "m")["messages"][0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        common::exec_ok(&session, &format!("DROP DATABASE {database}")).await;
        assert!(
            system.contains("\n  index ((expression), kind)"),
            "{system}"
        );
        for never in ["kerno-literal", "lower(", "coalesce"] {
            assert!(!system.contains(never), "{never}:\n{system}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tables_a_statement_names_are_described_with_their_keys_first() {
    for engine in common::engines() {
        let session = common::session(engine).await;
        let action = Action::ExplainPlan {
            sql: "select * from order_items i join orders o on o.id = i.order_id \
                  join customers c on c.id = o.customer_id"
                .into(),
            plan: "kerno test".into(),
        };
        // The app sends the tab's database only: MySQL's schemas are databases.
        let mut catalog = session.completion_catalog().await.unwrap();
        catalog = Catalog {
            schemas: (catalog.schemas.into_iter())
                .filter(|schema| schema.name == "kerno")
                .collect(),
            search_path: vec!["kerno".into()],
        };
        let mut context = Context::from_catalog(
            &catalog,
            engine.label(),
            None,
            Some("kerno".into()),
            None,
            action.text(),
        );
        assert_eq!(
            context.detail_tables(),
            [
                TableRef::new("kerno", "order_items"),
                TableRef::new("kerno", "orders"),
                TableRef::new("kerno", "customers"),
            ]
        );
        let details: HashMap<_, _> = ai::read_details(session.clone(), context.detail_tables())
            .await
            .into_iter()
            .map(|(table, detail)| (table, detail.unwrap()))
            .collect();
        context.add_details(&details);
        let system = ai::build_payload(&action, &context, "m")["messages"][0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        for line in [
            "- kerno.order_items (table)\n  order_id bigint",
            "\n  line_no smallint",
            "\n  primary key (order_id, line_no)",
            "\n  foreign key (order_id) references kerno.orders (id)",
            "\n  foreign key (product_id) references kerno.products (id)",
            "\n  index (product_id)",
            "- kerno.orders (table)\n  id bigint",
            "\n  notes text\n",
            "\n  primary key (id)",
            "\n  foreign key (customer_id) references kerno.customers (id)",
            "\n  index (customer_id)",
            "\n  index (status, placed_at)",
            "\n  unique (email)",
        ] {
            assert!(
                system.contains(line),
                "{engine:?}: {line:?} missing:\n{system}"
            );
        }
        assert!(system.contains("unsigned not null"), "{system}");
        for never in [
            "CURRENT_TIMESTAMP",
            "current_timestamp",
            "'EUR'",
            "@example.com",
        ] {
            assert!(!system.contains(never), "{engine:?}: {never}:\n{system}");
        }
    }
}
