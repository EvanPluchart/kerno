//! The SQLite driver against files these tests create in temporary
//! directories (K29). No container, and never a file of the user's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kerno_core::config::{ConnectParams, Engine, TlsConfig};
use kerno_core::exec::{self, Collected, ExecEvent, RowLimit};
use kerno_core::schema::{ColumnGeneration, ObjectRef, RelationKind, TableRef};
use kerno_core::sql::edits::{CellValue, RowChange, apply_edits, edit_statements};
use kerno_core::table_draft::{AutoIncrement, ColumnDraft, TableChange, TableDraft};
use kerno_core::{Driver, Error, Result, Session, TxState, TypeKind};
use kerno_sqlite::SqliteDriver;

fn params(file: &Path, read_only: bool) -> ConnectParams {
    ConnectParams {
        engine: Engine::Sqlite,
        host: String::new(),
        port: 0,
        tunnel: None,
        user: String::new(),
        password: None,
        database: None,
        file: Some(file.to_path_buf()),
        tls: TlsConfig::default(),
        read_only,
        options: BTreeMap::new(),
    }
}

async fn open(file: &Path) -> Result<Arc<dyn Session>> {
    SqliteDriver.connect(&params(file, false)).await
}

async fn run(session: &Arc<dyn Session>, sql: &str) -> Result<Collected> {
    exec::collect(session.clone(), sql).await
}

async fn rows(session: &Arc<dyn Session>, sql: &str) -> Vec<Vec<Option<String>>> {
    run(session, sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .results
        .first()
        .map(|result| result.rows())
        .unwrap_or_default()
}

/// A new database file in `dir`, holding the fixture schema.
async fn fixture(dir: &Path) -> (PathBuf, Arc<dyn Session>) {
    let file = dir.join("fixture.db");
    assert!(kerno_sqlite::new_database_file(&file).unwrap());
    let session = open(&file).await.unwrap();
    let script = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sqlite/schema.sql"),
    )
    .unwrap();
    // One execute: SQLite runs the script statement by statement, the
    // trigger's body included.
    run(&session, &script).await.unwrap();
    (file, session)
}

fn connect_error(outcome: Result<Arc<dyn Session>>) -> String {
    match outcome {
        Err(Error::Connect(message)) => message,
        Err(other) => panic!("expected a connect error, got {other}"),
        Ok(_) => panic!("expected a connect error, the file opened"),
    }
}

/// Acceptance 1: a new file, an existing one, a missing one, one that is
/// not a database, and the version a test shows.
#[tokio::test]
async fn files_open_as_they_are_and_never_get_created() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("new.db");
    assert!(kerno_sqlite::new_database_file(&file).unwrap(), "created");
    let session = open(&file).await.unwrap();
    assert!(session.server().version.starts_with("SQLite 3."));
    assert_eq!(
        session.server().named(Engine::Sqlite),
        session.server().version
    );
    assert_eq!(session.database().as_deref(), Some("main"));
    run(&session, "CREATE TABLE kept (a)").await.unwrap();
    session.close().await;
    // A second "new" file on an existing path keeps it as it is.
    assert!(!kerno_sqlite::new_database_file(&file).unwrap());
    let again = open(&file).await.unwrap();
    assert_eq!(
        rows(&again, "SELECT name FROM sqlite_schema").await,
        [[Some("kept".into())]]
    );

    let missing = dir.path().join("missing.db");
    let message = connect_error(open(&missing).await);
    assert!(message.contains("does not exist"), "{message}");
    assert!(!missing.exists(), "a missing file must not be created");

    let garbage = dir.path().join("garbage.db");
    std::fs::write(&garbage, vec![0x5a; 4096]).unwrap();
    let message = connect_error(open(&garbage).await);
    assert!(message.contains("not a SQLite database"), "{message}");
    assert!(
        message.contains("file is not a database"),
        "the raw message: {message}"
    );

    let message = connect_error(open(dir.path()).await);
    assert!(message.contains("folder"), "{message}");
}

/// Acceptance 1 and 8: a file the user may only read is refused for
/// writing in plain words, and opens read-only when the connection says so;
/// a read-only connection opens a writable file read-only too.
#[tokio::test]
async fn read_only_files_and_connections() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (file, session) = fixture(dir.path()).await;
    session.close().await;

    let reader = SqliteDriver.connect(&params(&file, true)).await.unwrap();
    assert_eq!(
        rows(&reader, "SELECT count(*) FROM customers").await,
        [[Some("3".into())]]
    );
    let error = run(&reader, "DELETE FROM customers").await.unwrap_err();
    assert!(error.to_string().contains("readonly"), "{error}");
    reader.close().await;

    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    let message = connect_error(open(&file).await);
    assert!(message.contains("can only be read"), "{message}");
    let reader = SqliteDriver.connect(&params(&file, true)).await.unwrap();
    assert_eq!(
        rows(&reader, "SELECT count(*) FROM products").await,
        [[Some("2".into())]]
    );
    reader.close().await;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
}

