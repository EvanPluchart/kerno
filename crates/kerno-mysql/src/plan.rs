//! Reading what `EXPLAIN` prints on MySQL and MariaDB.
//!
//! `EXPLAIN FORMAT=JSON` nests objects named after what they do
//! (`query_block`, `nested_loop`, `table`, `ordering_operation` on MySQL,
//! `filesort` on MariaDB ...): each becomes a node. MariaDB's
//! `ANALYZE FORMAT=JSON` adds what really happened in `r_*` keys. MySQL's
//! `EXPLAIN ANALYZE` prints an indented tree of text instead, one
//! `-> step  (cost=... rows=...) (actual time=... rows=... loops=...)` per
//! line.

use kerno_core::plan::{Json, Plan, PlanNode, detail_text, parse_json, sort_details};
use kerno_core::{Error, Result};

/// The details shown first: how the table is read, then the conditions.
const FIRST: [&str; 7] = [
    "Access type",
    "Key",
    "Used key parts",
    "Ref",
    "Index condition",
    "Attached condition",
    "Sort key",
];

fn shape(what: impl std::fmt::Display) -> Error {
    Error::Protocol(format!("EXPLAIN printed {what}"))
}

/// Reads the single cell `EXPLAIN FORMAT=JSON` returns on MySQL and
/// MariaDB, or `ANALYZE FORMAT=JSON` on MariaDB.
pub(crate) fn parse_json_plan(output: &str) -> Result<Plan> {
    let json = parse_json(output)?;
    let block = json.get("query_block").filter(|block| block.is_object());
    let root = node(
        "query_block",
        block.ok_or_else(|| shape("no \"query_block\" object"))?,
    );
    Ok(Plan {
        planning_ms: number(&json["query_optimization"]["r_total_time_ms"]),
        execution_ms: root.actual_ms,
        root,
    })
}

/// A JSON number, or a number in a string: MySQL quotes its costs.
fn number(value: &Json) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}

/// `attached_condition` as a label: `Attached condition`.
fn label(key: &str) -> String {
    let mut label = key.replace('_', " ");
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label
}

/// What MySQL's own tree format calls each access type of the JSON format.
fn access_name(access: &str) -> &str {
    match access {
        "ALL" => "Table scan",
        "index" => "Index scan",
        "range" => "Index range scan",
        "ref" | "ref_or_null" => "Index lookup",
        "eq_ref" => "Single-row index lookup",
        "const" | "system" => "Constant row",
        "fulltext" => "Full-text index search",
        other => other,
    }
}

fn has_objects(value: &Json) -> bool {
    value.is_object()
        || value
            .as_array()
            .is_some_and(|items| items.iter().any(Json::is_object))
}

