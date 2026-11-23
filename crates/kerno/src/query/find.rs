//! Find in the fetched rows of a grid, results and table data alike (J6;
//! DBeaver #38133, Beekeeper #524, #941): a bar above the grid with the
//! text, the count, next and previous, case, and "matching rows only".
//!
//! The cells are matched off the UI thread on a snapshot of the store, as
//! many matching cells per fetched row (two bytes a row); rows that arrive
//! later are matched on their own. Highlights are worked out for the cells
//! on screen only. "Matching rows only" is a row mapping in the order shown,
//! on top of the client-side sort.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::table::TableState;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, Styled as _, Subscription, Task,
    TestSupportExt as _, Window, div, px,
};
use kerno_core::find::{Matcher, count_rows, matching_rows};

use super::grid::{Grid, MAX_CELL_CHARS, current};
use super::grid_view::StopOnDrop;

/// Scans of up to this many cells run at once; larger ones off the UI
/// thread.
const INLINE_CELLS: usize = 20_000;

/// What the counts of a find are of.
#[derive(Clone, PartialEq)]
struct Of {
    matcher: Matcher,
    /// The columns of the result that are shown.
    columns: Vec<usize>,
    /// The grid's generation, for a grid whose cells take pending values.
    edits: Option<u64>,
}

/// What a grid's find bar holds: the text, the options, and the matches.
pub struct Find {
    input: Entity<InputState>,
    case: bool,
    only: bool,
    of: Option<Of>,
    /// Matching cells per fetched row, as many rows as were scanned.
    counts: Arc<[u16]>,
    total: u64,
    /// The rows shown while "matching rows only" is on, in the order shown.
    rows: Option<Arc<[u32]>>,
    /// Where each of `rows` stands among every row in the sort, when
    /// sorted (its number unfiltered); unsorted, it is the row itself.
    numbers: Option<Arc<[u32]>>,
    /// The sort `rows` follows.
    rows_order: Option<Arc<[u32]>>,
    /// The match last moved to (row and column of the grid), and its rank.
    current: Option<(usize, usize, u64)>,
    job: Option<(Task<()>, StopOnDrop)>,
    _subscriptions: Vec<Subscription>,
}

/// The text and options of a find, kept while a table's page is read
/// again in a new grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carried {
    pub text: String,
    pub case: bool,
    pub only: bool,
}

impl Find {
    /// The rows shown in its place while "matching rows only" is on.
    pub(super) fn rows(&self) -> Option<&Arc<[u32]>> {
        self.rows.as_ref().filter(|_| self.only)
    }

    /// The number a row of the grid has without the filter (from 1), while
    /// "matching rows only" is on.
    pub(super) fn number(&self, row: usize) -> Option<usize> {
        let rows = self.rows()?;
        let at = match &self.numbers {
            Some(numbers) => numbers.get(row),
            None => rows.get(row),
        };
        at.map(|&at| at as usize + 1)
    }

    /// Whether a cell (a fetched row, `cell` its value) is a match, and
    /// whether it is the current one; `None` when it is no match.
    pub(super) fn state(
        &self,
        source_row: usize,
        cell: Option<&[u8]>,
        column: &kerno_core::Column,
        at: (usize, usize),
    ) -> Option<bool> {
        let of = self.of.as_ref()?;
        let counted = self.counts.get(source_row).is_some_and(|&count| count > 0);
        (counted && of.matcher.matches(cell, column)).then(|| {
            self.current
                .is_some_and(|(row, column, _)| (row, column) == at)
        })
    }

    /// The text and options, to carry to a new grid.
    pub fn carried(&self, cx: &App) -> Carried {
        Carried {
            text: self.input.read(cx).value().to_string(),
            case: self.case,
            only: self.only,
        }
    }
}

type Table = Entity<TableState<Grid>>;

/// The text and options of a grid's find, when it is open.
pub fn carried(table: &Table, cx: &App) -> Option<Carried> {
    table
        .read(cx)
        .delegate()
        .find
        .as_ref()
        .map(|find| find.carried(cx))
}

/// Opens the find bar of a grid (Cmd+F, "Find in results…"), or puts the
/// keyboard back in it with its text selected.
pub fn open(table: &Table, window: &mut Window, cx: &mut App) {
    let carried = table
        .read(cx)
        .delegate()
        .find
        .as_ref()
        .map(|find| find.carried(cx));
    if carried.is_none() {
        restore(
            table,
            Carried {
                text: String::new(),
                case: false,
                only: false,
            },
            window,
            cx,
        );
    }
    let Some(input) = table
        .read(cx)
        .delegate()
        .find
        .as_ref()
        .map(|find| find.input.clone())
    else {
        return;
    };
    input.update(cx, |input, cx| {
        input.focus(window, cx);
        input.select_all(window, cx);
    });
}

