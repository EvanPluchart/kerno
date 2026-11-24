//! Reading the catalog, rebuilding DDL and the administrative views, against
//! the fixture schema of the `kerno-postgres` container. Skipped with a
//! message when the container is down.

mod common;

use common::{exec_ok, rows, run, scalar};
use kerno_core::schema::{
    ColumnGeneration, ConstraintKind, ObjectRef, RelationKind, RoutineKind, TableRef, TypeShape,
};
use kerno_core::{AdminView, Error, TypeKind};

/// The rows of a list whose first column is not another test's scratch
/// database or schema.
fn fixture_rows(rows: &[Vec<Option<String>>]) -> Vec<&Vec<Option<String>>> {
    rows.iter()
        .filter(|row| {
            row[0].as_deref().is_some_and(|name| {
                !name.starts_with("kerno_test_") && !name.starts_with("kerno_ui_")
            })
        })
        .collect()
}

fn names<T>(items: &[T], name: impl Fn(&T) -> &str) -> Vec<&str> {
    items.iter().map(name).collect()
}

#[tokio::test]
async fn databases_and_schemas() {
    let Some(session) = common::session().await else {
        return;
    };

    let databases = session.databases().await.unwrap();
    let user: Vec<_> = databases
        .iter()
        .filter(|d| !d.is_system)
        .map(|d| d.name.as_str())
        // Other tests create and drop scratch databases meanwhile.
        .filter(|name| !name.starts_with("kerno_test_") && !name.starts_with("kerno_ui_"))
        .collect();
    assert_eq!(user, ["kerno", "kerno_other", "postgres"]);
    let templates: Vec<_> = databases
        .iter()
        .filter(|d| d.is_system)
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(templates, ["template0", "template1"]);
    let kerno = databases.iter().find(|d| d.name == "kerno").unwrap();
    assert_eq!(kerno.owner.as_deref(), Some("kerno"));
    assert_eq!(kerno.encoding.as_deref(), Some("UTF8"));
    // The seeded million-row table alone is about 300 MB.
    assert!(
        kerno.size_bytes.unwrap() > 200_000_000,
        "{:?}",
        kerno.size_bytes
    );
    // template0 does not allow connections: no size, and no error either.
    assert_eq!(
        databases
            .iter()
            .find(|d| d.name == "template0")
            .unwrap()
            .size_bytes,
        None
    );

    let schemas = session.schemas().await.unwrap();
    let user: Vec<_> = schemas
        .iter()
        .filter(|s| !s.is_system)
        .map(|s| s.name.as_str())
        .collect();
    // Other tests create scratch schemas; they all start with kerno_test_ or kerno_ui_.
    let fixture: Vec<_> = user
        .iter()
        .filter(|name| !name.starts_with("kerno_test_") && !name.starts_with("kerno_ui_"))
        .copied()
        .collect();
    assert_eq!(fixture, ["analytics", "public", "shop"]);
    let system: Vec<_> = schemas
        .iter()
        .filter(|s| s.is_system)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(system, ["information_schema", "pg_catalog"]);
    let shop = schemas.iter().find(|s| s.name == "shop").unwrap();
    assert_eq!(
        shop.comment.as_deref(),
        Some("Orders, customers and the product catalogue")
    );
    assert_eq!(shop.owner.as_deref(), Some("kerno"));
}

/// K62: the scratch database and login role of
/// [`databases_an_account_may_not_open_are_marked`]; roles are cluster-wide,
/// the `kerno_test_` prefix makes them ours.
const K62_DATABASE: &str = "kerno_test_k62_closed";
const K62_ROLE: &str = "kerno_test_k62_role";

/// K62: an account without the CONNECT privilege on a database, and
/// `template0`, which takes no connection, are told apart from what it may
/// open; a connect to the closed one is said in plain words.
#[tokio::test]
async fn databases_an_account_may_not_open_are_marked() {
    let Some(session) = common::session().await else {
        return;
    };
    let cleanup = [
        format!(
            "SELECT pg_catalog.pg_terminate_backend(pid) FROM pg_catalog.pg_stat_activity \
             WHERE usename = '{K62_ROLE}'"
        ),
        format!("DROP DATABASE IF EXISTS {K62_DATABASE} WITH (FORCE)"),
        format!("DROP ROLE IF EXISTS {K62_ROLE}"),
    ];
    for sql in &cleanup {
        exec_ok(&session, sql).await;
    }
    // Run apart, so the cleanup below also runs when it fails.
    let outcome = tokio::spawn(closed_databases(session.clone())).await;
    let mut failed = Vec::new();
    for sql in &cleanup {
        if let Err(error) = run(&session, sql).await {
            failed.push(format!("{sql}: {error}"));
        }
    }
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
    assert!(failed.is_empty(), "cleanup failed: {failed:#?}");
}

async fn closed_databases(session: std::sync::Arc<dyn kerno_core::Session>) {
    for sql in [
        format!("CREATE DATABASE {K62_DATABASE}"),
        format!("REVOKE CONNECT ON DATABASE {K62_DATABASE} FROM PUBLIC"),
        format!("CREATE ROLE {K62_ROLE} LOGIN PASSWORD 'kerno_test_pw'"),
    ] {
        exec_ok(&session, &sql).await;
    }
    // Not the superuser `kerno`, which may connect anywhere.
    let mut params = kerno_core::testing::params(kerno_core::Engine::Postgres);
    params.user = K62_ROLE.into();
    params.database = Some("kerno".into());
    let account = common::connect(&params).await.unwrap();
    let databases = account.databases().await.unwrap();
    account.close().await;
    let access = |name: &str| {
        let database = databases.iter().find(|d| d.name == name).unwrap();
        (database.allows_connections, database.can_connect)
    };
    assert_eq!(access("kerno"), (true, true));
    assert_eq!(access(K62_DATABASE), (true, false));
    assert_eq!(access("template0"), (false, false));
    assert_eq!(access("template1"), (true, true));

    // Tried anyway: refused, and said in plain words.
    params.database = Some(K62_DATABASE.into());
    let error = common::connect(&params).await.err().expect("refused");
    assert!(matches!(error, Error::Connect(_)), "{error:?}");
    let config = kerno_core::config::ConnectionConfig {
        database: Some(K62_DATABASE.into()),
        ..kerno_core::testing::config(kerno_core::Engine::Postgres)
    };
    let explained = kerno_core::error::explain(&error.to_string(), &config);
    assert_eq!(
        explained.headline,
        format!("You may not connect to database “{K62_DATABASE}”.")
    );
}

