//! K49 (the owner's request of 2026-11-23): a foreign key is seen in the
//! grid, a mark in its column's header saying where it leads, and followed
//! in one gesture, the arrow of a hovered or selected cell, ⌘-click on the
//! value or ⌘↓, exactly as the cell menu's "Open … where …" does.
//! Fixtures read only; scratch objects `kerno_test_k49`.

use gpui_kit::Modifiers;
use gpui_kit::component::table::TableSelection;

use super::sqlite::{file_config, fixture_file};
use super::*;
use crate::query::grid::{arrow_tip, follow_key};

/// The kit's cell of a row and a column of the grid (Kerno's own cell
/// fills it).
fn cell(row: usize, column: usize) -> gpui_kit::ElementId {
    gpui_kit::ElementId::Name(format!("table-cell-{row}:{column}").into())
}

fn arrow(row: usize, column: usize) -> (&'static str, usize) {
    ("fk-arrow", (row << 12) | column)
}

/// What an element says to a screen reader (its tooltip); `None` when it
/// is not drawn.
fn said(app: &Fixture, cx: &mut TestAppContext, id: (&'static str, usize)) -> Option<String> {
    app.step(cx, |window, _| {
        window
            .try_find(id)
            .map(|found| found.description().unwrap_or_default().to_string())
    })
}

/// The tooltip of the mark of the header of a column of the grid.
fn mark(app: &Fixture, cx: &mut TestAppContext, column: usize) -> Option<String> {
    said(app, cx, ("column-link", column))
}

fn hover(app: &Fixture, cx: &mut TestAppContext, id: impl Into<gpui_kit::ElementId>) {
    app.step(cx, |window, cx| window.hover(id, cx));
}

/// The tooltip an arrow leading to `label` has, with the keys of this
/// system and the keymap.
fn tip(cx: &mut TestAppContext, label: &str) -> String {
    cx.update(|cx| {
        let key = follow_key(cx);
        arrow_tip(label, cx.platform_info().is_mac(), key.as_deref())
    })
}

/// The value of the table column `name` in a row of the active data tab.
fn value(app: &Fixture, cx: &mut TestAppContext, row: usize, name: &str) -> Option<String> {
    let grid = app.data_grid(cx);
    cx.update(|cx| grid.read(cx).delegate().table_text(row, name))
}

/// The row of the active data tab whose `id` is `id`.
fn row_of(app: &Fixture, cx: &mut TestAppContext, id: &str) -> usize {
    (0..20)
        .find(|&row| value(app, cx, row, "id").as_deref() == Some(id))
        .unwrap_or_else(|| panic!("no row {id}"))
}

/// Where the active data tab's rows come from: its table and its filter.
fn shown(app: &Fixture, cx: &mut TestAppContext) -> (TableRef, Filter) {
    let tab = app.data(cx);
    cx.update(|cx| {
        let tab = tab.read(cx);
        (tab.table().clone(), tab.applied_filter().clone())
    })
}

/// Waits for a new data tab, the `count`-th tab, to read its rows.
fn opened(app: &Fixture, cx: &mut TestAppContext, count: usize) {
    app.wait_until(cx, "the referenced rows", |app, cx| {
        app.titles(cx).len() == count && app.data_state(cx).starts_with("Rows 1–")
    });
}

/// The rows the context menu's first jump of a row opens: its label, its
/// table and its filter.
fn menu_jump(app: &Fixture, cx: &mut TestAppContext, row: usize) -> (String, TableRef, Filter) {
    let tab = app.data(cx);
    let related = cx.update(|cx| tab.read(cx).related(row, cx));
    let first = related.into_iter().next().expect("a jump in the menu");
    (first.label, first.table, first.filter)
}

/// Acceptance 1 to 3 and 5 on PostgreSQL's fixture and the same on MySQL:
/// the mark of `customer_id`'s header, the arrow of a hovered and of a
/// selected cell, and the three gestures each opening what the cell menu
/// opens; a double click still edits, a click on the arrow neither edits nor
/// selects the row, shift and click still stretches a range.
#[gpui_kit::test]
fn a_foreign_key_is_marked_and_followed_from_its_cell(cx: &mut TestAppContext) {
    for (engine, schema) in [(Engine::Postgres, "shop"), (Engine::MySql, "kerno")] {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        let reopen = |app: &Fixture, cx: &mut TestAppContext| {
            app.open_table(cx, schema, "orders", RelationKind::Table);
            app.wait_until(cx, "the orders", |app, cx| {
                app.data_state(cx).starts_with("Rows 1–")
            });
        };
        reopen(&app, cx);
        // Seen: the second column (customer_id) carries the mark, the key
        // (id) none.
        let customers = format!("→ {schema}.customers (id)");
        assert_eq!(mark(&app, cx, 2).as_deref(), Some(customers.as_str()));
        assert_eq!(mark(&app, cx, 1), None);
        assert_eq!(mark(&app, cx, 3), None);

        // No arrow until a cell is under the pointer or selected; one cell
        // at a time.
        assert!(!app.has(cx, arrow(0, 2)));
        hover(&app, cx, cell(0, 2));
        assert!(app.has(cx, arrow(0, 2)) && !app.has(cx, arrow(1, 2)));
        let (label, table, filter) = menu_jump(&app, cx, 0);
        let customer = value(&app, cx, 0, "customer_id").unwrap();
        assert_eq!(
            label,
            format!("Open {schema}.customers where id = {customer}")
        );
        let expected = tip(cx, &label);
        assert!(expected.contains("-click or "), "{expected}");
        println!("the arrow's tooltip: {expected}");
        assert_eq!(said(&app, cx, arrow(0, 2)), Some(expected));
        // Another column's cell: no arrow anywhere.
        hover(&app, cx, cell(0, 3));
        assert!(!app.has(cx, arrow(0, 2)));
        // A selected cell keeps its arrow when the pointer leaves it.
        app.click(cx, cell(1, 2));
        hover(&app, cx, cell(3, 4));
        assert!(app.has(cx, arrow(1, 2)) && !app.has(cx, arrow(3, 2)));

        // A click on the arrow opens what the menu opens, in a tab of its own.
        let (_, _, row1) = menu_jump(&app, cx, 1);
        app.click(cx, arrow(1, 2));
        opened(&app, cx, 2);
        assert_eq!(shown(&app, cx), (table.clone(), row1));
        assert_eq!(app.titles(cx), ["orders", "customers"]);
        // The orders' grid kept its selection: the click was the arrow's.
        reopen(&app, cx);
        let grid = app.data_grid(cx);
        assert_eq!(
            cx.update(|cx| grid.read(cx).selection()),
            TableSelection::Cell(1, 2)
        );

        // ⌘-click (Ctrl-click elsewhere) on the value.
        let (_, _, row2) = menu_jump(&app, cx, 2);
        app.step(cx, |window, cx| {
            window.click_with_modifiers(cell(2, 2), Modifiers::secondary_key(cx), cx)
        });
        opened(&app, cx, 3);
        assert_eq!(shown(&app, cx), (table.clone(), row2));

        // The key, on the selected cell: ⌘↓ (Ctrl+Down elsewhere).
        reopen(&app, cx);
        app.click(cx, cell(0, 2));
        app.press(cx, &["secondary-down"]);
        opened(&app, cx, 4);
        assert_eq!(shown(&app, cx), (table.clone(), filter));
        // On a cell of no foreign key it says so and opens nothing.
        reopen(&app, cx);
        app.click(cx, cell(0, 3));
        let before = notification_count(&app, cx);
        app.press(cx, &["secondary-down"]);
        assert_eq!(app.titles(cx).len(), 4);
        assert_eq!(notification_count(&app, cx), before + 1);

        // Editing is not disturbed: a double click on the value edits it…
        app.step(cx, |window, cx| window.double_click(cell(4, 2), cx));
        let grid = app.data_grid(cx);
        assert_eq!(
            cx.update(|cx| grid.read(cx).delegate().editing()),
            Some((4, 1))
        );
        // …and the command, while typing, stays in the cell (Ctrl+Down on
        // Linux; the input takes ⌘↓ itself on a Mac)…
        let before = notification_count(&app, cx);
        app.dispatch(cx, actions::FollowForeignKey);
        assert_eq!(app.titles(cx).len(), 4);
        assert_eq!(notification_count(&app, cx), before);
        assert_eq!(
            cx.update(|cx| grid.read(cx).delegate().editing()),
            Some((4, 1))
        );
        app.press(cx, &["escape"]);
        // …a double click on the arrow opens one tab and edits nothing…
        hover(&app, cx, cell(5, 2));
        app.step(cx, |window, cx| window.double_click(arrow(5, 2), cx));
        opened(&app, cx, 5);
        reopen(&app, cx);
        let grid = app.data_grid(cx);
        assert_eq!(cx.update(|cx| grid.read(cx).delegate().editing()), None);
        assert_eq!(app.pending(cx), 0);
        // …and shift and click still stretches a range.
        app.click(cx, cell(0, 1));
        app.step(cx, |window, cx| {
            window.click_with_modifiers(cell(2, 2), Modifiers::shift(), cx)
        });
        let selected = cx.update(|cx| {
            let table = grid.read(cx);
            table.delegate().selected(table.selection())
        });
        assert_eq!(
            selected,
            Some(crate::query::grid::Selected {
                rows: 0..=2,
                columns: vec![0, 1]
            })
        );
    }
}

/// The scratch tables of one engine: `parents (id)`, `pairs (a, b)` and
/// `kids` pointing to both, a kid with neither.
fn family(engine: Engine) -> Scratch {
    let (cleanup, create) = scratch_schema(engine, "kerno_test_k49");
    Scratch::new(
        engine,
        &[&cleanup],
        &[
            &create,
            "CREATE TABLE kerno_test_k49.parents (id INT PRIMARY KEY, name VARCHAR(20))",
            "CREATE TABLE kerno_test_k49.pairs (a INT, b INT, name VARCHAR(20), PRIMARY KEY (a, b))",
            "CREATE TABLE kerno_test_k49.kids (id INT PRIMARY KEY, parent_id INT, pa INT, pb INT, \
             FOREIGN KEY (parent_id) REFERENCES kerno_test_k49.parents (id), \
             FOREIGN KEY (pa, pb) REFERENCES kerno_test_k49.pairs (a, b))",
            "INSERT INTO kerno_test_k49.parents VALUES (1, 'one'), (2, 'two')",
            "INSERT INTO kerno_test_k49.pairs VALUES (1, 1, 'one-one'), (1, 2, 'one-two')",
            "INSERT INTO kerno_test_k49.kids VALUES (1, 1, 1, 2), (2, NULL, NULL, NULL), (3, 2, 1, 1)",
        ],
    )
}

/// A key of two columns marks both and leads to both values; a NULL key
/// shows no arrow; a pending value is followed, a pending default is not;
/// the record view has the same arrows. PostgreSQL and MariaDB.
#[gpui_kit::test]
fn a_key_of_two_columns_null_keys_pending_values_and_the_record_view(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MariaDb] {
        if !testing::available(engine) {
            continue;
        }
        let _scratch = family(engine);
        let app = connected(cx, engine);
        let reopen = |app: &Fixture, cx: &mut TestAppContext| {
            app.open_table(cx, "kerno_test_k49", "kids", RelationKind::Table);
            app.wait_for_rows(cx, "Rows 1–3 of 3");
        };
        reopen(&app, cx);
        // Columns: id, parent_id, pa, pb.
        assert_eq!(
            mark(&app, cx, 2).as_deref(),
            Some("→ kerno_test_k49.parents (id)")
        );
        let pair = "(pa, pb) → kerno_test_k49.pairs (a, b)";
        assert_eq!(mark(&app, cx, 3).as_deref(), Some(pair));
        assert_eq!(mark(&app, cx, 4).as_deref(), Some(pair));

        // NULL: no arrow, hovered or selected.
        let empty = row_of(&app, cx, "2");
        hover(&app, cx, cell(empty, 2));
        assert!(!app.has(cx, arrow(empty, 2)));
        app.click(cx, cell(empty, 3));
        assert!(!app.has(cx, arrow(empty, 3)));

        // The key of two columns leads to both values, from either column.
        let first = row_of(&app, cx, "1");
        hover(&app, cx, cell(first, 4));
        let label = "Open kerno_test_k49.pairs where a = 1 and b = 2";
        assert_eq!(said(&app, cx, arrow(first, 4)), Some(tip(cx, label)));
        hover(&app, cx, cell(first, 3));
        app.click(cx, arrow(first, 3));
        opened(&app, cx, 2);
        assert_eq!(app.data_rows(cx), [["1", "2", "one-two"]]);
        let (table, filter) = shown(&app, cx);
        assert_eq!(table, TableRef::new("kerno_test_k49", "pairs"));
        assert_eq!(
            filter.conditions,
            [Condition::equals("a", "1"), Condition::equals("b", "2")]
        );

        // A pending value is followed (as the cell menu does); a pending
        // default, known once written, is not. Nothing is applied.
        reopen(&app, cx);
        app.type_in_cell(cx, first, 2, "2", "enter");
        hover(&app, cx, cell(first, 2));
        assert_eq!(
            said(&app, cx, arrow(first, 2)),
            Some(tip(cx, "Open kerno_test_k49.parents where id = 2"))
        );
        app.click(cx, cell(first, 2));
        app.on_data(cx, |tab, window, cx| {
            tab.set_selected(kerno_core::sql::CellValue::Default, window, cx)
        });
        hover(&app, cx, cell(first, 2));
        assert!(!app.has(cx, arrow(first, 2)));
        app.on_data(cx, |tab, _, cx| tab.discard(cx));
        assert_eq!(app.pending(cx), 0);

        // The record view: the fields of the keys have the arrow, the
        // others none; a NULL key none.
        app.click(cx, cell(first, 1));
        app.dispatch(cx, actions::ToggleRecordView);
        assert!(app.has(cx, "record-view"));
        assert!(!app.has(cx, ("record-fk-arrow", 0usize)));
        for field in 1..4usize {
            assert!(app.has(cx, ("record-fk-arrow", field)), "field {field}");
        }
        assert_eq!(
            said(&app, cx, ("record-fk-arrow", 1)),
            Some(tip(cx, "Open kerno_test_k49.parents where id = 1"))
        );
        app.click(cx, ("record-fk-arrow", 1usize));
        opened(&app, cx, 3);
        assert_eq!(app.data_rows(cx), [["1", "one"]]);
        reopen(&app, cx);
        let grid = app.data_grid(cx);
        app.step(cx, |_, cx| {
            grid.update(cx, |table, cx| table.set_selected_cell(empty, 1, cx))
        });
        assert!(app.has(cx, "record-view"));
        assert!(!app.has(cx, ("record-fk-arrow", 1usize)));
        app.dispatch(cx, actions::ToggleRecordView);
    }
}

