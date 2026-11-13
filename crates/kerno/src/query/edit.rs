//! Changing the rows of a grid that takes edits — a table's data, or the
//! result of a query whose rows are one table's — the same way from both
//! tabs: typing, NULL and defaults, paste, rows added, copied, deleted and
//! reverted, undo. Each tab says itself why rows that take no change
//! cannot be edited, and applies the changes on its own session.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::table::TableState;
use gpui_kit::{App, Entity, Keystroke, SharedString, Window};
use kerno_core::pending::{Pending, neighbour};
use kerno_core::sql::CellValue;
use kerno_core::{ResultStore, TypeKind};

use super::grid::Grid;
use super::viewer;

type Table = Entity<TableState<Grid>>;

/// The rows and columns of the result that are selected.
pub fn selection(table: &Table, cx: &App) -> Option<(Vec<usize>, Vec<usize>)> {
    let table = table.read(cx);
    let selected = table.delegate().selected(table.selection())?;
    Some((selected.rows.collect(), selected.columns))
}

/// Changes the pending changes and redraws. What is being typed is kept
/// first: a key that changes rows may come while a cell is open.
pub fn edit(table: &Table, cx: &mut App, edit: impl FnOnce(&mut Pending, &ResultStore)) {
    table.update(cx, |table, cx| {
        let grid = table.delegate_mut();
        grid.commit_edit(cx);
        grid.edit(edit);
        table.refresh(cx);
        cx.notify();
    });
}

/// Changes the pending changes, as one step that undo takes back.
pub fn change(table: &Table, cx: &mut App, change: impl FnOnce(&mut Pending, &ResultStore)) {
    edit(table, cx, |pending, store| {
        pending.step(|pending| change(pending, store))
    });
}

/// Takes the last change back. Returns whether there was one.
pub fn undo(table: &Table, cx: &mut App) -> bool {
    let mut undone = false;
    edit(table, cx, |pending, _| undone = pending.undo());
    undone
}

/// Drops the pending values of the selected cells. Returns whether there
/// was one.
pub fn revert_cells(table: &Table, cx: &mut App) -> bool {
    let Some((rows, columns)) = selection(table, cx) else {
        return false;
    };
    let mut reverted = false;
    change(table, cx, |pending, store| {
        for &row in &rows {
            for &column in &columns {
                reverted |= pending.revert_cell(store.row_count(), row, column);
            }
        }
    });
    reverted
}

/// Gives the selected cells the values of the row above (`above`) or
/// below, as DBeaver's Ctrl+D and Ctrl+Alt+D do: a block of rows takes its
/// first (last) row's. Columns the server fills are left alone, and so is
/// the key of a row that exists.
pub fn copy_from_row(table: &Table, above: bool, cx: &mut App) {
    let copy = {
        let state = table.read(cx);
        let grid = state.delegate();
        let fetched = grid.row_count();
        let generated = grid.generated_columns();
        grid.selected(state.selection()).and_then(|selected| {
            let from = neighbour(&selected.rows, above, grid.total_rows())?;
            let cells: Vec<(usize, usize)> = selected
                .rows
                .clone()
                .filter(|&row| row != from)
                .flat_map(|row| selected.columns.iter().map(move |&column| (row, column)))
                .filter(|&(row, column)| {
                    grid.takes_value(row, column)
                        && !generated.contains(&column)
                        && (row >= fetched || !grid.is_key(column))
                })
                .collect();
            Some((from, cells))
        })
    };
    if let Some((from, cells)) = copy {
        change(table, cx, |pending, store| {
            pending.copy_cells(store, from, cells)
        });
    }
}

/// Selects a row and shows it.
fn show_row(table: &Table, row: Option<usize>, cx: &mut App) {
    if let Some(row) = row {
        table.update(cx, |table, cx| {
            table.set_selected_row(row, cx);
            table.scroll_to_row(row, cx);
        });
    }
}

/// Adds an empty row at the end and shows it.
pub fn add_row(table: &Table, cx: &mut App) {
    let mut added = None;
    change(table, cx, |pending, store| {
        added = Some(pending.add_row(store))
    });
    show_row(table, added, cx);
}

/// The selected rows, or `row` when given.
fn rows_or(table: &Table, row: Option<usize>, cx: &App) -> Option<Vec<usize>> {
    row.map(|row| vec![row])
        .or_else(|| Some(selection(table, cx)?.0))
}

/// Adds copies of the selected rows (or of `row`) at the end. Columns the
/// server fills (keys it generates: a copy would collide) are left to it.
pub fn duplicate_rows(table: &Table, row: Option<usize>, cx: &mut App) {
    let Some(rows) = rows_or(table, row, cx) else {
        return;
    };
    let generated = table.read(cx).delegate().generated_columns();
    let mut last = None;
    change(table, cx, |pending, store| {
        for row in rows {
            last = pending.duplicate(store, row, &generated).or(last);
        }
    });
    show_row(table, last, cx);
}

/// Marks the selected rows (or `row`) for deletion.
pub fn delete_rows(table: &Table, row: Option<usize>, cx: &mut App) {
    if let Some(rows) = rows_or(table, row, cx) {
        change(table, cx, |pending, store| {
            pending.delete(store.row_count(), rows)
        });
    }
}

