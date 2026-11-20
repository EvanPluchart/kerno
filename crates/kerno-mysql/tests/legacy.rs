//! Legacy MySQL servers (K22): MySQL 5.1, 5.5, 5.6 and 5.7, reached by the
//! same driver as 8.x, which reads the server's version at connect and
//! adapts (`Capabilities`). Every test runs once per legacy container that
//! is up (`scripts/test-env.sh up --legacy`), on the fixture of
//! `tests/fixtures/mysql-legacy`, and on scratch databases named
//! `kerno_test_k22_*`, dropped at the end. Skipped with a message when the
//! containers are down.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{exec_ok, run, scalar};
use kerno_core::config::{ConnectParams, ConnectionConfig, Engine, Secret, TlsConfig, TlsMode};
use kerno_core::error::{FailureKind, OLD_PASSWORD_HASH, OLD_SERVER_TLS};
use kerno_core::exec::{self, ExecEvent, RowLimit};
use kerno_core::schema::{Change, ObjectRef, RelationKind, RoutineKind, TableRef, Tie};
use kerno_core::sql::edits::KEPT_CHANGES;
use kerno_core::sql::{CellValue, RowChange, apply_edits, edit_statements};
use kerno_core::table_draft::TableChange;
use kerno_core::{AdminView, Error, Session, TxState, TypeKind, testing};

/// The legacy servers that are up, each with a session on `kerno`.
async fn servers() -> Vec<(&'static str, ConnectParams, Arc<dyn Session>)> {
    let mut found = Vec::new();
    for (name, params) in testing::legacy_mysql() {
        let session = common::connect(&params)
            .await
            .unwrap_or_else(|error| panic!("connect to {name}: {error}"));
        found.push((name, params, session));
    }
    found
}

/// `(major, minor)` of the session's server.
fn release(session: &Arc<dyn Session>) -> (u32, u32) {
    let (major, minor, _) = session.server().version_number;
    (major, minor)
}

/// A scratch database for one test, dropped and created again.
async fn scratch(session: &Arc<dyn Session>, test: &str) -> String {
    let database = format!("kerno_test_k22_{test}");
    exec_ok(session, &format!("DROP DATABASE IF EXISTS {database}")).await;
    exec_ok(
        session,
        &format!("CREATE DATABASE {database} CHARACTER SET utf8"),
    )
    .await;
    database
}

async fn drop_database(session: &Arc<dyn Session>, database: &str) {
    exec_ok(session, &format!("DROP DATABASE IF EXISTS {database}")).await;
}

/// K22: a 5.x server connects, says what it is ("MySQL 5.1.73"), and the
/// statements the app runs on every session work there: the isolation level
/// (`@@tx_isolation` before 5.7.20), a backslash in a literal (`_utf8`
/// before utf8mb4), text that is not ASCII. Mutations: `transaction_isolation_variable`
/// or `utf8mb4` set on every server fail with the server's 1193 / 1064.
#[tokio::test]
async fn a_legacy_server_connects_and_runs_what_every_session_runs() {
    for (name, _, session) in servers().await {
        eprintln!("--- {name}");
        let server = session.server().clone();
        assert_eq!(session.engine(), Engine::MySql, "{name}");
        assert_eq!(server.version_number.0, 5, "{name}: {}", server.version);
        assert_eq!(
            server.named(session.engine()),
            format!(
                "MySQL {}.{}.{}",
                server.version_number.0, server.version_number.1, server.version_number.2
            )
        );
        assert_eq!(session.database().as_deref(), Some("kerno"));
        session.ping().await.unwrap();
        assert_eq!(
            scalar(&session, session.dialect().isolation_sql()).await,
            "REPEATABLE-READ",
            "{name}"
        );
        // Without utf8mb4 the connection speaks utf8.
        let charset = scalar(&session, "SELECT @@character_set_client").await;
        let expected = if release(&session) >= (5, 5) {
            "utf8mb4"
        } else {
            "utf8"
        };
        assert_eq!(charset, expected, "{name}");
        // A backslash: the hexadecimal literal with a character set the
        // server has.
        for text in [r"C:\temp\new", "caf\u{e9} \\ Gr\u{fc}\u{df}e"] {
            let sql = format!("SELECT {}", session.dialect().quote_string(text));
            assert_eq!(scalar(&session, &sql).await, text, "{name}: {sql}");
        }
        // The fixture's text, written by the seed, read back as it is.
        assert_eq!(
            scalar(
                &session,
                "SELECT full_name FROM kerno.customers WHERE id = 3"
            )
            .await,
            "Chlo\u{e9} Durand"
        );
        assert_eq!(
            scalar(
                &session,
                "SELECT c_varchar FROM kerno.all_types WHERE id = 1"
            )
            .await,
            "Gr\u{fc}\u{df}e"
        );
        session.close().await;
    }
}

