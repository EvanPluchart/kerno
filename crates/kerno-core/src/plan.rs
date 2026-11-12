//! A query plan as a tree the app can draw, whatever the engine.
//!
//! Each driver reads what its `EXPLAIN` prints into this model (see
//! [`crate::Dialect::parse_plan`]). Costs and times of a node include its
//! children, the way PostgreSQL reports them; [`Plan::exclusive_ms`] and
//! [`Plan::slowest`] take the children's share back out.

use std::fmt;

use crate::error::{Error, Result};

/// A JSON value, for the drivers that read plans printed as JSON.
pub use serde_json::Value as Json;

/// The plan of one statement.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plan {
    /// The node that produces the statement's result.
    pub root: PlanNode,
    /// Time the server spent planning, in milliseconds, when it says so.
    pub planning_ms: Option<f64>,
    /// Time the server spent running the statement, in milliseconds
    /// (with `ANALYZE` only).
    pub execution_ms: Option<f64>,
}

/// One step of a plan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanNode {
    /// What the node does, as the engine names it (`Seq Scan`, `Hash Join`,
    /// `Table scan` ...).
    pub operation: String,
    /// The relation, index or alias the node works on, when there is one.
    pub target: Option<String>,
    /// Total cost of the node, children included, in the engine's unit.
    pub estimated_cost: Option<f64>,
    /// Rows the planner expects.
    pub estimated_rows: Option<f64>,
    /// Rows the node really produced, over all its loops (with `ANALYZE`).
    pub actual_rows: Option<f64>,
    /// Time really spent in the node and its children, over all its loops,
    /// in milliseconds (with `ANALYZE`).
    pub actual_ms: Option<f64>,
    /// How many times the node ran (with `ANALYZE`).
    pub loops: Option<f64>,
    /// Everything else the engine says about the node (filter, join
    /// condition, index condition, sort key, buffers ...): label and text,
    /// in a stable order.
    pub details: Vec<(String, String)>,
    /// The nodes this one reads from.
    pub children: Vec<PlanNode>,
}

/// A number of a node: its time or its cost.
type Metric = fn(&PlanNode) -> Option<f64>;

/// The node's own number, or the sum of its children's when the engine
/// gives none for the node itself (a wrapper without timings).
fn inclusive(node: &PlanNode, metric: Metric) -> Option<f64> {
    metric(node).or_else(|| {
        node.children
            .iter()
            .filter_map(|child| inclusive(child, metric))
            .reduce(|sum, value| sum + value)
    })
}

/// The node's own share: its number minus its children's, never negative.
fn exclusive(node: &PlanNode, metric: Metric) -> Option<f64> {
    let children: f64 = node
        .children
        .iter()
        .filter_map(|child| inclusive(child, metric))
        .sum();
    Some((metric(node)? - children).max(0.0))
}

/// Calls `visit` on `node` and everything under it, parents first, with
/// each node's path.
fn walk<'a>(
    node: &'a PlanNode,
    path: &mut Vec<usize>,
    visit: &mut impl FnMut(&'a PlanNode, &[usize]),
) {
    visit(node, path);
    for (index, child) in node.children.iter().enumerate() {
        path.push(index);
        walk(child, path, visit);
        path.pop();
    }
}

impl PlanNode {
    /// Time spent in the node and its children. A node the engine gives
    /// no time for takes the sum of its children's.
    pub fn inclusive_ms(&self) -> Option<f64> {
        inclusive(self, |node| node.actual_ms)
    }

    /// Estimated cost of the node and its children. A node the engine
    /// gives no cost for takes the sum of its children's.
    pub fn inclusive_cost(&self) -> Option<f64> {
        inclusive(self, |node| node.estimated_cost)
    }
}

impl Plan {
    /// The node at `path`: child indexes from the root, empty for the root.
    pub fn node(&self, path: &[usize]) -> Option<&PlanNode> {
        path.iter()
            .try_fold(&self.root, |node, &index| node.children.get(index))
    }

