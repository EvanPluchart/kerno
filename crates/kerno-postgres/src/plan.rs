//! Reading the output of `EXPLAIN (FORMAT JSON)`: an array holding one
//! object, with the tree under `Plan` and, after `ANALYZE`, the planning
//! and execution times beside it.

use kerno_core::plan::{Json, Plan, PlanNode, detail_text, parse_json, sort_details};
use kerno_core::{Error, Result};

/// The keys that say what a node works on, the most telling first.
const TARGETS: [&str; 4] = ["Relation Name", "Index Name", "CTE Name", "Function Name"];

/// The keys read into the fields of a node.
const FIELDS: [&str; 7] = [
    "Node Type",
    "Total Cost",
    "Plan Rows",
    "Actual Rows",
    "Actual Total Time",
    "Actual Loops",
    "Plans",
];

/// The details shown first: what the node is, then its conditions.
const FIRST: [&str; 14] = [
    "Subplan Name",
    "Join Type",
    "Strategy",
    "Index Name",
    "Alias",
    "Index Cond",
    "Recheck Cond",
    "Hash Cond",
    "Merge Cond",
    "Join Filter",
    "Filter",
    "Rows Removed by Filter",
    "Sort Key",
    "Group Key",
];

fn shape(what: &str) -> Error {
    Error::Protocol(format!("EXPLAIN printed {what}"))
}

/// Reads the single cell `EXPLAIN (FORMAT JSON)` returns.
pub(crate) fn parse(output: &str) -> Result<Plan> {
    let json = parse_json(output)?;
    let statement = json.get(0).unwrap_or(&json);
    let root = statement.get("Plan").filter(|plan| plan.is_object());
    Ok(Plan {
        root: node(root.ok_or_else(|| shape("no \"Plan\" object"))?)?,
        planning_ms: statement.get("Planning Time").and_then(Json::as_f64),
        execution_ms: statement.get("Execution Time").and_then(Json::as_f64),
    })
}

fn node(json: &Json) -> Result<PlanNode> {
    let text = |key: &str| json.get(key).and_then(Json::as_str);
    let number = |key: &str| json.get(key).and_then(Json::as_f64);
    let operation = text("Node Type").ok_or_else(|| shape("a plan node without \"Node Type\""))?;
    let target = TARGETS.iter().find(|key| text(key).is_some());
    let loops = number("Actual Loops");
    // PostgreSQL prints the rows and the time of one loop, on average.
    let total = |key: &str| Some(number(key)? * loops.unwrap_or(1.0));

    let mut details = Vec::new();
    for (key, value) in json.as_object().into_iter().flatten() {
        let said_already = FIELDS.contains(&key.as_str())
            || target.is_some_and(|target| target == key)
            || key == "Alias" && text("Alias") == text("Relation Name");
        // ANALYZE with BUFFERS prints ten counters per node, mostly zero.
        let empty_counter = key.ends_with(" Blocks") && value.as_f64() == Some(0.0);
        if !said_already
            && !empty_counter
            && let Some(text) = detail_text(value)
        {
            details.push((key.clone(), text));
        }
    }
    sort_details(&mut details, &FIRST);

    let children = json.get("Plans").and_then(Json::as_array);
    Ok(PlanNode {
        operation: operation.to_string(),
        target: target.and_then(|key| text(key)).map(str::to_string),
        estimated_cost: number("Total Cost"),
        estimated_rows: number("Plan Rows"),
        actual_rows: total("Actual Rows"),
        actual_ms: total("Actual Total Time"),
        loops,
        details,
        children: children
            .into_iter()
            .flatten()
            .map(node)
            .collect::<Result<_>>()?,
    })
}

#[cfg(test)]
mod tests {
    use kerno_core::{Dialect, Error, Plan};

