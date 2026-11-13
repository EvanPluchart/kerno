//! Reading the catalog, printing DDL and the administrative views, against
//! the fixture of the `kerno-mysql` and `kerno-mariadb` containers. Skipped
//! with a message when they are down.

mod common;

use std::sync::Arc;

use common::{engines, exec_ok, run, scalar};
use kerno_core::config::Engine;
use kerno_core::schema::{
    ColumnGeneration, ConstraintKind, ObjectRef, RelationKind, RoutineKind, TableRef,
    TableStructure,
};
use kerno_core::sql::{LexRules, split_statements};
use kerno_core::{AdminView, Error, Session, TypeKind};

fn names<T>(items: &[T], name: impl Fn(&T) -> &str) -> Vec<&str> {
    items.iter().map(name).collect()
}

/// Runs a script the way the app does: split into statements, one request
/// each.
async fn run_script(session: &Arc<dyn Session>, script: &str) {
    for statement in split_statements(script, LexRules::MYSQL) {
        let sql = statement.text(script);
        run(session, sql)
            .await
            .unwrap_or_else(|error| panic!("{error}\n{sql}"));
    }
}

#[tokio::test]
async fn databases_and_no_schemas() {
    for engine in engines() {
        let session = common::session(engine).await;
        let databases = session.databases().await.unwrap();
        // Other tests create scratch databases; they all start with kerno_test_ or kerno_ui_.
        let user: Vec<_> = databases
            .iter()
            .filter(|d| {
                !d.is_system
                    && !d.name.starts_with("kerno_test_")
                    && !d.name.starts_with("kerno_ui_")
            })
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(user, ["kerno", "kerno_analytics", "kerno_other"]);
        // Flagged, not hidden, and listed after the user's.
        let system: Vec<_> = databases
            .iter()
            .filter(|d| d.is_system)
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(
            system,
            ["information_schema", "mysql", "performance_schema", "sys"]
        );
        assert!(databases.iter().rev().take(4).all(|d| d.is_system));

        let analytics = databases
            .iter()
            .find(|d| d.name == "kerno_analytics")
            .unwrap();
        assert_eq!(analytics.encoding.as_deref(), Some("utf8mb4"));
        assert!(
            analytics
                .collation
                .as_deref()
                .is_some_and(|c| c.starts_with("utf8mb4_")),
            "{:?}",
            analytics.collation
        );
        // The seeded million-row table alone is over 200 MB.
        assert!(
            analytics.size_bytes.unwrap() > 200_000_000,
            "{:?}",
            analytics.size_bytes
        );
        assert_eq!(analytics.owner, None);

        // A database contains tables directly.
        assert!(session.schemas().await.unwrap().is_empty());
        assert!(session.extensions().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn objects_of_a_database() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mariadb = engine == Engine::MariaDb;
        let kerno = session.objects("kerno").await.unwrap();

        let relations: Vec<_> = kerno
            .relations
            .iter()
            .map(|r| (r.name.as_str(), r.kind))
            .collect();
        // In byte order on both engines, which collate names differently.
        assert_eq!(
            relations,
            [
                ("active_customers", RelationKind::View),
                ("all_types", RelationKind::Table),
                ("audit_log", RelationKind::Table),
                ("categories", RelationKind::Table),
                ("customers", RelationKind::Table),
                ("kerno_seed_done", RelationKind::Table),
                ("order_items", RelationKind::Table),
                ("order_totals", RelationKind::View),
                ("orders", RelationKind::Table),
                ("products", RelationKind::Table),
                ("reviews", RelationKind::Table),
            ]
        );
        let relation = |name: &str| kerno.relations.iter().find(|r| r.name == name).unwrap();
        // The storage engine's estimate: exact here, because the table is
        // small and was never changed after it was filled.
        assert_eq!(relation("orders").row_estimate, Some(5_000));
        assert!(relation("orders").size_bytes.unwrap() > 100_000);
        assert_eq!(relation("orders").engine.as_deref(), Some("InnoDB"));
        assert_eq!(
            relation("customers").comment.as_deref(),
            Some("People who can place orders")
        );
        assert_eq!(relation("orders").comment, None);
        // A view has neither rows of its own, nor a size, nor an engine, and
        // the word VIEW the server puts in its comment is not a comment.
        let view = relation("order_totals");
        assert_eq!(
            (
                view.row_estimate,
                view.size_bytes,
                &view.engine,
                &view.comment
            ),
            (None, None, &None, &None)
        );
        assert!(kerno.relations.iter().all(|r| !r.is_partition));

        let routines: Vec<_> = kerno
            .routines
            .iter()
            .map(|r| (r.name.as_str(), r.kind))
            .collect();
        assert_eq!(
            routines,
            [
                ("cancel_stale_orders", RoutineKind::Procedure),
                ("customer_lifetime_value", RoutineKind::Function),
                ("customer_orders", RoutineKind::Procedure),
            ]
        );
        // MariaDB prints display widths, MySQL 8 no longer does.
        let (bigint, int) = if mariadb {
            ("bigint(20) unsigned", "int(11)")
        } else {
            ("bigint unsigned", "int")
        };
        let ltv = &kerno.routines[1];
        assert_eq!(ltv.arguments, format!("IN p_customer_id {bigint}"));
        assert_eq!(ltv.returns.as_deref(), Some("decimal(14,2)"));
        assert_eq!(ltv.language.as_deref(), Some("SQL"));
        assert_eq!(ltv.identity_arguments, "");
        assert_eq!(
            ltv.comment.as_deref(),
            Some("Sum of the paid orders of a customer")
        );
        assert_eq!(
            kerno.routines[0].arguments,
            format!("IN p_older_than_days {int}")
        );
        assert_eq!(kerno.routines[0].returns, None);
        assert_eq!(kerno.routines[0].comment, None);
        assert_eq!(
            kerno.routines[2].arguments,
            format!("IN p_customer_id {bigint}, OUT p_count {int}")
        );

        let triggers: Vec<_> = kerno
            .triggers
            .iter()
            .map(|t| {
                format!(
                    "{} on {}: {} {} FOR EACH {}",
                    t.name, t.table, t.timing, t.events, t.level
                )
            })
            .collect();
        assert_eq!(
            triggers,
            [
                "orders_log_status on orders: AFTER UPDATE FOR EACH ROW",
                "reviews_check_rating on reviews: BEFORE INSERT FOR EACH ROW",
            ]
        );
        assert!(kerno.triggers.iter().all(|t| t.enabled));
        assert!(
            kerno.triggers[0].action.starts_with("BEGIN")
                && kerno.triggers[0].action.contains("INSERT INTO audit_log"),
            "{}",
            kerno.triggers[0].action
        );

        assert_eq!(names(&kerno.events, |e| &e.name), ["purge_old_audit_log"]);
        let event = &kerno.events[0];
        assert_eq!(event.status, "DISABLED");
        assert_eq!(event.schedule, "EVERY 1 DAY STARTS 2030-01-01 03:00:00");
        assert_eq!(
            event.comment.as_deref(),
            Some("Deletes audit entries older than a year")
        );

        assert!(kerno.sequences.is_empty());
        assert!(kerno.types.is_empty());

        // Another database, named explicitly: the session's own is kerno.
        let analytics = session.objects("kerno_analytics").await.unwrap();
        assert_eq!(
            names(&analytics.relations, |r| &r.name),
            ["big_table", "events"]
        );
        // An estimate, within a few percent of the million rows.
        let estimate = analytics.relations[0].row_estimate.unwrap();
        assert!((900_000..1_100_000).contains(&estimate), "{estimate}");
        assert!(analytics.routines.is_empty() && analytics.triggers.is_empty());

        // An unknown database is simply empty.
        let nothing = session.objects("no_such_database").await.unwrap();
        assert!(nothing.relations.is_empty() && nothing.routines.is_empty());
    }
}

#[tokio::test]
async fn structure_of_a_table() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mariadb = engine == Engine::MariaDb;
        let products = session
            .table_structure(&TableRef::new("kerno", "products"))
            .await
            .unwrap();

        // The type as the server prints it: MariaDB keeps display widths,
        // and its JSON is LONGTEXT with a check (recognised as JSON).
        let ty = |mysql: &'static str, maria: &'static str| if mariadb { maria } else { mysql };
        let columns: Vec<_> = products
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.data_type.as_str(), c.kind, c.nullable))
            .collect();
        assert_eq!(
            columns,
            [
                (
                    "id",
                    ty("bigint unsigned", "bigint(20) unsigned"),
                    TypeKind::Integer,
                    false
                ),
                ("public_id", "char(36)", TypeKind::Text, false),
                (
                    "category_id",
                    ty("int", "int(11)"),
                    TypeKind::Integer,
                    false
                ),
                ("sku", "varchar(32)", TypeKind::Text, false),
                ("name", "varchar(160)", TypeKind::Text, false),
                ("description", "text", TypeKind::Text, true),
                ("price", "decimal(10,2)", TypeKind::Decimal, false),
                ("stock", ty("int", "int(11)"), TypeKind::Integer, false),
                ("weight_kg", "float", TypeKind::Float, true),
                ("attributes", ty("json", "longtext"), TypeKind::Json, true),
                ("released_on", "date", TypeKind::Date, true),
                ("discontinued", "tinyint(1)", TypeKind::Boolean, false),
                (
                    "price_cents",
                    ty("bigint", "bigint(20)"),
                    TypeKind::Integer,
                    true
                ),
            ]
        );
        let column = |name: &str| products.columns.iter().find(|c| c.name == name).unwrap();
        assert_eq!(
            column("id").generation,
            Some(ColumnGeneration::AutoIncrement)
        );
        assert_eq!(column("id").primary_key_position, Some(1));
        assert_eq!(column("id").default, None);
        assert_eq!(column("stock").default.as_deref(), Some("0"));
        assert_eq!(column("discontinued").default.as_deref(), Some("0"));
        // NOT NULL without a default, and nullable without one: no default.
        assert_eq!(column("sku").default, None);
        assert_eq!(column("description").default, None);
        assert_eq!(column("sku").primary_key_position, None);
        assert_eq!(column("name").comment.as_deref(), Some("Display name"));
        assert_eq!(column("sku").comment, None);
        assert_eq!(column("sku").collation, None);
        // A stored generated column: its expression, and no default.
        assert_eq!(
            column("price_cents").generation,
            Some(ColumnGeneration::Stored(
                ty(
                    "cast((`price` * 100) as signed)",
                    "cast(`price` * 100 as signed)"
                )
                .into()
            ))
        );
        assert_eq!(column("price_cents").default, None);
        assert_eq!(products.row_identity(), Some(vec!["id".to_string()]));
        assert_eq!(products.comment, None);

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
                ("PRIMARY", "id".to_string(), true, true, "BTREE"),
                (
                    "products_category_idx",
                    "category_id".to_string(),
                    false,
                    false,
                    "BTREE"
                ),
                (
                    "products_name_ft",
                    "name".to_string(),
                    false,
                    false,
                    "FULLTEXT"
                ),
                ("products_sku_key", "sku".to_string(), true, false, "BTREE"),
            ]
        );
        assert_eq!(
            products.indexes[2].definition.as_deref(),
            Some("CREATE FULLTEXT INDEX `products_name_ft` ON `kerno`.`products` (`name`);")
        );
        assert!(products.indexes.iter().all(|i| i.predicate.is_none()));

        let constraints: Vec<_> = products
            .constraints
            .iter()
            .map(|c| (c.name.as_str(), c.kind, c.definition.as_str()))
            .collect();
        let mut expected = vec![
            ("PRIMARY", ConstraintKind::PrimaryKey, "PRIMARY KEY (`id`)"),
            ("products_sku_key", ConstraintKind::Unique, "UNIQUE (`sku`)"),
        ];
        if mariadb {
            // The check behind MariaDB's JSON, as the server lists it.
            expected.push((
                "attributes",
                ConstraintKind::Check,
                "CHECK (json_valid(`attributes`))",
            ));
        }
        // The same text on both, though MySQL wraps it in parentheses.
        expected.push((
            "products_price_check",
            ConstraintKind::Check,
            "CHECK (`price` >= 0)",
        ));
        assert_eq!(constraints, expected);
        assert_eq!(products.constraints[0].columns, ["id"]);
        assert_eq!(products.constraints[1].columns, ["sku"]);

        // What the server reports when no action was declared.
        let unspecified = ty("NO ACTION", "RESTRICT");
        assert_eq!(products.foreign_keys.len(), 1);
        let fk = &products.foreign_keys[0];
        assert_eq!(fk.name, "products_category_fk");
        assert_eq!(fk.table, TableRef::new("kerno", "products"));
        assert_eq!(fk.columns, ["category_id"]);
        assert_eq!(fk.referenced_table, TableRef::new("kerno", "categories"));
        assert_eq!(fk.referenced_columns, ["id"]);
        assert_eq!(
            (fk.on_update.as_str(), fk.on_delete.as_str()),
            (unspecified, unspecified)
        );

        let incoming: Vec<_> = products
            .referenced_by
            .iter()
            .map(|fk| {
                (
                    fk.name.as_str(),
                    fk.table.name.as_str(),
                    fk.columns.join(","),
                    fk.referenced_columns.join(","),
                    fk.on_delete.as_str(),
                )
            })
            .collect();
        assert_eq!(
            incoming,
            [
                (
                    "order_items_product_fk",
                    "order_items",
                    "product_id".to_string(),
                    "id".to_string(),
                    unspecified
                ),
                (
                    "reviews_product_fk",
                    "reviews",
                    "product_id".to_string(),
                    "id".to_string(),
                    "CASCADE"
                ),
            ]
        );
        assert!(products.triggers.is_empty());
    }
}

