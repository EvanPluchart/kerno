//! An execution plan as a tree: what each step does, what it was expected
//! to cost and, after EXPLAIN ANALYZE, what it took. The step that took the
//! most time of its own is highlighted.

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div, px, relative,
};
use kerno_core::{Plan, PlanNode};

/// One line of the tree.
pub struct PlanRow<'a> {
    pub depth: usize,
    pub path: Vec<usize>,
    pub node: &'a PlanNode,
}

/// The nodes of a plan, top down, each with its depth.
pub fn rows(plan: &Plan) -> Vec<PlanRow<'_>> {
    fn walk<'a>(node: &'a PlanNode, path: Vec<usize>, out: &mut Vec<PlanRow<'a>>) {
        out.push(PlanRow {
            depth: path.len(),
            path: path.clone(),
            node,
        });
        for (index, child) in node.children.iter().enumerate() {
            let mut path = path.clone();
            path.push(index);
            walk(child, path, out);
        }
    }
    let mut out = Vec::new();
    walk(&plan.root, Vec::new(), &mut out);
    out
}

/// Details that say how the plan was measured rather than what the step
/// does. They stay in the raw output; the tree shows conditions, keys and
/// methods.
fn is_bookkeeping(label: &str) -> bool {
    const PREFIXES: [&str; 12] = [
        "Actual ",
        "Startup",
        "Plan Width",
        "Parent Relationship",
        "Parallel",
        "Async",
        "Shared ",
        "Local ",
        "Temp ",
        "Disk Usage",
        "Inner Unique",
        "Scan Direction",
    ];
    PREFIXES.iter().any(|prefix| label.starts_with(prefix))
}

/// `1234.5` as `1,234.5`; whole numbers without decimals.
pub fn number(value: f64) -> String {
    let rounded = (value * 10.).round() / 10.;
    let text = if rounded.fract() == 0. || rounded.abs() >= 1000. {
        format!("{:.0}", rounded)
    } else {
        format!("{rounded:.1}")
    };
    let (whole, fraction) = text
        .split_once('.')
        .map_or((text.as_str(), ""), |(whole, fraction)| (whole, fraction));
    let (sign, digits) = whole
        .strip_prefix('-')
        .map_or(("", whole), |digits| ("-", digits));
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if fraction.is_empty() {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{fraction}")
    }
}

/// `0.42 ms`, `12.3 ms`, `1.25 s`.
pub fn millis(ms: f64) -> String {
    if ms >= 1000. {
        format!("{:.2} s", ms / 1000.)
    } else if ms >= 10. {
        format!("{ms:.1} ms")
    } else {
        format!("{ms:.2} ms")
    }
}

