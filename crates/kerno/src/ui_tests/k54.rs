//! K54 (the pass in the real app, 2026-11-23): a foreign key's arrow
//! (K49) is drawn only while the pointer is over its very cell; the Edit
//! menu has the system's editing entries. Fixtures read only.

use gpui_kit::{InputEvent as _, MouseExitEvent, MouseMoveEvent, Pixels, Point};

use super::*;

/// The kit's cell of a row and a column of the grid (Kerno's own cell
/// fills it).
fn cell(row: usize, column: usize) -> gpui_kit::ElementId {
    gpui_kit::ElementId::Name(format!("table-cell-{row}:{column}").into())
}

fn arrow(row: usize, column: usize) -> (&'static str, usize) {
    ("fk-arrow", (row << 12) | column)
}

fn hover(app: &Fixture, cx: &mut TestAppContext, id: impl Into<gpui_kit::ElementId>) {
    app.step(cx, |window, cx| window.hover(id, cx));
}

/// The rows of column 2 (`customer_id`) whose arrow is drawn.
fn arrows(app: &Fixture, cx: &mut TestAppContext) -> Vec<usize> {
    (0..12).filter(|&row| app.has(cx, arrow(row, 2))).collect()
}

/// Where an element is now, in the window.
fn center(
    app: &Fixture,
    cx: &mut TestAppContext,
    id: impl Into<gpui_kit::ElementId>,
) -> Point<Pixels> {
    app.step(cx, |window, _| window.find(id).bounds().center())
}

/// Draws a frame, then moves the pointer, or takes it out of the window:
/// the cell whose arrow shows right after the event, before anything is
/// drawn again (the app draws nothing for a move that changes nothing on
/// screen; a test's own moves draw a frame each).
fn bare_pointer(
    app: &Fixture,
    cx: &mut TestAppContext,
    grid: &Entity<TableState<Grid>>,
    to: Option<Point<Pixels>>,
) -> Option<(usize, usize)> {
    cx.update_window(app.window.into(), |_, window, cx| {
        window.render_frame(cx);
        let modifiers = window.modifiers();
        let event = match to {
            Some(position) => MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers,
            }
            .to_platform_input(),
            None => MouseExitEvent {
                position: window.mouse_position(),
                pressed_button: None,
                modifiers,
            }
            .to_platform_input(),
        };
        window.dispatch_event(event, cx);
        grid.read(cx).delegate().hovered()
    })
    .unwrap()
}

fn hovered(grid: &Entity<TableState<Grid>>, cx: &mut TestAppContext) -> Option<(usize, usize)> {
    cx.update(|cx| grid.read(cx).delegate().hovered())
}

/// The owner's path, on a query's result: hover row 4's `customer_id`,
/// click its arrow (the customers open in a tab), the pointer goes to the
/// editor's place, the result's tab is shown again: no arrow. Crossing the
/// grid over another column shows none; another key's cell has its own.
#[gpui_kit::test]
fn the_arrow_is_gone_when_its_tab_is_shown_again(cx: &mut TestAppContext) {
    for (engine, from) in [(Engine::Postgres, "shop.orders"), (Engine::MySql, "orders")] {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        app.run_in_new_tab(
            cx,
            &format!("select * from {from} limit 50"),
            "secondary-enter",
        );
        assert_eq!(app.result_verdict(cx), None, "the rows are one table's");
        hover(&app, cx, cell(4, 2));
        assert_eq!(arrows(&app, cx), [4]);
        let editor = center(&app, cx, "query-editor");
        app.click(cx, arrow(4, 2));
        app.wait_until(cx, "the customers", |app, cx| {
            app.titles(cx).len() == 2 && app.data_state(cx).starts_with("Rows 1–")
        });
        // Work elsewhere, then back to the result.
        app.step(cx, |window, cx| window.simulate_mouse_move(editor, cx));
        app.dispatch(cx, actions::PreviousTab);
        assert_eq!(app.active(cx), Some(0));
        assert_eq!(arrows(&app, cx), Vec::<usize>::new());
        // Across the grid over a column of no key: still none.
        hover(&app, cx, cell(4, 3));
        hover(&app, cx, cell(5, 1));
        assert_eq!(arrows(&app, cx), Vec::<usize>::new());
        // Another key's cell: its arrow only, gone on the header.
        hover(&app, cx, cell(6, 2));
        assert_eq!(arrows(&app, cx), [6]);
        hover(&app, cx, ("column-link", 2usize));
        assert_eq!(arrows(&app, cx), Vec::<usize>::new());
    }
}

