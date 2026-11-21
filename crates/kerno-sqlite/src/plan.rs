//! `EXPLAIN QUERY PLAN` as a tree. SQLite gives one row per step: its id,
//! its parent's id (0 at the top), an unused column and what it does
//! (<https://www.sqlite.org/eqp.html>). There are no costs, row counts or
//! times: EXPLAIN QUERY PLAN does not run the statement.

use kerno_core::plan::{Plan, PlanNode};
use kerno_core::{Error, Result};

/// Reads the rows of an `EXPLAIN QUERY PLAN`, one per line, their columns
/// separated by tabs (as the query tab joins them).
pub(crate) fn parse(output: &str) -> Result<Plan> {
    let bad = |line: &str| {
        Error::Protocol(format!(
            "an EXPLAIN QUERY PLAN row is id, parent, notused and detail; got \"{line}\""
        ))
    };
    let mut steps: Vec<(i64, i64, PlanNode)> = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let mut parts = line.splitn(4, '\t');
        let (Some(id), Some(parent), Some(_), Some(detail)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad(line));
        };
        let number = |text: &str| text.trim().parse::<i64>().map_err(|_| bad(line));
        steps.push((number(id)?, number(parent)?, node(detail.trim())));
    }
    if steps.is_empty() {
        return Err(Error::Protocol("EXPLAIN QUERY PLAN returned no row".into()));
    }
    Ok(Plan {
        root: PlanNode {
            operation: "Query plan".into(),
            children: children(&mut steps, 0),
            ..PlanNode::default()
        },
        planning_ms: None,
        execution_ms: None,
    })
}

/// The steps whose parent is `parent`, each with its own children, in the
/// order SQLite gave them.
fn children(steps: &mut Vec<(i64, i64, PlanNode)>, parent: i64) -> Vec<PlanNode> {
    let mut mine = Vec::new();
    let mut index = 0;
    while index < steps.len() {
        if steps[index].1 == parent {
            mine.push(steps.remove(index));
        } else {
            index += 1;
        }
    }
    mine.into_iter()
        .map(|(id, _, mut node)| {
            // An id is never its own parent; guard against a loop anyway.
            if id != parent {
                node.children = children(steps, id);
            }
            node
        })
        .collect()
}

/// One step: `SCAN t` and `SEARCH t USING …` name their table; the other
/// steps are their whole text.
fn node(detail: &str) -> PlanNode {
    let mut words = detail.splitn(3, ' ');
    let first = words.next().unwrap_or_default();
    if matches!(first, "SCAN" | "SEARCH")
        && let Some(target) = words.next()
    {
        let rest = words.next().unwrap_or_default().trim();
        return PlanNode {
            operation: first.to_string(),
            target: Some(target.to_string()),
            details: if rest.is_empty() {
                Vec::new()
            } else {
                vec![("How".to_string(), rest.to_string())]
            },
            ..PlanNode::default()
        };
    }
    PlanNode {
        operation: detail.to_string(),
        ..PlanNode::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_nest_under_their_parents() {
        let output = "2\t0\t0\tSEARCH orders USING INDEX orders_customer (customer_id=?)\n\
                      5\t0\t0\tSCAN customers\n\
                      9\t0\t0\tSCALAR SUBQUERY 1\n\
                      12\t9\t0\tSCAN lines\n\
                      30\t0\t0\tUSE TEMP B-TREE FOR ORDER BY";
        let plan = parse(output).unwrap();
        let top: Vec<&str> = plan
            .root
            .children
            .iter()
            .map(|node| node.operation.as_str())
            .collect();
        assert_eq!(
            top,
            [
                "SEARCH",
                "SCAN",
                "SCALAR SUBQUERY 1",
                "USE TEMP B-TREE FOR ORDER BY"
            ]
        );
        let search = &plan.root.children[0];
        assert_eq!(search.target.as_deref(), Some("orders"));
        assert_eq!(
            search.details,
            [(
                "How".to_string(),
                "USING INDEX orders_customer (customer_id=?)".to_string()
            )]
        );
        let subquery = &plan.root.children[2];
        assert_eq!(subquery.children.len(), 1);
        assert_eq!(subquery.children[0].target.as_deref(), Some("lines"));
        assert_eq!(plan.root.children[1].details, []);
    }

    #[test]
    fn other_shapes_are_refused() {
        assert!(parse("").is_err());
        assert!(parse("SCAN t").is_err());
        assert!(parse("a\t0\t0\tSCAN t").is_err());
    }
}