    /// The node's own time: its inclusive time minus its children's, never
    /// negative. `None` when there is no such node or it has no timing.
    pub fn exclusive_ms(&self, path: &[usize]) -> Option<f64> {
        exclusive(self.node(path)?, |node| node.actual_ms)
    }

    /// Path of the node with the largest exclusive time; without timings,
    /// of the node with the largest exclusive estimated cost.
    pub fn slowest(&self) -> Option<Vec<usize>> {
        let metrics: [Metric; 2] = [|node| node.actual_ms, |node| node.estimated_cost];
        metrics.into_iter().find_map(|metric| {
            let mut slowest: Option<(f64, Vec<usize>)> = None;
            walk(&self.root, &mut Vec::new(), &mut |node, path| {
                if let Some(own) = exclusive(node, metric)
                    && slowest.as_ref().is_none_or(|(most, _)| own > *most)
                {
                    slowest = Some((own, path.to_vec()));
                }
            });
            slowest.map(|(_, path)| path)
        })
    }
}

/// A number without the noise of float arithmetic: three decimals at most.
fn number(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// ` (label a=1 b=2)` for the fields that have a value; nothing when none has.
fn fields(label: &str, fields: &[(&str, Option<f64>)]) -> String {
    let fields: Vec<String> = fields
        .iter()
        .filter_map(|(name, value)| Some(format!("{name}={}", number((*value)?))))
        .collect();
    if fields.is_empty() {
        String::new()
    } else {
        format!(" ({label}{})", fields.join(" "))
    }
}

/// An indented outline, one node per line: what the plan is, as text.
impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut lines = Vec::new();
        walk(&self.root, &mut Vec::new(), &mut |node, path| {
            let target = node.target.as_ref().map(|target| format!(" on {target}"));
            let estimated = fields(
                "",
                &[("cost", node.estimated_cost), ("rows", node.estimated_rows)],
            );
            let actual = fields(
                "actual ",
                &[
                    ("rows", node.actual_rows),
                    ("ms", node.actual_ms),
                    ("loops", node.loops),
                ],
            );
            let numbers = format!("{estimated}{actual}");
            lines.push(format!(
                "{}{}{}{}{numbers}",
                "  ".repeat(path.len()),
                node.operation,
                target.unwrap_or_default(),
                if numbers.is_empty() { "" } else { " " },
            ));
        });
        lines.iter().try_for_each(|line| writeln!(f, "{line}"))
    }
}

/// Parses the JSON an engine printed as a plan.
pub fn parse_json(output: &str) -> Result<Json> {
    serde_json::from_str(output)
        .map_err(|error| Error::Protocol(format!("the plan is not valid JSON: {error}")))
}

/// The text of a JSON value among a node's details. `None` for what says
/// nothing: `null`, `false`, an empty string or list.
pub fn detail_text(value: &Json) -> Option<String> {
    let scalar = |value: &Json| match value {
        Json::String(text) => Some(text.clone()),
        Json::Number(_) | Json::Bool(_) => Some(value.to_string()),
        _ => None,
    };
    match value {
        Json::Null | Json::Bool(false) => None,
        Json::String(text) if text.is_empty() => None,
        Json::Array(items) if items.is_empty() => None,
        // A list of plain values reads better without the JSON around it.
        Json::Array(items) => Some(match items.iter().map(scalar).collect::<Option<Vec<_>>>() {
            Some(texts) => texts.join(", "),
            None => value.to_string(),
        }),
        Json::Object(_) => Some(value.to_string()),
        _ => scalar(value),
    }
}