/// The tree of a plan.
pub fn render(plan: &Plan, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let slowest = plan.slowest();
    let analyzed = plan.root.inclusive_ms().is_some();
    // Each node's own time (or cost) against the whole, for its bar.
    let total = if analyzed {
        plan.root.inclusive_ms()
    } else {
        plan.root.inclusive_cost()
    }
    .filter(|total| *total > 0.);
    let column = |width: f32, text: String| {
        div()
            .flex_none()
            .w(px(width))
            .text_right()
            .truncate()
            .child(SharedString::from(text))
    };
    let header = h_flex()
        .flex_none()
        .px_3()
        .py_1()
        .gap_3()
        .text_xs()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .child(div().flex_1().child("Step"))
        .child(column(90., "Est. cost".into()))
        .child(column(90., "Est. rows".into()))
        .when(analyzed, |this| {
            this.child(column(90., "Rows".into()))
                .child(column(90., "Own time".into()))
                .child(column(90., "Total time".into()))
        })
        .child(div().flex_none().w(px(80.)));
    let lines = rows(plan).into_iter().enumerate().map(|(index, row)| {
        let node = row.node;
        let hot = slowest.as_ref() == Some(&row.path);
        let own = if analyzed {
            plan.exclusive_ms(&row.path)
        } else {
            node.estimated_cost.map(|cost| {
                let below: f64 = node
                    .children
                    .iter()
                    .filter_map(PlanNode::inclusive_cost)
                    .sum();
                (cost - below).max(0.)
            })
        };
        let share = match (own, total) {
            (Some(own), Some(total)) => (own / total).clamp(0., 1.) as f32,
            _ => 0.,
        };
        let or_dash = |value: Option<String>| value.unwrap_or_else(|| "–".into());
        v_flex()
            .id(("plan-node", index))
            .test_support()
            .px_3()
            .py_1()
            .when(hot, |this| this.bg(theme.danger.opacity(0.12)))
            .child(
                h_flex()
                    .gap_3()
                    .text_sm()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .pl(px(16. * row.depth as f32))
                            .child(
                                div()
                                    .truncate()
                                    .when(hot, |this| this.text_color(theme.danger))
                                    .child(SharedString::from(node.operation.clone())),
                            )
                            .children(node.target.clone().map(|target| {
                                div()
                                    .truncate()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(theme.muted_foreground)
                                    .child(SharedString::from(target))
                            }))
                            .when(hot, |this| {
                                this.child(
                                    div().flex_none().text_xs().text_color(theme.danger).child(
                                        if analyzed {
                                            "slowest step"
                                        } else {
                                            "costliest step"
                                        },
                                    ),
                                )
                            }),
                    )
                    .child(column(90., or_dash(node.estimated_cost.map(number))))
                    .child(column(90., or_dash(node.estimated_rows.map(number))))
                    .when(analyzed, |this| {
                        this.child(column(90., or_dash(node.actual_rows.map(number))))
                            .child(column(
                                90.,
                                or_dash(plan.exclusive_ms(&row.path).map(millis)),
                            ))
                            .child(column(90., or_dash(node.inclusive_ms().map(millis))))
                    })
                    // How much of the whole this step's own share is.
                    .child(
                        div()
                            .flex_none()
                            .w(px(80.))
                            .h(px(6.))
                            .rounded_full()
                            .bg(theme.muted)
                            .child(div().h_full().rounded_full().w(relative(share)).bg(if hot {
                                theme.danger
                            } else {
                                theme.primary
                            })),
                    ),
            )
            .children(
                node.details
                    .iter()
                    .any(|(label, _)| !is_bookkeeping(label))
                    .then(|| {
                        div()
                            .pl(px(16. * row.depth as f32 + 8.))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(SharedString::from(
                                node.details
                                    .iter()
                                    .filter(|(label, _)| !is_bookkeeping(label))
                                    .take(4)
                                    .map(|(label, text)| format!("{label}: {text}"))
                                    .collect::<Vec<_>>()
                                    .join("  ·  "),
                            ))
                    }),
            )
    });
    let summary = [
        plan.planning_ms
            .map(|ms| format!("planning {}", millis(ms))),
        plan.execution_ms
            .map(|ms| format!("execution {}", millis(ms))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("  ·  ");
    v_flex()
        .id("plan-tree")
        .test_support()
        .size_full()
        .child(header)
        .child(
            v_flex()
                .id("plan-rows")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(lines),
        )
        .when(!summary.is_empty(), |this| {
            this.child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(summary)),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(operation: &str, children: Vec<PlanNode>) -> PlanNode {
        PlanNode {
            operation: operation.into(),
            target: None,
            estimated_cost: None,
            estimated_rows: None,
            actual_rows: None,
            actual_ms: None,
            loops: None,
            details: Vec::new(),
            children,
        }
    }

    #[test]
    fn rows_are_the_nodes_top_down_with_their_depth_and_path() {
        let plan = Plan {
            root: node(
                "Sort",
                vec![node(
                    "Hash Join",
                    vec![
                        node("Seq Scan", vec![]),
                        node("Hash", vec![node("Index Scan", vec![])]),
                    ],
                )],
            ),
            planning_ms: None,
            execution_ms: None,
        };
        let listed: Vec<(usize, Vec<usize>, &str)> = rows(&plan)
            .iter()
            .map(|row| (row.depth, row.path.clone(), row.node.operation.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                (0, vec![], "Sort"),
                (1, vec![0], "Hash Join"),
                (2, vec![0, 0], "Seq Scan"),
                (2, vec![0, 1], "Hash"),
                (3, vec![0, 1, 0], "Index Scan"),
            ]
        );
    }

    #[test]
    fn measurement_details_are_left_to_the_raw_output() {
        for label in [
            "Actual Startup Time",
            "Plan Width",
            "Shared Hit Blocks",
            "Parent Relationship",
        ] {
            assert!(is_bookkeeping(label), "{label}");
        }
        for label in [
            "Hash Cond",
            "Filter",
            "Sort Key",
            "Join Type",
            "Index Cond",
            "Access type",
        ] {
            assert!(!is_bookkeeping(label), "{label}");
        }
    }

    #[test]
    fn numbers_and_times_are_short_and_grouped() {
        for (value, text) in [
            (0., "0"),
            (12., "12"),
            (12.34, "12.3"),
            (1234.56, "1,235"),
            (1_000_000., "1,000,000"),
            (-1500., "-1,500"),
        ] {
            assert_eq!(number(value), text);
        }
        for (ms, text) in [
            (0.042, "0.04 ms"),
            (3.378, "3.38 ms"),
            (18.7, "18.7 ms"),
            (1250., "1.25 s"),
        ] {
            assert_eq!(millis(ms), text);
        }
    }
}