#[tokio::test]
async fn structure_edge_cases() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mariadb = engine == Engine::MariaDb;
        let structure = |name: &'static str| {
            let session = session.clone();
            async move {
                session
                    .table_structure(&TableRef::new("kerno", name))
                    .await
                    .unwrap()
            }
        };

        // Composite primary key, in key order.
        let items = structure("order_items").await;
        assert_eq!(
            items.row_identity(),
            Some(vec!["order_id".to_string(), "line_no".to_string()])
        );
        assert_eq!(items.indexes[0].columns, ["order_id", "line_no"]);
        assert_eq!(
            items.indexes[0].definition.as_deref(),
            Some("ALTER TABLE `kerno`.`order_items` ADD PRIMARY KEY (`order_id`, `line_no`);")
        );
        let positions: Vec<_> = items
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.primary_key_position))
            .collect();
        assert_eq!(
            positions,
            [
                ("order_id", Some(1)),
                ("line_no", Some(2)),
                ("product_id", None),
                ("quantity", None),
                ("unit_price", None)
            ]
        );
        assert_eq!(
            names(&items.foreign_keys, |fk| &fk.name),
            ["order_items_order_fk", "order_items_product_fk"]
        );
        assert_eq!(items.foreign_keys[0].on_delete, "CASCADE");
        assert_eq!(items.foreign_keys[0].referenced_table.name, "orders");

        // No key at all: rows cannot be addressed.
        let audit = structure("audit_log").await;
        assert_eq!(audit.row_identity(), None);
        assert!(audit.indexes.is_empty());
        assert!(
            audit
                .constraints
                .iter()
                .all(|c| c.kind == ConstraintKind::Check),
            "{:?}",
            audit.constraints
        );
        assert_eq!(audit.columns[3].kind, TypeKind::Json);

        // Enum and set labels, defaults in one form on both engines,
        // comment, incoming references, multi-column index.
        let customers = structure("customers").await;
        let column = |name: &str| customers.columns.iter().find(|c| c.name == name).unwrap();
        assert_eq!(column("tier").kind, TypeKind::Enum);
        assert_eq!(
            column("tier").data_type,
            "enum('standard','vip','wholesale')"
        );
        assert_eq!(column("tier").enum_values, ["standard", "vip", "wholesale"]);
        assert_eq!(column("tier").default.as_deref(), Some("'standard'"));
        assert_eq!(column("channels").kind, TypeKind::Enum);
        assert_eq!(column("channels").enum_values, ["email", "sms", "post"]);
        // The empty set, which is a default, not the absence of one.
        assert_eq!(column("channels").default.as_deref(), Some("''"));
        assert_eq!(column("is_active").kind, TypeKind::Boolean);
        assert_eq!(column("is_active").default.as_deref(), Some("1"));
        assert_eq!(
            column("updated_at").default.as_deref(),
            Some("CURRENT_TIMESTAMP")
        );
        assert_eq!(column("updated_at").generation, None);
        assert_eq!(column("phone").default, None);
        assert_eq!(column("email").enum_values, Vec::<String>::new());
        assert_eq!(
            customers.comment.as_deref(),
            Some("People who can place orders")
        );
        let index = customers
            .indexes
            .iter()
            .find(|i| i.name == "customers_country_city_idx")
            .unwrap();
        assert_eq!(index.columns, ["country", "city"]);
        assert!(!index.unique);
        assert_eq!(
            names(&customers.referenced_by, |fk| &fk.name),
            ["orders_customer_fk", "reviews_customer_fk"]
        );
        assert_eq!(customers.referenced_by[0].on_delete, "RESTRICT");
        assert!(customers.foreign_keys.is_empty());

        // A table that references itself appears on both sides.
        let categories = structure("categories").await;
        let own = &categories.foreign_keys[0];
        assert_eq!(own.referenced_table, TableRef::new("kerno", "categories"));
        assert_eq!(own.on_delete, "SET NULL");
        assert_eq!(
            names(&categories.referenced_by, |fk| &fk.table.name),
            ["categories", "products"]
        );

        let orders = structure("orders").await;
        assert_eq!(names(&orders.triggers, |t| &t.name), ["orders_log_status"]);
        assert_eq!(
            orders
                .columns
                .iter()
                .find(|c| c.name == "total")
                .unwrap()
                .default
                .as_deref(),
            Some("0.00")
        );
        assert_eq!(
            orders
                .columns
                .iter()
                .find(|c| c.name == "currency")
                .unwrap()
                .default
                .as_deref(),
            Some("'EUR'")
        );

        // Views have columns, and nothing that identifies a row.
        let view = structure("order_totals").await;
        assert_eq!(view.columns.len(), 6);
        assert_eq!(view.row_identity(), None);
        assert!(view.indexes.is_empty() && view.constraints.is_empty());
        assert_eq!(view.comment, None);

        // Every type of all_types has its family.
        let all = structure("all_types").await;
        let kinds: Vec<_> = all
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("id", TypeKind::Integer),
                ("c_tinyint", TypeKind::Integer),
                ("c_tinyint_u", TypeKind::Integer),
                ("c_bool", TypeKind::Boolean),
                ("c_smallint", TypeKind::Integer),
                ("c_mediumint", TypeKind::Integer),
                ("c_int", TypeKind::Integer),
                ("c_int_u", TypeKind::Integer),
                ("c_bigint", TypeKind::Integer),
                ("c_bigint_u", TypeKind::Integer),
                ("c_decimal", TypeKind::Decimal),
                ("c_float", TypeKind::Float),
                ("c_double", TypeKind::Float),
                ("c_bit", TypeKind::Bit),
                ("c_bit1", TypeKind::Bit),
                ("c_char", TypeKind::Text),
                ("c_varchar", TypeKind::Text),
                ("c_tinytext", TypeKind::Text),
                ("c_text", TypeKind::Text),
                ("c_mediumtext", TypeKind::Text),
                ("c_longtext", TypeKind::Text),
                ("c_binary", TypeKind::Binary),
                ("c_varbinary", TypeKind::Binary),
                ("c_blob", TypeKind::Binary),
                ("c_date", TypeKind::Date),
                ("c_time", TypeKind::Time),
                ("c_datetime", TypeKind::DateTime),
                ("c_timestamp", TypeKind::DateTime),
                ("c_year", TypeKind::Integer),
                ("c_enum", TypeKind::Enum),
                ("c_set", TypeKind::Enum),
                ("c_json", TypeKind::Json),
                ("c_point", TypeKind::Binary),
            ]
        );
        let data_type = |name: &str| {
            all.columns
                .iter()
                .find(|c| c.name == name)
                .unwrap()
                .data_type
                .as_str()
        };
        assert_eq!(data_type("c_bit"), "bit(8)");
        assert_eq!(data_type("c_decimal"), "decimal(20,6)");
        assert_eq!(data_type("c_time"), "time(6)");
        assert_eq!(
            data_type("c_bigint_u"),
            if mariadb {
                "bigint(20) unsigned"
            } else {
                "bigint unsigned"
            }
        );
        assert_eq!(data_type("c_set"), "set('a','b','c')");
        assert_eq!(
            all.columns
                .iter()
                .find(|c| c.name == "c_enum")
                .unwrap()
                .enum_values,
            ["sad", "ok", "happy"]
        );

        let missing = session
            .table_structure(&TableRef::new("kerno", "no_such_table"))
            .await;
        assert!(matches!(missing, Err(Error::Db(_))), "{missing:?}");
    }
}