/// The node an object of the plan describes. `key` is the name the object
/// has in its parent, which says what it does. Objects and lists of
/// objects inside it are its children; everything else is a detail.
fn node(key: &str, json: &Json) -> PlanNode {
    let mut node = PlanNode {
        operation: label(key),
        ..PlanNode::default()
    };
    // MariaDB: rows of one loop, and the node's own time.
    let mut rows_per_loop = None;
    let mut own_ms: Option<f64> = None;
    // serde_json keeps the keys of an object sorted or as written,
    // depending on a feature other crates may switch on: give the children
    // one order, subqueries after what reads from them.
    let mut fields: Vec<(&String, &Json)> = json.as_object().into_iter().flatten().collect();
    fields.sort_by_key(|&(name, _)| (name.ends_with("subqueries"), name.as_str()));
    for (name, value) in fields {
        match name.as_str() {
            "table_name" => node.target = value.as_str().map(str::to_string),
            "access_type" => {
                if let Some(access) = value.as_str() {
                    node.operation = access_name(access).to_string();
                    node.details.push((label(name), access.to_string()));
                }
            }
            // MySQL: the cost of the block, of the join up to this table,
            // or of a sort.
            "cost_info" => {
                for (cost, value) in value.as_object().into_iter().flatten() {
                    if matches!(cost.as_str(), "query_cost" | "prefix_cost" | "sort_cost") {
                        node.estimated_cost = number(value);
                    }
                    if cost != "query_cost"
                        && let Some(text) = detail_text(value)
                    {
                        node.details.push((label(cost), text));
                    }
                }
            }
            "cost" => node.estimated_cost = number(value),
            "rows_examined_per_scan" | "rows" => node.estimated_rows = number(value),
            "r_loops" => node.loops = number(value),
            "r_rows" | "r_output_rows" => rows_per_loop = number(value),
            "r_total_time_ms" | "r_table_time_ms" | "r_other_time_ms" => {
                if let Some(ms) = number(value) {
                    own_ms = Some(own_ms.unwrap_or(0.0) + ms);
                }
            }
            "nested_loop" if has_objects(value) => node.children.push(nested_loop(value)),
            _ => match value {
                Json::Array(items) if has_objects(value) => {
                    node.children
                        .extend(items.iter().map(|item| element(name, item)));
                }
                Json::Object(fields)
                    if matches!(name.as_str(), "table" | "query_block")
                        || fields.values().any(has_objects) =>
                {
                    node.children.push(self::node(name, value));
                }
                _ => {
                    if let Some(text) = detail_text(value) {
                        node.details.push((label(name), text));
                    }
                }
            },
        }
    }
    sort_details(&mut node.details, &FIRST);
    node.actual_rows = rows_per_loop.map(|rows| rows * node.loops.unwrap_or(1.0));
    node.actual_ms = own_ms;
    // Both engines give a table, a sort and the like their own cost and
    // time, without what hangs under them (a subquery, a materialization,
    // the tables a sort reads). Only a query block is given as a whole.
    if key != "query_block" {
        let children = |inclusive: fn(&PlanNode) -> Option<f64>| -> f64 {
            node.children.iter().filter_map(inclusive).sum()
        };
        let (cost, ms) = (
            children(PlanNode::inclusive_cost),
            children(PlanNode::inclusive_ms),
        );
        node.estimated_cost = node.estimated_cost.map(|own| own + cost);
        node.actual_ms = node.actual_ms.map(|own| own + ms);
    }
    node
}

/// One item of a list of objects. `{"table": {...}}` and
/// `{"query_block": {...}}` are what they hold.
fn element(key: &str, item: &Json) -> PlanNode {
    let mut fields = item.as_object().into_iter().flatten();
    match (fields.next(), fields.next()) {
        (Some((name, value)), None) if value.is_object() => node(name, value),
        _ => node(key, item),
    }
}

/// The tables of a join, in the order they are read.
fn nested_loop(tables: &Json) -> PlanNode {
    let tables = tables.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut children: Vec<PlanNode> = tables
        .iter()
        .map(|table| element("nested_loop", table))
        .collect();
    // MySQL prints with each table the cost of the join so far: what the
    // table adds is the difference with the one before.
    let mut so_far = 0.0;
    for (child, table) in children.iter_mut().zip(tables) {
        if let Some(prefix) = number(&table["table"]["cost_info"]["prefix_cost"]) {
            child.estimated_cost = child.estimated_cost.map(|cost| (cost - so_far).max(0.0));
            so_far = prefix;
        }
    }
    // MariaDB wraps even a single table in a loop.
    if children.len() == 1 {
        return children.remove(0);
    }
    PlanNode {
        operation: label("nested_loop"),
        children,
        ..PlanNode::default()
    }
}

/// Reads the tree `EXPLAIN ANALYZE` prints on MySQL: one step per line,
/// children indented under their parent.
pub(crate) fn parse_tree(output: &str) -> Result<Plan> {
    let mut steps = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let step = line.trim_start();
        let text = step
            .strip_prefix("->")
            .ok_or_else(|| shape(format!("a line that does not start with \"->\": {line}")))?;
        steps.push((line.len() - step.len(), tree_node(text.trim())));
    }
    let mut steps = steps.into_iter().peekable();
    let (indent, mut root) = steps.next().ok_or_else(|| shape("no step"))?;
    adopt(&mut root, indent, &mut steps);
    if steps.next().is_some() {
        return Err(shape("more than one root step"));
    }
    Ok(Plan {
        planning_ms: None,
        execution_ms: root.actual_ms,
        root,
    })
}