/// The ways out with the hover heard all along (K49's own listener, and
/// the watch of the hovered cell): to a cell of no key, to another key's
/// cell, to a header, out of the grid, out of the window.
#[gpui_kit::test]
fn the_arrow_leaves_with_the_pointer(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_until(cx, "the orders", |app, cx| {
        app.data_state(cx).starts_with("Rows 1–")
    });
    hover(&app, cx, cell(0, 2));
    assert_eq!(arrows(&app, cx), [0]);
    hover(&app, cx, cell(0, 3));
    assert_eq!(arrows(&app, cx), Vec::<usize>::new());
    hover(&app, cx, cell(1, 2));
    assert_eq!(arrows(&app, cx), [1]);
    hover(&app, cx, cell(2, 2));
    assert_eq!(arrows(&app, cx), [2]);
    hover(&app, cx, ("column-link", 2usize));
    assert_eq!(arrows(&app, cx), Vec::<usize>::new());
    hover(&app, cx, cell(3, 2));
    hover(&app, cx, "data-table-name");
    assert_eq!(arrows(&app, cx), Vec::<usize>::new());
    hover(&app, cx, cell(4, 2));
    let grid = app.data_grid(cx);
    bare_pointer(&app, cx, &grid, None);
    cx.run_until_parked();
    assert_eq!(arrows(&app, cx), Vec::<usize>::new());
}

/// After an edit closed with Escape (a key: the cell's own listener hears
/// nothing more until the pointer comes back), the pointer leaving the
/// cell, or the window, takes the arrow away at once, with no frame drawn
/// in between; rows scrolled under a still pointer take it too.
#[gpui_kit::test]
fn the_arrow_leaves_after_an_edit_and_under_scrolled_rows(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_until(cx, "the orders", |app, cx| {
        app.data_state(cx).starts_with("Rows 1–")
    });
    let grid = app.data_grid(cx);
    // Double-click, Escape, the selection moved off the cell by a key: the
    // pointer is still there, the arrow is its.
    let edit = |app: &Fixture, cx: &mut TestAppContext, row: usize| {
        app.step(cx, |window, cx| window.double_click(cell(row, 2), cx));
        let grid = app.data_grid(cx);
        assert_eq!(
            cx.update(|cx| grid.read(cx).delegate().editing()),
            Some((row, 1))
        );
        app.press(cx, &["escape", "left"]);
        assert_eq!(arrows(app, cx), [row]);
    };

    // To the next cell, a column of no key.
    edit(&app, cx, 4);
    let next = center(&app, cx, cell(4, 3));
    assert_eq!(bare_pointer(&app, cx, &grid, Some(next)), None);
    assert_eq!(arrows(&app, cx), Vec::<usize>::new());

    // Out of the window.
    edit(&app, cx, 5);
    assert_eq!(bare_pointer(&app, cx, &grid, None), None);
    assert_eq!(arrows(&app, cx), Vec::<usize>::new());

    // Two rows scrolled under the still pointer.
    edit(&app, cx, 4);
    let height = app.step(cx, |window, _| window.find(cell(4, 2)).bounds().size.height);
    // The frame drawn for the scroll looks for the pointer in the cell.
    cx.update_window(app.window.into(), |_, window, cx| {
        grid.update(cx, |table, cx| {
            let handle = table.vertical_scroll_handle.0.borrow().base_handle.clone();
            handle.set_offset(gpui_kit::point(px(0.), -height * 2.));
            cx.notify();
        });
        window.render_frame(cx);
    })
    .unwrap();
    assert_eq!(hovered(&grid, cx), None);
    assert_eq!(arrows(&app, cx), Vec::<usize>::new());
}