/// K22: a read-only connection opens on every legacy server. From 5.6.5 the
/// server refuses a write itself; before, it has no read-only sessions and
/// the dialect says so (the app then warns). Mutation: `read_only_transactions`
/// set on every server: 5.1 and 5.5 refuse the connection ("Unknown system
/// variable 'TRANSACTION'").
#[tokio::test]
async fn a_read_only_connection_opens_and_is_read_only_where_the_server_can() {
    for (name, params, _) in servers().await {
        eprintln!("--- {name}");
        let session = common::connect(&ConnectParams {
            read_only: true,
            ..params.clone()
        })
        .await
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        let write = "INSERT INTO kerno.audit_log VALUES ('2024-01-01', 'k22', 'x')";
        if release(&session) >= (5, 6) {
            assert!(session.dialect().read_only_session_sql().is_some());
            let error = common::db_error(&session, write).await;
            assert_eq!(error.code.as_deref(), Some("1792 (25006)"), "{name}");
        } else {
            assert_eq!(session.dialect().read_only_session_sql(), None, "{name}");
        }
        session.close().await;
    }
}

fn tls(mode: TlsMode) -> TlsConfig {
    TlsConfig {
        mode,
        ca_file: Some(testing::generated("tls/ca.crt")),
        ..TlsConfig::default()
    }
}

/// K22: TLS on the legacy servers. 5.6.51 speaks TLS 1.2 with ECDHE and is
/// used as 8.x is; 5.1 and 5.5 (yaSSL, TLS 1.0) and 5.7.44 (TLS 1.2 with DHE
/// only) offer a TLS rustls refuses: `prefer` goes on without TLS, the other
/// modes say why in plain words. Mutation: `old_server_tls` answering false:
/// `prefer` fails on 5.1, 5.5 and 5.7 with the handshake's own error.
#[tokio::test]
async fn an_old_tls_is_left_by_prefer_and_explained_by_the_strict_modes() {
    for (name, params, _) in servers().await {
        eprintln!("--- {name}");
        let with = |mode| ConnectParams {
            tls: tls(mode),
            ..params.clone()
        };
        let preferred = common::connect(&with(TlsMode::Prefer))
            .await
            .unwrap_or_else(|error| panic!("{name}: prefer: {error}"));
        let cipher = common::rows(&preferred, "SHOW SESSION STATUS LIKE 'Ssl_cipher'").await[0][1]
            .clone()
            .unwrap_or_default();
        preferred.close().await;
        if name == "kerno-mysql56" {
            assert!(!cipher.is_empty(), "{name}: prefer used TLS");
            let required = common::connect(&with(TlsMode::Require)).await.unwrap();
            required.close().await;
            continue;
        }
        assert_eq!(cipher, "", "{name}: prefer went on without TLS");
        for mode in [TlsMode::Require, TlsMode::VerifyCa, TlsMode::VerifyFull] {
            let error = match common::connect(&with(mode)).await {
                Err(error) => error,
                Ok(_) => panic!("{name}: {mode:?} connected"),
            };
            assert!(matches!(error, Error::Tls(_)), "{name}: {error:?}");
            assert!(
                error.to_string().contains(OLD_SERVER_TLS),
                "{name}: {error}"
            );
            let explained = kerno_core::error::explain(
                &error.to_string(),
                &ConnectionConfig::new(Engine::MySql),
            );
            assert_eq!(explained.kind, FailureKind::Tls);
            assert!(
                explained.next_step.contains("prefer"),
                "{}",
                explained.next_step
            );
        }
    }
}