/// Moves under `parent` the steps that follow it with a deeper indent.
fn adopt(
    parent: &mut PlanNode,
    indent: usize,
    steps: &mut std::iter::Peekable<impl Iterator<Item = (usize, PlanNode)>>,
) {
    while let Some((child_indent, mut child)) = steps.next_if(|(deeper, _)| *deeper > indent) {
        adopt(&mut child, child_indent, steps);
        parent.children.push(child);
    }
}

/// The value of `name=` in `(cost=0..0 rows=1)` or
/// `(actual time=0.02..0.03 rows=2.5 loops=715)`. Of a range, the end:
/// the total, not what it took to get the first row.
fn field(annotation: &str, name: &str) -> Option<f64> {
    let value = annotation.split_whitespace().find_map(|part| {
        part.trim_start_matches('(')
            .strip_prefix(name)?
            .strip_prefix('=')
    })?;
    let value = value.trim_end_matches(')');
    value.rsplit("..").next()?.parse().ok()
}

/// One line of the tree, without its arrow.
fn tree_node(line: &str) -> PlanNode {
    let mut node = PlanNode::default();
    let mut description = line;
    // What really happened: one loop's time and rows, on average.
    if let Some(at) = description.rfind("(actual time=") {
        let actual = &description[at..];
        node.loops = field(actual, "loops");
        let total = |name: &str| Some(field(actual, name)? * node.loops.unwrap_or(1.0));
        node.actual_ms = total("time");
        node.actual_rows = total("rows");
        description = &description[..at];
    } else if let Some(at) = description.rfind("(never executed)") {
        (node.loops, node.actual_ms, node.actual_rows) = (Some(0.0), Some(0.0), Some(0.0));
        description = &description[..at];
    }
    if let Some(at) = description.rfind("(cost=") {
        node.estimated_cost = field(&description[at..], "cost");
        node.estimated_rows = field(&description[at..], "rows");
        description = &description[..at];
    }
    let description = description.trim();

    // `Index lookup on o using orders_idx (status='paid')`,
    // `Filter: (o.total > 10)`, `Inner hash join (i.order_id = o.id)`,
    // or a plain `Nested loop inner join`. What follows the name of the
    // step is kept as a detail under that name.
    let colon = description.find(": ");
    let on = description
        .find(" on ")
        .filter(|on| colon.is_none_or(|colon| *on < colon));
    let (operation, rest) = if let Some(on) = on {
        let rest = &description[on + 4..];
        let (target, rest) = rest.split_once(' ').unwrap_or((rest, ""));
        node.target = Some(target.to_string());
        let rest = match rest.strip_prefix("using ") {
            Some(rest) => {
                let (index, rest) = rest.split_once(' ').unwrap_or((rest, ""));
                node.details
                    .push(("Index".to_string(), index.trim_end_matches(',').to_string()));
                rest
            }
            None => rest,
        };
        (&description[..on], rest.to_string())
    } else if let Some(colon) = colon {
        (&description[..colon], description[colon + 2..].to_string())
    } else if let Some((operation, rest)) = description.split_once(" (")
        && description.ends_with(')')
    {
        (operation, format!("({rest}"))
    } else {
        (description, String::new())
    };
    node.operation = operation.to_string();
    if !rest.is_empty() {
        node.details.push((node.operation.clone(), rest));
    }
    node
}

#[cfg(test)]
mod tests {
    use kerno_core::{Dialect, Engine, Error, Plan};

    use crate::MysqlDialect;

