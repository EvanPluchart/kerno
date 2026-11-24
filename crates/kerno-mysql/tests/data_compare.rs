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

const CI_SOURCE: &str = "kerno_test_dc_ci_a";
const CI_TARGET: &str = "kerno_test_dc_ci_b";

/// The plan of `table` of the two `_ci` databases, by the target's first key.
async fn ci_plan(session: &Arc<dyn Session>, table: &str) -> Result<Plan, String> {
    let (a_table, b_table) = (
        TableRef::new(CI_SOURCE, table),
        TableRef::new(CI_TARGET, table),
    );
    let a = session.table_structure(&a_table).await.unwrap();
    let b = session.table_structure(&b_table).await.unwrap();
    let shape = Shape::of(&a, &b)?;
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
    Plan::new(&request, &a, &b)
}

async fn ci_compared(engine: Engine, plan: &Plan) -> Outcome {
    let sessions = [common::session(engine).await, common::session(engine).await];
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

/// Compares `table`, checks what is found, runs the script (deletes ticked)
/// on the target, compares again: nothing left, and the target holds `rows`.
async fn ci_round(engine: Engine, table: &str, found: &[&str], rows: &str) {
    let session = common::session(engine).await;
    let plan = ci_plan(&session, table).await.unwrap();
    let outcome = ci_compared(engine, &plan).await;
    assert_eq!(summary(&plan, &outcome), found, "{table}");
    let script = data_compare::script(&plan, &outcome, session.dialect(), true).unwrap();
    for statement in split_statements(&script, session.dialect().lex_rules()) {
        let sql = statement.text(&script);
        // Each statement changes one row at most.
        let changed = run(&session, sql)
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}\nin\n{script}"))
            .results
            .iter()
            .filter_map(|result| result.rows_affected)
            .sum::<u64>();
        assert!(changed <= 1, "{sql} changed {changed} rows");
    }
    let again = ci_compared(engine, &plan).await;
    assert_eq!(
        summary(&plan, &again),
        Vec::<String>::new(),
        "{table}\n{script}"
    );
    assert_eq!(
        scalar(
            &session,
            &format!("SELECT COUNT(*) FROM {CI_TARGET}.{table}")
        )
        .await,
        rows,
        "{table}"
    );
}