/// Acceptance 1: a file another connection holds locked is told as such,
/// after SQLite's wait for the lock.
#[tokio::test]
async fn a_locked_file_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (file, holder) = fixture(dir.path()).await;
    run(&holder, "BEGIN EXCLUSIVE").await.unwrap();
    run(&holder, "UPDATE customers SET name = 'x' WHERE id = 1")
        .await
        .unwrap();
    let started = Instant::now();
    let message = connect_error(open(&file).await);
    assert!(message.contains("locked"), "{message}");
    assert!(started.elapsed() < Duration::from_secs(20));
    run(&holder, "ROLLBACK").await.unwrap();
    holder.close().await;
}

/// Acceptance 3 and 4: rows come only as fast as they are asked for, the
/// statement waits open in between, and a consumer that leaves stops it.
#[tokio::test]
async fn rows_stream_in_pages() {
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    run(
        &session,
        "CREATE TABLE big AS WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n \
         WHERE x < 50000) SELECT x AS id, 'row ' || x AS label FROM n",
    )
    .await
    .unwrap();
    let mut execution = exec::run(
        session.clone(),
        "SELECT * FROM big".into(),
        RowLimit::Rows(100),
    );
    let mut fetched = 0;
    loop {
        match tokio::time::timeout(Duration::from_millis(500), execution.next()).await {
            Ok(Some(ExecEvent::Rows(batch))) => fetched += batch.row_count(),
            Ok(Some(ExecEvent::Columns(columns))) => {
                assert_eq!(columns[0].kind, TypeKind::Integer);
                assert_eq!(columns[1].kind, TypeKind::Text);
            }
            Ok(other) => panic!("unexpected {other:?}"),
            // Nothing more until more rows are asked for.
            Err(_) => break,
        }
    }
    assert_eq!(fetched, 100, "only the rows asked for");
    execution.request_rows(1000);
    while fetched < 1100 {
        if let Some(ExecEvent::Rows(batch)) = execution.next().await {
            fetched += batch.row_count();
        }
    }
    assert_eq!(fetched, 1100);
    drop(execution);
    // The session is ready again once the abandoned statement stopped.
    assert_eq!(
        rows(&session, "SELECT count(*) FROM big").await,
        [[Some("50000".into())]]
    );
}

