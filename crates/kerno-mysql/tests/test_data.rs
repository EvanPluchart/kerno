//! Test data for a table (K72, D-141) on MySQL and MariaDB: a scratch table with
//! every common type, a primary key, a unique column, foreign keys to a second
//! scratch table, an `ENUM`, a `SET`, an auto-increment and a generated column
//! gets 1,000 rows in one transaction and keeps every rule; the script of 1,000
//! more runs as it is written; a parent without rows is refused before anything
//! runs; a row the server refuses keeps nothing. Everything lives in scratch
//! databases `kerno_test_k72_*`, dropped before and after, also on a failure.
//! Skipped for an engine whose container is down.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use common::{engines, exec_ok, run, scalar};
use kerno_core::schema::TableRef;
use kerno_core::sql::split_statements;
use kerno_core::test_data::{self, Kind, Plan, Settings, Shape};
use kerno_core::{Engine, Session, TxState};

/// Runs `body` on each engine with the scratch database `database` created,
/// and drops it after, whether `body` passed or panicked.
async fn in_scratch<F, B>(database: &'static str, body: B)
where
    F: std::future::Future<Output = ()> + Send + 'static,
    B: Fn(Arc<dyn Session>, Engine) -> F,
{
    for engine in engines() {
        let admin = common::session(engine).await;
        let cleanup = format!("DROP DATABASE IF EXISTS {database}");
        exec_ok(&admin, &cleanup).await;
        exec_ok(
            &admin,
            &format!("CREATE DATABASE {database} CHARACTER SET utf8mb4"),
        )
        .await;
        let outcome = tokio::spawn(body(admin.clone(), engine)).await;
        let dropped = run(&admin, &cleanup).await;
        if let Err(error) = outcome {
            std::panic::resume_unwind(error.into_panic());
        }
        dropped.expect("the scratch database dropped");
    }
}

/// The plan the dialog starts with for `rows` rows of `table`.
async fn guessed_plan(session: &Arc<dyn Session>, database: &str, table: &str, rows: u64) -> Plan {
    let facts = test_data::read(session.clone(), &TableRef::new(database, table))
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

const TYPES: &str = "kerno_test_k72_types";

async fn every_type(admin: Arc<dyn Session>, engine: Engine) {
    let db = TYPES;
    for sql in [
        format!(
            "CREATE TABLE {db}.kerno_test_k72_parent (id INT PRIMARY KEY, \
             code VARCHAR(20) NOT NULL UNIQUE)"
        ),
        format!(
            "INSERT INTO {db}.kerno_test_k72_parent VALUES \
             (1, 'p1'), (2, 'p2'), (3, 'p3'), (4, 'p4'), (5, 'p5')"
        ),
        format!(
            "CREATE TABLE {db}.kerno_test_k72_child (\
             id INT PRIMARY KEY, \
             email VARCHAR(120) NOT NULL UNIQUE, \
             name VARCHAR(100), \
             price DECIMAL(8,2), \
             qty SMALLINT UNSIGNED NOT NULL, \
             tiny TINYINT, \
             big BIGINT, \
             ratio DOUBLE, \
             flag TINYINT(1) NOT NULL, \
             born DATE, \
             at DATETIME, \
             stamp TIMESTAMP NULL, \
             t TIME, \
             yr YEAR, \
             public_uuid CHAR(36) NOT NULL, \
             doc JSON, \
             mood ENUM('sad', 'ok', 'happy') NOT NULL, \
             opts SET('a', 'b', 'c'), \
             data VARBINARY(32), \
             fixed BINARY(16), \
             bits BIT(4), \
             serial_n BIGINT NOT NULL AUTO_INCREMENT UNIQUE, \
             parent_id INT NOT NULL, \
             parent_code VARCHAR(20), \
             total DECIMAL(10,2) GENERATED ALWAYS AS (price * 2) STORED, \
             FOREIGN KEY (parent_id) REFERENCES {db}.kerno_test_k72_parent (id), \
             FOREIGN KEY (parent_code) REFERENCES {db}.kerno_test_k72_parent (code)\
             ) ENGINE=InnoDB"
        ),
    ] {
        exec_ok(&admin, &sql).await;
    }
    let facts = test_data::read(admin.clone(), &TableRef::new(db, "kerno_test_k72_child"))
        .await
        .unwrap();
    let guessed = test_data::guess(&facts);
    let kind = |name: &str| {
        let index = facts
            .structure
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap();
        guessed[index].kind
    };
    assert_eq!(kind("id"), Kind::Sequence, "{engine:?}");
    assert_eq!(kind("email"), Kind::Shape(Shape::Email));
    assert_eq!(kind("public_uuid"), Kind::Uuid);
    // MariaDB's JSON is a long text checked by json_valid().
    assert_eq!(kind("doc"), Kind::Json, "{engine:?}");
    assert_eq!(kind("mood"), Kind::OneOf);
    assert_eq!(kind("opts"), Kind::OneOf);
    assert_eq!(kind("parent_id"), Kind::Reference);
    assert_eq!(kind("serial_n"), Kind::Server);
    assert_eq!(kind("total"), Kind::Server);

    let plan = guessed_plan(&admin, db, "kerno_test_k72_child", 1000).await;
    let report = insert(&admin, plan).await;
    assert_eq!(report.failed, None, "{engine:?}");
    assert_eq!(report.inserted, 1000);
    assert_eq!(admin.tx_state(), TxState::Idle);
    let child = format!("{db}.kerno_test_k72_child");
    assert_eq!(
        scalar(
            &admin,
            &format!(
                "SELECT concat_ws('/', count(*), count(DISTINCT id), count(DISTINCT email), \
                 min(id), count(DISTINCT mood), count(DISTINCT parent_id), \
                 sum(total = price * 2) = count(price), count(DISTINCT serial_n)) FROM {child}"
            )
        )
        .await,
        "1000/1000/1000/1/3/5/1/1000",
        "{engine:?}"
    );
    let names: u64 = scalar(&admin, &format!("SELECT count(name) FROM {child}"))
        .await
        .parse()
        .unwrap();
    assert!((850..=950).contains(&names), "{names} names");
    assert_eq!(
        scalar(
            &admin,
            &format!(
                "SELECT count(*) FROM {child} c JOIN {db}.kerno_test_k72_parent p \
                 ON p.id = c.parent_id WHERE c.qty BETWEEN 1 AND 1000 \
                 AND coalesce(c.born BETWEEN '2020-01-01' AND '2026-12-31', 1) \
                 AND coalesce(c.yr BETWEEN 2000 AND 2030, 1) \
                 AND coalesce(json_valid(c.doc), 1) AND coalesce(length(c.fixed) = 16, 1) \
                 AND coalesce(c.tiny BETWEEN 1 AND 127, 1)"
            )
        )
        .await,
        "1000",
        "{engine:?}"
    );

    // The script of 1,000 more, read again: the key and the unique email
    // start beyond the rows there. It runs as written.
    let plan = guessed_plan(&admin, db, "kerno_test_k72_child", 1000).await;
    let script = plan
        .script(admin.dialect(), "1,000 rows of test data")
        .unwrap();
    for literal in [
        &format!("INSERT INTO `{db}`.`kerno_test_k72_child` (`id`, `email`"),
        "(1001, '",
        "X'",
        "b'",
        "1001@example.",
    ] {
        assert!(script.contains(literal), "{literal} in {}", &script[..2000]);
    }
    assert!(!script.contains("`total`") && !script.contains("`serial_n`"));
    for statement in split_statements(&script, admin.dialect().lex_rules()) {
        let sql = statement.text(&script);
        if let Err(error) = run(&admin, sql).await {
            panic!("{engine:?} {sql}: {error}");
        }
    }
    assert_eq!(
        scalar(
            &admin,
            &format!("SELECT concat(count(DISTINCT email), '/', max(id)) FROM {child}")
        )
        .await,
        "2000/2000"
    );
}

/// K72: 1,000 generated rows of every common type insert in one transaction
/// and keep the table's rules; the script of 1,000 more runs as written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_thousand_rows_of_every_type_insert_and_keep_the_rules() {
    in_scratch(TYPES, every_type).await;
}

const REFUSALS: &str = "kerno_test_k72_refusals";

async fn refusals(admin: Arc<dyn Session>, engine: Engine) {
    let db = REFUSALS;
    for sql in [
        format!("CREATE TABLE {db}.kerno_test_k72_empty (id INT PRIMARY KEY)"),
        format!(
            "CREATE TABLE {db}.kerno_test_k72_orphan (id INT PRIMARY KEY, \
             empty_id INT NOT NULL, \
             FOREIGN KEY (empty_id) REFERENCES {db}.kerno_test_k72_empty (id))"
        ),
        format!(
            "CREATE TABLE {db}.kerno_test_k72_checked (id INT PRIMARY KEY, \
             price DECIMAL(8,2) NOT NULL, \
             CONSTRAINT kerno_test_k72_cheap CHECK (price < 500))"
        ),
    ] {
        exec_ok(&admin, &sql).await;
    }
    let facts = test_data::read(admin.clone(), &TableRef::new(db, "kerno_test_k72_orphan"))
        .await
        .unwrap();
    let settings = Settings {
        rows: 10,
        seed: 1,
        columns: test_data::guess(&facts),
    };
    assert_eq!(
        Plan::new(&facts, &settings).unwrap_err(),
        format!(
            "“{db}.kerno_test_k72_empty” has no rows, and “empty_id” must hold one of its \
             keys: add rows there first."
        ),
        "{engine:?}"
    );
    let plan = guessed_plan(&admin, db, "kerno_test_k72_checked", 1000).await;
    let first_bad = plan
        .values()
        .position(|row| {
            let price = test_data::shown(&row.unwrap()[1]);
            price.parse::<f64>().unwrap() >= 500.0
        })
        .unwrap() as u64
        + 1;
    let report = insert(&admin, plan).await;
    let failure = report.failed.expect("a refused row");
    assert_eq!(report.inserted, 0);
    assert_eq!(failure.row, first_bad, "{engine:?}");
    assert!(
        failure.message.contains("kerno_test_k72_cheap"),
        "{engine:?}: {failure}"
    );
    assert_eq!(admin.tx_state(), TxState::Idle);
    assert_eq!(
        scalar(
            &admin,
            &format!("SELECT count(*) FROM {db}.kerno_test_k72_checked")
        )
        .await,
        "0"
    );
}

/// K72: an empty parent is refused before running; a failing row rolls the
/// whole insert back and is named.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_parent_is_refused_and_a_failing_row_keeps_nothing() {
    in_scratch(REFUSALS, refusals).await;
}
