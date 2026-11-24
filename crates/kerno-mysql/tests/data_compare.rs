//! The data compare (K60, D-126) of a MySQL table with a MariaDB one: two
//! scratch databases, one on each server, hold the same tables with known
//! differences in their rows, keyed by text in a case- and
//! accent-insensitive collation (which orders them otherwise than bytes
//! do); the compare finds the differences, its script is run on the
//! MariaDB target, and a second compare finds none. Skipped with a message
//! when either container is down. The fixture databases are never touched.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use common::{exec_ok, run, scalar};
use kerno_core::data_compare::{self, Limits, Outcome, Plan, Request, Shape};
use kerno_core::schema::TableRef;
use kerno_core::sql::split_statements;
use kerno_core::{Engine, Session, testing};

const SOURCE: &str = "kerno_test_dc_a";
const TARGET: &str = "kerno_test_dc_b";

/// `items` keyed by text in a `_ci` collation (`a`, `B`, `é`, `Z` there;
/// `B`, `Z`, `a`, `é` in bytes), with a column the server computes; JSON a
/// type on MySQL and a text column on MariaDB. `nodes` references itself.
fn tables(database: &str, collation: &str) -> Vec<String> {
    vec![
        format!("CREATE DATABASE {database} CHARACTER SET utf8mb4"),
        format!(
            "CREATE TABLE {database}.items (code varchar(20) COLLATE {collation} PRIMARY KEY, \
             n int, price decimal(12,2), at timestamp NULL, at6 datetime(6), doc json, \
             raw varbinary(16), note text, \
             total decimal(14,2) GENERATED ALWAYS AS (price * 2) STORED)"
        ),
        format!(
            "CREATE TABLE {database}.nodes (id int AUTO_INCREMENT PRIMARY KEY, parent int, \
             name varchar(40), FOREIGN KEY (parent) REFERENCES {database}.nodes (id))"
        ),
    ]
}

fn rows(database: &str) -> Vec<String> {
    let items =
        format!("INSERT INTO {database}.items (code, n, price, at, at6, doc, raw, note) VALUES");
    let nodes = format!("INSERT INTO {database}.nodes (id, parent, name) VALUES");
    if database == SOURCE {
        vec![
            format!(
                "{items} ('a', 1, 1.50, '2024-01-01 10:00:00', '2024-01-01 10:00:00.500000', \
                 '{{\"k\": 1}}', X'0001', 'n'), \
                 ('B', 2, 2, NULL, NULL, NULL, NULL, NULL), \
                 ('é', 3, 3, NULL, NULL, '{{\"a\": 1, \"bb\": [1, 2]}}', X'FF', 'accent'), \
                 ('Z', 4, 4, '2024-02-02 02:02:02', NULL, NULL, X'00', 'only here'), \
                 ('10', 5, 5, NULL, NULL, NULL, NULL, ''), \
                 ('9', 6, 6, NULL, NULL, NULL, NULL, NULL)"
            ),
            format!("{nodes} (1, NULL, 'root'), (3, 1, 'mid'), (2, 3, 'child of 3')"),
        ]
    } else {
        vec![
            format!(
                "{items} ('a', 1, 1.5, '2024-01-01 10:00:00', '2024-01-01 10:00:00.5', \
                 '{{\"k\":1}}', X'0001', 'n'), \
                 ('B', 20, 2, NULL, NULL, NULL, NULL, NULL), \
                 ('é', 3, 3, NULL, NULL, '{{\"a\":1,\"bb\":[1,2]}}', X'FE', 'accent'), \
                 ('_x', 7, 7, NULL, NULL, NULL, NULL, 'only there'), \
                 ('10', 5, 5, NULL, NULL, NULL, NULL, NULL), \
                 ('9', 6, 6.10, NULL, NULL, NULL, NULL, NULL)"
            ),
            format!("{nodes} (1, NULL, 'root'), (4, 1, 'stray'), (5, 4, 'stray child')"),
        ]
    }
}

async fn mysql() -> Arc<dyn Session> {
    common::session(Engine::MySql).await
}

async fn mariadb() -> Arc<dyn Session> {
    common::session(Engine::MariaDb).await
}

/// The plan of `table`, by the source's primary key and every column.
async fn plan(source: &Arc<dyn Session>, target: &Arc<dyn Session>, table: &str) -> Plan {
    let (a_table, b_table) = (TableRef::new(SOURCE, table), TableRef::new(TARGET, table));
    let a = source.table_structure(&a_table).await.unwrap();
    let b = target.table_structure(&b_table).await.unwrap();
    let shape = Shape::of(&a, &b).unwrap();
    let request = Request {
        source: a_table,
        target: b_table,
        key: shape.keys[0].clone(),
        columns: shape
            .shared
            .iter()
            .map(|column| column.name.clone())
            .collect(),
    };
    Plan::new(&request, &a, &b).unwrap()
}

