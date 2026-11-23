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

/// The entries the Edit menu starts with, in order ("-" for a separator),
/// with the system action each stands for.
fn edit_menu(cx: &mut TestAppContext) -> Vec<(String, &'static str)> {
    use gpui_kit::OsAction;
    let menus = cx.update(|cx| cx.get_menus()).unwrap();
    let edit = menus.iter().find(|menu| menu.name == "Edit").unwrap();
    edit.items
        .iter()
        .take(9)
        .map(|item| match item {
            gpui_kit::OwnedMenuItem::Action {
                name, os_action, ..
            } => (
                name.to_string(),
                match os_action {
                    Some(OsAction::Undo) => "undo",
                    Some(OsAction::Redo) => "redo",
                    Some(OsAction::Cut) => "cut",
                    Some(OsAction::Copy) => "copy",
                    Some(OsAction::Paste) => "paste",
                    Some(OsAction::SelectAll) => "select all",
                    None => "",
                },
            ),
            _ => ("-".to_string(), ""),
        })
        .collect()
}

fn menu_action(cx: &mut TestAppContext, name: &str) -> Box<dyn gpui_kit::Action> {
    let menus = cx.update(|cx| cx.get_menus()).unwrap();
    let edit = menus.iter().find(|menu| menu.name == "Edit").unwrap();
    edit.items
        .iter()
        .find_map(|item| match item {
            gpui_kit::OwnedMenuItem::Action {
                name: found,
                action,
                ..
            } if found.as_str() == name => Some(action.boxed_clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {name} in the Edit menu"))
}

/// Whether the entry is enabled, as the menu bar asks before it opens
/// (`is_action_available`, what macOS's menu validation calls).
fn enabled(cx: &mut TestAppContext, name: &str) -> bool {
    let action = menu_action(cx, name);
    cx.update(|cx| cx.is_action_available(action.as_ref()))
}

/// The Edit menu's entry `name` run as the menu bar runs it: enabled, then
/// its action given to the window that has the keyboard
/// (`cx.dispatch_action`, what GPUI's menu callback calls).
fn from_menu(app: &Fixture, cx: &mut TestAppContext, name: &str) {
    assert_eq!(
        cx.update(|cx| cx.active_window()),
        Some(app.window.into()),
        "the menu acts on this window"
    );
    assert!(enabled(cx, name), "{name} is greyed");
    let action = menu_action(cx, name);
    cx.update(|cx| cx.dispatch_action(action.as_ref()));
    cx.run_until_parked();
}

/// The Edit menu starts with Undo, Redo, Cut, Copy, Paste and Select All,
/// grouped as on a Mac and showing the text fields' keys; each acts on the
/// SQL editor, on a field of one line and on a grid (its Copy, Paste and
/// Select All), and is greyed where nothing focused takes it.
#[gpui_kit::test]
fn the_edit_menu_has_the_systems_editing_entries(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let expected = [
        ("Undo", "undo"),
        ("Redo", "redo"),
        ("-", ""),
        ("Cut", "cut"),
        ("Copy", "copy"),
        ("Paste", "paste"),
        ("Select All", "select all"),
        ("-", ""),
        ("Find…", ""),
    ]
    .map(|(name, os)| (name.to_string(), os));
    assert_eq!(edit_menu(cx), expected);
    // The key each shows on a Mac: the first binding of its action, which
    // is what the menu bar reads.
    if cx.update(|cx| cx.platform_info().is_mac()) {
        for (name, key) in [
            ("Undo", "cmd-z"),
            ("Redo", "cmd-shift-z"),
            ("Cut", "cmd-x"),
            ("Copy", "cmd-c"),
            ("Paste", "cmd-v"),
            ("Select All", "cmd-a"),
        ] {
            let action = menu_action(cx, name);
            let shown = cx.update(|cx| {
                let keymap = cx.key_bindings();
                let keymap = keymap.borrow();
                keymap
                    .bindings_for_action(action.as_ref())
                    .next()
                    .map(|binding| binding.keystrokes()[0].unparse(gpui_kit::OperatingSystem::Mac))
            });
            assert_eq!(shown.as_deref(), Some(key), "{name}");
        }
    }
    drop(app);
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    // The menu bar acts on the active window: the test platform's is
    // made active, as the app's is when it is in front.
    app.step(cx, |window, _| window.activate_window());

    // The SQL editor.
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 1");
    from_menu(&app, cx, "Undo");
    assert_eq!(app.active_query(cx).1, "");
    from_menu(&app, cx, "Redo");
    assert_eq!(app.active_query(cx).1, "select 1");
    from_menu(&app, cx, "Select All");
    from_menu(&app, cx, "Cut");
    assert_eq!(app.active_query(cx).1, "");
    assert_eq!(app.clipboard(cx).as_deref(), Some("select 1"));
    from_menu(&app, cx, "Paste");
    from_menu(&app, cx, "Paste");
    assert_eq!(app.active_query(cx).1, "select 1select 1");
    from_menu(&app, cx, "Select All");
    from_menu(&app, cx, "Copy");
    assert_eq!(app.clipboard(cx).as_deref(), Some("select 1select 1"));

    // A field of one line: the schema tree's filter.
    let tree = cx.update(|cx| app.workspace.read(cx).navigator(cx));
    let text = |cx: &mut TestAppContext| cx.update(|cx| tree.read(cx).filter_text(cx));
    app.dispatch(cx, actions::FocusNavigatorFilter);
    assert!(app.step(cx, |window, cx| tree.read(cx).filter_focused(window, cx)));
    app.type_text(cx, "zz");
    from_menu(&app, cx, "Undo");
    assert_eq!(text(cx), "");
    from_menu(&app, cx, "Redo");
    assert_eq!(text(cx), "zz");
    from_menu(&app, cx, "Select All");
    from_menu(&app, cx, "Cut");
    assert_eq!(text(cx), "");
    from_menu(&app, cx, "Paste");
    assert_eq!(text(cx), "zz");
    from_menu(&app, cx, "Select All");
    from_menu(&app, cx, "Copy");
    assert_eq!(app.clipboard(cx).as_deref(), Some("zz"));
    app.press(cx, &["secondary-a", "backspace"]);

    // A table's grid: Copy, Select All and Paste (a pending change,
    // discarded); Undo, Redo and Cut greyed there.
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_until(cx, "the orders", |app, cx| {
        app.data_state(cx).starts_with("Rows 1–")
    });
    app.click(cx, cell(0, 1));
    from_menu(&app, cx, "Copy");
    let first = app.data_rows(cx)[0][0].clone();
    assert_eq!(app.clipboard(cx), Some(first));
    for name in ["Undo", "Redo", "Cut"] {
        assert!(!enabled(cx, name), "{name} is not greyed on the grid");
    }
    from_menu(&app, cx, "Select All");
    from_menu(&app, cx, "Copy");
    let all = app.clipboard(cx).unwrap();
    assert_eq!(all.lines().count(), app.data_rows(cx).len(), "{all}");
    let customer = app.data_rows(cx)[0][1].clone();
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(format!("{customer}0")));
    app.click(cx, cell(0, 2));
    from_menu(&app, cx, "Paste");
    assert_eq!(app.pending(cx), 1);
    app.on_data(cx, |tab, _, cx| tab.discard(cx));
    assert_eq!(app.pending(cx), 0);

    // A query's result grid: the same three. The pending change is left
    // unapplied.
    app.run_in_new_tab(
        cx,
        "select * from shop.orders order by id limit 3",
        "secondary-enter",
    );
    assert_eq!(app.result_verdict(cx), None, "the rows are one table's");
    let grid = app.result_grid(cx, 0);
    let value = |cx: &mut TestAppContext, name: &str| {
        cx.update(|cx| grid.read(cx).delegate().table_text(0, name))
            .unwrap()
    };
    app.click(cx, cell(0, 1));
    from_menu(&app, cx, "Copy");
    assert_eq!(app.clipboard(cx), Some(value(cx, "id")));
    from_menu(&app, cx, "Select All");
    from_menu(&app, cx, "Copy");
    assert_eq!(app.clipboard(cx).unwrap().lines().count(), 3);
    let customer = value(cx, "customer_id");
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(format!("{customer}0")));
    app.click(cx, cell(0, 2));
    from_menu(&app, cx, "Paste");
    let pending = cx.update(|cx| grid.read(cx).delegate().pending().map(|p| p.count()));
    assert_eq!(pending, Some(1));

    // Nothing focused that edits: greyed, but Copy, which the window keeps
    // for text selected in its labels (the kit's root).
    let workspace = app.workspace.clone();
    app.step(cx, |window, cx| {
        workspace.read(cx).focus_handle(cx).focus(window, cx)
    });
    for name in ["Undo", "Redo", "Cut", "Paste", "Select All"] {
        assert!(!enabled(cx, name), "{name} is not greyed");
    }
    assert!(enabled(cx, "Copy"));
}