/// K71: a table another session holds locked (a long `ALTER`, a migration)
/// is listed without its size, at once: the size would wait for the lock,
/// and the tree, the backup and the copy dialogs with it.
#[tokio::test]
async fn a_locked_table_is_listed_at_once_without_its_size() {
    let (Some(session), Some(holder)) = (common::session().await, common::session().await) else {
        return;
    };
    let schema = common::scratch_schema("locked_listing");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(&session, &format!("CREATE TABLE {schema}.free (n int)")).await;
    exec_ok(&session, &format!("CREATE TABLE {schema}.locked (n int)")).await;
    exec_ok(&holder, "BEGIN").await;
    exec_ok(
        &holder,
        &format!("LOCK TABLE {schema}.locked IN ACCESS EXCLUSIVE MODE"),
    )
    .await;
    let listed =
        tokio::time::timeout(std::time::Duration::from_secs(10), session.objects(&schema)).await;
    exec_ok(&holder, "ROLLBACK").await;
    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;
    let listed = listed
        .expect("listed without waiting for the lock")
        .unwrap();
    let sizes: Vec<_> = (listed.relations.iter())
        .map(|relation| (relation.name.as_str(), relation.size_bytes.is_some()))
        .collect();
    assert_eq!(sizes, [("free", true), ("locked", false)]);
}

#[tokio::test]
async fn objects_of_a_schema() {
    let Some(session) = common::session().await else {
        return;
    };
    let shop = session.objects("shop").await.unwrap();

    let relations: Vec<_> = shop
        .relations
        .iter()
        .map(|r| (r.name.as_str(), r.kind))
        .collect();
    assert_eq!(
        relations,
        [
            ("audit_log", RelationKind::Table),
            ("categories", RelationKind::Table),
            ("customers", RelationKind::Table),
            ("order_items", RelationKind::Table),
            ("order_totals", RelationKind::View),
            ("orders", RelationKind::Table),
            ("product_sales", RelationKind::MaterializedView),
            ("products", RelationKind::Table),
            ("reviews", RelationKind::Table),
        ]
    );
    let relation = |name: &str| shop.relations.iter().find(|r| r.name == name).unwrap();
    assert_eq!(relation("orders").row_estimate, Some(5_000));
    assert!(relation("orders").size_bytes.unwrap() > 100_000);
    assert_eq!(
        relation("customers").comment.as_deref(),
        Some("People who can place orders")
    );
    // A view has neither rows of its own nor a size.
    assert_eq!(relation("order_totals").row_estimate, None);
    assert_eq!(relation("order_totals").size_bytes, None);
    assert_eq!(
        relation("order_totals").comment.as_deref(),
        Some("Order totals recomputed from the lines")
    );
    assert!(relation("product_sales").size_bytes.is_some());
    assert!(shop.relations.iter().all(|r| !r.is_partition));

    let routines: Vec<_> = shop
        .routines
        .iter()
        .map(|r| (r.name.as_str(), r.kind))
        .collect();
    assert_eq!(
        routines,
        [
            ("cancel_stale_orders", RoutineKind::Procedure),
            ("customer_lifetime_value", RoutineKind::Function),
            ("log_order_status", RoutineKind::Function),
            ("set_updated_at", RoutineKind::Function),
            ("slow_notice", RoutineKind::Function),
        ]
    );
    let ltv = &shop.routines[1];
    assert_eq!(ltv.arguments, "p_customer_id bigint");
    assert_eq!(ltv.identity_arguments, "p_customer_id bigint");
    assert_eq!(ltv.returns.as_deref(), Some("numeric"));
    assert_eq!(ltv.language.as_deref(), Some("sql"));
    assert_eq!(
        ltv.comment.as_deref(),
        Some("Sum of the paid orders of a customer")
    );
    assert_eq!(shop.routines[0].arguments, "IN p_older_than interval");
    assert_eq!(shop.routines[0].returns, None);
    assert_eq!(shop.routines[2].returns.as_deref(), Some("trigger"));

    // The declared sequence, plus the ones behind identity columns.
    let invoice = shop
        .sequences
        .iter()
        .find(|s| s.name == "invoice_number_seq")
        .unwrap();
    assert_eq!(invoice.start.as_deref(), Some("1000"));
    assert_eq!(invoice.increment.as_deref(), Some("1"));
    assert_eq!(invoice.data_type.as_deref(), Some("bigint"));
    assert_eq!(invoice.owned_by, None);
    assert!(!invoice.cycles);
    assert_eq!(invoice.last_value.as_deref(), Some("5999"));
    let identity = shop
        .sequences
        .iter()
        .find(|s| s.name == "customers_id_seq")
        .unwrap();
    assert_eq!(identity.owned_by.as_deref(), Some("customers.id"));

    let types: Vec<_> = shop.types.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(types, ["address", "email", "order_status"]);
    assert_eq!(
        shop.types[0].shape,
        TypeShape::Composite(vec![
            ("street".into(), "text".into()),
            ("city".into(), "text".into()),
            ("postal_code".into(), "text".into()),
            ("country".into(), "character(2)".into()),
        ])
    );
    match &shop.types[1].shape {
        TypeShape::Domain { base, checks } => {
            assert_eq!(base, "text");
            assert_eq!(checks.len(), 1);
            assert!(checks[0].starts_with("CHECK"), "{checks:?}");
        }
        other => panic!("expected a domain, got {other:?}"),
    }
    assert_eq!(
        shop.types[2].shape,
        TypeShape::Enum(
            [
                "pending",
                "paid",
                "shipped",
                "delivered",
                "cancelled",
                "refunded"
            ]
            .map(String::from)
            .to_vec()
        )
    );

    let triggers: Vec<_> = shop
        .triggers
        .iter()
        .map(|t| {
            format!(
                "{} on {}: {} {} FOR EACH {} -> {}",
                t.name, t.table, t.timing, t.events, t.level, t.action
            )
        })
        .collect();
    assert_eq!(
        triggers,
        [
            "customers_set_updated_at on customers: BEFORE UPDATE FOR EACH ROW -> shop.set_updated_at",
            "orders_log_status on orders: AFTER UPDATE FOR EACH ROW -> shop.log_order_status",
        ]
    );
    assert!(shop.triggers.iter().all(|t| t.enabled));
    assert!(shop.events.is_empty());
}