/// Orders details: the labels in `first`, in that order, then the others
/// by name.
pub fn sort_details(details: &mut [(String, String)], first: &[&str]) {
    let rank = |label: &str| first.iter().position(|name| *name == label);
    details.sort_by(|(a, _), (b, _)| {
        let key = |label: &str| (rank(label).unwrap_or(first.len()), label.to_string());
        key(a).cmp(&key(b))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(
        operation: &str,
        ms: Option<f64>,
        cost: Option<f64>,
        children: Vec<PlanNode>,
    ) -> PlanNode {
        PlanNode {
            operation: operation.into(),
            actual_ms: ms,
            estimated_cost: cost,
            children,
            ..PlanNode::default()
        }
    }

    fn plan(root: PlanNode) -> Plan {
        Plan {
            root,
            ..Plan::default()
        }
    }

    /// Sort (10 ms) over a join (8 ms) of a scan (5 ms) and a lookup that
    /// ran 100 times (2 ms in all).
    fn timed() -> Plan {
        plan(node(
            "Sort",
            Some(10.0),
            Some(100.0),
            vec![node(
                "Nested Loop",
                Some(8.0),
                Some(90.0),
                vec![
                    node("Seq Scan", Some(5.0), Some(20.0), vec![]),
                    node("Index Scan", Some(2.0), Some(65.0), vec![]),
                ],
            )],
        ))
    }

    #[test]
    fn nodes_are_found_by_path() {
        let plan = timed();
        assert_eq!(plan.node(&[]).unwrap().operation, "Sort");
        assert_eq!(plan.node(&[0]).unwrap().operation, "Nested Loop");
        assert_eq!(plan.node(&[0, 1]).unwrap().operation, "Index Scan");
        assert!(plan.node(&[1]).is_none());
        assert!(plan.node(&[0, 1, 0]).is_none());
    }

    #[test]
    fn exclusive_time_takes_the_children_out() {
        let plan = timed();
        assert_eq!(plan.exclusive_ms(&[]), Some(2.0));
        assert_eq!(plan.exclusive_ms(&[0]), Some(1.0));
        assert_eq!(plan.exclusive_ms(&[0, 0]), Some(5.0));
        assert_eq!(plan.exclusive_ms(&[0, 1]), Some(2.0));
        assert_eq!(plan.exclusive_ms(&[3]), None);
    }

    #[test]
    fn exclusive_time_is_never_negative() {
        // Under a parallel node the children's time, summed over the
        // workers' loops, exceeds the parent's wall-clock time.
        let plan = plan(node(
            "Gather",
            Some(890.0),
            None,
            vec![node("Seq Scan", Some(2594.0), None, vec![])],
        ));
        assert_eq!(plan.exclusive_ms(&[]), Some(0.0));
        assert_eq!(plan.exclusive_ms(&[0]), Some(2594.0));
        assert_eq!(plan.slowest(), Some(vec![0]));
    }

    #[test]
    fn a_node_without_timing_passes_its_children_through() {
        // MySQL prints no time for a subquery header; MariaDB none for
        // some wrappers.
        let plan = plan(node(
            "Filter",
            Some(9.0),
            None,
            vec![
                node("Table scan", Some(3.0), None, vec![]),
                node(
                    "Select #2",
                    None,
                    None,
                    vec![
                        node("Aggregate", Some(4.0), None, vec![]),
                        node("Materialize", Some(1.0), None, vec![]),
                    ],
                ),
            ],
        ));
        assert_eq!(plan.node(&[1]).unwrap().inclusive_ms(), Some(5.0));
        assert_eq!(plan.exclusive_ms(&[1]), None);
        assert_eq!(plan.exclusive_ms(&[]), Some(1.0));
        assert_eq!(plan.slowest(), Some(vec![1, 0]));
    }

    #[test]
    fn slowest_is_the_largest_exclusive_time() {
        assert_eq!(timed().slowest(), Some(vec![0, 0]));
        // The root itself can be the slowest.
        let mut plan = timed();
        plan.root.actual_ms = Some(30.0);
        assert_eq!(plan.slowest(), Some(vec![]));
    }

    #[test]
    fn slowest_falls_back_to_the_estimated_cost() {
        let mut plan = timed();
        fn clear(node: &mut PlanNode) {
            node.actual_ms = None;
            node.children.iter_mut().for_each(clear);
        }
        clear(&mut plan.root);
        assert_eq!(plan.exclusive_ms(&[]), None);
        // Exclusive costs: 10, 5, 20, 65.
        assert_eq!(plan.slowest(), Some(vec![0, 1]));
        assert_eq!(plan.node(&[0]).unwrap().inclusive_cost(), Some(90.0));
    }

    #[test]
    fn slowest_of_a_plan_without_numbers_is_unknown() {
        let plan = plan(node(
            "Result",
            None,
            None,
            vec![node("Scan", None, None, vec![])],
        ));
        assert_eq!(plan.slowest(), None);
        assert_eq!(plan.root.inclusive_ms(), None);
        assert_eq!(plan.root.inclusive_cost(), None);
    }

    #[test]
    fn the_outline_shows_one_node_per_line() {
        let plan = plan(PlanNode {
            operation: "Hash Join".into(),
            estimated_cost: Some(77.56),
            estimated_rows: Some(67.0),
            actual_rows: Some(75.0),
            actual_ms: Some(1.5),
            loops: Some(1.0),
            children: vec![
                PlanNode {
                    operation: "Seq Scan".into(),
                    target: Some("products".into()),
                    estimated_rows: Some(200.0),
                    // 0.1 ms and 2.5 rows per loop, 3 loops.
                    actual_rows: Some(2.5 * 3.0),
                    actual_ms: Some(0.1 * 3.0),
                    loops: Some(3.0),
                    ..PlanNode::default()
                },
                PlanNode {
                    operation: "Hash".into(),
                    ..PlanNode::default()
                },
            ],
            ..PlanNode::default()
        });
        assert_eq!(
            plan.to_string(),
            "Hash Join  (cost=77.56 rows=67) (actual rows=75 ms=1.5 loops=1)\n\
             \x20 Seq Scan on products  (rows=200) (actual rows=7.5 ms=0.3 loops=3)\n\
             \x20 Hash\n"
        );
    }

    #[test]
    fn json_that_is_not_json_is_an_error() {
        let error = parse_json("-> Table scan on t").unwrap_err();
        assert!(matches!(error, Error::Protocol(_)), "{error:?}");
        assert!(error.to_string().contains("not valid JSON"), "{error}");
        assert_eq!(parse_json("[1]").unwrap()[0], 1);
    }

    #[test]
    fn detail_texts() {
        let text = |json: &str| detail_text(&parse_json(json).unwrap());
        assert_eq!(text("\"(a = 1)\"").as_deref(), Some("(a = 1)"));
        assert_eq!(text("12").as_deref(), Some("12"));
        assert_eq!(text("0.25").as_deref(), Some("0.25"));
        assert_eq!(text("true").as_deref(), Some("true"));
        assert_eq!(
            text("[\"a\", \"b DESC\", 3]").as_deref(),
            Some("a, b DESC, 3")
        );
        assert_eq!(text("{\"pages\": 2}").as_deref(), Some("{\"pages\":2}"));
        assert_eq!(text("[{\"n\": 1}]").as_deref(), Some("[{\"n\":1}]"));
        for nothing in ["null", "false", "\"\"", "[]"] {
            assert_eq!(text(nothing), None, "{nothing}");
        }
    }

    #[test]
    fn details_are_ordered() {
        let mut details: Vec<(String, String)> = ["Zeta", "Filter", "Alpha", "Index Cond"]
            .iter()
            .map(|label| (label.to_string(), String::new()))
            .collect();
        sort_details(&mut details, &["Index Cond", "Filter"]);
        let labels: Vec<&str> = details.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(labels, ["Index Cond", "Filter", "Alpha", "Zeta"]);
    }
}
