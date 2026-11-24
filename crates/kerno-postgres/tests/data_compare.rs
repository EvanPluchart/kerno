//! The data compare (K60, D-126) on PostgreSQL: two scratch schemas hold
//! the same tables with known differences in their rows, keyed by text in a
//! locale collation (which orders them otherwise than bytes do) and by an
//! identity the server always makes; the compare finds the differences, its
//! script is run on the target, and a second compare finds none. Runs
//! against the `kerno-postgres` container; skipped with a message when it is
//! down. The fixture schemas are only read.

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use common::{exec_ok, run, scalar};
use kerno_core::Session;
use kerno_core::data_compare::{self, Limits, Outcome, Plan, Request, Shape};
use kerno_core::schema::TableRef;
use kerno_core::sql::split_statements;

const SOURCE: &str = "kerno_test_dc_a";
const TARGET: &str = "kerno_test_dc_b";

/// The same two tables in both schemas: `items` keyed by text in en_US
/// (`a`, `B`, `é`, `Z` in that order there; `B`, `Z`, `a`, `é` in bytes),
/// with a column the server computes; `nodes` keyed by an identity the
/// server always makes, referencing itself.
fn tables(schema: &str) -> Vec<String> {
    vec![
        format!("CREATE SCHEMA {schema}"),
        format!(
            "CREATE TABLE {schema}.items (code text COLLATE \"en_US\" PRIMARY KEY, n integer, \
             price numeric(12,2), at timestamptz, doc jsonb, raw bytea, tags text[], note text, \
             total numeric GENERATED ALWAYS AS (price * 2) STORED)"
        ),
        format!(
            "CREATE TABLE {schema}.nodes (id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
             parent integer REFERENCES {schema}.nodes (id), name text)"
        ),
    ]
}

fn rows(schema: &str) -> Vec<String> {
    let items = format!("INSERT INTO {schema}.items (code, n, price, at, doc, raw, tags, note)");
    let nodes = format!("INSERT INTO {schema}.nodes (id, parent, name) OVERRIDING SYSTEM VALUE");
    if schema == SOURCE {
        vec![
            format!(
                "{items} VALUES \
                 ('a', 1, 1.50, '2024-01-01 10:00:00+00', '{{\"k\": 1}}', '\\x0001', '{{x,y}}', 'n'), \
                 ('B', 2, 2, NULL, NULL, NULL, NULL, NULL), \
                 ('é', 3, 3, NULL, '{{\"a\": 1, \"b\": 2}}', '\\xff', NULL, 'accent'), \
                 ('Z', 4, 4, NULL, NULL, '\\x00', '{{\"with space\",z}}', 'only here'), \
                 ('10', 5, 5, '2024-06-01 12:00:00+02', NULL, NULL, NULL, ''), \
                 ('9', 6, 6, NULL, NULL, NULL, NULL, NULL)"
            ),
            format!("{nodes} VALUES (1, NULL, 'root'), (3, 1, 'mid'), (2, NULL, 'child of 3')"),
            format!("UPDATE {schema}.nodes SET parent = 3 WHERE id = 2"),
        ]
    } else {
        vec![
            format!(
                "{items} VALUES \
                 ('a', 1, 1.5, '2024-01-01 11:00:00+01', '{{\"k\":1}}', '\\x0001', '{{x,y}}', 'n'), \
                 ('B', 20, 2, NULL, NULL, NULL, NULL, NULL), \
                 ('é', 3, 3, NULL, '{{\"b\": 2, \"a\": 1}}', '\\xfe', NULL, 'accent'), \
                 ('_x', 7, 7, NULL, NULL, NULL, NULL, 'only there'), \
                 ('10', 5, 5, '2024-06-01 10:00:00+00', NULL, NULL, NULL, NULL), \
                 ('9', 6, 6.10, NULL, NULL, NULL, NULL, NULL)"
            ),
            format!("{nodes} VALUES (1, NULL, 'root'), (4, 1, 'stray'), (5, 4, 'stray child')"),
        ]
    }
}

/// The plan of `table`, by the source's primary key and every column.
async fn plan(session: &Arc<dyn Session>, table: &str) -> Plan {
    let (source, target) = (TableRef::new(SOURCE, table), TableRef::new(TARGET, table));
    let a = session.table_structure(&source).await.unwrap();
    let b = session.table_structure(&target).await.unwrap();
    let shape = Shape::of(&a, &b).unwrap();
    let request = Request {
        source,
        target,
        key: shape.keys[0].clone(),
        columns: shape
            .shared
            .iter()
            .map(|column| column.name.clone())
            .collect(),
    };
    Plan::new(&request, &a, &b).unwrap()
}