/// K22: an account with MySQL's pre-4.1 password hash (5.1 and 5.5 still
/// let one sign in) is refused by Kerno in plain words, as 5.6.5 and later
/// refuse it themselves. Mutation: the `MysqlOldPasswordDisabled` arm of
/// `connect_error` removed: the raw "Driver error" comes back instead.
#[tokio::test]
async fn an_old_password_hash_is_refused_in_plain_words() {
    for (name, params, session) in servers().await {
        if release(&session) >= (5, 6) {
            continue;
        }
        eprintln!("--- {name}");
        let error = match common::connect(&ConnectParams {
            user: "kerno_old".into(),
            password: Some(Secret::new("kerno_old_pw")),
            ..params.clone()
        })
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("{name}: the old hash was accepted"),
        };
        assert!(error.to_string().contains(OLD_PASSWORD_HASH), "{error}");
        let config = ConnectionConfig {
            user: "kerno_old".into(),
            ..ConnectionConfig::new(Engine::MySql)
        };
        let explained = kerno_core::error::explain(&error.to_string(), &config);
        assert_eq!(explained.kind, FailureKind::Authentication);
        assert!(
            explained.headline.contains("kerno_old"),
            "{}",
            explained.headline
        );
        session.close().await;
    }
}

/// K22: results stream, and a running statement is cancelled with `KILL
/// QUERY` on a side connection, on every legacy server.
#[tokio::test]
async fn results_stream_and_a_statement_is_cancelled() {
    for (name, _, session) in servers().await {
        eprintln!("--- {name}");
        let collected = run(
            &session,
            "SELECT a.ID FROM information_schema.COLLATIONS a, information_schema.COLLATIONS b \
             LIMIT 10000",
        )
        .await
        .unwrap();
        let rows: usize = collected.results[0]
            .batches
            .iter()
            .map(|batch| batch.row_count())
            .sum();
        assert_eq!(rows, 10000, "{name}");

        let mut execution = exec::run(session.clone(), "SELECT SLEEP(60)".into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = Instant::now();
        session.cancel().await.unwrap();
        loop {
            match common::next_event(&mut execution).await {
                Some(ExecEvent::Finished(result)) => {
                    result.unwrap();
                    break;
                }
                Some(_) => {}
                None => panic!("{name}: ended without Finished"),
            }
        }
        assert!(started.elapsed() < Duration::from_secs(5), "{name}");
        assert_eq!(scalar(&session, "SELECT 1").await, "1");
        session.close().await;
    }
}

/// K22: the navigator's reads work on every legacy server: databases, the
/// objects of one (routine arguments from `PARAMETERS` from 5.5.3, none on
/// 5.1), a table's structure, the completion catalog, and every `SHOW
/// CREATE`. Mutation: `parameters_table` set on every server: `objects`
/// fails on 5.1 with "Unknown table 'PARAMETERS' in information_schema".
#[tokio::test]
async fn the_catalog_is_read_on_every_legacy_server() {
    for (name, _, session) in servers().await {
        eprintln!("--- {name}");
        let databases = session.databases().await.unwrap();
        let kerno = databases.iter().find(|d| d.name == "kerno").unwrap();
        assert_eq!(kerno.encoding.as_deref(), Some("utf8"), "{name}");
        assert!(databases.iter().any(|d| d.name == "mysql" && d.is_system));

        let objects = session.objects("kerno").await.unwrap();
        let relation = |table: &str| {
            objects
                .relations
                .iter()
                .find(|r| r.name == table)
                .unwrap_or_else(|| panic!("{name}: no {table}"))
        };
        assert_eq!(relation("notes").engine.as_deref(), Some("MyISAM"));
        assert_eq!(relation("orders").engine.as_deref(), Some("InnoDB"));
        assert_eq!(relation("order_totals").kind, RelationKind::View);
        let lifetime = objects
            .routines
            .iter()
            .find(|r| r.name == "customer_lifetime_value")
            .unwrap();
        assert_eq!(lifetime.kind, RoutineKind::Function);
        assert_eq!(lifetime.returns.as_deref(), Some("decimal(14,2)"), "{name}");
        let procedure = objects
            .routines
            .iter()
            .find(|r| r.name == "customer_orders")
            .unwrap();
        if release(&session) >= (5, 5) {
            assert_eq!(
                procedure.arguments, "IN p_customer_id int(10) unsigned, OUT p_count int(11)",
                "{name}"
            );
        } else {
            assert_eq!(procedure.arguments, "", "{name}");
        }
        assert_eq!(objects.triggers.len(), 1, "{name}");
        assert_eq!(objects.triggers[0].name, "orders_log_status");
        assert_eq!(objects.events.len(), 1, "{name}");
        assert!(objects.events[0].schedule.starts_with("EVERY 1 DAY"));

        let products = session
            .table_structure(&TableRef::new("kerno", "products"))
            .await
            .unwrap();
        let names: Vec<&str> = products.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "id",
                "category_id",
                "sku",
                "name",
                "price",
                "stock",
                "released_on"
            ]
        );
        assert_eq!(products.columns[0].primary_key_position, Some(1));
        assert_eq!(products.columns[3].comment.as_deref(), Some("Display name"));
        assert_eq!(products.columns[5].default.as_deref(), Some("0"));
        assert!(
            products
                .indexes
                .iter()
                .any(|i| i.name == "products_sku_key" && i.unique)
        );
        assert_eq!(products.foreign_keys.len(), 1);
        assert_eq!(products.foreign_keys[0].referenced_table.name, "categories");
        // No check constraints listed before MySQL 8.0.16.
        assert!(
            products
                .constraints
                .iter()
                .all(|c| c.kind != kerno_core::schema::ConstraintKind::Check)
        );
        let customers = session
            .table_structure(&TableRef::new("kerno", "customers"))
            .await
            .unwrap();
        let updated = customers
            .columns
            .iter()
            .find(|c| c.name == "updated_at")
            .unwrap();
        assert_eq!(updated.default.as_deref(), Some("CURRENT_TIMESTAMP"));
        assert_eq!(updated.on_update.as_deref(), Some("CURRENT_TIMESTAMP"));
        assert_eq!(customers.referenced_by.len(), 1, "{name}");

        let catalog = session.completion_catalog().await.unwrap();
        assert!(catalog.schemas.iter().any(|s| {
            s.name == "kerno"
                && s.relations
                    .iter()
                    .any(|r| r.name == "orders" && !r.foreign_keys.is_empty())
        }));

        let table = TableRef::new("kerno", "products");
        for object in [
            ObjectRef::Relation(table.clone(), RelationKind::Table),
            ObjectRef::Relation(TableRef::new("kerno", "order_totals"), RelationKind::View),
            ObjectRef::Routine {
                schema: "kerno".into(),
                name: "customer_orders".into(),
                kind: RoutineKind::Procedure,
                identity_arguments: String::new(),
            },
            ObjectRef::Trigger {
                table: TableRef::new("kerno", "orders"),
                name: "orders_log_status".into(),
            },
            ObjectRef::Event(TableRef::new("kerno", "purge_old_audit_log")),
            ObjectRef::Database("kerno".into()),
            ObjectRef::Index {
                table: table.clone(),
                name: "products_sku_key".into(),
            },
        ] {
            let ddl = session
                .ddl(&object)
                .await
                .unwrap_or_else(|error| panic!("{name}: {object:?}: {error}"));
            assert!(ddl.contains("CREATE"), "{name}: {ddl}");
        }
        let dialect = session.dialect();
        for sql in [
            dialect.properties_sql(&table, RelationKind::Table).unwrap(),
            dialect.database_properties_sql("kerno").unwrap(),
            dialect.database_options_sql().unwrap().to_string(),
            dialect.estimated_rows_sql(&table),
        ] {
            run(&session, &sql)
                .await
                .unwrap_or_else(|error| panic!("{name}: {sql}: {error}"));
        }
        session.close().await;
    }
}