#[tokio::test]
async fn extension_objects_are_not_listed_as_the_users() {
    let Some(session) = common::session().await else {
        return;
    };
    let public = session.objects("public").await.unwrap();
    // pgcrypto, pg_trgm and hstore put dozens of functions and a type in
    // public; none of them is the user's.
    assert_eq!(names(&public.routines, |r| &r.name), Vec::<&str>::new());
    assert_eq!(names(&public.types, |t| &t.name), ["mood"]);
    assert_eq!(
        names(&public.relations, |r| &r.name),
        ["all_types", "kerno_seed_done"]
    );

    let extensions = session.extensions().await.unwrap();
    let installed: Vec<_> = extensions
        .iter()
        .filter(|e| e.installed_version.is_some())
        .map(|e| (e.name.as_str(), e.schema.as_deref()))
        .collect();
    assert_eq!(
        installed,
        [
            ("hstore", Some("public")),
            ("pg_trgm", Some("public")),
            ("pgcrypto", Some("public")),
            ("plpgsql", Some("pg_catalog")),
        ]
    );
    // Available ones follow, not installed.
    let available = extensions.iter().find(|e| e.name == "citext").unwrap();
    assert_eq!(available.installed_version, None);
    assert!(available.default_version.is_some());
    assert!(available.comment.is_some());

    // An unknown schema is simply empty.
    let nothing = session.objects("no_such_schema").await.unwrap();
    assert!(nothing.relations.is_empty() && nothing.routines.is_empty());
}

#[tokio::test]
async fn structure_of_a_table() {
    let Some(session) = common::session().await else {
        return;
    };
    let products = session
        .table_structure(&TableRef::new("shop", "products"))
        .await
        .unwrap();

    let columns: Vec<_> = products
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str(), c.kind, c.nullable))
        .collect();
    assert_eq!(
        columns,
        [
            ("id", "bigint", TypeKind::Integer, false),
            ("public_id", "uuid", TypeKind::Uuid, false),
            ("category_id", "integer", TypeKind::Integer, false),
            ("sku", "character varying(32)", TypeKind::Text, false),
            ("name", "text", TypeKind::Text, false),
            ("description", "text", TypeKind::Text, true),
            ("price", "numeric(10,2)", TypeKind::Decimal, false),
            ("stock", "integer", TypeKind::Integer, false),
            ("weight_kg", "real", TypeKind::Float, true),
            ("attributes", "jsonb", TypeKind::Json, false),
            ("tags", "text[]", TypeKind::Array, false),
            ("dimensions", "numeric(8,2)[]", TypeKind::Array, true),
            ("released_on", "date", TypeKind::Date, true),
            ("discontinued", "boolean", TypeKind::Boolean, false),
        ]
    );
    let column = |name: &str| products.columns.iter().find(|c| c.name == name).unwrap();
    assert_eq!(
        column("id").generation,
        Some(ColumnGeneration::IdentityAlways)
    );
    assert_eq!(column("id").primary_key_position, Some(1));
    assert_eq!(column("id").default, None);
    assert_eq!(
        column("public_id").default.as_deref(),
        Some("gen_random_uuid()")
    );
    assert_eq!(column("stock").default.as_deref(), Some("0"));
    assert_eq!(column("tags").default.as_deref(), Some("'{}'::text[]"));
    assert_eq!(column("sku").primary_key_position, None);
    assert_eq!(products.row_identity(), Some(vec!["id".to_string()]));

    let indexes: Vec<_> = products
        .indexes
        .iter()
        .map(|i| {
            (
                i.name.as_str(),
                i.columns.join(","),
                i.unique,
                i.primary,
                i.method.as_str(),
            )
        })
        .collect();
    assert_eq!(
        indexes,
        [
            ("products_pkey", "id".to_string(), true, true, "btree"),
            (
                "products_active_price_idx",
                "price".to_string(),
                false,
                false,
                "btree"
            ),
            (
                "products_attributes_idx",
                "attributes".to_string(),
                false,
                false,
                "gin"
            ),
            (
                "products_category_idx",
                "category_id".to_string(),
                false,
                false,
                "btree"
            ),
            (
                "products_name_trgm_idx",
                "name".to_string(),
                false,
                false,
                "gin"
            ),
            ("products_sku_key", "sku".to_string(), true, false, "btree"),
        ]
    );
    let partial = &products.indexes[1];
    assert_eq!(partial.predicate.as_deref(), Some("NOT discontinued"));
    assert!(
        partial
            .definition
            .as_deref()
            .unwrap()
            .starts_with("CREATE INDEX products_active_price_idx")
    );

    let constraints: Vec<_> = products
        .constraints
        .iter()
        .map(|c| (c.name.as_str(), c.kind, c.columns.join(",")))
        .collect();
    assert_eq!(
        constraints,
        [
            (
                "products_pkey",
                ConstraintKind::PrimaryKey,
                "id".to_string()
            ),
            (
                "products_sku_key",
                ConstraintKind::Unique,
                "sku".to_string()
            ),
            (
                "products_price_check",
                ConstraintKind::Check,
                "price".to_string()
            ),
            (
                "products_stock_check",
                ConstraintKind::Check,
                "stock".to_string()
            ),
        ]
    );
    assert_eq!(products.constraints[0].definition, "PRIMARY KEY (id)");
    assert_eq!(products.constraints[3].definition, "CHECK (stock >= 0)");

    assert_eq!(products.foreign_keys.len(), 1);
    let fk = &products.foreign_keys[0];
    assert_eq!(fk.name, "products_category_id_fkey");
    assert_eq!(fk.table, TableRef::new("shop", "products"));
    assert_eq!(fk.columns, ["category_id"]);
    assert_eq!(fk.referenced_table, TableRef::new("shop", "categories"));
    assert_eq!(fk.referenced_columns, ["id"]);
    assert_eq!(
        (fk.on_update.as_str(), fk.on_delete.as_str()),
        ("NO ACTION", "NO ACTION")
    );

    let incoming: Vec<_> = products
        .referenced_by
        .iter()
        .map(|fk| {
            (
                fk.table.name.as_str(),
                fk.columns.join(","),
                fk.on_delete.as_str(),
            )
        })
        .collect();
    assert_eq!(
        incoming,
        [
            ("order_items", "product_id".to_string(), "NO ACTION"),
            ("reviews", "product_id".to_string(), "CASCADE"),
        ]
    );
    assert!(products.triggers.is_empty());
}