/// Puts a find bar with this text and these options on a grid, and finds.
pub fn restore(table: &Table, carried: Carried, window: &mut Window, cx: &mut App) {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Find in the fetched rows")
            .default_value(carried.text)
    });
    table.update(cx, |state, cx| {
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |state, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => {
                        if let Some(find) = &mut state.delegate_mut().find {
                            find.current = None;
                        }
                        follow(state, cx);
                    }
                    InputEvent::PressEnter { shift, .. } => step(state, !shift, cx),
                    _ => {}
                },
            ),
            // Rows that arrive, a sort, a column hidden or an edit change
            // what matches or where.
            cx.observe_self(follow),
        ];
        state.delegate_mut().find = Some(Find {
            input,
            case: carried.case,
            only: carried.only,
            of: None,
            counts: Arc::from([]),
            total: 0,
            rows: None,
            numbers: None,
            rows_order: None,
            current: None,
            job: None,
            _subscriptions: subscriptions,
        });
        follow(state, cx);
        cx.notify();
    });
}

/// Closes the find bar: every row shows again, and the grid takes the
/// keyboard.
pub fn close(table: &Table, window: &mut Window, cx: &mut App) {
    table.update(cx, |state, cx| {
        state.delegate_mut().find = None;
        state.focus_handle(cx).focus(window, cx);
        cx.notify();
    });
}

/// Turns "matching rows only" on or off. It cannot be turned on over
/// pending changes: they are kept by row as fetched.
pub fn set_only(table: &Table, only: bool, window: &mut Window, cx: &mut App) {
    let refused = table.update(cx, |state, cx| {
        let pending = state.delegate().has_pending();
        let Some(find) = &mut state.delegate_mut().find else {
            return false;
        };
        if only && pending {
            return true;
        }
        find.only = only;
        find.current = None;
        follow(state, cx);
        cx.notify();
        false
    });
    if refused {
        window.push_notification(
            Notification::warning(
                "Apply or discard the pending changes before showing matching rows only.",
            ),
            cx,
        );
    }
}

/// Tells upper and lower case apart, or not.
pub fn set_case(table: &Table, case: bool, cx: &mut App) {
    table.update(cx, |state, cx| {
        if let Some(find) = &mut state.delegate_mut().find {
            find.case = case;
            find.current = None;
        }
        follow(state, cx);
        cx.notify();
    });
}

/// Moves to the next match (or the previous), in the order shown.
pub fn next(table: &Table, forward: bool, cx: &mut App) {
    table.update(cx, |state, cx| step(state, forward, cx));
}

/// Gives the keyboard back to the bar's text after a click on one of its
/// buttons: Enter, Shift+Enter and Escape keep working.
fn refocus(table: &Table, window: &mut Window, cx: &mut App) {
    let input = table
        .read(cx)
        .delegate()
        .find
        .as_ref()
        .map(|find| find.input.clone());
    if let Some(input) = input {
        input.update(cx, |input, cx| input.focus(window, cx));
    }
}

/// What the bar should count now, given the grid.
fn wanted(grid: &Grid, cx: &App) -> Option<Of> {
    let find = grid.find.as_ref()?;
    let text = find.input.read(cx).value().to_string();
    Some(Of {
        matcher: Matcher::new(&text, find.case, MAX_CELL_CHARS)?,
        columns: grid.layout().to_vec(),
        edits: grid.has_edits().then(|| grid.generation()),
    })
}