/// Acceptance 3: a long statement is stopped by a cancel (SQLite's
/// interrupt), and the session runs the next one. Fails when `cancel`
/// does not interrupt.
#[tokio::test]
async fn a_long_statement_is_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let long = "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n \
                WHERE x < 5000000000) SELECT count(*) FROM n";
    let running = tokio::spawn({
        let session = session.clone();
        async move { exec::collect(session, long).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    session.cancel().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("the statement stopped")
        .unwrap();
    assert!(matches!(outcome, Err(Error::Cancelled)), "{outcome:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(rows(&session, "SELECT 1").await, [[Some("1".into())]]);

    // A consumer that leaves stops a statement that sends nothing.
    let execution = exec::run(session.clone(), long.into(), RowLimit::All);
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(execution);
    let started = Instant::now();
    assert_eq!(rows(&session, "SELECT 2").await, [[Some("2".into())]]);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// Acceptance 4: every storage class as it is stored, NULL apart from an
/// empty text, blobs as bytes, and the declared types' rendering only where
/// the first value fits it.
#[tokio::test]
async fn storage_classes_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let collected = run(&session, "SELECT * FROM all_types ORDER BY id")
        .await
        .unwrap();
    let result = &collected.results[0];
    let kind = |name: &str| {
        let column = result.columns.iter().find(|c| c.name == name).unwrap();
        (column.type_name.clone(), column.kind)
    };
    assert_eq!(kind("i"), ("INTEGER".into(), TypeKind::Integer));
    assert_eq!(kind("r"), ("REAL".into(), TypeKind::Float));
    assert_eq!(kind("dec"), ("DECIMAL(10, 4)".into(), TypeKind::Decimal));
    assert_eq!(kind("v"), ("VARCHAR(20)".into(), TypeKind::Text));
    assert_eq!(kind("b"), ("BLOB".into(), TypeKind::Binary));
    assert_eq!(kind("bool"), ("BOOLEAN".into(), TypeKind::Boolean));
    assert_eq!(kind("day"), ("DATE".into(), TypeKind::Date));
    assert_eq!(kind("stamp"), ("DATETIME".into(), TypeKind::DateTime));
    assert_eq!(kind("doc"), ("JSON".into(), TypeKind::Json));
    assert_eq!(
        kind("untyped"),
        ("text".into(), TypeKind::Text),
        "its first value"
    );
    let batch = &result.batches[0];
    let at = |row: usize, name: &str| {
        let column = result.columns.iter().position(|c| c.name == name).unwrap();
        batch.cell(row, column).map(<[u8]>::to_vec)
    };
    assert_eq!(
        at(0, "big"),
        Some(b"9007199254740993".to_vec()),
        "no float on the way"
    );
    assert_eq!(at(0, "r"), Some(b"0.1".to_vec()));
    assert_eq!(
        at(2, "r"),
        Some(b"1.0".to_vec()),
        "a whole REAL stays a REAL"
    );
    assert_eq!(at(0, "t"), Some("héllo".as_bytes().to_vec()));
    assert_eq!(at(1, "t"), Some(Vec::new()), "empty text");
    assert_eq!(at(2, "t"), None, "NULL");
    assert_eq!(at(0, "b"), Some(vec![0x00, 0xff, 0x10]), "raw bytes");
    assert_eq!(at(1, "b"), Some(Vec::new()), "an empty blob is not NULL");
    // Each cell by its storage class: text in an INTEGER column, a number
    // in a DATE column, a blob in a column that is not binary.
    assert_eq!(at(2, "i"), Some(b"not a number".to_vec()));
    assert_eq!(at(1, "day"), Some(b"1700000000".to_vec()));
    assert_eq!(at(2, "untyped"), Some(b"X'cafe'".to_vec()));

    // A date kept as a Unix time is an integer column when it comes first.
    let collected = run(&session, "SELECT day FROM all_types WHERE id = 2")
        .await
        .unwrap();
    assert_eq!(collected.results[0].columns[0].kind, TypeKind::Integer);
    // An expression shows what it holds.
    let collected = run(&session, "SELECT x'0102' AS b, 1.5 AS f, 'a' AS t")
        .await
        .unwrap();
    let kinds: Vec<TypeKind> = collected.results[0]
        .columns
        .iter()
        .map(|c| c.kind)
        .collect();
    assert_eq!(kinds, [TypeKind::Binary, TypeKind::Float, TypeKind::Text]);
}

/// Acceptance 3: SQLite's message and the place it points at.
#[tokio::test]
async fn errors_say_where() {
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let error = run(&session, "SELECT 1;\nSELECT * FRM customers")
        .await
        .unwrap_err();
    let db = error.db().expect("a statement error");
    assert!(db.message.contains("syntax error"), "{}", db.message);
    // "FRM" starts at character 20 of the script, 1-based.
    assert_eq!(db.position, Some(20));
    let error = run(&session, "SELECT nope FROM customers")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no such column"), "{error}");
    // A parameter nobody filled in is refused, not run as NULL. Fails
    // without the check of `run_statement`.
    let error = run(&session, "SELECT $x IS NULL").await.unwrap_err();
    assert!(error.to_string().contains("parameter $x"), "{error}");
}

/// Acceptance 3: a transaction opened and closed is reported as such.
#[tokio::test]
async fn transactions_are_followed() {
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    assert_eq!(session.tx_state(), TxState::Idle);
    run(&session, session.dialect().begin_sql()).await.unwrap();
    assert_eq!(session.tx_state(), TxState::Open);
    run(&session, "DELETE FROM events").await.unwrap();
    run(&session, "ROLLBACK").await.unwrap();
    assert_eq!(session.tx_state(), TxState::Idle);
    assert_ne!(
        rows(&session, "SELECT count(*) FROM events").await,
        [[Some("0".into())]]
    );
    // Rows changed are counted; a CREATE changes none.
    let collected = run(&session, "UPDATE customers SET vip = 1").await.unwrap();
    assert_eq!(collected.results[0].rows_affected, Some(3));
    let collected = run(&session, "CREATE TABLE kerno_test_t (a)")
        .await
        .unwrap();
    assert_eq!(collected.results[0].rows_affected, Some(0));
}

/// Acceptance 2 and 6: what the tree and the structure tabs read.
#[tokio::test]
async fn the_fixture_is_introspected() {
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let databases = session.databases().await.unwrap();
    assert_eq!(databases.len(), 1);
    assert_eq!(databases[0].name, "main");
    assert!(databases[0].size_bytes.unwrap() > 0);
    assert!(session.schemas().await.unwrap().is_empty());

    let objects = session.objects("main").await.unwrap();
    let names: Vec<(&str, RelationKind, Option<u64>)> = objects
        .relations
        .iter()
        .map(|r| (r.name.as_str(), r.kind, r.row_estimate))
        .collect();
    assert!(
        names.contains(&("customers", RelationKind::Table, Some(3))),
        "{names:?}"
    );
    assert!(names.contains(&("customer_orders", RelationKind::View, None)));
    assert!(
        !names.iter().any(|(name, ..)| name.starts_with("sqlite_")),
        "SQLite's own tables are not listed: {names:?}"
    );
    assert_eq!(objects.triggers.len(), 1);
    let trigger = &objects.triggers[0];
    assert_eq!(
        (
            trigger.name.as_str(),
            trigger.table.as_str(),
            trigger.timing.as_str(),
            trigger.events.as_str()
        ),
        ("orders_note_default", "orders", "AFTER", "INSERT")
    );

    let table = |name: &str| TableRef::new("main", name);
    let customers = session.table_structure(&table("customers")).await.unwrap();
    let id = &customers.columns[0];
    assert_eq!(
        id.generation,
        Some(ColumnGeneration::AutoIncrement),
        "the row id"
    );
    assert!(!id.nullable);
    assert_eq!(customers.row_identity(), Some(vec!["id".to_string()]));
    let created = customers
        .columns
        .iter()
        .find(|c| c.name == "created_at")
        .unwrap();
    assert_eq!(created.default.as_deref(), Some("CURRENT_TIMESTAMP"));
    assert_eq!(created.kind, TypeKind::DateTime);
    assert!(
        customers
            .constraints
            .iter()
            .any(|c| c.definition == "UNIQUE (\"email\")")
    );
    let referencing: Vec<&str> = customers
        .referenced_by
        .iter()
        .map(|key| key.table.name.as_str())
        .collect();
    assert_eq!(referencing, ["orders"]);

    let products = session.table_structure(&table("products")).await.unwrap();
    assert_eq!(
        products.row_identity(),
        Some(vec!["sku".to_string()]),
        "WITHOUT ROWID"
    );
    assert_eq!(products.columns[0].generation, None);
    assert!(products.indexes.iter().any(|index| index.primary));

    let lines = session
        .table_structure(&table("order_lines"))
        .await
        .unwrap();
    assert_eq!(
        lines.row_identity(),
        Some(vec!["order_id".to_string(), "line_no".to_string()])
    );
    let keys: Vec<(&str, Vec<String>, Vec<String>)> = lines
        .foreign_keys
        .iter()
        .map(|key| {
            (
                key.referenced_table.name.as_str(),
                key.columns.clone(),
                key.referenced_columns.clone(),
            )
        })
        .collect();
    assert!(
        keys.contains(&("orders", vec!["order_id".into()], vec!["id".into()])),
        "{keys:?}"
    );
    assert!(keys.contains(&("products", vec!["sku".into()], vec!["sku".into()])));
    let partial = lines
        .indexes
        .iter()
        .find(|i| i.name == "order_lines_sku")
        .unwrap();
    assert_eq!(partial.predicate.as_deref(), Some("quantity > 0"));

    let orders = session.table_structure(&table("orders")).await.unwrap();
    assert_eq!(orders.triggers.len(), 1);
    assert_eq!(orders.foreign_keys[0].on_delete, "CASCADE");

    let events = session.table_structure(&table("events")).await.unwrap();
    assert_eq!(events.row_identity(), None, "no key: read-only");
    let payload = events.columns.iter().find(|c| c.name == "payload").unwrap();
    assert_eq!(
        (payload.data_type.as_str(), payload.kind),
        ("", TypeKind::Binary)
    );

    let view = session
        .table_structure(&table("customer_orders"))
        .await
        .unwrap();
    assert_eq!(view.columns.len(), 3);
    let missing = session.table_structure(&table("nope")).await.unwrap_err();
    assert!(
        missing.to_string().contains("no table or view “nope”"),
        "{missing}"
    );

    let catalog = session.completion_catalog().await.unwrap();
    assert_eq!(catalog.search_path, ["main"]);
    let lines = catalog.schemas[0]
        .relations
        .iter()
        .find(|r| r.name == "order_lines")
        .unwrap();
    assert_eq!(lines.columns.len(), 4);
    let to_orders = lines
        .foreign_keys
        .iter()
        .find(|key| key.referenced_table == "orders")
        .unwrap();
    assert_eq!(
        to_orders.referenced_columns,
        ["id"],
        "the referenced primary key"
    );
}

/// Acceptance 6: the statements SQLite kept, and a whole database's.
#[tokio::test]
async fn ddl_is_what_sqlite_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let orders = session
        .ddl(&ObjectRef::Relation(
            TableRef::new("main", "orders"),
            RelationKind::Table,
        ))
        .await
        .unwrap();
    assert!(orders.starts_with("CREATE TABLE orders ("), "{orders}");
    assert!(orders.contains("CREATE INDEX orders_customer"), "{orders}");
    assert!(
        orders.contains("CREATE TRIGGER orders_note_default"),
        "{orders}"
    );
    let whole = session
        .ddl(&ObjectRef::Database("main".into()))
        .await
        .unwrap();
    assert!(whole.contains("CREATE VIEW customer_orders"));
    let auto = session
        .ddl(&ObjectRef::Index {
            table: TableRef::new("main", "customers"),
            name: "sqlite_autoindex_customers_1".into(),
        })
        .await
        .unwrap_err();
    assert!(auto.to_string().contains("made by SQLite"), "{auto}");
}

/// Acceptance 5: edits apply in one transaction; a failing one rolls back
/// the ones before it. `DEFAULT` becomes the declared default.
#[tokio::test]
async fn edits_apply_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let table = TableRef::new("main", "customers");
    let structure = session.table_structure(&table).await.unwrap();
    let dialect = session.dialect();
    let key = |id: &str| vec![("id".to_string(), CellValue::Text(id.into()))];
    let statements = edit_statements(
        dialect,
        &table,
        &structure.columns,
        &[
            RowChange::Update {
                key: key("1"),
                set: vec![
                    ("name".into(), CellValue::Text("Ada L.".into())),
                    ("vip".into(), CellValue::Default),
                ],
            },
            RowChange::Insert {
                values: vec![
                    ("id".into(), CellValue::Default),
                    ("email".into(), CellValue::Text("dee@example.com".into())),
                ],
            },
            RowChange::Delete { key: key("3") },
        ],
    )
    .unwrap();
    assert!(statements[0].contains("\"vip\" = (0)"), "{statements:?}");
    apply_edits(&session, &statements).await.unwrap();
    assert_eq!(
        rows(&session, "SELECT id, name, vip FROM customers ORDER BY id").await,
        [
            vec![Some("1".into()), Some("Ada L.".into()), Some("0".into())],
            vec![Some("2".into()), None, Some("0".into())],
            vec![Some("4".into()), None, Some("0".into())],
        ]
    );
    assert_eq!(session.tx_state(), TxState::Idle);

    // The second statement breaks the unique email: the first is undone.
    let failing = edit_statements(
        dialect,
        &table,
        &structure.columns,
        &[
            RowChange::Update {
                key: key("1"),
                set: vec![("name".into(), CellValue::Text("changed".into()))],
            },
            RowChange::Update {
                key: key("2"),
                set: vec![("email".into(), CellValue::Text("ada@example.com".into()))],
            },
        ],
    )
    .unwrap();
    let failure = apply_edits(&session, &failing).await.unwrap_err();
    assert_eq!(failure.statement, Some(1), "{failure}");
    assert!(failure.to_string().contains("UNIQUE"), "{failure}");
    assert_eq!(
        rows(&session, "SELECT name FROM customers WHERE id = 1").await,
        [[Some("Ada L.".into())]]
    );
    assert_eq!(session.tx_state(), TxState::Idle);
}

fn column(name: &str, data_type: &str) -> ColumnDraft {
    ColumnDraft {
        original: None,
        name: name.into(),
        data_type: data_type.into(),
        nullable: true,
        default: None,
        comment: None,
        auto_increment: AutoIncrement::Off,
        on_update: None,
        invisible: false,
    }
}

/// Acceptance 6: a table made from a draft, then the four alters SQLite
/// does, run on the file.
#[tokio::test]
async fn create_and_alter_run_on_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let dialect = session.dialect();
    let mut id = column("id", "INTEGER");
    id.auto_increment = AutoIncrement::On;
    let draft = TableDraft {
        table: TableRef::new("main", "kerno_test_made"),
        comment: None,
        columns: vec![id, column("label", "TEXT"), column("old", "TEXT")],
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        primary_key: vec!["id".into()],
    };
    for sql in dialect.create_table_sql(&draft).unwrap() {
        run(&session, &sql)
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    run(&session, "INSERT INTO kerno_test_made (label) VALUES ('a')")
        .await
        .unwrap();
    let table = TableRef::new("main", "kerno_test_made");
    let changes = [
        TableChange::RenameColumn {
            from: "label".into(),
            to: "title".into(),
        },
        TableChange::AddColumn(column("added", "INTEGER")),
        TableChange::DropColumn("old".into()),
        TableChange::RenameTable {
            to: "kerno_test_renamed".into(),
        },
    ];
    for sql in dialect.alter_table_sql(&table, &changes).unwrap() {
        run(&session, &sql)
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    let structure = session
        .table_structure(&TableRef::new("main", "kerno_test_renamed"))
        .await
        .unwrap();
    let names: Vec<&str> = structure.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "title", "added"]);
    assert_eq!(
        rows(&session, "SELECT id, title FROM kerno_test_renamed").await,
        [[Some("1".into()), Some("a".into())]]
    );
}