/// Acceptance 4: a query's result grid marks and follows the foreign keys
/// of the one table its rows are from (PostgreSQL and MySQL name a result
/// column's table); a SQLite result names none and stays as it was, while a
/// SQLite table's data has both.
#[gpui_kit::test]
fn a_result_of_one_table_and_sqlite_follow_their_foreign_keys(cx: &mut TestAppContext) {
    for (engine, from) in [(Engine::Postgres, "shop.orders"), (Engine::MySql, "orders")] {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        app.run_in_new_tab(cx, &format!("select * from {from}"), "secondary-enter");
        assert_eq!(app.result_verdict(cx), None, "the rows are one table's");
        let schema = if engine == Engine::Postgres {
            "shop"
        } else {
            "kerno"
        };
        let customers = format!("→ {schema}.customers (id)");
        assert_eq!(mark(&app, cx, 2).as_deref(), Some(customers.as_str()));
        assert_eq!(mark(&app, cx, 1), None);
        let grid = app.result_grid(cx, 0);
        let customer = cx.update(|cx| grid.read(cx).delegate().table_text(0, "customer_id"));
        let label = format!("Open {schema}.customers where id = {}", customer.unwrap());
        hover(&app, cx, cell(0, 2));
        assert_eq!(said(&app, cx, arrow(0, 2)), Some(tip(cx, &label)));
        app.click(cx, arrow(0, 2));
        opened(&app, cx, 2);
        let (table, filter) = shown(&app, cx);
        assert_eq!(table, TableRef::new(schema, "customers"));
        assert_eq!(filter.conditions.len(), 1);
        assert_eq!(app.data_rows(cx).len(), 1);
    }

    let dir = tempfile::tempdir().unwrap();
    let file = fixture_file(dir.path());
    let app = connected_as(cx, file_config(&file));
    app.open_table(cx, "main", "orders", RelationKind::Table);
    app.wait_until(cx, "the orders", |app, cx| {
        app.data_state(cx).starts_with("Rows 1–")
    });
    let grid = app.data_grid(cx);
    let column = cx.update(|cx| {
        let grid = grid.read(cx).delegate();
        grid.layout()
            .iter()
            .position(|&source| grid.store().columns()[source].name == "customer_id")
            .unwrap()
            + 1
    });
    assert_eq!(
        mark(&app, cx, column).as_deref(),
        Some("→ main.customers (id)")
    );
    hover(&app, cx, cell(0, column));
    app.click(cx, arrow(0, column));
    opened(&app, cx, 2);
    assert_eq!(shown(&app, cx).0, TableRef::new("main", "customers"));
    assert_eq!(app.data_rows(cx).len(), 1);
    // A SQLite result.
    app.run_in_new_tab(cx, "SELECT * FROM orders", "secondary-enter");
    app.wait_until(cx, "the rows", |app, cx| {
        app.outcomes(cx)
            .iter()
            .any(|outcome| outcome.ends_with("rows"))
    });
    // It names its tables now (K59): the same mark.
    app.wait_until(cx, "the mark", |app, cx| {
        mark(app, cx, column).as_deref() == Some("→ main.customers (id)")
    });
}