#[tokio::test]
async fn structure_edge_cases() {
    let Some(session) = common::session().await else {
        return;
    };

    // Composite primary key, in key order.
    let items = session
        .table_structure(&TableRef::new("shop", "order_items"))
        .await
        .unwrap();
    assert_eq!(
        items.row_identity(),
        Some(vec!["order_id".to_string(), "line_no".to_string()])
    );
    assert_eq!(items.indexes[0].columns, ["order_id", "line_no"]);
    assert_eq!(items.foreign_keys.len(), 2);
    let cascade = items
        .foreign_keys
        .iter()
        .find(|fk| fk.referenced_table.name == "orders")
        .unwrap();
    assert_eq!(cascade.on_delete, "CASCADE");

    // No key at all: rows cannot be addressed.
    let audit = session
        .table_structure(&TableRef::new("shop", "audit_log"))
        .await
        .unwrap();
    assert_eq!(audit.row_identity(), None);
    assert!(audit.indexes.is_empty() && audit.constraints.is_empty());

    // Domain, enum, composite, comment, trigger, self-reference.
    let customers = session
        .table_structure(&TableRef::new("shop", "customers"))
        .await
        .unwrap();
    let column = |name: &str| customers.columns.iter().find(|c| c.name == name).unwrap();
    assert_eq!(column("email").data_type, "shop.email");
    assert_eq!(column("email").kind, TypeKind::Text);
    assert_eq!(column("shipping_address").data_type, "shop.address");
    assert_eq!(
        column("tags").comment.as_deref(),
        Some("Free-form labels used by support")
    );
    assert_eq!(
        customers.comment.as_deref(),
        Some("People who can place orders")
    );
    assert_eq!(
        names(&customers.triggers, |t| &t.name),
        ["customers_set_updated_at"]
    );

    let orders = session
        .table_structure(&TableRef::new("shop", "orders"))
        .await
        .unwrap();
    let status = orders.columns.iter().find(|c| c.name == "status").unwrap();
    assert_eq!(status.kind, TypeKind::Enum);
    assert_eq!(status.data_type, "shop.order_status");
    assert_eq!(status.enum_values.len(), 6);
    assert_eq!(status.enum_values[0], "pending");
    assert_eq!(
        status.default.as_deref(),
        Some("'pending'::shop.order_status")
    );
    let invoice = orders
        .columns
        .iter()
        .find(|c| c.name == "invoice_number")
        .unwrap();
    // A default that draws from an independent sequence is a default, not
    // a serial column: the sequence is not the column's.
    assert_eq!(invoice.generation, None);
    assert_eq!(
        invoice.default.as_deref(),
        Some("nextval('shop.invoice_number_seq'::regclass)")
    );
    assert!(invoice.nullable);

    let categories = session
        .table_structure(&TableRef::new("shop", "categories"))
        .await
        .unwrap();
    assert_eq!(
        categories.columns[0].generation,
        Some(ColumnGeneration::IdentityByDefault)
    );
    let own = &categories.foreign_keys[0];
    assert_eq!(own.referenced_table, TableRef::new("shop", "categories"));
    assert_eq!(own.on_delete, "SET NULL");
    assert!(
        categories
            .referenced_by
            .iter()
            .any(|fk| fk.table.name == "categories")
    );
    assert!(
        categories
            .referenced_by
            .iter()
            .any(|fk| fk.table.name == "products")
    );

    // Views and materialized views have columns too.
    let view = session
        .table_structure(&TableRef::new("shop", "order_totals"))
        .await
        .unwrap();
    assert_eq!(view.columns.len(), 6);
    assert_eq!(view.row_identity(), None);
    let matview = session
        .table_structure(&TableRef::new("shop", "product_sales"))
        .await
        .unwrap();
    assert_eq!(matview.columns.len(), 5);
    assert_eq!(
        names(&matview.indexes, |i| &i.name),
        ["product_sales_product_idx"]
    );

    // The all_types table: array of enum and enum labels through the element type.
    let all = session
        .table_structure(&TableRef::new("public", "all_types"))
        .await
        .unwrap();
    let enum_column = all.columns.iter().find(|c| c.name == "c_enum").unwrap();
    assert_eq!(enum_column.enum_values, ["sad", "ok", "happy"]);

    let missing = session
        .table_structure(&TableRef::new("shop", "no_such_table"))
        .await;
    assert!(matches!(missing, Err(Error::Db(_))), "{missing:?}");
}

#[tokio::test]
async fn expression_generated_and_partitioned_objects() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("structure");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(
        &session,
        &format!(
            "CREATE TABLE {schema}.\"Mixed Case\" (
                 \"Id\" serial PRIMARY KEY,
                 email text NOT NULL,
                 qty int NOT NULL,
                 price numeric NOT NULL,
                 total numeric GENERATED ALWAYS AS (qty * price) STORED,
                 note text COLLATE \"C\"
             )"
        ),
    )
    .await;
    exec_ok(
        &session,
        &format!("CREATE UNIQUE INDEX lower_email ON {schema}.\"Mixed Case\" (lower(email))"),
    )
    .await;
    exec_ok(
        &session,
        &format!("CREATE INDEX with_include ON {schema}.\"Mixed Case\" (qty) INCLUDE (price)"),
    )
    .await;
    exec_ok(
        &session,
        &format!(
            "CREATE TABLE {schema}.measurements (day date NOT NULL, value int) PARTITION BY RANGE (day);
             CREATE TABLE {schema}.measurements_2024 PARTITION OF {schema}.measurements
                 FOR VALUES FROM ('2024-01-01') TO ('2025-01-01')"
        ),
    )
    .await;

    let table = TableRef::new(schema.clone(), "Mixed Case");
    let structure = session.table_structure(&table).await.unwrap();
    let column = |name: &str| structure.columns.iter().find(|c| c.name == name).unwrap();
    assert_eq!(
        column("Id").generation,
        Some(ColumnGeneration::AutoIncrement)
    );
    assert_eq!(
        column("total").generation,
        Some(ColumnGeneration::Stored("qty::numeric * price".into()))
    );
    assert_eq!(
        column("total").default,
        None,
        "a generation expression is not a default"
    );
    assert_eq!(column("note").collation.as_deref(), Some("C"));
    assert_eq!(column("email").collation, None);

    let index = |name: &str| structure.indexes.iter().find(|i| i.name == name).unwrap();
    // An expression index reports its expression, and cannot identify rows.
    assert_eq!(index("lower_email").columns, ["lower(email)"]);
    // INCLUDE columns are not key columns.
    assert_eq!(index("with_include").columns, ["qty"]);
    // A mixed-case column is reported by its real name, unquoted.
    assert_eq!(structure.row_identity(), Some(vec!["Id".to_string()]));

    let objects = session.objects(&schema).await.unwrap();
    let kinds: Vec<_> = objects
        .relations
        .iter()
        .map(|r| (r.name.as_str(), r.kind, r.is_partition))
        .collect();
    assert_eq!(
        kinds,
        [
            ("Mixed Case", RelationKind::Table, false),
            ("measurements", RelationKind::PartitionedTable, false),
            ("measurements_2024", RelationKind::Table, true),
        ]
    );
    // Never analysed: no estimate rather than a wrong one.
    assert_eq!(objects.relations[0].row_estimate, None);

    let ddl = session
        .ddl(&ObjectRef::Relation(
            TableRef::new(schema.clone(), "measurements"),
            RelationKind::PartitionedTable,
        ))
        .await
        .unwrap();
    assert!(ddl.contains("PARTITION BY RANGE (day)"), "{ddl}");
    let ddl = session
        .ddl(&ObjectRef::Relation(
            TableRef::new(schema.clone(), "measurements_2024"),
            RelationKind::Table,
        ))
        .await
        .unwrap();
    assert!(ddl.contains("PARTITION OF"), "{ddl}");
    assert!(
        ddl.contains("FOR VALUES FROM ('2024-01-01') TO ('2025-01-01')"),
        "{ddl}"
    );

    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;
}