/// Acceptance 3: EXPLAIN QUERY PLAN read into the plan tree, from the rows
/// joined by tabs as the query tab joins them.
#[tokio::test]
async fn query_plans_become_trees() {
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let dialect = session.dialect();
    let sql = dialect.explain(
        "SELECT * FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.customer_id = 1",
        false,
    );
    let raw: Vec<String> = rows(&session, &sql)
        .await
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(Option::unwrap_or_default)
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect();
    let plan = dialect.parse_plan(&raw.join("\n"), false).unwrap();
    let operations: Vec<&str> = plan
        .root
        .children
        .iter()
        .map(|n| n.operation.as_str())
        .collect();
    assert!(
        operations.contains(&"SEARCH"),
        "{operations:?} from {raw:?}"
    );
    assert!(dialect.explain_unavailable(true).is_some());
}

async fn export(
    session: &Arc<dyn Session>,
    sql: &str,
    format: kerno_core::export::ExportFormat,
    path: &Path,
) -> Result<kerno_core::export_file::ExportProgress> {
    let job = kerno_core::export_file::ExportJob {
        sql: sql.into(),
        format,
        encoding: kerno_core::export_file::Encoding::Utf8,
        path: path.into(),
    };
    kerno_core::export_file::export_to_file(session.clone(), job, |_| {}, Arc::default()).await
}