/// Compares `plan`, the source on MySQL, the target on MariaDB.
async fn compared(plan: &Plan) -> Outcome {
    let sessions = [mysql().await, mariadb().await];
    let outcome = data_compare::run(
        sessions.clone(),
        plan,
        Limits::default(),
        |_| {},
        &AtomicBool::new(false),
    )
    .await
    .unwrap_or_else(|error| panic!("{}: {error}", plan.source.name));
    for session in sessions {
        session.close().await;
    }
    outcome
}

/// Each difference as `mark key changed-columns`.
fn summary(plan: &Plan, outcome: &Outcome) -> Vec<String> {
    outcome
        .rows
        .iter()
        .map(|row| {
            let (side, at) = match row.source {
                Some(at) => (0, at),
                None => (1, row.target.expect("a row on one side")),
            };
            let changed: Vec<&str> = row
                .changed
                .iter()
                .map(|&index| plan.columns[index as usize].name.as_str())
                .collect();
            format!(
                "{} {} {}",
                row.difference.mark().0,
                outcome.kept[side].text(at, 0).unwrap(),
                changed.join(",")
            )
            .trim_end()
            .to_string()
        })
        .collect()
}

/// Runs `script` on a new MariaDB session, as a query tab on the target
/// would; the session's time zone is not UTC.
async fn run_script(script: &str) {
    let runner = mariadb().await;
    exec_ok(&runner, "SET time_zone = '+05:00'").await;
    for statement in split_statements(script, runner.dialect().lex_rules()) {
        let sql = statement.text(script);
        if let Err(error) = run(&runner, sql).await {
            panic!("{sql}: {error}\nin\n{script}");
        }
    }
}

async fn round_trip() {
    let (source, target) = (mysql().await, mariadb().await);
    for sql in tables(SOURCE, "utf8mb4_0900_ai_ci")
        .iter()
        .chain(&rows(SOURCE))
    {
        exec_ok(&source, sql).await;
    }
    for sql in tables(TARGET, "utf8mb4_unicode_ci")
        .iter()
        .chain(&rows(TARGET))
    {
        exec_ok(&target, sql).await;
    }

    let items = plan(&source, &target, "items").await;
    let found = compared(&items).await;
    // In bytes: '10' < '9' < 'B' < 'Z' < '_x' < 'a' < 'é'. The JSON of
    // MySQL (a type) and of MariaDB (text as written) are the same
    // documents; .5 and .500000 the same time.
    assert_eq!(
        summary(&items, &found),
        [
            "≠ 10 note",
            "≠ 9 price,total",
            "≠ B n",
            "+ Z",
            "− _x",
            "≠ é raw"
        ]
    );
    assert_eq!(found.counts.same, 1);
    let script = data_compare::script(&items, &found, target.dialect(), true).unwrap();
    // A TIMESTAMP is written in UTC, as it was read.
    assert!(
        script.starts_with("-- The values were read in UTC"),
        "{script}"
    );
    assert!(!script.contains("`total` ="), "{script}");

    let nodes = plan(&source, &target, "nodes").await;
    let found_nodes = compared(&nodes).await;
    assert_eq!(summary(&nodes, &found_nodes), ["+ 2", "+ 3", "− 4", "− 5"]);
    let node_script = data_compare::script(&nodes, &found_nodes, target.dialect(), true).unwrap();
    let at = |text: &str| {
        node_script
            .find(text)
            .unwrap_or_else(|| panic!("{text} in {node_script}"))
    };
    assert!(at("(3, 1, ") < at("(2, 3, "));
    assert!(at("`id` = 5;") < at("`id` = 4;"));

    // Nothing ran: the target is as it was.
    assert_eq!(
        scalar(&target, &format!("SELECT count(*) FROM {TARGET}.items")).await,
        "6"
    );
    run_script(&script).await;
    run_script(&node_script).await;
    for plan in [&items, &nodes] {
        let again = compared(plan).await;
        assert_eq!(
            summary(plan, &again),
            Vec::<String>::new(),
            "{}",
            plan.source.name
        );
    }
}

/// K60: the rows of a MySQL table and a MariaDB one compare by their key
/// whatever the collation, the script makes the MariaDB table like the
/// MySQL one, and a second compare finds nothing left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_script_makes_a_mariadb_table_like_a_mysql_one() {
    if !testing::available(Engine::MySql) || !testing::available(Engine::MariaDb) {
        return;
    }
    let (source, target) = (mysql().await, mariadb().await);
    let cleanup = [
        (source.clone(), format!("DROP DATABASE IF EXISTS {SOURCE}")),
        (target.clone(), format!("DROP DATABASE IF EXISTS {TARGET}")),
    ];
    for (session, sql) in &cleanup {
        exec_ok(session, sql).await;
    }
    let outcome = tokio::spawn(round_trip()).await;
    let mut failed = Vec::new();
    for (session, sql) in &cleanup {
        if let Err(error) = run(session, sql).await {
            failed.push(format!("{sql}: {error}"));
        }
    }
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
    assert!(failed.is_empty(), "cleanup failed: {failed:#?}");
}