/// Brings the counts and the shown rows up to date with the text, the
/// options, the rows fetched and their order; small grids at once, large
/// ones off the UI thread, one scan at a time.
fn follow(state: &mut TableState<Grid>, cx: &mut Context<TableState<Grid>>) {
    let grid = state.delegate();
    let Some(find) = &grid.find else {
        return;
    };
    let wanted = wanted(grid, cx);
    let fetched = grid.store().row_count();
    let order = grid.sort_order();
    if wanted.is_none() && find.of.is_none() {
        return;
    }
    let same = find.of == wanted;
    if !same {
        // Something else to find: what was counted is of no use.
        let find = state.delegate_mut().find.as_mut().expect("a find");
        find.job = None;
        find.of = wanted.clone();
        find.counts = Arc::from([]);
        find.total = 0;
        find.rows = None;
        find.numbers = None;
        find.rows_order = None;
        find.current = None;
        if wanted.is_none() {
            cx.notify();
            return;
        }
    } else if find.job.is_some() {
        // The scan running ends first; what it missed comes next.
        return;
    }
    let grid = state.delegate();
    let find = grid.find.as_ref().expect("a find");
    let from = find.counts.len();
    let mapped = !find.only
        || (find.rows.is_some()
            && from == fetched
            && same_order(find.rows_order.as_ref(), order.as_ref()));
    if from >= fetched && mapped {
        return;
    }
    let Some(of) = find.of.clone() else {
        return;
    };
    let only = find.only;
    // Pending values are matched as shown, here: a table's page is small.
    // A small result is matched at once; a large one off the UI thread,
    // even when only its rows are put in order again.
    let pending = grid.has_pending();
    if fetched * of.columns.len() <= INLINE_CELLS || pending {
        let columns = grid.store().columns().clone();
        // With changes pending, rows are shown as fetched (no sort here),
        // and with their pending values.
        let value = |row: usize, column: usize| {
            if pending {
                grid.cell(row, column)
            } else {
                grid.store().cell(row, column)
            }
        };
        let mut counts = find.counts.to_vec();
        counts.extend((from..fetched).map(|row| {
            let matched = of
                .columns
                .iter()
                .filter(|&&column| of.matcher.matches(value(row, column), &columns[column]))
                .count();
            u16::try_from(matched).unwrap_or(u16::MAX)
        }));
        let total = counts.iter().map(|&count| u64::from(count)).sum();
        let rows = only.then(|| shown(&counts, order.as_deref()));
        settle(state, counts.into(), total, rows, order, cx);
        return;
    }
    let (store, prefix) = (grid.store().clone(), find.counts.clone());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let snapshot = order.clone();
    let scanning = crate::bridge::spawn_blocking(cx, move || {
        let added = count_rows(&store, &of.columns, &of.matcher, from..fetched, &stopped)?;
        let mut counts = Vec::with_capacity(fetched);
        counts.extend_from_slice(&prefix);
        counts.extend(added);
        let total = counts.iter().map(|&count| u64::from(count)).sum::<u64>();
        let rows = only.then(|| shown(&counts, snapshot.as_deref()));
        Some((Arc::<[u16]>::from(counts), total, rows))
    });
    let task = cx.spawn(async move |table, cx| {
        if let Some((counts, total, rows)) = scanning.await {
            let _ = table.update(cx, |state, cx| {
                if let Some(find) = &mut state.delegate_mut().find {
                    find.job = None;
                }
                settle(state, counts, total, rows, order, cx);
            });
        }
    });
    let find = state.delegate_mut().find.as_mut().expect("a find");
    find.job = Some((task, StopOnDrop(stop)));
    cx.notify();
}

/// The rows "matching rows only" shows, and their numbers when sorted.
type Shown = (Arc<[u32]>, Option<Arc<[u32]>>);

/// The matching rows in `order`, with where each stands in it.
fn shown(counts: &[u16], order: Option<&[u32]>) -> Shown {
    let rows = Arc::from(matching_rows(counts, order));
    let numbers = order.map(|order| {
        order
            .iter()
            .enumerate()
            .filter(|&(_, &row)| counts.get(row as usize).is_some_and(|&count| count > 0))
            .map(|(at, _)| u32::try_from(at).unwrap_or(u32::MAX))
            .collect()
    });
    (rows, numbers)
}