#[tokio::test]
async fn defaults_generated_columns_and_special_indexes() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mariadb = engine == Engine::MariaDb;
        let database = common::fresh_database(&session, "structure").await;
        exec_ok(
            &session,
            &format!(
                r"CREATE TABLE {database}.`Mixed Case` (
                    `Id` INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
                    s VARCHAR(20) NOT NULL DEFAULT 'it''s \\ x',
                    blank VARCHAR(10) DEFAULT '',
                    n INT DEFAULT -5,
                    f DOUBLE DEFAULT 1.5,
                    b BIT(4) DEFAULT b'101',
                    u CHAR(36) DEFAULT (UUID()),
                    d DATE DEFAULT '2020-01-02',
                    ts TIMESTAMP(6) NULL DEFAULT CURRENT_TIMESTAMP(6) ON UPDATE CURRENT_TIMESTAMP(6),
                    no_default VARCHAR(5) DEFAULT NULL,
                    null_word VARCHAR(5) DEFAULT 'NULL',
                    doubled INT AS (n * 2) VIRTUAL,
                    label VARCHAR(30) AS (CONCAT(s, 'a''b')) STORED,
                    choice ENUM('a', 'b''c', 'd,e') DEFAULT 'b''c',
                    latin VARCHAR(5) CHARACTER SET latin1 COLLATE latin1_bin,
                    CONSTRAINT not_quote CHECK (s <> 'a''b'),
                    UNIQUE KEY by_s_prefix (s(5)),
                    KEY by_n_desc (n DESC, f)
                ) COMMENT = 'it''s a probe'"
            ),
        )
        .await;

        let table = TableRef::new(database.clone(), "Mixed Case");
        let structure = session.table_structure(&table).await.unwrap();
        let column = |name: &str| structure.columns.iter().find(|c| c.name == name).unwrap();

        // Defaults as SQL, the same on both engines although they print
        // them differently.
        for (name, expected) in [
            ("Id", None),
            // The text is `it's \ x`: a backslash makes the dialect write it
            // in the form that does not depend on the backslash mode.
            ("s", Some("_utf8mb4 X'69742773205c2078'")),
            ("blank", Some("''")),
            ("n", Some("-5")),
            ("f", Some("1.5")),
            ("b", Some("b'101'")),
            ("u", Some("(uuid())")),
            ("d", Some("'2020-01-02'")),
            ("ts", Some("CURRENT_TIMESTAMP(6)")),
            ("no_default", None),
            ("null_word", Some("'NULL'")),
            ("doubled", None),
            ("label", None),
            ("choice", Some("'b''c'")),
        ] {
            assert_eq!(column(name).default.as_deref(), expected, "{name}");
        }
        // Each default, written back as it was read, is the value a row
        // gets when nothing is given.
        exec_ok(
            &session,
            &session
                .dialect()
                .insert_defaults(&TableRef::new(database.clone(), "Mixed Case")),
        )
        .await;
        for name in ["s", "blank", "n", "f", "d", "null_word", "choice"] {
            let default = column(name).default.clone().unwrap();
            assert_eq!(
                scalar(
                    &session,
                    &format!("SELECT {default} <=> `{name}` FROM {database}.`Mixed Case`")
                )
                .await,
                "1",
                "{name}: {default}"
            );
        }

        assert_eq!(
            column("Id").generation,
            Some(ColumnGeneration::AutoIncrement)
        );
        assert_eq!(
            column("doubled").generation,
            Some(ColumnGeneration::Virtual("`n` * 2".into()))
        );
        // The expression is SQL again: MySQL stores it with every quote
        // escaped by a backslash.
        match &column("label").generation {
            Some(ColumnGeneration::Stored(expression)) => {
                assert!(
                    expression.starts_with("concat(`s`,") && expression.ends_with("'a\\'b')"),
                    "{expression}"
                );
                assert_eq!(
                    scalar(
                        &session,
                        &format!("SELECT {expression} FROM (SELECT 'x' AS s) one")
                    )
                    .await,
                    "xa'b"
                );
            }
            other => panic!("expected a stored generated column, got {other:?}"),
        }
        assert_eq!(column("choice").enum_values, ["a", "b'c", "d,e"]);
        // A collation is reported only when it is not the table's.
        assert_eq!(column("latin").collation.as_deref(), Some("latin1_bin"));
        assert_eq!(column("s").collation, None);
        assert_eq!(structure.comment.as_deref(), Some("it's a probe"));

        // A mixed-case column is reported by its real name; the prefix and
        // the direction of key parts are in the index's statement.
        assert_eq!(structure.row_identity(), Some(vec!["Id".to_string()]));
        let index = |name: &str| structure.indexes.iter().find(|i| i.name == name).unwrap();
        assert_eq!(index("by_s_prefix").columns, ["s"]);
        assert!(index("by_s_prefix").unique);
        assert_eq!(
            index("by_s_prefix").definition.as_deref(),
            Some(
                format!(
                    "CREATE UNIQUE INDEX `by_s_prefix` ON `{database}`.`Mixed Case` (`s`(5)) USING BTREE;"
                )
                .as_str()
            )
        );
        assert_eq!(index("by_n_desc").columns, ["n", "f"]);
        assert_eq!(
            index("by_n_desc").definition.as_deref(),
            Some(
                format!(
                    "CREATE INDEX `by_n_desc` ON `{database}`.`Mixed Case` (`n` DESC, `f`) USING BTREE;"
                )
                .as_str()
            )
        );

        let check = structure
            .constraints
            .iter()
            .find(|c| c.name == "not_quote")
            .unwrap();
        assert_eq!(check.kind, ConstraintKind::Check);
        assert!(
            check.definition.starts_with("CHECK (`s` <> ")
                && check.definition.ends_with("'a\\'b')"),
            "{}",
            check.definition
        );

        // The index statements run as they are, and rebuild the same indexes.
        for name in ["by_s_prefix", "by_n_desc"] {
            let definition = index(name).definition.clone().unwrap();
            exec_ok(
                &session,
                &format!("DROP INDEX `{name}` ON {database}.`Mixed Case`"),
            )
            .await;
            run_script(&session, &definition).await;
        }
        let rebuilt = session.table_structure(&table).await.unwrap();
        assert_eq!(rebuilt.indexes, structure.indexes);

        if !mariadb {
            // A functional key part has no column, only an expression
            // (MySQL 8.0.13; MariaDB indexes generated columns instead).
            exec_ok(
                &session,
                &format!("CREATE INDEX by_lower ON {database}.`Mixed Case` ((LOWER(s)), n)"),
            )
            .await;
            let structure = session.table_structure(&table).await.unwrap();
            let index = structure
                .indexes
                .iter()
                .find(|i| i.name == "by_lower")
                .unwrap();
            assert_eq!(index.columns, ["lower(`s`)", "n"]);
            let definition = index.definition.clone().unwrap();
            assert_eq!(
                definition,
                format!(
                    "CREATE INDEX `by_lower` ON `{database}`.`Mixed Case` ((lower(`s`)), `n`) USING BTREE;"
                )
            );
            exec_ok(
                &session,
                &format!("DROP INDEX by_lower ON {database}.`Mixed Case`"),
            )
            .await;
            run_script(&session, &definition).await;
            assert_eq!(
                session.table_structure(&table).await.unwrap().indexes,
                structure.indexes
            );
        }

        // A unique index over a nullable column does not identify rows; one
        // over NOT NULL columns does when there is no primary key.
        exec_ok(
            &session,
            &format!(
                "CREATE TABLE {database}.keys_only (a INT NULL, b INT NOT NULL, c INT NOT NULL, \
                 UNIQUE KEY a_nullable (a), UNIQUE KEY z_solid (c, b))"
            ),
        )
        .await;
        let keys = session
            .table_structure(&TableRef::new(database.clone(), "keys_only"))
            .await
            .unwrap();
        assert_eq!(
            keys.row_identity(),
            Some(vec!["c".to_string(), "b".to_string()])
        );
        assert!(
            keys.columns
                .iter()
                .all(|c| c.primary_key_position.is_none())
        );

        // A partitioned table is one table.
        exec_ok(
            &session,
            &format!(
                "CREATE TABLE {database}.measurements (day DATE NOT NULL, value INT) \
                 PARTITION BY RANGE (YEAR(day)) \
                 (PARTITION p2024 VALUES LESS THAN (2025), PARTITION rest VALUES LESS THAN MAXVALUE)"
            ),
        )
        .await;
        let objects = session.objects(&database).await.unwrap();
        let kinds: Vec<_> = objects
            .relations
            .iter()
            .map(|r| (r.name.as_str(), r.kind, r.is_partition))
            .collect();
        assert_eq!(
            kinds,
            [
                ("Mixed Case", RelationKind::Table, false),
                ("keys_only", RelationKind::Table, false),
                ("measurements", RelationKind::PartitionedTable, false),
            ]
        );
        let ddl = session
            .ddl(&ObjectRef::Relation(
                TableRef::new(database.clone(), "measurements"),
                RelationKind::PartitionedTable,
            ))
            .await
            .unwrap();
        assert!(ddl.contains("PARTITION BY RANGE"), "{ddl}");

        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