    const QUERIES: [&str; 7] = [
        "seq_scan",
        "pk_lookup",
        "index_scan",
        "join3",
        "aggregate_sort",
        "subquery",
        "big",
    ];

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/explain/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    /// The captured plan `name` (`mysql_join3`, `mariadb_join3_analyze` ...).
    fn parsed(name: &str) -> Plan {
        let analyze = name.ends_with("_analyze");
        let (engine, extension) = match (name.starts_with("mariadb"), analyze) {
            (true, _) => (Engine::MariaDb, "json"),
            (false, false) => (Engine::MySql, "json"),
            (false, true) => (Engine::MySql, "txt"),
        };
        MysqlDialect::new(engine)
            .parse_plan(&fixture(&format!("{name}.{extension}")), analyze)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
    }

    fn detail<'a>(plan: &'a Plan, path: &[usize], label: &str) -> Option<&'a str> {
        let node = plan.node(path).expect("a node at this path");
        node.details
            .iter()
            .find(|(name, _)| name == label)
            .map(|(_, text)| text.as_str())
    }

    fn close(actual: Option<f64>, expected: f64) -> bool {
        actual.is_some_and(|actual| (actual - expected).abs() < 1e-9)
    }

    #[test]
    fn mysql_json_a_table_under_its_query_block() {
        let plan = parsed("mysql_seq_scan");
        assert_eq!(plan.root.operation, "Query block");
        assert_eq!(plan.root.estimated_cost, Some(57.0));
        assert_eq!(detail(&plan, &[], "Select id"), Some("1"));
        assert_eq!(plan.root.children.len(), 1);

        let table = plan.node(&[0]).unwrap();
        assert_eq!(table.operation, "Table scan");
        assert_eq!(table.target.as_deref(), Some("customers"));
        assert_eq!(table.estimated_cost, Some(57.0));
        assert_eq!(table.estimated_rows, Some(500.0));
        assert_eq!(
            (table.actual_rows, table.actual_ms, table.loops),
            (None, None, None)
        );
        assert!(table.children.is_empty());
        // How the table is read comes first, then the rest by name.
        let labels: Vec<&str> = table
            .details
            .iter()
            .map(|(label, _)| label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Access type",
                "Attached condition",
                "Data read per join",
                "Eval cost",
                "Filtered",
                "Prefix cost",
                "Read cost",
                "Rows produced per join",
                "Used columns",
            ]
        );
        assert_eq!(detail(&plan, &[0], "Access type"), Some("ALL"));
        assert_eq!(
            detail(&plan, &[0], "Attached condition"),
            Some("(`kerno`.`customers`.`full_name` like '%a%')")
        );
        assert_eq!(detail(&plan, &[0], "Filtered"), Some("11.11"));
        assert!(
            detail(&plan, &[0], "Used columns")
                .unwrap()
                .starts_with("id, email, full_name")
        );
        assert_eq!((plan.planning_ms, plan.execution_ms), (None, None));
    }

    #[test]
    fn mysql_json_access_types_read_as_the_tree_format_names_them() {
        let operation = |name: &str| parsed(name).node(&[0]).unwrap().operation.clone();
        assert_eq!(operation("mysql_pk_lookup"), "Constant row");
        assert_eq!(operation("mysql_index_scan"), "Index lookup");
        let plan = parsed("mysql_index_scan");
        assert_eq!(detail(&plan, &[0], "Key"), Some("orders_customer_idx"));
        assert_eq!(detail(&plan, &[0], "Ref"), Some("const"));
        assert_eq!(plan.node(&[0]).unwrap().details[1].0, "Key");
    }

    #[test]
    fn mysql_json_a_nested_loop_gives_each_table_its_own_cost() {
        // MySQL prints the cost of the join so far with each table:
        // 105.45, 355.70, 1206.63.
        let plan = parsed("mysql_join3");
        assert_eq!(plan.root.estimated_cost, Some(1206.63));
        let join = plan.node(&[0]).unwrap();
        assert_eq!(join.operation, "Nested loop");
        assert_eq!(join.estimated_cost, None);
        let tables: Vec<(&str, Option<&str>, Option<f64>)> = join
            .children
            .iter()
            .map(|table| {
                (
                    table.operation.as_str(),
                    table.target.as_deref(),
                    table.estimated_rows,
                )
            })
            .collect();
        assert_eq!(
            tables,
            [
                ("Index lookup", Some("o"), Some(715.0)),
                ("Single-row index lookup", Some("c"), Some(1.0)),
                ("Index lookup", Some("i"), Some(1.0)),
            ]
        );
        assert!(close(join.children[0].estimated_cost, 105.45));
        assert!(close(join.children[1].estimated_cost, 250.25));
        assert!(close(join.children[2].estimated_cost, 850.93));
        assert!(close(join.inclusive_cost(), 1206.63));
        assert_eq!(
            detail(&plan, &[0, 0], "Index condition"),
            Some("(`kerno`.`o`.`status` = 'paid')")
        );
        // Without timings the most expensive table stands out.
        assert_eq!(plan.slowest(), Some(vec![0, 2]));
    }

    #[test]
    fn mysql_json_wrappers_and_subqueries_are_nodes() {
        let plan = parsed("mysql_aggregate_sort");
        assert_eq!(plan.node(&[0]).unwrap().operation, "Ordering operation");
        assert_eq!(detail(&plan, &[0], "Using filesort"), Some("true"));
        assert_eq!(plan.node(&[0, 0]).unwrap().operation, "Grouping operation");
        assert_eq!(detail(&plan, &[0, 0], "Using filesort"), None);
        let table = plan.node(&[0, 0, 0]).unwrap();
        assert_eq!(
            (table.operation.as_str(), table.target.as_deref()),
            ("Index scan", Some("orders"))
        );

        let plan = parsed("mysql_subquery");
        let outer = plan.node(&[0, 0]).unwrap();
        assert_eq!(outer.target.as_deref(), Some("p"));
        let subquery = plan.node(&[0, 0, 0]).unwrap();
        assert_eq!(subquery.operation, "Attached subqueries");
        assert_eq!(detail(&plan, &[0, 0, 0], "Cacheable"), Some("true"));
        let block = plan.node(&[0, 0, 0, 0]).unwrap();
        assert_eq!(block.operation, "Query block");
        assert_eq!(detail(&plan, &[0, 0, 0, 0], "Select id"), Some("3"));
        assert_eq!(block.children[0].target.as_deref(), Some("products"));
        // The subquery's cost (25) is added to the table it hangs under (25).
        assert!(close(outer.estimated_cost, 50.0));
        assert_eq!(
            plan.node(&[0, 1]).unwrap().target.as_deref(),
            Some("order_items")
        );
    }

    #[test]
    fn mysql_tree_lines_become_nodes() {
        let plan = parsed("mysql_seq_scan_analyze");
        let filter = &plan.root;
        assert_eq!(filter.operation, "Filter");
        assert_eq!(filter.target, None);
        assert_eq!(
            filter.details,
            [(
                "Filter".to_string(),
                "(customers.full_name like '%a%')".to_string()
            )]
        );
        assert_eq!(filter.estimated_cost, Some(57.0));
        assert_eq!(filter.estimated_rows, Some(55.6));
        assert_eq!(filter.actual_rows, Some(420.0));
        assert_eq!(filter.actual_ms, Some(4.14));
        assert_eq!(filter.loops, Some(1.0));
        let scan = plan.node(&[0]).unwrap();
        assert_eq!(
            (scan.operation.as_str(), scan.target.as_deref()),
            ("Table scan", Some("customers"))
        );
        assert!(scan.details.is_empty());
        assert_eq!(scan.actual_ms, Some(2.98));
        assert!(close(plan.exclusive_ms(&[]), 4.14 - 2.98));
        assert_eq!((plan.planning_ms, plan.execution_ms), (None, Some(4.14)));
    }

    #[test]
    fn mysql_tree_rows_and_times_are_totals_over_the_loops() {
        let plan = parsed("mysql_join3_analyze");
        assert_eq!(plan.root.operation, "Nested loop inner join");
        assert_eq!(plan.root.actual_rows, Some(1787.0));
        let lookup = plan.node(&[0, 0]).unwrap();
        assert_eq!(
            (lookup.operation.as_str(), lookup.target.as_deref()),
            ("Index lookup", Some("o"))
        );
        assert_eq!(
            lookup.details,
            [
                ("Index", "orders_status_placed_idx"),
                (
                    "Index lookup",
                    "(status='paid'), with index condition: (o.`status` = 'paid')"
                ),
            ]
            .map(|(label, text)| (label.to_string(), text.to_string()))
        );
        // `(actual time=874e-6..888e-6 rows=1 loops=715)`
        let customers = plan.node(&[0, 1]).unwrap();
        assert_eq!(customers.target.as_deref(), Some("c"));
        assert_eq!(customers.loops, Some(715.0));
        assert_eq!(customers.actual_rows, Some(715.0));
        assert!(close(customers.actual_ms, 888e-6 * 715.0));
        // `(cost=1 rows=1.9) (actual time=0.0205..0.0207 rows=2.5 loops=715)`
        let items = plan.node(&[1]).unwrap();
        assert_eq!(items.target.as_deref(), Some("i"));
        assert_eq!(items.estimated_rows, Some(1.9));
        assert!(close(items.actual_rows, 1787.5));
        assert!(close(items.actual_ms, 0.0207 * 715.0));
        assert_eq!(plan.slowest(), Some(vec![1]));
    }

    #[test]
    fn mysql_tree_odd_lines() {
        // `(cost=0..0 rows=1)`: a range of costs, the total is the last.
        let plan = parsed("mysql_pk_lookup_analyze");
        assert_eq!(plan.root.operation, "Rows fetched before execution");
        assert_eq!(plan.root.estimated_cost, Some(0.0));
        assert_eq!(plan.root.actual_rows, Some(1.0));
        assert!(plan.root.children.is_empty());

        // `rows=1e+6`, and steps the optimizer gives no cost for.
        let plan = parsed("mysql_big_analyze");
        assert_eq!(plan.root.operation, "Limit");
        assert_eq!(
            plan.root.details,
            [("Limit".to_string(), "5 row(s)".to_string())]
        );
        assert_eq!(plan.root.estimated_cost, None);
        let scan = plan.node(&[0, 0, 0, 0, 0]).unwrap();
        assert_eq!(scan.target.as_deref(), Some("big_table"));
        assert_eq!(scan.actual_rows, Some(1_000_000.0));
        assert_eq!(
            scan.details,
            [("Index".to_string(), "big_table_bucket_idx".to_string())]
        );

        // A subquery header has no numbers: its children carry them.
        let plan = parsed("mysql_subquery_analyze");
        let header = plan.node(&[0, 1]).unwrap();
        assert_eq!(header.operation, "Select #3");
        assert_eq!(
            header.details,
            [(
                "Select #3".to_string(),
                "(subquery in condition; run only once)".to_string()
            )]
        );
        assert_eq!(
            (header.estimated_cost, header.actual_ms, header.loops),
            (None, None, None)
        );
        assert!(close(header.inclusive_ms(), 0.599));
        assert_eq!(
            plan.node(&[0, 1, 0, 0]).unwrap().target.as_deref(),
            Some("products")
        );
        // A step that never ran.
        let plan = MysqlDialect::new(Engine::MySql)
            .parse_plan(
                "-> Table scan on t  (cost=2 rows=3) (never executed)\n",
                true,
            )
            .unwrap();
        let root = &plan.root;
        assert_eq!(
            (root.actual_rows, root.actual_ms, root.loops),
            (Some(0.0), Some(0.0), Some(0.0))
        );
        assert_eq!(root.estimated_rows, Some(3.0));
    }

    #[test]
    fn mariadb_json_without_analyze() {
        let plan = parsed("mariadb_seq_scan");
        assert_eq!(plan.root.operation, "Query block");
        assert_eq!(plan.root.estimated_cost, Some(0.0974836));
        // A loop over one table is that table.
        let table = plan.node(&[0]).unwrap();
        assert_eq!(
            (table.operation.as_str(), table.target.as_deref()),
            ("Table scan", Some("customers"))
        );
        assert_eq!(table.estimated_rows, Some(500.0));
        assert_eq!(table.estimated_cost, Some(0.0974836));
        assert_eq!(
            (table.actual_rows, table.actual_ms, table.loops),
            (None, None, None)
        );
        assert_eq!(
            detail(&plan, &[0], "Attached condition"),
            Some("customers.full_name like '%a%'")
        );
        assert_eq!((plan.planning_ms, plan.execution_ms), (None, None));
    }

    #[test]
    fn mariadb_analyze_adds_what_really_happened() {
        let plan = parsed("mariadb_join3_analyze");
        assert_eq!(plan.planning_ms, Some(0.051229574));
        assert_eq!(plan.execution_ms, Some(20.3629013));
        assert_eq!(plan.root.loops, Some(1.0));
        let join = plan.node(&[0]).unwrap();
        assert_eq!(join.operation, "Nested loop");
        let targets: Vec<Option<&str>> = join
            .children
            .iter()
            .map(|table| table.target.as_deref())
            .collect();
        assert_eq!(targets, [Some("o"), Some("c"), Some("i")]);
        // `r_rows` is one loop's rows; the times are totals already.
        let items = &join.children[2];
        assert_eq!(items.loops, Some(715.0));
        assert!(close(items.actual_rows, 2.499300699 * 715.0));
        assert!(close(items.actual_ms, 17.91292715 + 0.086410828));
        assert_eq!(items.estimated_rows, Some(2.0));
        assert_eq!(items.estimated_cost, Some(0.88567495));
        assert_eq!(
            detail(&plan, &[0, 2], "R engine stats").map(|text| text.starts_with('{')),
            Some(true)
        );
        assert_eq!(plan.slowest(), Some(vec![0, 2]));
    }

    #[test]
    fn mariadb_wrappers_take_their_children_in() {
        // The sort took 0.052 ms of its own; the table under it 1.176 ms.
        let plan = parsed("mariadb_aggregate_sort_analyze");
        let sort = plan.node(&[0]).unwrap();
        assert_eq!(sort.operation, "Filesort");
        assert_eq!(detail(&plan, &[0], "Sort key"), Some("count(0) desc"));
        assert_eq!(sort.actual_rows, Some(6.0));
        assert!(close(
            sort.actual_ms,
            0.052146621 + 0.345226473 + 0.830594385
        ));
        assert!(close(plan.exclusive_ms(&[0]), 0.052146621));
        assert_eq!(plan.node(&[0, 0]).unwrap().operation, "Temporary table");
        assert_eq!(
            plan.node(&[0, 0, 0]).unwrap().target.as_deref(),
            Some("orders")
        );
        assert_eq!(plan.slowest(), Some(vec![0, 0, 0]));

        // A materialized subquery hangs under the table that reads it.
        let plan = parsed("mariadb_subquery_analyze");
        let reader = plan.node(&[0, 1]).unwrap();
        assert_eq!(reader.target.as_deref(), Some("<subquery2>"));
        assert_eq!(reader.actual_rows, Some(75.0));
        assert_eq!(plan.node(&[0, 1, 0]).unwrap().operation, "Materialized");
        assert_eq!(plan.node(&[0, 1, 0, 0]).unwrap().operation, "Query block");
        assert_eq!(
            plan.node(&[0, 1, 0, 0, 0]).unwrap().target.as_deref(),
            Some("order_items")
        );
        // The scalar subquery is a block of its own beside the join.
        assert_eq!(detail(&plan, &[1], "Select id"), Some("3"));
        assert_eq!(
            plan.node(&[1, 0]).unwrap().target.as_deref(),
            Some("products")
        );

        // A constant table is read while planning: no loop, no rows.
        let plan = parsed("mariadb_pk_lookup_analyze");
        let table = plan.node(&[0]).unwrap();
        assert_eq!(table.operation, "Constant row");
        assert_eq!(
            (table.loops, table.actual_rows, table.actual_ms),
            (Some(0.0), None, None)
        );
    }

    /// Every captured plan, printed as an outline, is the reviewed
    /// `<name>.plan` beside it.
    #[test]
    fn every_captured_plan_reads_as_pinned() {
        for engine in ["mysql", "mariadb"] {
            for query in QUERIES {
                for suffix in ["", "_analyze"] {
                    let name = format!("{engine}_{query}{suffix}");
                    assert_eq!(
                        parsed(&name).to_string(),
                        fixture(&format!("{name}.plan")),
                        "{name}"
                    );
                }
            }
        }
    }

    #[test]
    fn what_is_not_a_plan_is_an_error() {
        let mysql = MysqlDialect::new(Engine::MySql);
        let mariadb = MysqlDialect::new(Engine::MariaDb);
        let cases: [(&MysqlDialect, bool, &str, &str); 8] = [
            (&mysql, false, "-> Table scan on t", "not valid JSON"),
            (&mysql, false, "{}", "no \"query_block\""),
            (&mysql, false, "[1, 2]", "no \"query_block\""),
            (&mysql, false, "{\"query_block\": 3}", "no \"query_block\""),
            // MySQL's second JSON format (explain_json_format_version = 2).
            (
                &mysql,
                false,
                "{\"query\": \"select 1\", \"operation\": \"x\"}",
                "no \"query_block\"",
            ),
            (
                &mariadb,
                true,
                "{\"query_optimization\": {}}",
                "no \"query_block\"",
            ),
            (&mysql, true, "", "no step"),
            (&mysql, true, "{\"query_block\": {}}", "\"->\""),
        ];
        for (dialect, analyze, output, message) in cases {
            let error = dialect.parse_plan(output, analyze).unwrap_err();
            assert!(matches!(error, Error::Protocol(_)), "{output}: {error:?}");
            assert!(error.to_string().contains(message), "{output}: {error}");
        }
        let error = mysql
            .parse_plan("-> Table scan on a\n-> Table scan on b\n", true)
            .unwrap_err();
        assert!(error.to_string().contains("more than one"), "{error}");
    }

    #[test]
    fn subqueries_come_after_what_reads_from_them() {
        let plan = MysqlDialect::new(Engine::MySql)
            .parse_plan(
                "{\"query_block\": {\"select_list_subqueries\": [{\"query_block\": \
                 {\"table\": {\"table_name\": \"inner\"}}}], \
                 \"table\": {\"table_name\": \"outer\"}, \
                 \"having_subqueries\": [{\"dependent\": true, \"query_block\": {}}]}}",
                false,
            )
            .unwrap();
        let children: Vec<(&str, Option<&str>)> = plan
            .root
            .children
            .iter()
            .map(|child| (child.operation.as_str(), child.target.as_deref()))
            .collect();
        assert_eq!(
            children,
            [
                ("Table", Some("outer")),
                ("Having subqueries", None),
                ("Query block", None),
            ]
        );
        assert_eq!(plan.node(&[1, 0]).unwrap().operation, "Query block");
        assert_eq!(plan.node(&[2, 0]).unwrap().target.as_deref(), Some("inner"));
    }

    #[test]
    fn unknown_keys_are_details_or_nodes() {
        let plan = MysqlDialect::new(Engine::MySql)
            .parse_plan(
                "{\"query_block\": {\"message\": \"No tables used\", \"new_thing\": {\"table\": \
                 {\"table_name\": \"t\", \"access_type\": \"novel\", \"stats\": {\"n\": 1}}}}}",
                false,
            )
            .unwrap();
        assert_eq!(
            plan.root.details,
            [("Message".to_string(), "No tables used".to_string())]
        );
        assert_eq!(plan.node(&[0]).unwrap().operation, "New thing");
        let table = plan.node(&[0, 0]).unwrap();
        assert_eq!(
            (table.operation.as_str(), table.target.as_deref()),
            ("novel", Some("t"))
        );
        assert_eq!(detail(&plan, &[0, 0], "Stats"), Some("{\"n\":1}"));
    }
}