/// K22: every type a 5.x server has is read and written back through the
/// edit statements, in a transaction on InnoDB.
#[tokio::test]
async fn every_type_round_trips_through_an_edit() {
    for (name, _, session) in servers().await {
        eprintln!("--- {name}");
        let database = scratch(&session, "types").await;
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
        let read = |id: u32| {
            let session = session.clone();
            let database = database.clone();
            async move {
                let collected = run(
                    &session,
                    &format!("SELECT * FROM {database}.all_types WHERE id = {id}"),
                )
                .await
                .unwrap();
                let result = &collected.results[0];
                let batch = &result.batches[0];
                result
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let value = match batch.cell(0, index) {
                            None => CellValue::Null,
                            Some(bytes) if column.kind == TypeKind::Binary => {
                                CellValue::Bytes(bytes.to_vec())
                            }
                            Some(bytes) => {
                                CellValue::Text(String::from_utf8(bytes.to_vec()).unwrap())
                            }
                        };
                        (column.name.clone(), value)
                    })
                    .collect::<Vec<_>>()
            }
        };
        let original = read(1).await;
        // Row 2 (all NULL) takes every value of row 1.
        let set: Vec<(String, CellValue)> = original
            .iter()
            .filter(|(n, _)| n != "id")
            .cloned()
            .collect();
        let changes = [RowChange::Update {
            key: vec![("id".into(), CellValue::Text("2".into()))],
            set,
        }];
        let statements =
            edit_statements(session.dialect(), &table, &structure.columns, &changes).unwrap();
        apply_edits(&session, &statements)
            .await
            .unwrap_or_else(|failure| panic!("{name}: {failure}\n{}", statements.join("\n")));
        let copied = read(2).await;
        assert_eq!(&copied[1..], &original[1..], "{name}");
        assert_eq!(session.tx_state(), TxState::Idle);
        drop_database(&session, &database).await;
        session.close().await;
    }
}

