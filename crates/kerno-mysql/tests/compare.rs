//! The schema compare (K30; NAV-57, CM-21, CM-59, TOOL-22) on MySQL and
//! MariaDB: two scratch databases with known differences are compared, the
//! script is run on the target, and a second compare finds them identical.
//! Skipped with a message when the containers are down. The fixture
//! databases are never touched.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use common::{engines, exec_ok, run};
use kerno_core::compare::{self, Item, State};
use kerno_core::sql::split_statements;
use kerno_core::{Engine, Session};

const SOURCE: &str = "kerno_test_cmp_a";
const TARGET: &str = "kerno_test_cmp_b";

const SETUP: &[&str] = &[
    "CREATE DATABASE kerno_test_cmp_a CHARACTER SET utf8mb4",
    "CREATE TABLE kerno_test_cmp_a.customers (id int AUTO_INCREMENT PRIMARY KEY, \
     email varchar(190) NOT NULL, UNIQUE KEY customers_email (email), \
     CONSTRAINT customers_email_check CHECK (email <> ''))",
    "CREATE TABLE kerno_test_cmp_a.orders (id int AUTO_INCREMENT PRIMARY KEY, \
     customer_id int NOT NULL, price decimal(12,2) NOT NULL DEFAULT 0, note text, \
     KEY orders_customer_idx (customer_id), \
     CONSTRAINT orders_price_check CHECK (price >= 0), \
     CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES kerno_test_cmp_a.customers (id))",
    "CREATE TABLE kerno_test_cmp_a.lines (id int PRIMARY KEY, order_id int NOT NULL, body text, \
     FULLTEXT KEY lines_body (body), \
     CONSTRAINT lines_order_fk FOREIGN KEY (order_id) REFERENCES kerno_test_cmp_a.orders (id))",
    "CREATE VIEW kerno_test_cmp_a.big_orders AS \
     SELECT id, customer_id FROM kerno_test_cmp_a.orders WHERE customer_id > 100",
    "CREATE PROCEDURE kerno_test_cmp_a.count_orders() SELECT COUNT(*) FROM orders",
    "CREATE TRIGGER kerno_test_cmp_a.orders_touch BEFORE INSERT ON kerno_test_cmp_a.orders \
     FOR EACH ROW SET NEW.note = COALESCE(NEW.note, 'new')",
    "CREATE DATABASE kerno_test_cmp_b CHARACTER SET utf8mb4",
    "CREATE TABLE kerno_test_cmp_b.customers (id int AUTO_INCREMENT PRIMARY KEY, email varchar(100))",
    "CREATE TABLE kerno_test_cmp_b.orders (id int AUTO_INCREMENT PRIMARY KEY, \
     price decimal(10,2) NOT NULL DEFAULT 0, customer_id int NOT NULL, legacy text, \
     CONSTRAINT orders_price_check CHECK (price > 0))",
    "CREATE VIEW kerno_test_cmp_b.big_orders AS \
     SELECT id, customer_id FROM kerno_test_cmp_b.orders WHERE customer_id > 50",
    "CREATE TABLE kerno_test_cmp_b.old_stuff (id int PRIMARY KEY)",
    "CREATE PROCEDURE kerno_test_cmp_b.old_proc() SELECT 1",
];

async fn read(session: &Arc<dyn Session>, database: &str) -> compare::Side {
    compare::load(session.clone(), database, |_| {}, &AtomicBool::new(false))
        .await
        .unwrap_or_else(|error| panic!("{database}: {error}"))
}

async fn compared(session: &Arc<dyn Session>) -> Vec<Item> {
    let (a, b) = (read(session, SOURCE).await, read(session, TARGET).await);
    compare::compare(&a, &b, session.dialect(), session.dialect())
}

fn states(items: &[Item]) -> Vec<(String, State)> {
    items
        .iter()
        .map(|item| (item.name.clone(), item.state))
        .collect()
}

async fn round_trip(engine: Engine) {
    let session = common::session(engine).await;
    for sql in SETUP {
        exec_ok(&session, sql).await;
    }
    let items = compared(&session).await;
    use State::*;
    assert_eq!(
        states(&items),
        [
            ("customers".into(), Different),
            ("lines".into(), OnlyInSource),
            ("old_stuff".into(), OnlyInTarget),
            ("orders".into(), Different),
            ("big_orders".into(), Different),
            ("count_orders".into(), OnlyInSource),
            ("old_proc".into(), OnlyInTarget),
            ("orders_touch on orders".into(), OnlyInSource),
        ]
    );
    let orders = items.iter().find(|item| item.name == "orders").unwrap();
    for difference in [
        "column `legacy` is only in the target",
        "column `note` is missing in the target",
        "column `price`: decimal(10,2) → decimal(12,2)",
        "index `orders_customer_idx` is missing in the target",
        "foreign key `orders_customer_fk` is missing in the target",
    ] {
        assert!(
            orders.differences.iter().any(|d| d == difference),
            "{difference} in {:#?}",
            orders.differences
        );
    }
    assert!(
        orders
            .differences
            .iter()
            .any(|d| d.starts_with("column `customer_id` moves")
                || d.starts_with("column `price` moves")),
        "{:#?}",
        orders.differences
    );

    let script = compare::script(&items, |_| true, true);
    assert!(!script.contains(SOURCE), "{script}");
    // As the query tab the script opens in is in the target database.
    let runner = common::session(engine).await;
    exec_ok(&runner, &format!("USE {TARGET}")).await;
    for statement in split_statements(&script, runner.dialect().lex_rules()) {
        let sql = statement.text(&script);
        if let Err(error) = run(&runner, sql).await {
            panic!("{sql}: {error}\nin\n{script}");
        }
    }

    let items = compared(&session).await;
    let left: Vec<_> = items
        .iter()
        .filter(|item| item.state != State::Same)
        .map(|item| (item.name.clone(), item.differences.clone()))
        .collect();
    assert!(left.is_empty(), "{left:#?}\n{script}");
}

/// K30: the differences are found, the script brings the target to the
/// source, and a second compare finds nothing left (drops asked for).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_script_brings_the_target_to_the_source() {
    for engine in engines() {
        let admin = common::session(engine).await;
        let cleanup: Vec<String> = [SOURCE, TARGET]
            .iter()
            .map(|database| format!("DROP DATABASE IF EXISTS {database}"))
            .collect();
        for sql in &cleanup {
            exec_ok(&admin, sql).await;
        }
        let outcome = tokio::spawn(round_trip(engine)).await;
        let mut failed = Vec::new();
        for sql in &cleanup {
            if let Err(error) = run(&admin, sql).await {
                failed.push(format!("{sql}: {error}"));
            }
        }
        if let Err(error) = outcome {
            std::panic::resume_unwind(error.into_panic());
        }
        assert!(failed.is_empty(), "cleanup failed: {failed:#?}");
    }
}