/// Compares `plan` on two sessions of their own.
async fn compared(plan: &Plan) -> Outcome {
    let sessions = [session().await, session().await];
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

async fn session() -> Arc<dyn Session> {
    common::session().await.expect("the server")
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

/// Runs `script` on a new session, as a query tab on the target would.
async fn run_script(script: &str) {
    let runner = session().await;
    for statement in split_statements(script, runner.dialect().lex_rules()) {
        let sql = statement.text(script);
        if let Err(error) = run(&runner, sql).await {
            panic!("{sql}: {error}\nin\n{script}");
        }
    }
}

async fn round_trip() {
    let admin = session().await;
    for schema in [SOURCE, TARGET] {
        for sql in tables(schema).iter().chain(&rows(schema)) {
            exec_ok(&admin, sql).await;
        }
    }

    let items = plan(&admin, "items").await;
    assert_eq!(
        items
            .columns
            .iter()
            .find(|c| c.name == "total")
            .map(|c| c.written()),
        Some(false)
    );
    let found = compared(&items).await;
    // In bytes: '10' < '9' < 'B' < 'Z' < '_x' < 'a' < 'é'.
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
    assert_eq!(found.counts.read, [6, 6]);
    assert_eq!(found.counts.same, 1);
    let dialect = admin.dialect();
    let script = data_compare::script(&items, &found, dialect, true).unwrap();
    assert!(
        script.contains(&format!(
            "UPDATE \"{TARGET}\".\"items\" SET \"price\" = 6.00 WHERE \"code\" = '9';"
        )),
        "{script}"
    );
    assert!(!script.contains("\"total\" ="), "{script}");

    let nodes = plan(&admin, "nodes").await;
    let found_nodes = compared(&nodes).await;
    assert_eq!(summary(&nodes, &found_nodes), ["+ 2", "+ 3", "− 4", "− 5"]);
    let node_script = data_compare::script(&nodes, &found_nodes, dialect, true).unwrap();
    // 2 references 3: 3 first; 5 references 4: 5 deleted first.
    let at = |text: &str| {
        node_script
            .find(text)
            .unwrap_or_else(|| panic!("{text} in {node_script}"))
    };
    assert!(at("VALUES (3, 1, 'mid')") < at("VALUES (2, 3, 'child of 3')"));
    assert!(at("\"id\" = 5;") < at("\"id\" = 4;"));
    assert!(
        node_script.contains("OVERRIDING SYSTEM VALUE"),
        "{node_script}"
    );

    // Nothing ran: the target is as it was.
    assert_eq!(
        scalar(&admin, &format!("SELECT count(*) FROM {TARGET}.items")).await,
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
        assert_eq!(again.counts.differences(), 0);
    }
}

/// K60: the differences of two tables are found whatever the collation of
/// their text key, the script makes the target like the source, and a
/// second compare finds nothing left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_script_makes_the_target_like_the_source() {
    let Some(admin) = common::session().await else {
        return;
    };
    let cleanup: Vec<String> = [SOURCE, TARGET]
        .iter()
        .map(|schema| format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .collect();
    for sql in &cleanup {
        exec_ok(&admin, sql).await;
    }
    let outcome = tokio::spawn(round_trip()).await;
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

/// The big run (`docs/BENCH.md`): `analytics.big_table` (1,000,000 rows,
/// read only) against a scratch copy in `kerno_test_dc_big` with planted
/// differences. Run alone, under `/usr/bin/time -l` for the peak memory:
/// `cargo test -p kerno-postgres --release --test data_compare -- --ignored`.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[ignore = "the big run of docs/BENCH.md: a minute, and 300 MB copied"]
async fn a_million_rows_against_a_million() {
    let Some(admin) = common::session().await else {
        return;
    };
    let copy = "kerno_test_dc_big";
    let cleanup = format!("DROP SCHEMA IF EXISTS {copy} CASCADE");
    exec_ok(&admin, &cleanup).await;
    let outcome = tokio::spawn(async move {
        let admin = session().await;
        for sql in [
            format!("CREATE SCHEMA {copy}"),
            format!("CREATE TABLE {copy}.big_table AS SELECT * FROM analytics.big_table"),
            format!("ALTER TABLE {copy}.big_table ADD PRIMARY KEY (id)"),
            // 1,000 rows only in the source, 1,000 only in the target,
            // 2,000 changed: 4,000 differences.
            format!("DELETE FROM {copy}.big_table WHERE id % 1000 = 7"),
            format!(
                "INSERT INTO {copy}.big_table SELECT id + 2000000, bucket, small, hash, username, \
                 amount, score, ratio, is_even, day, at_time, created, seen_at, note, uid, doc, \
                 arr, ip, mood, padding FROM analytics.big_table WHERE id <= 1000"
            ),
            format!(
                "UPDATE {copy}.big_table SET note = 'changed' \
                 WHERE id % 1000 = 500 AND id <= 1000000"
            ),
            format!(
                "UPDATE {copy}.big_table SET amount = amount + 1 \
                 WHERE id % 1000 = 900 AND id <= 1000000"
            ),
            format!("ANALYZE {copy}.big_table"),
        ] {
            exec_ok(&admin, &sql).await;
        }
        let source = TableRef::new("analytics", "big_table");
        let target = TableRef::new(copy, "big_table");
        let a = admin.table_structure(&source).await.unwrap();
        let b = admin.table_structure(&target).await.unwrap();
        let shape = Shape::of(&a, &b).unwrap();
        let request = Request {
            source,
            target,
            key: shape.keys[0].clone(),
            columns: shape
                .shared
                .iter()
                .map(|column| column.name.clone())
                .collect(),
        };
        let plan = Plan::new(&request, &a, &b).unwrap();
        let started = Instant::now();
        let found = compared(&plan).await;
        let elapsed = started.elapsed();
        let counts = found.counts;
        eprintln!(
            "big run: {:.1} s, read {:?}, only in source {}, only in target {}, different {}, \
             same {}, kept {} (complete: {}), kept bytes {}",
            elapsed.as_secs_f64(),
            counts.read,
            counts.only_in_source,
            counts.only_in_target,
            counts.different,
            counts.same,
            found.rows.len(),
            found.complete,
            found.kept[0].memory_bytes() + found.kept[1].memory_bytes()
        );
        assert_eq!(counts.read, [1_000_000, 1_000_000]);
        assert_eq!(
            (
                counts.only_in_source,
                counts.only_in_target,
                counts.different
            ),
            (1_000, 1_000, 2_000)
        );
        let script = data_compare::script(&plan, &found, admin.dialect(), true).unwrap();
        eprintln!("big run: script of {} bytes", script.len());
    })
    .await;
    if let Err(error) = run(&admin, &cleanup).await {
        eprintln!("cleanup failed: {error}");
    }
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
}