/// K22: a failed apply on InnoDB changes nothing; on MyISAM (the default
/// engine of MySQL 5.1, and still there on 8.x) the server cannot undo the
/// statements before the failing one, and the failure says so. Mutation:
/// `said_if_kept` returning the failure unchanged: the message lacks the
/// sentence.
#[tokio::test]
async fn a_failed_apply_says_when_the_table_could_not_be_rolled_back() {
    for (name, _, session) in servers().await {
        eprintln!("--- {name}");
        let database = scratch(&session, "myisam").await;
        for engine in ["InnoDB", "MyISAM"] {
            let table = TableRef::new(database.clone(), format!("t_{engine}"));
            let qualified = session.dialect().table_name(&table);
            exec_ok(
                &session,
                &format!("CREATE TABLE {qualified} (id INT PRIMARY KEY, v INT) ENGINE = {engine}"),
            )
            .await;
            exec_ok(&session, &format!("INSERT INTO {qualified} VALUES (1, 0)")).await;
            let structure = session.table_structure(&table).await.unwrap();
            let insert = |id: &str| RowChange::Insert {
                values: vec![
                    ("id".into(), CellValue::Text(id.into())),
                    ("v".into(), CellValue::Text("7".into())),
                ],
            };
            // The second insert hits the key of row 1.
            let statements = edit_statements(
                session.dialect(),
                &table,
                &structure.columns,
                &[insert("2"), insert("1")],
            )
            .unwrap();
            let failure = apply_edits(&session, &statements).await.unwrap_err();
            assert_eq!(failure.statement, Some(1));
            let count = scalar(&session, &format!("SELECT COUNT(*) FROM {qualified}")).await;
            if engine == "InnoDB" {
                assert_eq!(count, "1", "{name}");
                assert!(!failure.to_string().contains(KEPT_CHANGES), "{failure}");
            } else {
                assert_eq!(count, "2", "{name}: MyISAM keeps the first insert");
                assert!(
                    failure.to_string().contains(KEPT_CHANGES),
                    "{name}: {failure}"
                );
                assert!(failure.to_string().contains("1062"), "{failure}");
            }
            assert_eq!(session.tx_state(), TxState::Idle);
        }
        drop_database(&session, &database).await;
        session.close().await;
    }
}

/// K22: MySQL before 5.7 is not in strict mode by default: a value too long
/// for its column, or a character its character set cannot hold, is changed
/// with only a warning. The apply stops there and changes nothing, as a
/// strict server's error does. Mutation: `VALUE_CHANGED` emptied: the
/// apply reports success on 5.1, 5.5 and 5.6 and the row holds a cut value.
#[tokio::test]
async fn a_value_the_server_would_change_is_not_applied() {
    for (name, _, session) in servers().await {
        eprintln!("--- {name}");
        let database = scratch(&session, "strict").await;
        let table = TableRef::new(database.clone(), "t");
        let qualified = session.dialect().table_name(&table);
        exec_ok(
            &session,
            &format!(
                "CREATE TABLE {qualified} (id INT PRIMARY KEY, c CHAR(5), t VARCHAR(20)) \
                 ENGINE = InnoDB DEFAULT CHARSET = utf8"
            ),
        )
        .await;
        exec_ok(
            &session,
            &format!("INSERT INTO {qualified} VALUES (1, 'abc', 'x')"),
        )
        .await;
        let structure = session.table_structure(&table).await.unwrap();
        for (column, value) in [("c", "abcdefgh"), ("t", "a\u{1F600}")] {
            let statements = edit_statements(
                session.dialect(),
                &table,
                &structure.columns,
                &[RowChange::Update {
                    key: vec![("id".into(), CellValue::Text("1".into()))],
                    set: vec![(column.into(), CellValue::Text(value.into()))],
                }],
            )
            .unwrap();
            let failure = apply_edits(&session, &statements)
                .await
                .expect_err(&format!("{name}: {column} = {value:?} was applied"));
            eprintln!("{name}: {column}: {failure}");
            assert_eq!(
                common::rows(&session, &format!("SELECT c, t FROM {qualified}")).await,
                [[Some("abc".to_string()), Some("x".to_string())]],
                "{name}"
            );
        }
        drop_database(&session, &database).await;
        session.close().await;
    }
}