/// Whether two sorts are the same one.
fn same_order(a: Option<&Arc<[u32]>>, b: Option<&Arc<[u32]>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// Keeps what a scan found, and draws it.
fn settle(
    state: &mut TableState<Grid>,
    counts: Arc<[u16]>,
    total: u64,
    rows: Option<Shown>,
    order: Option<Arc<[u32]>>,
    cx: &mut Context<TableState<Grid>>,
) {
    let Some(find) = &mut state.delegate_mut().find else {
        return;
    };
    find.counts = counts;
    find.total = total;
    (find.rows, find.numbers) = rows.map_or((None, None), |(rows, numbers)| (Some(rows), numbers));
    find.rows_order = order;
    // The rank of the current match moves with what was counted.
    if let Some((row, column, _)) = find.current {
        let rank = rank(state.delegate(), row, column);
        if let Some(find) = &mut state.delegate_mut().find {
            find.current = rank.map(|rank| (row, column, rank));
        }
    }
    cx.notify();
}

/// The matching columns of a row of the grid, left to right (from 1).
fn matches_in_row(grid: &Grid, row: usize) -> Vec<usize> {
    let Some(of) = grid.find.as_ref().and_then(|find| find.of.as_ref()) else {
        return Vec::new();
    };
    let columns = grid.store().columns();
    grid.layout()
        .iter()
        .enumerate()
        .filter(|&(_, &source)| of.matcher.matches(grid.cell(row, source), &columns[source]))
        .map(|(index, _)| index + 1)
        .collect()
}

/// Rows of the grid that hold fetched rows: what a find goes through.
fn shown_rows(grid: &Grid) -> usize {
    grid.total_rows()
        .min(grid.find.as_ref().map_or(0, |find| find.counts.len()))
        .min(match grid.find.as_ref().and_then(Find::rows) {
            Some(rows) => rows.len(),
            None => usize::MAX,
        })
}

/// Whether a row of the grid holds a match, as counted.
fn counted(grid: &Grid, row: usize) -> bool {
    let Some(find) = &grid.find else {
        return false;
    };
    find.counts
        .get(grid.source_row(row))
        .is_some_and(|&count| count > 0)
}

/// The rank of the match at a cell of the grid, from 1, in the order shown.
fn rank(grid: &Grid, row: usize, column: usize) -> Option<u64> {
    let find = grid.find.as_ref()?;
    let before: u64 = (0..row)
        .map(|row| {
            find.counts
                .get(grid.source_row(row))
                .map_or(0, |&count| u64::from(count))
        })
        .sum();
    let here = matches_in_row(grid, row);
    let at = here.iter().position(|&found| found == column)?;
    Some(before + at as u64 + 1)
}

/// Moves to the next match, or the previous, from the current one or the
/// selected cell; past the last, back to the first.
fn step(state: &mut TableState<Grid>, forward: bool, cx: &mut Context<TableState<Grid>>) {
    let grid = state.delegate();
    let Some(find) = &grid.find else {
        return;
    };
    let rows = shown_rows(grid);
    if rows == 0 || find.total == 0 {
        return;
    }
    let (row, column) = match find.current {
        Some((row, column, _)) => (row, column),
        None => match state.selection() {
            gpui_kit::component::table::TableSelection::None if forward => (0, 0),
            gpui_kit::component::table::TableSelection::None => (usize::MAX, usize::MAX),
            selection => current(selection),
        },
    };
    let row = row.min(rows - 1);
    let mut target = None;
    // The rest of the row, then the next rows; around once at most.
    let here = matches_in_row(grid, row);
    let in_row = if forward {
        here.iter().find(|&&found| found > column).copied()
    } else {
        here.iter().rev().find(|&&found| found < column).copied()
    };
    if let Some(found) = in_row {
        target = Some((row, found));
    } else {
        for offset in 1..=rows {
            let next = if forward {
                (row + offset) % rows
            } else {
                (row + rows - offset % rows) % rows
            };
            if !counted(grid, next) {
                continue;
            }
            let found = matches_in_row(grid, next);
            let pick = if forward { found.first() } else { found.last() };
            if let Some(&found) = pick {
                target = Some((next, found));
                break;
            }
        }
    }
    let Some((row, column)) = target else {
        return;
    };
    let rank = rank(grid, row, column).unwrap_or(0);
    if let Some(find) = &mut state.delegate_mut().find {
        find.current = Some((row, column, rank));
    }
    Grid::select(state, row, column, cx);
    state.scroll_to_col(column, cx);
}

/// `n` with its thousands apart: `1,000,000`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// What the bar says of the matches.
fn count_text(find: &Find) -> String {
    match (&find.of, find.current) {
        (None, _) => String::new(),
        (Some(_), _) if find.counts.is_empty() && find.job.is_some() => "Searching…".into(),
        (Some(_), _) if find.total == 0 => "No match".into(),
        (Some(_), Some((_, _, rank))) if rank > 0 => {
            format!("{} of {}", thousands(rank), thousands(find.total))
        }
        (Some(_), _) if find.total == 1 => "1 match".into(),
        (Some(_), _) => format!("{} matches", thousands(find.total)),
    }
}

/// How many rows a grid shows while "matching rows only" is on: "11 of 30
/// rows", "of 1,000 fetched rows" while more may come. The find bar, the
/// result's footer and the status bar say it alike.
pub fn rows_shown(grid: &Grid) -> Option<String> {
    let rows = grid.find.as_ref()?.rows()?;
    let fetched = grid.store().row_count() as u64;
    let what = if grid.is_complete() {
        "rows"
    } else {
        "fetched rows"
    };
    Some(format!(
        "{} of {} {what}",
        thousands(rows.len() as u64),
        thousands(fetched)
    ))
}

impl Find {
    /// The rows were read again from the first (K36): what was counted is
    /// counted again.
    pub(super) fn recount(&mut self) {
        self.of = None;
        self.job = None;
    }
}

/// The bar above a grid, when its find is open.
pub fn bar(table: &Table, cx: &App) -> Option<AnyElement> {
    let state = table.read(cx);
    let grid = state.delegate();
    let find = grid.find.as_ref()?;
    let theme = cx.theme();
    let count = count_text(find);
    let rows = rows_shown(grid);
    let more = rows.is_some() && grid.has_more();
    let (case, only) = (find.case, find.only);
    let on = |table: &Table| table.clone();
    let (previous, following, toggle_case, toggle_only, closing) =
        (on(table), on(table), on(table), on(table), on(table));
    let escape = on(table);
    Some(
        h_flex()
            .id("grid-find")
            .test_support()
            .key_context("GridFind")
            .flex_none()
            .w_full()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.table_head)
            .text_xs()
            // Escape, once the input has no use for it, closes the bar.
            .on_action(move |_: &Escape, window, cx| close(&escape, window, cx))
            .on_action({
                let table = table.clone();
                move |_: &crate::actions::FindNextInResults, _, cx| next(&table, true, cx)
            })
            .on_action({
                let table = table.clone();
                move |_: &crate::actions::FindPreviousInResults, _, cx| next(&table, false, cx)
            })
            .child(
                div().w(px(260.)).flex_none().child(
                    Input::new(&find.input)
                        .id("grid-find-input")
                        .xsmall()
                        .cleanable(true),
                ),
            )
            .child(
                div()
                    .id("grid-find-count")
                    .test_support()
                    .min_w(px(96.))
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(count.clone()))
                    .test_text(count),
            )
            .child(
                Button::new("grid-find-previous")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronUp)
                    .tooltip("Previous match (Shift+Enter)")
                    .on_click(move |_, window, cx| {
                        next(&previous, false, cx);
                        refocus(&previous, window, cx);
                    }),
            )
            .child(
                Button::new("grid-find-next")
                    .ghost()
                    .small()
                    .icon(IconName::ChevronDown)
                    .tooltip("Next match (Enter)")
                    .on_click(move |_, window, cx| {
                        next(&following, true, cx);
                        refocus(&following, window, cx);
                    }),
            )
            .child(
                Button::new("grid-find-case")
                    .ghost()
                    .small()
                    .icon(IconName::CaseSensitive)
                    .toggled(case)
                    .tooltip("Match case")
                    .on_click(move |_, window, cx| {
                        set_case(&toggle_case, !case, cx);
                        refocus(&toggle_case, window, cx);
                    }),
            )
            .child(
                Checkbox::new("grid-find-only")
                    .label("Matching rows only")
                    .checked(only)
                    .on_click(move |checked: &bool, window, cx| {
                        set_only(&toggle_only, *checked, window, cx);
                        refocus(&toggle_only, window, cx);
                    }),
            )
            .children(rows.map(|rows| {
                div()
                    .id("grid-find-rows")
                    .test_support()
                    .text_color(theme.muted_foreground)
                    .child(SharedString::from(rows.clone()))
                    .test_text(rows)
            }))
            .when(more, |bar| {
                bar.child(
                    Button::new("grid-find-fetch-all")
                        .outline()
                        .xsmall()
                        .label("Fetch all")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(crate::actions::FetchAllRows), cx)
                        }),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("grid-find-close")
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .tooltip("Close (Escape)")
                    .on_click(move |_, window, cx| close(&closing, window, cx)),
            )
            .into_any_element(),
    )
}

/// What a find of a grid shows, for tests: the count text, the number of
/// rows shown while "matching rows only" is on, and the current match (row
/// and column of the grid).
#[cfg(test)]
pub type Summary = Option<(String, Option<usize>, Option<(usize, usize)>)>;

/// What the find of a grid shows; for tests.
#[cfg(test)]
pub fn summary(grid: &Grid) -> Summary {
    let find = grid.find.as_ref()?;
    Some((
        count_text(find),
        find.rows().map(|rows| rows.len()),
        find.current.map(|(row, column, _)| (row, column)),
    ))
}

/// Whether the find of a grid is still scanning; for tests.
#[cfg(test)]
pub fn scanning(grid: &Grid) -> bool {
    grid.find.as_ref().is_some_and(|find| find.job.is_some())
}

#[cfg(test)]
mod tests {
    use super::thousands;

    #[test]
    fn counts_are_written_with_their_thousands_apart() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(340), "340");
        assert_eq!(thousands(1_000_000), "1,000,000");
        assert_eq!(thousands(12_345_678), "12,345,678");
    }
}
