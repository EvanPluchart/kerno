//! Test data for a table (K72, D-141) on SQLite, in database files these tests
//! create in temporary directories: a table with each type SQLite names, a
//! primary key, a unique column, a foreign key to a second table and a
//! generated column gets 1,000 rows in one transaction and keeps every rule; its
//! script runs as written; a parent without rows is refused before anything
//! runs; a row the file refuses keeps nothing. SQLite has no enum type: a CHECK
//! that lists values is a rule Kerno says it does not check.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use kerno_core::config::{ConnectParams, Engine, TlsConfig};
use kerno_core::exec::{self, Collected};
use kerno_core::schema::TableRef;
use kerno_core::sql::split_statements;
use kerno_core::test_data::{self, Kind, Plan, Settings, Shape};
use kerno_core::{Driver, Result, Session, TxState};
use kerno_sqlite::SqliteDriver;

async fn open(file: &Path) -> Arc<dyn Session> {
    assert!(kerno_sqlite::new_database_file(file).unwrap());
    let params = ConnectParams {
        engine: Engine::Sqlite,
        host: String::new(),
        port: 0,
        tunnel: None,
        user: String::new(),
        password: None,
        database: None,
        file: Some(file.to_path_buf()),
        tls: TlsConfig::default(),
        read_only: false,
        options: BTreeMap::new(),
    };
    SqliteDriver.connect(&params).await.unwrap()
}

async fn run(session: &Arc<dyn Session>, sql: &str) -> Result<Collected> {
    exec::collect(session.clone(), sql).await
}

async fn scalar(session: &Arc<dyn Session>, sql: &str) -> String {
    run(session, sql).await.unwrap().results[0].rows()[0][0]
        .clone()
        .unwrap_or_default()
}

async fn guessed_plan(session: &Arc<dyn Session>, table: &str, rows: u64) -> Plan {
    let facts = test_data::read(session.clone(), &TableRef::new("main", table))
        .await
        .unwrap();
    let settings = Settings {
        rows,
        seed: 72,
        columns: test_data::guess(&facts),
    };
    Plan::new(&facts, &settings).unwrap()
}

async fn insert(session: &Arc<dyn Session>, plan: Plan) -> test_data::Report {
    let cancel = Arc::new(AtomicBool::new(false));
    test_data::insert(session.clone(), plan, |_| {}, cancel)
        .await
        .unwrap()
}