/// K22: every administration list runs on every legacy server (lock waits
/// are not kept by MySQL 5.1's built-in InnoDB: none), and a session is
/// cancelled and terminated. Mutation: `roles` set on every server: the
/// accounts list fails with "Table 'mysql.role_edges' doesn't exist".
#[tokio::test]
async fn the_administration_lists_run_on_every_legacy_server() {
    for (name, params, session) in servers().await {
        eprintln!("--- {name}");
        for view in [
            AdminView::Sessions,
            AdminView::Roles,
            AdminView::Grants,
            AdminView::DatabaseSizes,
            AdminView::TableSizes,
            AdminView::Settings,
            AdminView::Locks,
        ] {
            let Some(sql) = session.admin_query(view) else {
                assert!(
                    view == AdminView::Locks && release(&session) == (5, 1),
                    "{name}: no {view:?}"
                );
                continue;
            };
            let collected = run(&session, &sql)
                .await
                .unwrap_or_else(|error| panic!("{name}: {view:?}: {error}"));
            assert!(
                view == AdminView::Locks || !collected.results[0].batches.is_empty(),
                "{name}: {view:?} is empty"
            );
        }
        // Another session, stopped then ended from this one.
        let other = common::connect(&params).await.unwrap();
        let id = scalar(&other, "SELECT CONNECTION_ID()").await;
        session.cancel_session(&id).await.unwrap();
        session.terminate_session(&id).await.unwrap();
        let gone = other.gone().await;
        assert!(
            gone.is_some(),
            "{name}: the killed session is not seen as gone"
        );
        assert_eq!(other.tx_state(), TxState::Idle);
        session.close().await;
    }
}

/// K22: what a drop or a rename reaches. Before MySQL 8.0.13 there is no
/// `VIEW_TABLE_USAGE`: the views are found by a search of their source, as
/// on MariaDB. Mutation: `view_table_usage` set on every server: the drop's
/// list fails with "Unknown table 'VIEW_TABLE_USAGE' in information_schema".
#[tokio::test]
async fn what_a_drop_or_a_rename_reaches() {
    for (name, _, session) in servers().await {
        eprintln!("--- {name}");
        let found = session
            .dependents(&Change::Drop {
                object: ObjectRef::Relation(TableRef::new("kerno", "orders"), RelationKind::Table),
                cascade: false,
            })
            .await
            .unwrap();
        let lines: Vec<(Tie, String)> = found
            .iter()
            .map(|d| (d.tie, format!("{} {}", d.kind, d.name)))
            .collect();
        assert!(
            lines.contains(&(
                Tie::Blocks,
                "foreign key order_items_order_fk on kerno.order_items".into()
            )),
            "{name}: {lines:?}"
        );
        assert!(
            lines.contains(&(Tie::Mentions, "view kerno.order_totals".into())),
            "{name}: {lines:?}"
        );
        let renamed = session
            .dependents(&Change::Rename(ObjectRef::Relation(
                TableRef::new("kerno", "orders"),
                RelationKind::Table,
            )))
            .await
            .unwrap();
        assert!(
            renamed
                .iter()
                .any(|d| d.kind == "view" && d.name == "kerno.order_totals"),
            "{name}: {renamed:?}"
        );
        session.close().await;
    }
}