    use crate::PgDialect;

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/explain/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    fn parsed(name: &str) -> Plan {
        PgDialect
            .parse_plan(
                &fixture(&format!("{name}.json")),
                name.ends_with("_analyze"),
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"))
    }

    fn detail<'a>(plan: &'a Plan, path: &[usize], label: &str) -> Option<&'a str> {
        let node = plan.node(path).expect("a node at this path");
        node.details
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, text)| text.as_str())
    }

    #[test]
    fn a_scan_without_analyze_has_estimates_only() {
        let plan = parsed("seq_scan");
        let root = &plan.root;
        assert_eq!(root.operation, "Seq Scan");
        assert_eq!(root.target.as_deref(), Some("customers"));
        assert_eq!(root.estimated_cost, Some(22.25));
        assert_eq!(root.estimated_rows, Some(400.0));
        assert_eq!(
            (root.actual_rows, root.actual_ms, root.loops),
            (None, None, None)
        );
        assert!(root.children.is_empty());
        assert_eq!((plan.planning_ms, plan.execution_ms), (None, None));
        // What says nothing is left out: false flags, the consumed keys,
        // an alias that is the relation's name.
        assert_eq!(
            root.details,
            [
                ("Filter", "(full_name ~~ '%a%'::text)"),
                ("Plan Width", "224"),
                ("Startup Cost", "0.0"),
            ]
            .map(|(label, text)| (label.to_string(), text.to_string()))
        );
    }

    #[test]
    fn analyze_adds_actual_numbers_and_times() {
        let plan = parsed("seq_scan_analyze");
        let root = &plan.root;
        assert_eq!(root.actual_rows, Some(400.0));
        assert_eq!(root.actual_ms, Some(0.117));
        assert_eq!(root.loops, Some(1.0));
        assert_eq!(plan.planning_ms, Some(0.024));
        assert_eq!(plan.execution_ms, Some(0.128));
        assert_eq!(detail(&plan, &[], "Rows Removed by Filter"), Some("100"));
        // Buffer counters that are zero are left out, the others kept.
        assert!(detail(&plan, &[], "Local Hit Blocks").is_none());
        assert!(
            root.details
                .iter()
                .any(|(label, _)| label.starts_with("Shared ") && label.ends_with(" Blocks"))
        );
    }

    #[test]
    fn an_index_scan_names_its_relation_and_keeps_the_index() {
        let plan = parsed("pk_lookup");
        assert_eq!(plan.root.operation, "Index Scan");
        assert_eq!(plan.root.target.as_deref(), Some("orders"));
        assert_eq!(detail(&plan, &[], "Index Name"), Some("orders_pkey"));
        assert_eq!(detail(&plan, &[], "Index Cond"), Some("(id = 42)"));
        // Conditions come first.
        assert_eq!(plan.root.details[0].0, "Index Name");
        assert_eq!(plan.root.details[1].0, "Index Cond");

        // A bitmap index scan has no relation: its target is the index.
        let plan = parsed("index_scan");
        assert_eq!(plan.root.operation, "Bitmap Heap Scan");
        assert_eq!(plan.root.target.as_deref(), Some("orders"));
        let index = plan.node(&[0]).unwrap();
        assert_eq!(index.operation, "Bitmap Index Scan");
        assert_eq!(index.target.as_deref(), Some("orders_customer_idx"));
        assert_eq!(detail(&plan, &[0], "Index Cond"), Some("(customer_id = 7)"));
    }

    #[test]
    fn a_join_of_three_tables_is_a_tree() {
        let plan = parsed("join3_analyze");
        let targets = |path: &[usize]| plan.node(path).unwrap().target.clone();
        assert_eq!(plan.root.operation, "Hash Join");
        assert_eq!(
            detail(&plan, &[], "Hash Cond"),
            Some("(o.customer_id = c.id)")
        );
        assert_eq!(detail(&plan, &[], "Join Type"), Some("Inner"));
        assert_eq!(targets(&[0, 0]).as_deref(), Some("order_items"));
        assert_eq!(detail(&plan, &[0, 0], "Alias"), Some("i"));
        assert_eq!(targets(&[0, 1, 0]).as_deref(), Some("orders"));
        assert_eq!(
            targets(&[0, 1, 0, 0]).as_deref(),
            Some("orders_status_placed_idx")
        );
        assert_eq!(targets(&[1, 0]).as_deref(), Some("customers"));
        assert_eq!(plan.node(&[0, 0]).unwrap().actual_rows, Some(12500.0));
        // The scan of order_items: 3.378 ms of its own.
        assert_eq!(plan.exclusive_ms(&[0, 0]), Some(3.378));
        assert_eq!(plan.slowest(), Some(vec![0, 0]));
    }

    #[test]
    fn lists_are_joined() {
        let plan = parsed("aggregate_sort");
        assert_eq!(detail(&plan, &[], "Sort Key"), Some("(count(*)) DESC"));
        assert_eq!(detail(&plan, &[0], "Group Key"), Some("status"));
        assert_eq!(detail(&plan, &[0], "Strategy"), Some("Hashed"));
    }

    #[test]
    fn rows_and_times_are_totals_over_the_loops() {
        // The inner side of the nested loop ran 100 times: PostgreSQL
        // prints one loop's rows and time.
        let plan = parsed("subquery_analyze");
        let inner = plan.node(&[2]).unwrap();
        assert_eq!(inner.operation, "Index Scan");
        assert_eq!(inner.target.as_deref(), Some("order_items"));
        assert_eq!(inner.loops, Some(100.0));
        assert_eq!(inner.actual_rows, Some(100.0));
        assert!((inner.actual_ms.unwrap() - 0.3).abs() < 1e-9);
        // The subquery is a child like the others.
        assert_eq!(detail(&plan, &[0], "Subplan Name"), Some("InitPlan 1"));
        assert_eq!(
            plan.node(&[0, 0]).unwrap().target.as_deref(),
            Some("products")
        );

        // Three processes ran the parallel scan of the big table.
        let plan = parsed("big_analyze");
        let scan = plan.node(&[0, 0, 0, 0, 0, 0]).unwrap();
        assert_eq!(scan.target.as_deref(), Some("big_table"));
        assert_eq!(scan.loops, Some(3.0));
        assert_eq!(scan.actual_rows, Some(929_199.0));
        assert_eq!(
            detail(&plan, &[0, 0, 0, 0, 0, 0], "Parallel Aware"),
            Some("true")
        );
        assert_eq!(plan.slowest(), Some(vec![0, 0, 0, 0, 0, 0]));
    }

    /// Every captured plan, printed as an outline, is the reviewed
    /// `<name>.plan` beside it.
    #[test]
    fn every_captured_plan_reads_as_pinned() {
        for query in [
            "seq_scan",
            "pk_lookup",
            "index_scan",
            "join3",
            "aggregate_sort",
            "subquery",
            "big",
        ] {
            for name in [query.to_string(), format!("{query}_analyze")] {
                assert_eq!(
                    parsed(&name).to_string(),
                    fixture(&format!("{name}.plan")),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn what_is_not_a_plan_is_an_error() {
        for (output, message) in [
            ("-> Table scan", "not valid JSON"),
            ("[]", "no \"Plan\""),
            ("[{\"Plan\": 3}]", "no \"Plan\""),
            ("{\"query_block\": {}}", "no \"Plan\""),
            ("[{\"Plan\": {\"Plans\": []}}]", "\"Node Type\""),
            (
                "[{\"Plan\": {\"Node Type\": \"Sort\", \"Plans\": [3]}}]",
                "\"Node Type\"",
            ),
        ] {
            let error = PgDialect.parse_plan(output, false).unwrap_err();
            assert!(matches!(error, Error::Protocol(_)), "{output}: {error:?}");
            assert!(error.to_string().contains(message), "{output}: {error}");
        }
        // A single object, without the array, is read too; unknown keys
        // become details.
        let plan = PgDialect
            .parse_plan(
                "{\"Plan\": {\"Node Type\": \"Result\", \"New Key\": 1}}",
                false,
            )
            .unwrap();
        assert_eq!(plan.root.operation, "Result");
        assert_eq!(
            plan.root.details,
            [("New Key".to_string(), "1".to_string())]
        );
    }
}
