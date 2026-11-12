//! Query plans on PostgreSQL: what `Dialect::explain` asks for comes back
//! as one cell that `Dialect::parse_plan` reads, with and without
//! `ANALYZE`. Runs against the `kerno-postgres` container; skipped with a
//! message when it is down.
//!
//! The outputs of these queries are also kept in `tests/explain/`, where
//! the parser's unit tests pin the tree read from each.

mod common;

use kerno_core::PlanNode;

/// Name, SQL, and the tables the plan must name as targets.
const QUERIES: &[(&str, &str, &[&str])] = &[
    (
        "seq_scan",
        "SELECT * FROM shop.customers WHERE full_name LIKE '%a%'",
        &["customers"],
    ),
    (
        "pk_lookup",
        "SELECT * FROM shop.orders WHERE id = 42",
        &["orders"],
    ),
    (
        "index_scan",
        "SELECT id, total FROM shop.orders WHERE customer_id = 7",
        &["orders"],
    ),
    (
        "join3",
        "SELECT c.full_name, o.id, i.quantity FROM shop.customers c \
         JOIN shop.orders o ON o.customer_id = c.id \
         JOIN shop.order_items i ON i.order_id = o.id WHERE o.status = 'paid'",
        &["customers", "orders", "order_items"],
    ),
    (
        "aggregate_sort",
        "SELECT status, count(*) AS n, sum(total) AS revenue FROM shop.orders \
         GROUP BY status ORDER BY n DESC",
        &["orders"],
    ),
    (
        "subquery",
        "SELECT p.name FROM shop.products p \
         WHERE p.id IN (SELECT product_id FROM shop.order_items WHERE quantity > 2) \
         AND p.price > (SELECT avg(price) FROM shop.products)",
        &["products", "order_items"],
    ),
    (
        "big",
        "SELECT bucket, count(*) AS n FROM analytics.big_table \
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
    let Some(session) = common::session().await else {
        return;
    };
    let dialect = session.dialect();
    for &(name, sql, tables) in QUERIES {
        for analyze in [false, true] {
            let rows = common::rows(&session, &dialect.explain(sql, analyze)).await;
            assert_eq!((rows.len(), rows[0].len()), (1, 1), "{name}: one cell");
            let output = rows[0][0].as_deref().expect("a plan");
            let plan = dialect
                .parse_plan(output, analyze)
                .unwrap_or_else(|error| panic!("{name} (analyze: {analyze}): {error}\n{output}"));
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
                assert!(node.estimated_cost.is_some(), "{what}");
                assert!(node.estimated_rows.is_some(), "{what}");
                // With ANALYZE every node says what it really did.
                assert_eq!(node.actual_rows.is_some(), analyze, "{what}");
                assert_eq!(node.actual_ms.is_some(), analyze, "{what}");
                assert_eq!(node.loops.is_some(), analyze, "{what}");
            }
            assert_eq!(plan.planning_ms.is_some(), analyze, "{what}");
            assert_eq!(plan.execution_ms.is_some(), analyze, "{what}");

            let slowest = plan.slowest().expect("a slowest node");
            assert!(plan.node(&slowest).is_some(), "{slowest:?} in {what}");
            assert_eq!(plan.exclusive_ms(&slowest).is_some(), analyze, "{what}");
        }
    }
}

#[tokio::test]
async fn the_scan_of_the_big_table_is_the_slowest_step() {
    let Some(session) = common::session().await else {
        return;
    };
    let dialect = session.dialect();
    let sql = "SELECT count(*) FROM analytics.big_table WHERE score > 100";
    let output = common::scalar(&session, &dialect.explain(sql, true)).await;
    let plan = dialect.parse_plan(&output, true).unwrap();
    let slowest = plan.node(&plan.slowest().unwrap()).unwrap();
    assert_eq!(slowest.target.as_deref(), Some("big_table"), "{plan}");
    // 929,200 of the million rows pass the filter, whatever the number
    // of processes that shared the scan.
    let rows = slowest.actual_rows.unwrap();
    assert!(
        (rows - 929_200.0).abs() <= 2.0 * slowest.loops.unwrap(),
        "{plan}"
    );
}