/// K22: a plan in JSON from MySQL 5.6.5, read into a tree; none before, and
/// no EXPLAIN ANALYZE before 8.0.18: the dialect says so in words naming
/// both versions. Mutation: `explain_unavailable` answering `None`: the
/// 5.1 / 5.5 run fails with the server's syntax error.
#[tokio::test]
async fn plans_where_the_server_has_them_and_words_where_it_does_not() {
    for (name, _, session) in servers().await {
        eprintln!("--- {name}");
        let dialect = session.dialect();
        let version = session.server().named(Engine::MySql);
        match dialect.explain_unavailable(false) {
            Some(reason) => {
                assert!(release(&session) < (5, 6), "{name}: {reason}");
                assert!(
                    reason.contains("5.6.5") && reason.contains(&version),
                    "{reason}"
                );
            }
            None => {
                let sql = dialect.explain("SELECT * FROM kerno.orders WHERE id = 1", false);
                let output = scalar(&session, &sql).await;
                let plan = dialect
                    .parse_plan(&output, false)
                    .unwrap_or_else(|error| panic!("{name}: {error}\n{output}"));
                assert!(!plan.root.operation.is_empty(), "{name}");
            }
        }
        let analyze = dialect
            .explain_unavailable(true)
            .expect("no EXPLAIN ANALYZE on 5.x");
        // Before 5.6.5 the first thing missing is the JSON plan itself.
        let first = if release(&session) >= (5, 6) {
            "8.0.18"
        } else {
            "5.6.5"
        };
        assert!(
            analyze.contains(first) && analyze.contains(&version),
            "{analyze}"
        );
        session.close().await;
    }
}

/// K22: what the structure editor writes for an old server. A column
/// renamed and changed is one `CHANGE COLUMN` everywhere; renamed alone it
/// needs `RENAME COLUMN` (8.0.3), refused here in words; an index is
/// renamed from 5.7.1. Imported JSON becomes `longtext` before 5.7.8.
/// Mutations: `rename_column` or `rename_index` set on every server: the
/// server answers with its own syntax error (1064).
#[tokio::test]
async fn the_structure_editor_writes_what_the_server_takes() {
    for (name, _, session) in servers().await {
        eprintln!("--- {name}");
        let database = scratch(&session, "alter").await;
        let table = TableRef::new(database.clone(), "t");
        let dialect = session.dialect();
        exec_ok(
            &session,
            &format!(
                "CREATE TABLE {} (id INT PRIMARY KEY, a INT, b {}, KEY t_a (a)) ENGINE = InnoDB",
                dialect.table_name(&table),
                dialect.type_for(TypeKind::Json)
            ),
        )
        .await;
        let json = dialect.type_for(TypeKind::Json);
        if release(&session) >= (5, 7) {
            assert_eq!(json, "json", "{name}");
        } else {
            assert_eq!(json, "longtext", "{name}");
        }

        let structure = session.table_structure(&table).await.unwrap();
        let a = structure
            .columns
            .iter()
            .find(|c| c.name == "a")
            .unwrap()
            .clone();
        let mut draft = kerno_core::table_draft::ColumnDraft::from(&a);
        draft.name = "a2".into();
        draft.data_type = "bigint".into();
        let statements = dialect
            .alter_table_sql(
                &table,
                &[
                    TableChange::RenameColumn {
                        from: "a".into(),
                        to: "a2".into(),
                    },
                    TableChange::AlterColumn {
                        name: "a2".into(),
                        from: a,
                        to: draft,
                    },
                ],
            )
            .unwrap();
        for sql in &statements {
            exec_ok(&session, sql).await;
        }
        let renamed = session.table_structure(&table).await.unwrap();
        assert!(
            renamed
                .columns
                .iter()
                .any(|c| c.name == "a2" && c.data_type == "bigint(20)")
        );

        let alone = dialect.alter_table_sql(
            &table,
            &[TableChange::RenameColumn {
                from: "a2".into(),
                to: "a3".into(),
            }],
        );
        match alone {
            // What is written is run: a server without it says so itself.
            Ok(statements) => {
                for sql in &statements {
                    exec_ok(&session, sql).await;
                }
                panic!("{name}: RENAME COLUMN written for a 5.x server");
            }
            Err(Error::Unsupported(message)) => {
                assert!(message.contains("8.0.3"), "{message}");
            }
            Err(other) => panic!("{name}: {other}"),
        }

        let index = ObjectRef::Index {
            table: table.clone(),
            name: "t_a".into(),
        };
        match dialect.rename_sql(&index, "t_a_renamed") {
            Ok(sql) => {
                exec_ok(&session, &sql).await;
                assert!(release(&session) >= (5, 7), "{name}");
            }
            Err(Error::Unsupported(message)) => {
                assert!(release(&session) < (5, 7), "{name}: {message}");
                assert!(message.contains("5.7.1"), "{message}");
            }
            Err(other) => panic!("{name}: {other}"),
        }
        drop_database(&session, &database).await;
        session.close().await;
    }
}