/// Acceptance 7: a result goes to every file format through the generic
/// path; its SQL INSERT file runs back on the file; a CSV comes back into an
/// existing table and into a new one.
#[tokio::test]
async fn results_export_and_csv_imports_on_a_file() {
    use kerno_core::export::{DelimitedOptions, ExportFormat};
    use kerno_core::import::{CsvOptions, ImportJob, Target, import_csv};
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let query = "SELECT id, email, name, vip FROM customers ORDER BY id";
    let at = |name: &str| dir.path().join(name);
    let csv = at("customers.csv");
    let done = export(
        &session,
        query,
        ExportFormat::Delimited(DelimitedOptions::csv()),
        &csv,
    )
    .await
    .unwrap();
    assert_eq!(done.rows, 3);
    let text = std::fs::read_to_string(&csv).unwrap();
    assert!(text.starts_with("id,email,name,vip"), "{text}");
    for (name, format) in [
        ("c.json", ExportFormat::JsonArray),
        ("c.ndjson", ExportFormat::NdJson),
        ("c.md", ExportFormat::Markdown),
        ("c.xlsx", ExportFormat::Xlsx(Default::default())),
    ] {
        let written = export(&session, query, format, &at(name)).await.unwrap();
        assert_eq!(written.rows, 3, "{name}");
        assert!(std::fs::metadata(at(name)).unwrap().len() > 0, "{name}");
    }
    let json = std::fs::read_to_string(at("c.json")).unwrap();
    assert!(
        json.contains("\"email\": \"ada@example.com\"")
            || json.contains("\"email\":\"ada@example.com\""),
        "{json}"
    );

    // SQL INSERT into a copy of the table, run as a script.
    run(
        &session,
        "CREATE TABLE kerno_test_copy AS SELECT id, email, name, vip FROM customers WHERE 0",
    )
    .await
    .unwrap();
    let insert = ExportFormat::SqlInsert {
        table: TableRef::new("main", "kerno_test_copy"),
    };
    export(&session, query, insert, &at("c.sql")).await.unwrap();
    run(&session, &std::fs::read_to_string(at("c.sql")).unwrap())
        .await
        .unwrap();
    assert_eq!(
        rows(&session, "SELECT count(*), sum(vip) FROM kerno_test_copy").await,
        [[Some("3".into()), Some("1".into())]]
    );

    // The CSV into the existing copy, then into a new table.
    let options = CsvOptions {
        delimiter: ',',
        header: true,
        null_text: String::new(),
        encoding: kerno_core::export_file::Encoding::Utf8,
    };
    let mapping: Vec<Option<String>> = ["id", "email", "name", "vip"]
        .map(|name| Some(name.to_string()))
        .to_vec();
    run(&session, "DELETE FROM kerno_test_copy").await.unwrap();
    let existing = TableRef::new("main", "kerno_test_copy");
    let columns = session.table_structure(&existing).await.unwrap().columns;
    let report = import_csv(
        session.clone(),
        columns,
        ImportJob {
            path: csv.clone(),
            options: options.clone(),
            target: Target::Existing(existing),
            mapping: mapping.clone(),
            batch_rows: 2,
        },
        |_| {},
        Arc::default(),
    )
    .await
    .unwrap();
    assert_eq!((report.inserted, report.failed), (3, None));
    let dialect = session.dialect();
    let target = Target::New {
        table: TableRef::new("main", "kerno_test_imported"),
        columns: [
            ("id", TypeKind::Integer),
            ("email", TypeKind::Text),
            ("name", TypeKind::Text),
            ("vip", TypeKind::Boolean),
        ]
        .map(|(name, kind)| (name.to_string(), dialect.type_for(kind).to_string()))
        .to_vec(),
    };
    let report = import_csv(
        session.clone(),
        Vec::new(),
        ImportJob {
            path: csv,
            options,
            target,
            mapping,
            batch_rows: 500,
        },
        |_| {},
        Arc::default(),
    )
    .await
    .unwrap();
    assert_eq!(report.inserted, 3);
    assert_eq!(
        rows(
            &session,
            "SELECT email FROM kerno_test_imported WHERE id = 1"
        )
        .await,
        [[Some("ada@example.com".into())]]
    );
    assert_eq!(session.tx_state(), TxState::Idle);
}

