//! Query plans on MySQL and MariaDB: what `Dialect::explain` asks for
//! comes back as one cell that `Dialect::parse_plan` reads, with and
//! without actual timings. Runs against the `kerno-mysql` and
//! `kerno-mariadb` containers; skipped with a message when they are down.
//!
//! The outputs of these queries are also kept in `tests/explain/`, where
//! the parser's unit tests pin the tree read from each.

mod common;

use common::engines;
use kerno_core::PlanNode;

/// Name, SQL, and the tables or aliases the plan must name as targets.
const QUERIES: &[(&str, &str, &[&str])] = &[
    (
        "seq_scan",
        "SELECT * FROM kerno.customers WHERE full_name LIKE '%a%'",
        &["customers"],
    ),
    // A row found by its primary key is read while planning: MySQL's tree
    // then no longer names the table.
    ("pk_lookup", "SELECT * FROM kerno.orders WHERE id = 42", &[]),
    (
        "index_scan",
        "SELECT id, total FROM kerno.orders WHERE customer_id = 7",
        &["orders"],
    ),
    (
        "join3",
        "SELECT c.full_name, o.id, i.quantity FROM kerno.customers c \
         JOIN kerno.orders o ON o.customer_id = c.id \
         JOIN kerno.order_items i ON i.order_id = o.id WHERE o.status = 'paid'",
        &["c", "o", "i"],
    ),
    (
        "aggregate_sort",
        "SELECT status, count(*) AS n, sum(total) AS revenue FROM kerno.orders \
         GROUP BY status ORDER BY n DESC",
        &["orders"],
    ),
    (
        "subquery",
        "SELECT p.name FROM kerno.products p \
         WHERE p.id IN (SELECT product_id FROM kerno.order_items WHERE quantity > 2) \
         AND p.price > (SELECT avg(price) FROM kerno.products)",
        &["p", "order_items", "products"],
    ),
    (
        "big",
        "SELECT bucket, count(*) AS n FROM kerno_analytics.big_table \
         WHERE score > 100 GROUP BY bucket ORDER BY n DESC LIMIT 5",
        &["big_table"],
    ),
];

fn collect<'a>(node: &'a PlanNode, nodes: &mut Vec<&'a PlanNode>) {
    nodes.push(node);
    for child in &node.children {
        collect(child, nodes);
    }
}

#[tokio::test]
async fn plans_are_read_with_and_without_analyze() {
    for engine in engines() {
        let session = common::session(engine).await;
        let dialect = session.dialect();
        for &(name, sql, tables) in QUERIES {
            for analyze in [false, true] {
                let rows = common::rows(&session, &dialect.explain(sql, analyze)).await;
                assert_eq!((rows.len(), rows[0].len()), (1, 1), "{name}: one cell");
                let output = rows[0][0].as_deref().expect("a plan");
                let plan = dialect.parse_plan(output, analyze).unwrap_or_else(|error| {
                    panic!("{name} (analyze: {analyze}): {error}\n{output}")
                });
                let what = format!("{name} (analyze: {analyze}):\n{plan}");

                let mut nodes = Vec::new();
                collect(&plan.root, &mut nodes);
                for table in tables {
                    assert!(
                        nodes
                            .iter()
                            .any(|node| node.target.as_deref() == Some(*table)),
                        "no node on {table} in {what}"
                    );
                }
                for node in &nodes {
                    assert!(!node.operation.is_empty(), "{what}");
                    if !analyze {
                        assert_eq!(
                            (node.actual_rows, node.actual_ms, node.loops),
                            (None, None, None),
                            "{what}"
                        );
                    }
                    // A table is read: it says how many rows it expects
                    // and, with timings, how many it gave. One that never
                    // ran (no loop) has nothing to say.
                    if node.target.is_some() {
                        assert!(node.estimated_rows.is_some(), "{what}");
                        if analyze && node.loops != Some(0.0) {
                            assert!(node.actual_rows.is_some(), "{what}");
                            assert!(node.actual_ms.is_some(), "{what}");
                        }
                    }
                }
                assert_eq!(plan.execution_ms.is_some(), analyze, "{what}");

                // Without timings a plan may have no cost at all (a
                // constant row on MariaDB): then no node stands out.
                let slowest = plan.slowest();
                assert!(slowest.is_some() || !analyze, "{what}");
                if let Some(slowest) = slowest {
                    assert!(plan.node(&slowest).is_some(), "{slowest:?} in {what}");
                    assert_eq!(plan.exclusive_ms(&slowest).is_some(), analyze, "{what}");
                }
            }
        }
    }
}

#[tokio::test]
async fn the_scan_of_the_big_table_is_the_slowest_step() {
    for engine in engines() {
        let session = common::session(engine).await;
        let dialect = session.dialect();
        let sql = "SELECT count(*) FROM kerno_analytics.big_table WHERE score > 100";
        let output = common::scalar(&session, &dialect.explain(sql, true)).await;
        let plan = dialect.parse_plan(&output, true).unwrap();
        let slowest = plan.node(&plan.slowest().unwrap()).unwrap();
        assert_eq!(slowest.target.as_deref(), Some("big_table"), "{plan}");
        assert_eq!(slowest.actual_rows, Some(1_000_000.0), "{plan}");
    }
}