#[tokio::test]
async fn sequences_where_the_engine_has_them() {
    for engine in engines() {
        let session = common::session(engine).await;
        let sequence = ObjectRef::Sequence(TableRef::new("kerno", "no_such_sequence"));
        if engine != Engine::MariaDb {
            // MySQL has no sequences at all.
            let error = session.ddl(&sequence).await.unwrap_err();
            assert!(matches!(error, Error::Unsupported(_)), "{error:?}");
            continue;
        }
        let error = session.ddl(&sequence).await.unwrap_err();
        assert!(matches!(error, Error::Db(_)), "{error:?}");

        let database = common::fresh_database(&session, "sequences").await;
        exec_ok(
            &session,
            &format!(
                "CREATE SEQUENCE {database}.invoice_numbers START WITH 1000 INCREMENT BY 5 \
                 MINVALUE 10 MAXVALUE 9000 CYCLE"
            ),
        )
        .await;
        exec_ok(&session, &format!("CREATE SEQUENCE {database}.plain")).await;
        // Other things only MariaDB has: its own UUID and address types.
        exec_ok(
            &session,
            &format!("CREATE TABLE {database}.only_here (id UUID PRIMARY KEY, address INET6)"),
        )
        .await;
        exec_ok(
            &session,
            &format!(
                "INSERT INTO {database}.only_here VALUES ('A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11', '::1')"
            ),
        )
        .await;
        let structure = session
            .table_structure(&TableRef::new(database.clone(), "only_here"))
            .await
            .unwrap();
        assert_eq!(
            structure
                .columns
                .iter()
                .map(|c| (c.data_type.as_str(), c.kind))
                .collect::<Vec<_>>(),
            [("uuid", TypeKind::Uuid), ("inet6", TypeKind::Other)]
        );
        assert_eq!(
            common::rows(&session, &format!("SELECT * FROM {database}.only_here")).await,
            [[
                Some("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".to_string()),
                Some("::1".to_string())
            ]]
        );
        exec_ok(&session, &format!("DROP TABLE {database}.only_here")).await;
        exec_ok(
            &session,
            &format!("CREATE TABLE {database}.t (id INT PRIMARY KEY)"),
        )
        .await;

        let objects = session.objects(&database).await.unwrap();
        // A sequence is not listed among the tables, though the server
        // keeps it there.
        assert_eq!(names(&objects.relations, |r| &r.name), ["t"]);
        assert_eq!(
            names(&objects.sequences, |s| &s.name),
            ["invoice_numbers", "plain"]
        );
        let invoice = &objects.sequences[0];
        assert_eq!(invoice.start.as_deref(), Some("1000"));
        assert_eq!(invoice.increment.as_deref(), Some("5"));
        assert_eq!(invoice.min.as_deref(), Some("10"));
        assert_eq!(invoice.max.as_deref(), Some("9000"));
        assert!(invoice.cycles);
        assert!(!objects.sequences[1].cycles);
        assert_eq!(objects.sequences[1].start.as_deref(), Some("1"));

        let ddl = session
            .ddl(&ObjectRef::Sequence(TableRef::new(
                database.clone(),
                "invoice_numbers",
            )))
            .await
            .unwrap();
        assert!(
            ddl.starts_with("CREATE SEQUENCE `invoice_numbers`"),
            "{ddl}"
        );
        assert!(
            ddl.contains("start with 1000") && ddl.contains("increment by 5"),
            "{ddl}"
        );
        assert!(ddl.ends_with(';'), "{ddl}");
        // It runs as it is, in the sequence's database.
        exec_ok(
            &session,
            &format!("DROP SEQUENCE {database}.invoice_numbers"),
        )
        .await;
        exec_ok(&session, &format!("USE {database}")).await;
        run_script(&session, &ddl).await;
        assert_eq!(
            session.objects(&database).await.unwrap().sequences[0],
            *invoice
        );

        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

/// The fixture tables, recreated from their generated DDL in another
/// database, must have the same structure as the originals.
#[tokio::test]
async fn table_ddl_recreates_the_same_structure() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "ddl").await;
        // The scripts leave names unqualified, as the server prints them:
        // they run in the database the objects belong to.
        exec_ok(&session, &format!("USE {database}")).await;

        // In dependency order, so foreign keys find their targets.
        for name in [
            "categories",
            "customers",
            "products",
            "orders",
            "order_items",
            "reviews",
            "audit_log",
            "all_types",
        ] {
            let original = TableRef::new("kerno", name);
            let ddl = session
                .ddl(&ObjectRef::Relation(original.clone(), RelationKind::Table))
                .await
                .unwrap();
            assert!(
                ddl.starts_with(&format!("CREATE TABLE `{name}` (")),
                "{ddl}"
            );
            run_script(&session, &ddl).await;

            let copy = TableRef::new(database.clone(), name);
            let before = session.table_structure(&original).await.unwrap();
            let after = session.table_structure(&copy).await.unwrap();
            assert_eq!(after.columns, before.columns, "columns of {name}");
            assert_eq!(after.comment, before.comment, "comment of {name}");
            assert_eq!(
                after.constraints, before.constraints,
                "constraints of {name}"
            );
            let index = |s: &TableStructure| {
                s.indexes
                    .iter()
                    .map(|i| {
                        (
                            i.name.clone(),
                            i.columns.clone(),
                            i.unique,
                            i.primary,
                            i.method.clone(),
                            // The statement names the table's database.
                            i.definition
                                .clone()
                                .map(|d| d.replace(&format!("`{database}`."), "`kerno`.")),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(index(&after), index(&before), "indexes of {name}");
            // A reference to a table of the same database follows the table.
            let foreign = |s: &TableStructure, schema: &str| {
                s.foreign_keys
                    .iter()
                    .map(|f| {
                        assert_eq!(f.table.schema, schema);
                        assert_eq!(f.referenced_table.schema, schema);
                        (
                            f.name.clone(),
                            f.columns.clone(),
                            f.referenced_table.name.clone(),
                            f.referenced_columns.clone(),
                            f.on_update.clone(),
                            f.on_delete.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                foreign(&after, &database),
                foreign(&before, "kerno"),
                "foreign keys of {name}"
            );
            let trigger = |s: &TableStructure| {
                s.triggers
                    .iter()
                    .map(|t| {
                        (
                            t.name.clone(),
                            t.timing.clone(),
                            t.events.clone(),
                            t.action.clone(),
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(trigger(&after), trigger(&before), "triggers of {name}");
        }
        // The triggers came with their tables.
        assert_eq!(
            names(&session.objects(&database).await.unwrap().triggers, |t| &t
                .name),
            ["orders_log_status", "reviews_check_rating"]
        );

        // The fixture itself was only read.
        assert_eq!(session.objects("kerno").await.unwrap().relations.len(), 11);
        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

#[tokio::test]
async fn ddl_of_other_objects() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mariadb = engine == Engine::MariaDb;
        let ddl = |object: ObjectRef| {
            let session = session.clone();
            async move {
                session
                    .ddl(&object)
                    .await
                    .unwrap_or_else(|error| panic!("{object:?}: {error}"))
            }
        };
        let routine = |name: &str, kind: RoutineKind| ObjectRef::Routine {
            schema: "kerno".into(),
            name: name.into(),
            kind,
            identity_arguments: String::new(),
        };

        let view = ddl(ObjectRef::Relation(
            TableRef::new("kerno", "order_totals"),
            RelationKind::View,
        ))
        .await;
        assert!(view.starts_with("CREATE "), "{view}");
        assert!(view.contains(" VIEW `order_totals` AS select "), "{view}");
        assert!(view.contains("left join `order_items` `i`"), "{view}");
        assert!(view.ends_with(';') && !view.ends_with(";;"), "{view}");

        let function = ddl(routine("customer_lifetime_value", RoutineKind::Function)).await;
        assert!(function.starts_with("DELIMITER //\nCREATE "), "{function}");
        assert!(
            function.contains(
                "FUNCTION `customer_lifetime_value`(p_customer_id BIGINT UNSIGNED) RETURNS decimal(14,2)"
            ),
            "{function}"
        );
        assert!(function.contains("RETURN v_total;"), "{function}");
        assert!(function.ends_with("END//\nDELIMITER ;"), "{function}");

        let procedure = ddl(routine("customer_orders", RoutineKind::Procedure)).await;
        assert!(
            procedure.contains(
                "PROCEDURE `customer_orders`(IN p_customer_id BIGINT UNSIGNED, OUT p_count INT)"
            ),
            "{procedure}"
        );

        let trigger = ddl(ObjectRef::Trigger {
            table: TableRef::new("kerno", "reviews"),
            name: "reviews_check_rating".into(),
        })
        .await;
        assert!(trigger.starts_with("DELIMITER //\nCREATE "), "{trigger}");
        assert!(trigger.contains("reviews_check_rating"), "{trigger}");
        assert!(trigger.contains("SIGNAL SQLSTATE '45000'"), "{trigger}");

        let event = ddl(ObjectRef::Event(TableRef::new(
            "kerno",
            "purge_old_audit_log",
        )))
        .await;
        assert!(
            event.contains(
                "EVENT `purge_old_audit_log` ON SCHEDULE EVERY 1 DAY STARTS '2030-01-01 03:00:00'"
            ),
            "{event}"
        );
        assert!(event.contains("DISABLE"), "{event}");
        assert!(event.contains("DO DELETE FROM audit_log"), "{event}");

        // An index has no SHOW CREATE: its statement is built from the catalog.
        let index = ddl(ObjectRef::Index {
            table: TableRef::new("kerno", "customers"),
            name: "customers_country_city_idx".into(),
        })
        .await;
        assert_eq!(
            index,
            "CREATE INDEX `customers_country_city_idx` ON `kerno`.`customers` (`country`, `city`) USING BTREE;"
        );

        // A database is the schema of these engines: both names work.
        let database = ddl(ObjectRef::Database("kerno".into())).await;
        assert!(
            database.starts_with("CREATE DATABASE `kerno` "),
            "{database}"
        );
        assert!(database.contains("utf8mb4"), "{database}");
        assert!(database.ends_with(';'), "{database}");
        assert_eq!(
            ddl(ObjectRef::Schema("kerno_other".into())).await,
            database.replace("`kerno`", "`kerno_other`")
        );

        // The scripts run as they are in another database, and what they
        // create is what they describe.
        let scratch = common::fresh_database(&session, "objects").await;
        exec_ok(&session, &format!("USE {scratch}")).await;
        for table in ["customers", "orders", "order_items", "audit_log"] {
            exec_ok(
                &session,
                &format!("CREATE TABLE {table} LIKE kerno.{table}"),
            )
            .await;
        }
        for script in [&view, &function, &procedure, &event] {
            run_script(&session, script).await;
        }
        run_script(
            &session,
            &ddl(ObjectRef::Trigger {
                table: TableRef::new("kerno", "orders"),
                name: "orders_log_status".into(),
            })
            .await,
        )
        .await;
        let moved = session.objects(&scratch).await.unwrap();
        assert_eq!(
            names(&moved.routines, |r| &r.name),
            ["customer_lifetime_value", "customer_orders"]
        );
        assert_eq!(names(&moved.triggers, |t| &t.name), ["orders_log_status"]);
        assert_eq!(moved.events, session.objects("kerno").await.unwrap().events);
        assert!(
            moved
                .relations
                .iter()
                .any(|r| r.name == "order_totals" && r.kind == RelationKind::View)
        );
        // The copies print the same statements as the originals.
        let copy = session
            .ddl(&ObjectRef::Routine {
                schema: scratch.clone(),
                name: "customer_orders".into(),
                kind: RoutineKind::Procedure,
                identity_arguments: String::new(),
            })
            .await
            .unwrap();
        assert_eq!(copy, procedure);
        exec_ok(&session, &format!("DROP DATABASE {scratch}")).await;

        // Things that do not exist, or that the engine does not have, say so.
        for object in [
            ObjectRef::Relation(TableRef::new("kerno", "no_such_table"), RelationKind::Table),
            ObjectRef::Relation(TableRef::new("kerno", "no_such_view"), RelationKind::View),
            routine("no_such_routine", RoutineKind::Function),
            ObjectRef::Trigger {
                table: TableRef::new("kerno", "orders"),
                name: "no_such_trigger".into(),
            },
            ObjectRef::Event(TableRef::new("kerno", "no_such_event")),
            ObjectRef::Index {
                table: TableRef::new("kerno", "orders"),
                name: "no_such_index".into(),
            },
            ObjectRef::Database("no_such_database".into()),
        ] {
            let missing = session.ddl(&object).await;
            assert!(
                matches!(missing, Err(Error::Db(_))),
                "{object:?}: {missing:?}"
            );
        }
        let user_type = session
            .ddl(&ObjectRef::Type(TableRef::new("kerno", "x")))
            .await;
        assert!(
            matches!(user_type, Err(Error::Unsupported(_))),
            "{user_type:?}"
        );
        let window = session.ddl(&routine("x", RoutineKind::Window)).await;
        assert!(matches!(window, Err(Error::Unsupported(_))), "{window:?}");
        let _ = mariadb;
    }
}

#[tokio::test]
async fn administrative_views() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mariadb = engine == Engine::MariaDb;
        let view = |view: AdminView| {
            let session = session.clone();
            async move {
                let sql = session
                    .admin_query(view)
                    .expect("both engines have every admin view");
                let collected = run(&session, &sql)
                    .await
                    .unwrap_or_else(|e| panic!("{view:?}: {e}\n{sql}"));
                // Nothing cut short, nothing deprecated.
                assert!(
                    collected.notices.is_empty(),
                    "{view:?}: {:?}",
                    collected.notices
                );
                let result = collected.results.into_iter().next().unwrap();
                let columns: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
                (columns, result.rows())
            }
        };

        let own_id = scalar(&session, "SELECT CONNECTION_ID()").await;
        let (columns, sessions) = view(AdminView::Sessions).await;
        assert_eq!(
            columns[0], "id",
            "column 0 is the id the cancel/terminate calls take"
        );
        let flag = columns.iter().position(|c| c == "this_session").unwrap();
        let own: Vec<_> = sessions
            .iter()
            .filter(|row| row[flag].as_deref() == Some("1"))
            .collect();
        assert_eq!(own.len(), 1);
        assert_eq!(own[0][0].as_deref(), Some(own_id.as_str()));
        assert_eq!(own[0][1].as_deref(), Some("kerno"));
        let query = columns.iter().position(|c| c == "query").unwrap();
        assert!(
            own[0][query]
                .as_deref()
                .is_some_and(|q| q.starts_with("SELECT p.ID AS id")),
            "{:?}",
            own[0][query]
        );

        let (columns, roles) = view(AdminView::Roles).await;
        assert_eq!(&columns[..2], ["user", "host"]);
        let column = |name: &str| columns.iter().position(|c| c == name).unwrap();
        let role = |name: &str| {
            roles
                .iter()
                .find(|row| row[0].as_deref() == Some(name))
                .unwrap_or_else(|| panic!("no account {name}"))
                .clone()
        };
        assert_eq!(role("kerno")[1].as_deref(), Some("%"));
        assert_eq!(role("kerno")[column("super")].as_deref(), Some("1"));
        assert_eq!(role("kerno")[column("can_grant")].as_deref(), Some("1"));
        assert_eq!(
            role("kerno_readonly")[column("super")].as_deref(),
            Some("0")
        );
        assert_eq!(role("kerno_readonly")[column("member_of")], None);
        assert!(columns.contains(&"authentication".to_string()));

        let (columns, grants) = view(AdminView::Grants).await;
        assert_eq!(
            columns,
            [
                "grantee",
                "kind",
                "database",
                "object",
                "privileges",
                "with_grant_option"
            ]
        );
        let grant = |grantee: &str, kind: &str, object: Option<&str>| {
            grants
                .iter()
                .find(|row| {
                    row[0].as_deref() == Some(grantee)
                        && row[1].as_deref() == Some(kind)
                        && row[3].as_deref() == object
                })
                .map(|row| (row[2].clone(), row[4].clone().unwrap()))
        };
        let readonly = "'kerno_readonly'@'%'";
        assert_eq!(
            grant(readonly, "table", Some("audit_log")),
            Some((Some("kerno".into()), "INSERT, SELECT".into()))
        );
        assert_eq!(
            grant(readonly, "database", None),
            Some((Some("kerno".into()), "SELECT".into()))
        );
        assert_eq!(
            grant(readonly, "global", None),
            Some((None, "USAGE".into()))
        );
        // An administrator's long list of privileges is whole.
        let (_, all) = grant("'kerno'@'%'", "global", None).unwrap();
        for privilege in ["ALTER", "SELECT", "SHUTDOWN", "UPDATE"] {
            assert!(
                all.split(", ").any(|p| p == privilege),
                "{privilege}: {all}"
            );
        }
        let grantable = grants
            .iter()
            .find(|row| {
                row[0].as_deref() == Some("'kerno'@'%'") && row[1].as_deref() == Some("global")
            })
            .unwrap();
        assert_eq!(grantable[5].as_deref(), Some("1"));

        let (columns, databases) = view(AdminView::DatabaseSizes).await;
        assert_eq!(
            columns,
            ["database", "encoding", "collation", "tables", "size_bytes"]
        );
        assert_eq!(
            databases[0][0].as_deref(),
            Some("kerno_analytics"),
            "largest first"
        );
        assert_eq!(databases[0][3].as_deref(), Some("2"));
        assert!(databases[0][4].as_deref().unwrap().parse::<u64>().unwrap() > 200_000_000);
        assert!(
            databases
                .iter()
                .any(|row| row[0].as_deref() == Some("mysql"))
        );

        let (columns, tables) = view(AdminView::TableSizes).await;
        assert_eq!(&columns[..2], ["database", "table"]);
        assert_eq!(
            (tables[0][0].as_deref(), tables[0][1].as_deref()),
            (Some("kerno_analytics"), Some("big_table")),
            "largest first"
        );
        assert_eq!(tables[0][2].as_deref(), Some("InnoDB"));
        // Views and the system's own tables are not listed.
        assert!(tables.iter().all(|row| {
            row[1].as_deref() != Some("order_totals")
                && !["mysql", "sys", "information_schema", "performance_schema"]
                    .contains(&row[0].as_deref().unwrap())
        }));

        let (columns, settings) = view(AdminView::Settings).await;
        assert_eq!(&columns[..2], ["name", "value"]);
        assert!(settings.len() > 300, "{}", settings.len());
        let setting = |name: &str| {
            settings
                .iter()
                .find(|row| row[0].as_deref() == Some(name))
                .unwrap_or_else(|| panic!("no setting {name}"))[1]
                .clone()
        };
        assert_eq!(setting("max_connections").as_deref(), Some("100"));
        assert_eq!(setting("character_set_server").as_deref(), Some("utf8mb4"));
        // The session's value, where it differs from the global one.
        exec_ok(&session, "SET SESSION wait_timeout = 4321").await;
        let (_, settings) = view(AdminView::Settings).await;
        let timeout = settings
            .iter()
            .find(|row| row[0].as_deref() == Some("wait_timeout"))
            .unwrap();
        assert_eq!(timeout[1].as_deref(), Some("4321"));
        assert_ne!(timeout[2].as_deref(), Some("4321"));
        let _ = mariadb;
    }
}

#[tokio::test]
async fn introspection_does_not_disturb_the_session() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "undisturbed").await;
        exec_ok(
            &session,
            &format!("CREATE TABLE {database}.notes (body TEXT)"),
        )
        .await;
        exec_ok(&session, &format!("USE {database}")).await;
        exec_ok(&session, "START TRANSACTION").await;
        exec_ok(&session, "INSERT INTO notes (body) VALUES ('uncommitted')").await;
        session.databases().await.unwrap();
        session.objects("kerno").await.unwrap();
        session
            .table_structure(&TableRef::new("kerno", "orders"))
            .await
            .unwrap();
        session
            .ddl(&ObjectRef::Relation(
                TableRef::new("kerno", "orders"),
                RelationKind::Table,
            ))
            .await
            .unwrap();
        // Still in the same database and the same transaction.
        assert_eq!(session.database().as_deref(), Some(database.as_str()));
        assert_eq!(session.tx_state(), kerno_core::TxState::Open);
        assert_eq!(scalar(&session, "SELECT COUNT(*) FROM notes").await, "1");
        exec_ok(&session, "ROLLBACK").await;
        assert_eq!(scalar(&session, "SELECT COUNT(*) FROM notes").await, "0");
        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

#[tokio::test]
async fn completion_catalog_lists_the_user_databases() {
    for engine in engines() {
        let session = common::session(engine).await;
        let catalog = session.completion_catalog().await.unwrap();

        // Other tests create scratch databases meanwhile.
        let databases: Vec<_> = catalog
            .schemas
            .iter()
            .map(|s| s.name.as_str())
            .filter(|name| !name.starts_with("kerno_test_") && !name.starts_with("kerno_ui_"))
            .collect();
        assert_eq!(databases, ["kerno", "kerno_analytics", "kerno_other"]);
        assert_eq!(catalog.search_path, ["kerno"]);

        let kerno = catalog.schemas.iter().find(|s| s.name == "kerno").unwrap();
        assert!(kerno.relations.is_sorted_by(|a, b| a.name <= b.name));
        let relation = |name: &str| kerno.relations.iter().find(|r| r.name == name).unwrap();
        let orders = relation("orders");
        assert_eq!(orders.kind, RelationKind::Table);
        assert_eq!(
            names(&orders.columns, |c| &c.name),
            [
                "id",
                "customer_id",
                "status",
                "placed_at",
                "shipped_at",
                "total",
                "currency",
                "notes"
            ]
        );
        // MariaDB prints display widths (`bigint(20) unsigned`), MySQL 8 not.
        assert!(
            orders.columns[0].data_type.starts_with("bigint"),
            "{}",
            orders.columns[0].data_type
        );
        assert_eq!(orders.columns[5].data_type, "decimal(12,2)");
        assert_eq!(orders.foreign_keys.len(), 1);
        let customer = &orders.foreign_keys[0];
        assert_eq!(customer.columns, ["customer_id"]);
        assert_eq!(customer.referenced_schema, "kerno");
        assert_eq!(customer.referenced_table, "customers");
        assert_eq!(customer.referenced_columns, ["id"]);
        assert_eq!(relation("order_totals").kind, RelationKind::View);
        assert!(!relation("order_totals").columns.is_empty());
        assert_eq!(
            kerno.routines,
            [
                "cancel_stale_orders",
                "customer_lifetime_value",
                "customer_orders"
            ]
        );
    }
}

#[tokio::test]
async fn the_row_estimate_of_the_big_table_is_of_its_order() {
    for engine in engines() {
        let session = common::session(engine).await;
        let sql = session
            .dialect()
            .estimated_rows_sql(&TableRef::new("kerno_analytics", "big_table"));
        let estimate: u64 = scalar(&session, &sql).await.parse().unwrap();
        println!("{engine:?} estimated rows of kerno_analytics.big_table: {estimate}");
        // InnoDB samples pages: an estimate, of the right order.
        assert!((100_000..=10_000_000).contains(&estimate), "{estimate}");
        let missing = session
            .dialect()
            .estimated_rows_sql(&TableRef::new("kerno_analytics", "no_such_table"));
        assert!(common::rows(&session, &missing).await.is_empty());
    }
}

/// The columns and rows of the first result of `sql`.
async fn table(session: &Arc<dyn Session>, sql: &str) -> (Vec<String>, Vec<Vec<Option<String>>>) {
    let collected = run(session, sql)
        .await
        .unwrap_or_else(|error| panic!("{error}\n{sql}"));
    let result = collected.results.into_iter().next().unwrap();
    let columns = result.columns.iter().map(|c| c.name.clone()).collect();
    (columns, result.rows())
}

fn property(columns: &[String], row: &[Option<String>], name: &str) -> Option<String> {
    let at = columns
        .iter()
        .position(|column| column == name)
        .unwrap_or_else(|| panic!("no property {name} in {columns:?}"));
    row[at].clone()
}

#[tokio::test]
async fn properties_describe_tables_and_views() {
    for engine in engines() {
        let session = common::session(engine).await;
        let d = session.dialect();
        let sql = d
            .properties_sql(&TableRef::new("kerno", "customers"), RelationKind::Table)
            .unwrap();
        let (columns, rows) = table(&session, &sql).await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(property(&columns, row, "Engine").as_deref(), Some("InnoDB"));
        assert_eq!(
            property(&columns, row, "Kind").as_deref(),
            Some("BASE TABLE")
        );
        assert_eq!(
            property(&columns, row, "Comment").as_deref(),
            Some("People who can place orders")
        );
        assert!(property(&columns, row, "Collation").is_some());
        assert!(property(&columns, row, "Created").is_some());
        assert_eq!(property(&columns, row, "Definer"), None);

        // A view's "comment" is the word VIEW: not shown; its definer is.
        let sql = d
            .properties_sql(&TableRef::new("kerno", "order_totals"), RelationKind::View)
            .unwrap();
        let (columns, rows) = table(&session, &sql).await;
        assert_eq!(
            property(&columns, &rows[0], "Kind").as_deref(),
            Some("VIEW")
        );
        assert_eq!(property(&columns, &rows[0], "Comment"), None);
        assert!(property(&columns, &rows[0], "Definer").is_some());
    }
}

#[tokio::test]
async fn properties_describe_databases() {
    for engine in engines() {
        let session = common::session(engine).await;
        let d = session.dialect();
        let (columns, rows) = table(&session, &d.database_properties_sql("kerno").unwrap()).await;
        assert_eq!(rows.len(), 1, "{engine:?}");
        let row = &rows[0];
        assert!(property(&columns, row, "Character set").is_some());
        assert!(property(&columns, row, "Collation").is_some());
        let tables: u64 = property(&columns, row, "Tables").unwrap().parse().unwrap();
        assert!(tables > 0, "{engine:?}");
        let size: u64 = property(&columns, row, "Data size")
            .unwrap()
            .parse()
            .unwrap();
        assert!(size > 0, "{engine:?}");
        // MariaDB keeps a comment on a database; MySQL has none to show.
        assert_eq!(
            columns.iter().any(|column| column == "Comment"),
            engine == Engine::MariaDb
        );
        let (_, rows) = table(&session, &d.database_properties_sql("no_such").unwrap()).await;
        assert!(rows.is_empty(), "{engine:?}");
    }
}

#[tokio::test]
async fn a_database_is_created_with_a_character_set_and_collation() {
    for engine in engines() {
        let session = common::session(engine).await;
        let d = session.dialect();
        let (_, choices) = table(&session, d.database_options_sql().unwrap()).await;
        let has = |field: &str, value: &str, applies_to: Option<&str>| {
            choices.iter().any(|row| {
                row[0].as_deref() == Some(field)
                    && row[1].as_deref() == Some(value)
                    && row[2].as_deref() == applies_to
            })
        };
        assert!(has("encoding", "utf8mb4", None) && has("encoding", "latin1", None));
        assert!(has("collation", "latin1_swedish_ci", Some("latin1")));

        let name = common::scratch_database("created_with_options");
        exec_ok(&session, &format!("DROP DATABASE IF EXISTS {name}")).await;
        let options = kerno_core::dialect::DatabaseOptions {
            encoding: Some("latin1".into()),
            collation: Some("latin1_bin".into()),
            ..Default::default()
        };
        exec_ok(
            &session,
            &d.create_database_with_sql(&name, &options).unwrap(),
        )
        .await;
        let made = common::rows(
            &session,
            &format!(
                "SELECT DEFAULT_CHARACTER_SET_NAME, DEFAULT_COLLATION_NAME \
                 FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = '{name}'"
            ),
        )
        .await;
        exec_ok(&session, &format!("DROP DATABASE {name}")).await;
        assert_eq!(
            made,
            [[Some("latin1".to_string()), Some("latin1_bin".to_string())]]
        );
    }
}

#[tokio::test]
async fn the_lock_view_names_the_waiting_session_and_its_blocker() {
    for engine in engines() {
        let holder = common::session(engine).await;
        let waiter = common::session(engine).await;
        let watcher = common::session(engine).await;
        let database = common::fresh_database(&holder, "locks").await;
        exec_ok(
            &holder,
            &format!("CREATE TABLE {database}.t (id INT PRIMARY KEY, v INT) ENGINE = InnoDB"),
        )
        .await;
        exec_ok(&holder, &format!("INSERT INTO {database}.t VALUES (1, 1)")).await;
        let holder_id = scalar(&holder, "SELECT CONNECTION_ID()").await;
        let waiter_id = scalar(&waiter, "SELECT CONNECTION_ID()").await;

        // The waiter gives up on its own after a few seconds, whatever
        // happens to this test.
        exec_ok(&holder, "START TRANSACTION").await;
        exec_ok(
            &holder,
            &format!("UPDATE {database}.t SET v = 2 WHERE id = 1"),
        )
        .await;
        exec_ok(&waiter, "SET SESSION innodb_lock_wait_timeout = 8").await;
        let blocked = {
            let (waiter, sql) = (
                waiter.clone(),
                format!("UPDATE {database}.t SET v = 3 WHERE id = 1"),
            );
            tokio::spawn(async move { run(&waiter, &sql).await.map(drop) })
        };
        let sql = watcher.admin_query(AdminView::Locks).unwrap();
        let mut found = None;
        for _ in 0..50 {
            let (columns, rows) = table(&watcher, &sql).await;
            if let Some(row) = rows
                .into_iter()
                .find(|row| row[0].as_deref() == Some(waiter_id.as_str()))
            {
                found = Some((columns, row));
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        exec_ok(&holder, "ROLLBACK").await;
        let _ = blocked.await;
        exec_ok(&holder, &format!("DROP DATABASE {database}")).await;
        let (columns, row) = found.expect("the waiting session is listed");
        assert_eq!(columns[0], "waiting_id");
        let blocking = columns.iter().position(|c| c == "blocking_id").unwrap();
        assert_eq!(row[blocking].as_deref(), Some(holder_id.as_str()));
        let query = columns.iter().position(|c| c == "waiting_query").unwrap();
        assert!(row[query].as_deref().unwrap().contains("SET v = 3"));
    }
}