/// Acceptance 9: the ER diagram of the file comes from the catalog path,
/// its foreign keys as edges.
#[tokio::test]
async fn the_er_diagram_reads_the_file() {
    use kerno_core::erd::{Scope, load};
    use std::sync::atomic::AtomicBool;
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let loaded = load(
        session,
        &Scope::Schema("main".into()),
        |_| {},
        &AtomicBool::new(false),
    )
    .await
    .unwrap();
    let diagram = loaded.diagram;
    let names: Vec<&str> = diagram
        .nodes
        .iter()
        .map(|n| n.table.name.as_str())
        .collect();
    assert!(
        names.contains(&"orders") && names.contains(&"order_lines"),
        "{names:?}"
    );
    let to_customers = diagram.edges.iter().any(|edge| {
        diagram.nodes[edge.from].table.name == "orders"
            && diagram.nodes[edge.to].table.name == "customers"
    });
    assert!(to_customers, "{:?}", diagram.edges);
    assert!(loaded.skipped.is_empty(), "{:?}", loaded.skipped);
}

/// K31, K32: a schema document of the file names its tables, views and
/// trigger (the PDF in its outline), holds only what was picked, and never
/// the file's path.
#[tokio::test]
async fn a_schema_document_of_the_file() {
    use kerno_core::schema_doc::{self, Document, Fonts, Format, Options, Pick};
    use std::sync::atomic::AtomicBool;
    let dir = tempfile::tempdir().unwrap();
    let (file, session) = fixture(dir.path()).await;
    let document = |side| Document {
        title: schema_doc::default_title("main", None),
        engine: Engine::Sqlite,
        server: schema_doc::server_label(Engine::Sqlite, session.server()),
        database: "main".into(),
        schema: None,
        date: schema_doc::today(),
        side,
    };
    let objects = session.objects("main").await.unwrap();
    let all = schema_doc::pickable(&objects)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let pick = Pick {
        relations: all,
        code: true,
    };
    let side = schema_doc::load(
        session.clone(),
        "main",
        &pick,
        |_| {},
        &AtomicBool::new(false),
    )
    .await
    .unwrap();
    let whole = document(side);
    let path = file.display().to_string();
    for format in Format::ALL {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../kerno/assets/fonts");
        let read = |file: &str| std::fs::read(dir.join(file)).unwrap();
        let (regular, semibold, mono) = (
            read("inter/Inter-Regular.ttf"),
            read("inter/Inter-SemiBold.ttf"),
            read("jetbrains-mono/JetBrainsMono-Regular.ttf"),
        );
        let fonts = Fonts {
            regular: &regular,
            semibold: &semibold,
            mono: &mono,
        };
        let bytes = schema_doc::render(
            &whole,
            format,
            &Options::default(),
            session.dialect(),
            &fonts,
        )
        .unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for table in ["customers", "products", "orders", "order_lines"] {
            assert!(text.contains(table), "{format:?}: {table}");
        }
        if matches!(
            format,
            Format::Html | Format::Pdf | Format::Markdown | Format::Sql
        ) {
            assert!(text.contains("customer_orders"), "{format:?}");
            assert!(text.contains("orders_note_default"), "{format:?}");
        }
        assert!(!text.contains(&path), "{format:?}");
        assert!(!text.contains("fixture.db"), "{format:?}");
    }
    let pick = Pick {
        relations: ["orders".to_string()].into(),
        code: false,
    };
    let side = schema_doc::load(
        session.clone(),
        "main",
        &pick,
        |_| {},
        &AtomicBool::new(false),
    )
    .await
    .unwrap();
    assert_eq!(side.objects.len(), 1);
    assert_eq!(side.objects[0].object.name(), "orders");
}