async fn ci_cases(engine: Engine) {
    let session = common::session(engine).await;
    // MySQL 8's `_0900_ai_ci` has no pad; its `_unicode_ci` pads, as every
    // MariaDB collation does.
    let (ci, pad) = match engine {
        Engine::MySql => ("utf8mb4_0900_ai_ci", "utf8mb4_unicode_ci"),
        _ => ("utf8mb4_general_ci", "utf8mb4_general_ci"),
    };
    for database in [CI_SOURCE, CI_TARGET] {
        for sql in [
            format!("CREATE DATABASE {database} CHARACTER SET utf8mb4"),
            format!(
                "CREATE TABLE {database}.people (id int PRIMARY KEY, \
                 email varchar(40) COLLATE {ci} NOT NULL{})",
                if database == CI_SOURCE { " UNIQUE" } else { "" }
            ),
            format!(
                "CREATE TABLE {database}.codes (code varchar(10) COLLATE {ci} PRIMARY KEY, n int)"
            ),
            format!(
                "CREATE TABLE {database}.padded (code varchar(10) COLLATE {pad} PRIMARY KEY, n int)"
            ),
            format!(
                "CREATE TABLE {database}.times (at datetime({}) PRIMARY KEY, n int)",
                if database == CI_SOURCE { 3 } else { 6 }
            ),
            format!("CREATE TABLE {database}.floats (id int PRIMARY KEY, f float)"),
        ] {
            exec_ok(&session, &sql).await;
        }
    }
    for sql in [
        format!("INSERT INTO {CI_SOURCE}.people VALUES (1, 'bob@x')"),
        format!("INSERT INTO {CI_TARGET}.people VALUES (1, 'Bob@x'), (2, 'bob@x')"),
        format!("INSERT INTO {CI_SOURCE}.codes VALUES ('a', 1), ('b', 2)"),
        format!("INSERT INTO {CI_TARGET}.codes VALUES ('A', 1), ('b', 3), ('c', 4)"),
        format!("INSERT INTO {CI_SOURCE}.padded VALUES ('a', 1)"),
        format!("INSERT INTO {CI_TARGET}.padded VALUES ('a ', 1)"),
        format!(
            "INSERT INTO {CI_SOURCE}.times VALUES ('2024-01-01 10:00:00.500', 1), \
             ('2024-01-01 10:00:01', 2)"
        ),
        format!(
            "INSERT INTO {CI_TARGET}.times VALUES ('2024-01-01 10:00:00.500', 1), \
             ('2024-01-01 10:00:01', 3)"
        ),
        // F3: the text protocol prints all three as 1.23457 and 3.14159.
        format!("INSERT INTO {CI_SOURCE}.floats VALUES (1, 1.2345678), (2, 2.5), (3, 3.1415927)"),
        format!("INSERT INTO {CI_TARGET}.floats VALUES (1, 1.2345679), (2, 2.5)"),
    ] {
        exec_ok(&session, &sql).await;
    }

    // F1: the target holds `email` without a unique key, and two of its
    // rows are equal for its server: that key is not offered, and a plan on
    // it is refused, so no statement can hit both rows. Its primary key is.
    let a = session
        .table_structure(&TableRef::new(CI_SOURCE, "people"))
        .await
        .unwrap();
    let b = session
        .table_structure(&TableRef::new(CI_TARGET, "people"))
        .await
        .unwrap();
    let shape = Shape::of(&a, &b).unwrap();
    assert_eq!(shape.keys, [vec!["id".to_string()]]);
    assert_eq!(shape.source_only_keys, [vec!["email".to_string()]]);
    let request = Request {
        source: TableRef::new(CI_SOURCE, "people"),
        target: TableRef::new(CI_TARGET, "people"),
        key: vec!["email".into()],
        columns: vec!["id".into()],
    };
    assert!(Plan::new(&request, &a, &b).is_err());
    ci_round(engine, "people", &["≠ 1 email", "− 2"], "1").await;

    // F2: `a` against `A` under a `_ci` key is one row, its key respelled.
    ci_round(engine, "codes", &["≠ a", "≠ b n", "− c"], "2").await;
    let session = common::session(engine).await;
    assert_eq!(
        scalar(
            &session,
            &format!("SELECT code FROM {CI_TARGET}.codes WHERE n = 1")
        )
        .await,
        "a"
    );
    // F2: `a` against `a ` with PAD SPACE.
    ci_round(engine, "padded", &["≠ a"], "1").await;
    // F2: a DATETIME(3) key against a DATETIME(6) one.
    ci_round(engine, "times", &["≠ 2024-01-01 10:00:01.000 n"], "2").await;
    // F3: two FLOATs that differ in the eighth digit differ, and the script
    // writes them whole (compared again exactly: nothing left).
    ci_round(engine, "floats", &["≠ 1 f", "+ 3"], "3").await;
    assert_eq!(
        scalar(
            &session,
            &format!("SELECT f + 0e0 FROM {CI_TARGET}.floats WHERE id = 3")
        )
        .await,
        scalar(
            &session,
            &format!("SELECT f + 0e0 FROM {CI_SOURCE}.floats WHERE id = 3")
        )
        .await
    );
}

/// K60 review, F1 and F2: keys the target's server holds equal and whose
/// bytes differ (`_ci`, PAD SPACE, two precisions of a time) are one row,
/// and no statement of the script changes more than one row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keys_the_target_holds_equal_are_one_row() {
    for engine in common::engines() {
        let admin = common::session(engine).await;
        let cleanup =
            [CI_SOURCE, CI_TARGET].map(|database| format!("DROP DATABASE IF EXISTS {database}"));
        for sql in &cleanup {
            exec_ok(&admin, sql).await;
        }
        let outcome = tokio::spawn(ci_cases(engine)).await;
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