/// The fixture tables, recreated from their generated DDL in another
/// schema, must have the same structure as the originals.
#[tokio::test]
async fn table_ddl_recreates_the_same_structure() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("ddl");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;

    // In dependency order, so foreign keys find their targets.
    for name in [
        "categories",
        "customers",
        "products",
        "orders",
        "order_items",
        "reviews",
        "audit_log",
    ] {
        let original = TableRef::new("shop", name);
        let ddl = session
            .ddl(&ObjectRef::Relation(original.clone(), RelationKind::Table))
            .await
            .unwrap();
        // Move the table and what it references to the scratch schema;
        // types and functions stay in shop.
        let mut moved = ddl.clone();
        for table in [
            "categories",
            "customers",
            "products",
            "orders",
            "order_items",
            "reviews",
            "audit_log",
        ] {
            moved = moved.replace(
                &format!("\"shop\".\"{table}\""),
                &format!("\"{schema}\".\"{table}\""),
            );
            moved = moved.replace(&format!("shop.{table}("), &format!("{schema}.{table}("));
            moved = moved.replace(
                &format!("ON shop.{table} "),
                &format!("ON {schema}.{table} "),
            );
        }
        run(&session, &moved).await.unwrap_or_else(|error| {
            panic!("the DDL of shop.{name} does not run: {error}\n{moved}")
        });

        let copy = TableRef::new(schema.clone(), name);
        let before = session.table_structure(&original).await.unwrap();
        let after = session.table_structure(&copy).await.unwrap();
        assert_eq!(after.columns, before.columns, "columns of {name}");
        assert_eq!(after.comment, before.comment, "comment of {name}");
        let constraint = |s: &kerno_core::schema::TableStructure| {
            s.constraints
                .iter()
                .map(|c| (c.name.clone(), c.kind, c.definition.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            constraint(&after),
            constraint(&before),
            "constraints of {name}"
        );
        let index = |s: &kerno_core::schema::TableStructure| {
            s.indexes
                .iter()
                .map(|i| {
                    (
                        i.name.clone(),
                        i.columns.clone(),
                        i.unique,
                        i.method.clone(),
                        i.predicate.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(index(&after), index(&before), "indexes of {name}");
        let foreign = |s: &kerno_core::schema::TableStructure| {
            s.foreign_keys
                .iter()
                .map(|f| {
                    (
                        f.name.clone(),
                        f.columns.clone(),
                        f.referenced_table.name.clone(),
                        f.on_delete.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(foreign(&after), foreign(&before), "foreign keys of {name}");
        assert_eq!(
            names(&after.triggers, |t| &t.name),
            names(&before.triggers, |t| &t.name),
            "triggers of {name}"
        );
    }

    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;
}

#[tokio::test]
async fn ddl_of_other_objects() {
    let Some(session) = common::session().await else {
        return;
    };
    let ddl = |object: ObjectRef| {
        let session = session.clone();
        async move { session.ddl(&object).await.unwrap() }
    };

    let view = ddl(ObjectRef::Relation(
        TableRef::new("shop", "order_totals"),
        RelationKind::View,
    ))
    .await;
    assert!(
        view.starts_with("CREATE OR REPLACE VIEW \"shop\".\"order_totals\" AS\n"),
        "{view}"
    );
    assert!(view.contains("LEFT JOIN shop.order_items"), "{view}");
    assert!(
        view.contains(
            "COMMENT ON VIEW \"shop\".\"order_totals\" IS 'Order totals recomputed from the lines';"
        ),
        "{view}"
    );

    let matview = ddl(ObjectRef::Relation(
        TableRef::new("shop", "product_sales"),
        RelationKind::MaterializedView,
    ))
    .await;
    assert!(
        matview.starts_with("CREATE MATERIALIZED VIEW \"shop\".\"product_sales\" AS\n"),
        "{matview}"
    );
    assert!(matview.contains("WITH DATA;"), "{matview}");
    assert!(
        matview.contains("CREATE UNIQUE INDEX product_sales_product_idx"),
        "{matview}"
    );

    let function = ddl(ObjectRef::Routine {
        schema: "shop".into(),
        name: "customer_lifetime_value".into(),
        kind: RoutineKind::Function,
        identity_arguments: "p_customer_id bigint".into(),
    })
    .await;
    assert!(
        function.starts_with(
            "CREATE OR REPLACE FUNCTION shop.customer_lifetime_value(p_customer_id bigint)"
        ),
        "{function}"
    );
    assert!(function.contains("LANGUAGE sql"), "{function}");
    assert!(function.trim_end().ends_with(';'), "{function}");

    let procedure = ddl(ObjectRef::Routine {
        schema: "shop".into(),
        name: "cancel_stale_orders".into(),
        kind: RoutineKind::Procedure,
        identity_arguments: "IN p_older_than interval".into(),
    })
    .await;
    assert!(
        procedure.starts_with("CREATE OR REPLACE PROCEDURE shop.cancel_stale_orders"),
        "{procedure}"
    );

    let sequence = ddl(ObjectRef::Sequence(TableRef::new(
        "shop",
        "invoice_number_seq",
    )))
    .await;
    assert!(
        sequence.starts_with("CREATE SEQUENCE \"shop\".\"invoice_number_seq\""),
        "{sequence}"
    );
    assert!(sequence.contains("START WITH 1000"), "{sequence}");
    assert!(sequence.contains("NO CYCLE"), "{sequence}");

    let enumeration = ddl(ObjectRef::Type(TableRef::new("shop", "order_status"))).await;
    assert!(
        enumeration.starts_with("CREATE TYPE \"shop\".\"order_status\" AS ENUM ("),
        "{enumeration}"
    );
    assert!(
        enumeration.contains("'pending',\n    'paid'"),
        "{enumeration}"
    );
    let composite = ddl(ObjectRef::Type(TableRef::new("shop", "address"))).await;
    assert!(
        composite.contains("\"country\" character(2)"),
        "{composite}"
    );
    let domain = ddl(ObjectRef::Type(TableRef::new("shop", "email"))).await;
    assert!(
        domain.starts_with("CREATE DOMAIN \"shop\".\"email\" AS text\n    CHECK"),
        "{domain}"
    );

    let trigger = ddl(ObjectRef::Trigger {
        table: TableRef::new("shop", "orders"),
        name: "orders_log_status".into(),
    })
    .await;
    assert!(
        trigger
            .starts_with("CREATE TRIGGER orders_log_status AFTER UPDATE OF status ON shop.orders"),
        "{trigger}"
    );

    let index = ddl(ObjectRef::Index {
        table: TableRef::new("shop", "products"),
        name: "products_active_price_idx".into(),
    })
    .await;
    assert_eq!(
        index,
        "CREATE INDEX products_active_price_idx ON shop.products USING btree (price DESC) WHERE (NOT discontinued);"
    );

    assert_eq!(
        ddl(ObjectRef::Schema("shop".into())).await,
        "CREATE SCHEMA \"shop\" AUTHORIZATION \"kerno\";"
    );
    let database = ddl(ObjectRef::Database("kerno".into())).await;
    assert!(
        database.starts_with(
            "CREATE DATABASE \"kerno\"\n    WITH OWNER = \"kerno\"\n    ENCODING = 'UTF8'"
        ),
        "{database}"
    );

    // The generated scripts for the standalone objects run as they are.
    let schema = common::scratch_schema("objects");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    for script in [&sequence, &enumeration, &composite, &domain] {
        let moved = script.replace("\"shop\".", &format!("\"{schema}\"."));
        run(&session, &moved)
            .await
            .unwrap_or_else(|error| panic!("{error}\n{moved}"));
    }
    let moved = session.objects(&schema).await.unwrap();
    assert_eq!(
        names(&moved.types, |t| &t.name),
        ["address", "email", "order_status"]
    );
    assert_eq!(names(&moved.sequences, |s| &s.name), ["invoice_number_seq"]);
    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;

    // Things that do not exist, or cannot be printed, say so.
    let missing = session
        .ddl(&ObjectRef::Sequence(TableRef::new("shop", "no_such_seq")))
        .await;
    assert!(matches!(missing, Err(Error::Db(_))), "{missing:?}");
    let event = session
        .ddl(&ObjectRef::Event(TableRef::new("shop", "x")))
        .await;
    assert!(matches!(event, Err(Error::Unsupported(_))), "{event:?}");
}

#[tokio::test]
async fn administrative_views() {
    let Some(session) = common::session().await else {
        return;
    };
    let view = |view: AdminView| {
        let session = session.clone();
        async move {
            let sql = session
                .admin_query(view)
                .expect("PostgreSQL has every admin view");
            let collected = run(&session, &sql)
                .await
                .unwrap_or_else(|e| panic!("{view:?}: {e}\n{sql}"));
            let result = collected.results.into_iter().next().unwrap();
            let columns: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
            (columns, result.rows())
        }
    };

    let own_pid = scalar(&session, "SELECT pg_backend_pid()").await;
    let (columns, sessions) = view(AdminView::Sessions).await;
    assert_eq!(
        columns[0], "pid",
        "column 0 is the id the cancel/terminate calls take"
    );
    let flag = columns.iter().position(|c| c == "this_session").unwrap();
    let own: Vec<_> = sessions
        .iter()
        .filter(|row| row[flag].as_deref() == Some("t"))
        .collect();
    assert_eq!(own.len(), 1);
    assert_eq!(own[0][0].as_deref(), Some(own_pid.as_str()));

    let (columns, roles) = view(AdminView::Roles).await;
    assert_eq!(columns[0], "role");
    let role = |name: &str| {
        roles
            .iter()
            .find(|row| row[0].as_deref() == Some(name))
            .unwrap()
            .clone()
    };
    let member_of = columns.iter().position(|c| c == "member_of").unwrap();
    assert_eq!(
        role("kerno_readonly")[member_of].as_deref(),
        Some("kerno_app")
    );
    assert_eq!(
        role("kerno_app")[1].as_deref(),
        Some("f"),
        "kerno_app cannot log in"
    );
    assert_eq!(
        role("kerno")[2].as_deref(),
        Some("t"),
        "kerno is a superuser"
    );
    assert!(
        roles
            .iter()
            .all(|row| !row[0].as_deref().unwrap().starts_with("pg_"))
    );

    let (columns, grants) = view(AdminView::Grants).await;
    assert_eq!(
        &columns[..5],
        ["grantee", "kind", "schema", "object", "privileges"]
    );
    let grant = |grantee: &str, object: &str| {
        grants
            .iter()
            .find(|row| row[0].as_deref() == Some(grantee) && row[3].as_deref() == Some(object))
            .map(|row| row[4].clone().unwrap())
    };
    assert_eq!(
        grant("kerno_readonly", "audit_log").as_deref(),
        Some("INSERT, SELECT")
    );
    assert_eq!(grant("kerno_app", "orders").as_deref(), Some("SELECT"));
    assert!(
        grants.iter().any(
            |row| row[0].as_deref() == Some("kerno_app") && row[1].as_deref() == Some("schema")
        )
    );

    let (columns, databases) = view(AdminView::DatabaseSizes).await;
    assert_eq!(
        columns,
        ["database", "owner", "encoding", "size_bytes", "size"]
    );
    // Other tests' scratch databases come and go meanwhile.
    assert_eq!(
        fixture_rows(&databases)[0][0].as_deref(),
        Some("kerno"),
        "largest first"
    );

    let (columns, tables) = view(AdminView::TableSizes).await;
    assert_eq!(&columns[..2], ["schema", "table"]);
    // Other tests' scratch tables come and go meanwhile.
    let fixture = fixture_rows(&tables);
    assert_eq!(
        (fixture[0][0].as_deref(), fixture[0][1].as_deref()),
        (Some("analytics"), Some("big_table")),
        "largest first"
    );
    assert_eq!(fixture[0][2].as_deref(), Some("1000000"));

    let (columns, settings) = view(AdminView::Settings).await;
    assert_eq!(&columns[..2], ["name", "value"]);
    assert!(settings.len() > 200);
    let ssl = settings
        .iter()
        .find(|row| row[0].as_deref() == Some("ssl"))
        .unwrap();
    assert_eq!(ssl[1].as_deref(), Some("on"));

    let _ = rows(&session, "SELECT 1").await;
}

#[tokio::test]
async fn a_table_dropped_while_the_sizes_are_read_is_not_listed() {
    let Some(reader) = common::session().await else {
        return;
    };
    let other = common::session().await.unwrap();
    let schema = common::scratch_schema("sizes_gone");
    exec_ok(&other, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&other, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(
        &other,
        &format!("CREATE TABLE {schema}.t AS SELECT generate_series(1, 10) AS n"),
    )
    .await;
    // The reader's snapshot still has the table when it is dropped: its
    // size can no longer be read, as when a drop commits during the list.
    exec_ok(&reader, "BEGIN ISOLATION LEVEL REPEATABLE READ").await;
    assert_eq!(
        scalar(
            &reader,
            &format!("SELECT count(*) FROM pg_class WHERE relnamespace = '{schema}'::regnamespace")
        )
        .await,
        "1"
    );
    exec_ok(&other, &format!("DROP TABLE {schema}.t")).await;
    let sizes = reader.admin_query(AdminView::TableSizes).unwrap();
    let listed = run(&reader, &sizes).await;
    exec_ok(&reader, "ROLLBACK").await;
    exec_ok(&other, &format!("DROP SCHEMA {schema} CASCADE")).await;

    let tables = listed.unwrap().results.remove(0).rows();
    assert!(
        tables
            .iter()
            .all(|row| row[0].as_deref() != Some(schema.as_str())),
        "{tables:?}"
    );
    // Largest first: nothing without a size above the rest.
    assert!(tables.iter().all(|row| row[5].is_some()), "{tables:?}");
}

#[tokio::test]
async fn completion_catalog_lists_the_user_schemas() {
    let Some(session) = common::session().await else {
        return;
    };
    let started = std::time::Instant::now();
    let catalog = session.completion_catalog().await.unwrap();
    eprintln!("completion_catalog: {:?}", started.elapsed());

    // Other tests create scratch schemas meanwhile.
    let schemas: Vec<_> = catalog
        .schemas
        .iter()
        .map(|s| s.name.as_str())
        .filter(|name| !name.starts_with("kerno_test_") && !name.starts_with("kerno_ui_"))
        .collect();
    assert_eq!(schemas, ["analytics", "public", "shop"]);
    assert!(
        catalog
            .schemas
            .iter()
            .all(|s| s.name != "information_schema" && !s.name.starts_with("pg_")),
        "{schemas:?}"
    );
    assert_eq!(catalog.search_path, ["public"]);

    let shop = catalog.schemas.iter().find(|s| s.name == "shop").unwrap();
    let relation = |name: &str| shop.relations.iter().find(|r| r.name == name).unwrap();
    let orders = relation("orders");
    assert_eq!(orders.kind, RelationKind::Table);
    assert_eq!(
        names(&orders.columns, |c| &c.name),
        [
            "id",
            "customer_id",
            "status",
            "invoice_number",
            "placed_at",
            "shipped_at",
            "total",
            "currency",
            "notes"
        ]
    );
    assert_eq!(orders.columns[0].data_type, "bigint");
    assert_eq!(orders.columns[6].data_type, "numeric(12,2)");
    let customer = orders
        .foreign_keys
        .iter()
        .find(|fk| fk.referenced_table == "customers")
        .unwrap();
    assert_eq!(customer.columns, ["customer_id"]);
    assert_eq!(customer.referenced_schema, "shop");
    assert_eq!(customer.referenced_columns, ["id"]);
    assert_eq!(relation("order_totals").kind, RelationKind::View);
    assert!(!relation("order_totals").columns.is_empty());
    assert_eq!(
        relation("product_sales").kind,
        RelationKind::MaterializedView
    );
    // In byte order, functions and procedures alike.
    assert!(shop.routines.is_sorted(), "{:?}", shop.routines);
    for routine in ["cancel_stale_orders", "customer_lifetime_value"] {
        assert!(shop.routines.iter().any(|r| r == routine), "{routine}");
    }
    assert!(shop.relations.is_sorted_by(|a, b| a.name <= b.name));
}

#[tokio::test]
async fn the_row_estimate_of_the_big_table_is_close_to_its_count() {
    let Some(session) = common::session().await else {
        return;
    };
    let sql = session
        .dialect()
        .estimated_rows_sql(&TableRef::new("analytics", "big_table"));
    let estimate: i64 = scalar(&session, &sql).await.parse().unwrap();
    println!("estimated rows of analytics.big_table: {estimate}");
    // -1: never analysed; the fixture analyses it, but a fresh server may not.
    assert!(
        estimate == -1 || (900_000..=1_100_000).contains(&estimate),
        "{estimate}"
    );
    // No such table: no row.
    let missing = session
        .dialect()
        .estimated_rows_sql(&TableRef::new("analytics", "no_such_table"));
    assert!(rows(&session, &missing).await.is_empty());
}

/// The value of the property `name` in the one row of a properties query.
fn property(columns: &[String], row: &[Option<String>], name: &str) -> Option<String> {
    let at = columns
        .iter()
        .position(|column| column == name)
        .unwrap_or_else(|| panic!("no property {name} in {columns:?}"));
    row[at].clone()
}

#[tokio::test]
async fn properties_describe_tables_and_views() {
    let Some(session) = common::session().await else {
        return;
    };
    let describe = |table: TableRef, kind: RelationKind| {
        let session = session.clone();
        async move {
            let sql = session.dialect().properties_sql(&table, kind).unwrap();
            let collected = run(&session, &sql).await.unwrap();
            let result = collected.results.into_iter().next().unwrap();
            let columns: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
            (columns, result.rows())
        }
    };
    let (columns, rows) = describe(TableRef::new("shop", "customers"), RelationKind::Table).await;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(&columns[..3], ["Owner", "Kind", "Persistence"]);
    assert_eq!(property(&columns, row, "Owner").as_deref(), Some("kerno"));
    assert_eq!(property(&columns, row, "Kind").as_deref(), Some("table"));
    assert_eq!(
        property(&columns, row, "Persistence").as_deref(),
        Some("permanent")
    );
    assert_eq!(
        property(&columns, row, "Comment").as_deref(),
        Some("People who can place orders")
    );
    let size: u64 = property(&columns, row, "Total size")
        .unwrap()
        .parse()
        .unwrap();
    assert!(size > 0);

    // A view has no size, no rows of its own, no tablespace.
    let (columns, rows) = describe(TableRef::new("shop", "order_totals"), RelationKind::View).await;
    assert_eq!(
        property(&columns, &rows[0], "Kind").as_deref(),
        Some("view")
    );
    assert_eq!(property(&columns, &rows[0], "Total size"), None);
    assert_eq!(property(&columns, &rows[0], "Tablespace"), None);
    let (columns, rows) = describe(
        TableRef::new("shop", "product_sales"),
        RelationKind::MaterializedView,
    )
    .await;
    assert_eq!(
        property(&columns, &rows[0], "Kind").as_deref(),
        Some("materialized view")
    );
    // Nothing is found for a table that does not exist.
    let (_, rows) = describe(TableRef::new("shop", "no_such"), RelationKind::Table).await;
    assert!(rows.is_empty());
}

#[tokio::test]
async fn properties_describe_databases_and_schemas() {
    let Some(session) = common::session().await else {
        return;
    };
    let d = session.dialect();
    let describe = |sql: String| {
        let session = session.clone();
        async move {
            let collected = run(&session, &sql).await.unwrap();
            let result = collected.results.into_iter().next().unwrap();
            let columns: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
            (columns, result.rows())
        }
    };
    let (columns, rows) = describe(d.database_properties_sql("kerno").unwrap()).await;
    let row = &rows[0];
    assert_eq!(property(&columns, row, "Owner").as_deref(), Some("kerno"));
    assert_eq!(property(&columns, row, "Encoding").as_deref(), Some("UTF8"));
    assert_eq!(property(&columns, row, "Template").as_deref(), Some("f"));
    assert!(property(&columns, row, "Collation").is_some());
    let size: u64 = property(&columns, row, "Total size")
        .unwrap()
        .parse()
        .unwrap();
    assert!(size > 0);

    let (columns, rows) = describe(d.schema_properties_sql("shop").unwrap()).await;
    let row = &rows[0];
    assert_eq!(property(&columns, row, "Owner").as_deref(), Some("kerno"));
    assert_eq!(
        property(&columns, row, "Comment").as_deref(),
        Some("Orders, customers and the product catalogue")
    );
    let tables: u64 = property(&columns, row, "Tables").unwrap().parse().unwrap();
    assert!(tables > 0);
    // Nothing is found for a schema that does not exist.
    let (_, rows) = describe(d.schema_properties_sql("no_such").unwrap()).await;
    assert!(rows.is_empty());
}

#[tokio::test]
async fn a_database_is_created_with_the_options_the_server_lists() {
    let Some(session) = common::session().await else {
        return;
    };
    let d = session.dialect();
    let choices = rows(&session, d.database_options_sql().unwrap()).await;
    let values = |field: &str| -> Vec<String> {
        choices
            .iter()
            .filter(|row| row[0].as_deref() == Some(field))
            .map(|row| row[1].clone().unwrap())
            .collect()
    };
    assert!(values("owner").contains(&"kerno".to_string()));
    assert!(values("owner").iter().all(|role| !role.starts_with("pg_")));
    assert!(values("template").contains(&"template0".to_string()));
    let encodings = values("encoding");
    assert!(
        encodings.contains(&"UTF8".to_string()) && encodings.contains(&"SQL_ASCII".to_string())
    );
    // Client-only encodings cannot be a database's.
    assert!(!encodings.contains(&"SJIS".to_string()), "{encodings:?}");
    assert!(values("collation").contains(&"C".to_string()));
    assert_eq!(values("tablespace"), ["pg_default"]);

    let name = "kerno_test_created_with_options";
    exec_ok(&session, &format!("DROP DATABASE IF EXISTS {name}")).await;
    let options = kerno_core::dialect::DatabaseOptions {
        owner: Some("kerno".into()),
        template: Some("template0".into()),
        encoding: Some("UTF8".into()),
        collation: Some("C".into()),
        ctype: Some("C".into()),
        tablespace: Some("pg_default".into()),
    };
    exec_ok(
        &session,
        &d.create_database_with_sql(name, &options).unwrap(),
    )
    .await;
    let made = rows(
        &session,
        &format!(
            "SELECT pg_catalog.pg_get_userbyid(datdba), pg_catalog.pg_encoding_to_char(encoding), \
             datcollate, datctype FROM pg_catalog.pg_database WHERE datname = '{name}'"
        ),
    )
    .await;
    exec_ok(&session, &format!("DROP DATABASE {name}")).await;
    assert_eq!(
        made,
        [[Some("kerno"), Some("UTF8"), Some("C"), Some("C")].map(|v| v.map(str::to_string))]
    );
}

#[tokio::test]
async fn the_lock_view_names_the_waiting_session_and_its_blocker() {
    let Some(holder) = common::session().await else {
        return;
    };
    let waiter = common::connect(&kerno_core::testing::params(common::ENGINE))
        .await
        .unwrap();
    let watcher = common::session().await.unwrap();
    let schema = common::scratch_schema("locks");
    exec_ok(&holder, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&holder, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(
        &holder,
        &format!("CREATE TABLE {schema}.t (id int PRIMARY KEY, v int); INSERT INTO {schema}.t VALUES (1, 1)"),
    )
    .await;
    let holder_pid = scalar(&holder, "SELECT pg_backend_pid()").await;
    let waiter_pid = scalar(&waiter, "SELECT pg_backend_pid()").await;

    // The holder keeps the row; the waiter gives up on its own after a
    // few seconds, whatever happens to this test.
    exec_ok(&holder, "BEGIN").await;
    exec_ok(
        &holder,
        &format!("UPDATE {schema}.t SET v = 2 WHERE id = 1"),
    )
    .await;
    exec_ok(&waiter, "SET lock_timeout = '8s'").await;
    let blocked = {
        let (waiter, sql) = (
            waiter.clone(),
            format!("UPDATE {schema}.t SET v = 3 WHERE id = 1"),
        );
        tokio::spawn(async move { run(&waiter, &sql).await.map(drop) })
    };
    let sql = watcher.admin_query(AdminView::Locks).unwrap();
    let mut found = None;
    for _ in 0..50 {
        let collected = run(&watcher, &sql).await.unwrap();
        let result = collected.results.into_iter().next().unwrap();
        let columns: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
        if let Some(row) = result
            .rows()
            .into_iter()
            .find(|row| row[0].as_deref() == Some(waiter_pid.as_str()))
        {
            found = Some((columns, row));
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    exec_ok(&holder, "ROLLBACK").await;
    let _ = blocked.await;
    exec_ok(&holder, &format!("DROP SCHEMA {schema} CASCADE")).await;
    let (columns, row) = found.expect("the waiting session is listed");
    assert_eq!(columns[0], "waiting_id");
    let blocking = columns.iter().position(|c| c == "blocking_id").unwrap();
    assert_eq!(row[blocking].as_deref(), Some(holder_pid.as_str()));
    let query = columns.iter().position(|c| c == "waiting_query").unwrap();
    assert!(row[query].as_deref().unwrap().contains("SET v = 3"));
}