/// K50 (D-119): what the AI assistant tells the model of a file's tables: each column's type
/// and whether it can be NULL, the keys and the indexes, the tables a statement names first;
/// no default and no index predicate.
#[tokio::test]
async fn the_ai_context_says_nulls_keys_and_indexes() {
    use kerno_core::ai::{self, Action, Context};
    let dir = tempfile::tempdir().unwrap();
    let (_file, session) = fixture(dir.path()).await;
    let action = Action::ExplainQuery {
        sql: "select * from order_lines l join orders o on o.id = l.order_id".into(),
    };
    let catalog = session.completion_catalog().await.unwrap();
    let mut context = Context::from_catalog(&catalog, "SQLite", None, None, None, action.text());
    assert_eq!(
        context.detail_tables(),
        [
            TableRef::new("main", "order_lines"),
            TableRef::new("main", "orders")
        ]
    );
    let details: std::collections::HashMap<_, _> =
        ai::read_details(session.clone(), context.detail_tables())
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
        "- main.order_lines (table)\n  order_id INTEGER not null\n  line_no INTEGER not null",
        "\n  primary key (order_id, line_no)",
        "\n  foreign key (sku) references main.products (sku)",
        "\n  index (sku)",
        // Its default (1) is not said.
        "\n  quantity INTEGER not null\n",
        "- main.orders (table)\n  id INTEGER not null",
        "\n  note TEXT\n",
        "\n  foreign key (customer_id) references main.customers (id)",
        "\n  index (customer_id)",
    ] {
        assert!(system.contains(line), "{line:?} missing:\n{system}");
    }
    for never in ["quantity > 0", "DEFAULT 1"] {
        assert!(!system.contains(never), "{never}:\n{system}");
    }

    // Indexes on expressions can hold values: only column names are sent.
    for sql in [
        "CREATE INDEX kerno_test_lower ON orders (lower(note), placed_on)",
        "CREATE UNIQUE INDEX kerno_test_coalesce ON orders (coalesce(note, 'kerno-literal-1'))",
        "CREATE INDEX kerno_test_partial ON orders (placed_on) WHERE note <> 'kerno-literal-2'",
    ] {
        run(&session, sql).await.unwrap();
    }
    let read = ai::read_details(session.clone(), vec![TableRef::new("main", "orders")]).await;
    let details: std::collections::HashMap<_, _> = read
        .into_iter()
        .map(|(table, detail)| (table, detail.unwrap()))
        .collect();
    context.add_details(&details);
    let system = ai::build_payload(&action, &context, "m")["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        system.contains("\n  index ((expression), placed_on)"),
        "{system}"
    );
    for never in ["kerno-literal", "lower(", "coalesce"] {
        assert!(!system.contains(never), "{never}:\n{system}");
    }
}

/// K59: the bundled SQLite says where each column of a result comes from
/// (its column metadata), through a join and under an alias, as MySQL does;
/// an expression has none. A view's columns are named by the table they read
/// (SQLite looks through the view).
#[tokio::test]
async fn a_join_names_the_table_column_of_each_column() {
    use kerno_core::links::named_origins;
    let dir = tempfile::tempdir().unwrap();
    let (_, session) = fixture(dir.path()).await;
    let columns = |sql: &'static str| {
        let session = session.clone();
        async move { run(&session, sql).await.unwrap().results[0].columns.clone() }
    };
    let at = |table: &str, column: &str| Some((TableRef::new("main", table), column.to_string()));
    let joined = columns(
        "select o.id, o.customer_id as who, c.name, o.id + 1 as next \
         from orders o join customers c on c.id = o.customer_id",
    )
    .await;
    assert_eq!(
        named_origins(&session, &joined).await.unwrap(),
        [
            at("orders", "id"),
            at("orders", "customer_id"),
            at("customers", "name"),
            None
        ]
    );
    let view = columns("select customer_id, email, orders from customer_orders").await;
    assert_eq!(
        named_origins(&session, &view).await.unwrap(),
        [at("customers", "id"), at("customers", "email"), None]
    );
}