/// Takes a row back to what was fetched.
pub fn revert_row(table: &Table, row: usize, cx: &mut App) {
    change(table, cx, |pending, _| pending.revert(row));
}

/// Forgets every pending change.
pub fn discard(table: &Table, cx: &mut App) {
    change(table, cx, |pending, _| pending.clear());
}

/// Gives the selected cells `value`; with `nullable_only`, not those of a
/// NOT NULL column. Cells that take no value are left alone.
pub fn set_cells(table: &Table, value: CellValue, nullable_only: bool, cx: &mut App) {
    let Some((rows, columns)) = selection(table, cx) else {
        return;
    };
    let cells: Vec<(usize, usize)> = {
        let grid = table.read(cx).delegate();
        rows.iter()
            .flat_map(|&row| columns.iter().map(move |&column| (row, column)))
            .filter(|&(row, column)| {
                grid.takes_value(row, column) && (!nullable_only || grid.nullable(column))
            })
            .collect()
    };
    change(table, cx, |pending, store| {
        for (row, column) in cells {
            pending.set(store, row, column, value.clone());
        }
    });
}

/// Puts the text of the clipboard in the selected cells: one value in each,
/// or a block of tab-separated cells from the first one. Each pasted cell
/// is a pending change, as a typed one is.
pub fn paste(table: &Table, text: &str, cx: &mut App) {
    let cells = {
        let state = table.read(cx);
        let Some(selected) = state.delegate().selected(state.selection()) else {
            return;
        };
        state.delegate().pasted(&selected, text)
    };
    change(table, cx, |pending, store| {
        for (row, column, value) in cells {
            pending.set(store, row, column, value);
        }
    });
}

/// Types in a cell of the grid; one that cannot be typed in shows its
/// whole value instead.
pub fn edit_cell(table: &Table, row: usize, column: usize, window: &mut Window, cx: &mut App) {
    let editing = table.update(cx, |table, cx| {
        table.delegate_mut().begin_edit(row, column, window, cx)
    });
    if !editing {
        let grid = table.read(cx).delegate();
        if let Some((column, value)) = grid.value_at(row, column) {
            let (column, value) = (column.clone(), value.map(<[u8]>::to_vec));
            viewer::open(&column, value.as_deref(), window, cx);
        }
    }
}

/// A double click on a cell: types in it, or shows the whole value of one
/// that cannot be typed in. A boolean that takes a value is toggled by the
/// click on it already selected, not by a double click.
pub fn double_click(table: &Table, row: usize, column: usize, window: &mut Window, cx: &mut App) {
    let grid = table.read(cx).delegate();
    let toggled = grid
        .value_at(row, column)
        .is_some_and(|(column, _)| column.kind == TypeKind::Boolean)
        && grid.editable(row, column).is_ok();
    if !toggled {
        edit_cell(table, row, column, window, cx);
    }
}

/// Whether rows take changes: `Ok`, or why not when there is something
/// to say.
pub type Allowed = Result<(), Option<SharedString>>;

/// Says why the rows take no change, when they do not.
fn allowed(allowed: &Allowed, window: &mut Window, cx: &mut App) -> bool {
    if let Err(Some(why)) = allowed {
        window.push_notification(Notification::warning(why.clone()), cx);
    }
    allowed.is_ok()
}

/// What a key typed in the grid does to its rows, the grid focused: paste,
/// Delete and Backspace set NULL, Enter and F2 type in the selected cell,
/// Space toggles a boolean. Returns whether it was one of those keys, which
/// the caller then stops; `edits` says whether the rows take changes.
pub fn on_key(
    table: &Table,
    keystroke: &Keystroke,
    edits: Allowed,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let secondary = keystroke.modifiers.secondary(cx) && !keystroke.modifiers.shift;
    match keystroke.key.as_str() {
        "v" if secondary => {
            let text = cx.read_from_clipboard().and_then(|item| item.text());
            if let Some(text) = text.filter(|text| !text.is_empty())
                && allowed(&edits, window, cx)
            {
                paste(table, &text, cx);
            }
            true
        }
        "delete" | "backspace" if !keystroke.modifiers.modified() => {
            if allowed(&edits, window, cx) {
                set_cells(table, CellValue::Null, true, cx);
            }
            true
        }
        "enter" | "f2" if !keystroke.modifiers.modified() => match table.read(cx).selected_cell() {
            Some((row, column)) => {
                edit_cell(table, row, column, window, cx);
                true
            }
            None => false,
        },
        // Space toggles a boolean, as a click on DBeaver's checkbox does.
        "space" if !keystroke.modifiers.modified() => {
            let boolean = table.read(cx).selected_cell().filter(|&(row, column)| {
                let grid = table.read(cx).delegate();
                grid.value_at(row, column)
                    .is_some_and(|(described, _)| described.kind == TypeKind::Boolean)
                    && grid.editable(row, column).is_ok()
            });
            if let Some((row, column)) = boolean {
                edit_cell(table, row, column, window, cx);
            }
            boolean.is_some()
        }
        _ => false,
    }
}