/// K72: 1,000 generated rows insert in one transaction and keep the table's
/// rules; the script of 1,000 more runs as written.
#[tokio::test]
async fn a_thousand_rows_of_every_type_insert_and_keep_the_rules() {
    let dir = tempfile::tempdir().unwrap();
    let session = open(&dir.path().join("kerno_test_k72.db")).await;
    for sql in [
        "CREATE TABLE kerno_test_k72_parent (id INTEGER PRIMARY KEY, code TEXT NOT NULL UNIQUE)",
        "INSERT INTO kerno_test_k72_parent VALUES (1, 'p1'), (2, 'p2'), (3, 'p3')",
        "CREATE TABLE kerno_test_k72_child (\
         id INT PRIMARY KEY, \
         email VARCHAR(120) NOT NULL UNIQUE, \
         name TEXT, \
         price DECIMAL(8,2), \
         ratio REAL, \
         flag BOOLEAN NOT NULL, \
         born DATE, \
         at DATETIME, \
         t TIME, \
         doc JSON, \
         data BLOB, \
         mood TEXT NOT NULL CHECK (mood IN ('sad', 'ok', 'happy')) DEFAULT 'ok', \
         parent_id INTEGER NOT NULL REFERENCES kerno_test_k72_parent (id), \
         total REAL GENERATED ALWAYS AS (price * 2) STORED)",
    ] {
        run(&session, sql).await.unwrap();
    }
    let facts = test_data::read(
        session.clone(),
        &TableRef::new("main", "kerno_test_k72_child"),
    )
    .await
    .unwrap();
    let guessed = test_data::guess(&facts);
    let kinds: Vec<Kind> = guessed.iter().map(|line| line.kind).collect();
    assert_eq!(
        kinds,
        [
            Kind::Sequence,
            Kind::Shape(Shape::Email),
            Kind::Shape(Shape::FullName),
            Kind::Decimal,
            Kind::Decimal,
            Kind::Boolean,
            Kind::Date,
            Kind::DateTime,
            Kind::Time,
            Kind::Json,
            Kind::Bytes,
            Kind::Words,
            Kind::Reference,
            Kind::Server,
        ]
    );
    // The listed values are a CHECK Kerno says it does not check; left to
    // its default, the rows keep it.
    assert!(
        test_data::notes(&facts)
            .iter()
            .any(|note| note.contains("CHECK rules Kerno does not read"))
    );
    let mut settings = Settings {
        rows: 1000,
        seed: 72,
        columns: guessed,
    };
    settings.columns[11] = test_data::defaults(&facts, 11, Kind::Default);
    let report = insert(&session, Plan::new(&facts, &settings).unwrap()).await;
    assert_eq!(report.failed, None);
    assert_eq!(report.inserted, 1000);
    assert_eq!(session.tx_state(), TxState::Idle);
    assert_eq!(
        scalar(
            &session,
            "SELECT count(*) || '/' || count(DISTINCT id) || '/' || count(DISTINCT email) || '/' \
             || min(id) || '/' || count(DISTINCT parent_id) || '/' || sum(flag IN (0, 1)) \
             || '/' || count(DISTINCT mood) || '/' || (sum(total = price * 2) = count(price)) \
             FROM kerno_test_k72_child"
        )
        .await,
        "1000/1000/1000/1/3/1000/1/1"
    );
    assert_eq!(
        scalar(
            &session,
            "SELECT sum(json_valid(doc)) = count(doc) AND count(doc) < 1000 \
             FROM kerno_test_k72_child"
        )
        .await,
        "1"
    );

    // Read again: the key and the unique email start beyond the rows there;
    // the listed values left to their default again.
    let facts = test_data::read(
        session.clone(),
        &TableRef::new("main", "kerno_test_k72_child"),
    )
    .await
    .unwrap();
    let mut settings = Settings {
        rows: 1000,
        seed: 72,
        columns: test_data::guess(&facts),
    };
    settings.columns[11] = test_data::defaults(&facts, 11, Kind::Default);
    let plan = Plan::new(&facts, &settings).unwrap();
    let script = plan.script(session.dialect(), "test data").unwrap();
    assert!(script.contains("(1001, '"), "{}", &script[..1000]);
    for statement in split_statements(&script, session.dialect().lex_rules()) {
        let sql = statement.text(&script);
        if let Err(error) = run(&session, sql).await {
            panic!("{sql}: {error}");
        }
    }
    assert_eq!(
        scalar(
            &session,
            "SELECT count(DISTINCT email) || '/' || max(id) FROM kerno_test_k72_child"
        )
        .await,
        "2000/2000"
    );
    session.close().await;
}

/// K72: an empty parent is refused before running; a failing row rolls the
/// whole insert back and is named.
#[tokio::test]
async fn an_empty_parent_is_refused_and_a_failing_row_keeps_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let session = open(&dir.path().join("kerno_test_k72_refusals.db")).await;
    for sql in [
        "CREATE TABLE kerno_test_k72_empty (id INTEGER PRIMARY KEY)",
        "CREATE TABLE kerno_test_k72_orphan (id INT PRIMARY KEY, \
         empty_id INTEGER NOT NULL REFERENCES kerno_test_k72_empty (id))",
        "CREATE TABLE kerno_test_k72_checked (id INT PRIMARY KEY, \
         price DECIMAL(8,2) NOT NULL, CONSTRAINT kerno_test_k72_cheap CHECK (price < 500))",
    ] {
        run(&session, sql).await.unwrap();
    }
    let facts = test_data::read(
        session.clone(),
        &TableRef::new("main", "kerno_test_k72_orphan"),
    )
    .await
    .unwrap();
    let settings = Settings {
        rows: 10,
        seed: 1,
        columns: test_data::guess(&facts),
    };
    assert_eq!(
        Plan::new(&facts, &settings).unwrap_err(),
        "“main.kerno_test_k72_empty” has no rows, and “empty_id” must hold one of its keys: \
         add rows there first."
    );
    let plan = guessed_plan(&session, "kerno_test_k72_checked", 1000).await;
    let first_bad = plan
        .values()
        .position(|row| {
            let price = test_data::shown(&row.unwrap()[1]);
            price.parse::<f64>().unwrap() >= 500.0
        })
        .unwrap() as u64
        + 1;
    let report = insert(&session, plan).await;
    let failure = report.failed.expect("a refused row");
    assert_eq!(report.inserted, 0);
    assert_eq!(failure.row, first_bad);
    assert!(
        failure.message.contains("kerno_test_k72_cheap"),
        "{failure}"
    );
    assert_eq!(session.tx_state(), TxState::Idle);
    assert_eq!(
        scalar(&session, "SELECT count(*) FROM kerno_test_k72_checked").await,
        "0"
    );
    session.close().await;
}
