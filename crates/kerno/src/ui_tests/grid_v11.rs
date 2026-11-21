//! J6 (CLAUDE.md §4 J, docs/plans/M11.md Part 4): find and "matching rows
//! only" over the fetched rows (DBeaver #38133, Beekeeper #524), the layout
//! of a table's columns remembered (DBeaver #38133), the Unix time hint
//! (Beekeeper #941).

use std::time::Instant;

use super::tool_wizards::paints;
use super::*;
use crate::query::find;
use crate::query::grid::GridAction;

fn summary(cx: &mut TestAppContext, grid: &Entity<TableState<Grid>>) -> find::Summary {
    cx.update(|cx| find::summary(grid.read(cx).delegate()))
}

/// Gives a grid the keyboard, as a click on it does.
fn focus(app: &Fixture, cx: &mut TestAppContext, grid: &Entity<TableState<Grid>>) {
    app.step(cx, |window, cx| {
        grid.read(cx).focus_handle(cx).focus(window, cx)
    });
}

/// The cell id of the grid's markers: row and column of the grid.
fn cell(row: usize, column: usize) -> usize {
    (row << 12) | column
}

/// The first column of what the grid draws.
fn first_column(cx: &mut TestAppContext, grid: &Entity<TableState<Grid>>) -> Vec<String> {
    cx.update(|cx| grid.read(cx).delegate().shown())
        .into_iter()
        .map(|row| row[0].clone())
        .collect()
}

/// Runs a statement of 30 rows (`n`, `label` = item n) in a new tab.
fn thirty_rows(app: &Fixture, cx: &mut TestAppContext) -> Entity<TableState<Grid>> {
    app.run_in_new_tab(
        cx,
        "select g as n, 'item ' || g as label from generate_series(1, 30) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["30 rows"]);
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid")
}

/// Cmd+F in a result finds in its rows: the count, Enter and Shift+Enter
/// through the matches in the order shown, the matches marked, the case
/// told apart on demand, Escape closes (DBeaver #38133).
#[gpui_kit::test]
fn cmd_f_in_a_result_counts_steps_through_and_marks_the_matches(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let grid = thirty_rows(&app, cx);
    focus(&app, cx, &grid);
    app.press(cx, &["secondary-f"]);
    assert!(app.has(cx, "grid-find"));
    app.type_text(cx, "ITEM 2");
    // "item 2" and "item 20" to "item 29", in the label column.
    assert_eq!(summary(cx, &grid), Some(("11 matches".into(), None, None)));
    app.press(cx, &["enter"]);
    assert_eq!(
        summary(cx, &grid),
        Some(("1 of 11".into(), None, Some((1, 2))))
    );
    assert!(app.has(cx, ("find-current", cell(1, 2))));
    // The next match is far below: the grid scrolls to it.
    app.press(cx, &["enter"]);
    assert_eq!(
        summary(cx, &grid),
        Some(("2 of 11".into(), None, Some((19, 2))))
    );
    assert!(app.has(cx, ("find-current", cell(19, 2))));
    assert!(app.has(cx, ("find-match", cell(20, 2))));
    assert!(!app.has(cx, ("find-match", cell(19, 1))));
    assert_eq!(cx.update(|cx| grid.read(cx).selected_cell()), Some((19, 2)));
    // Back, and around: before the first is the last.
    app.press(cx, &["shift-enter", "shift-enter"]);
    assert_eq!(
        summary(cx, &grid),
        Some(("11 of 11".into(), None, Some((28, 2))))
    );
    // Upper case told apart: nothing is "ITEM".
    app.click(cx, "grid-find-case");
    assert_eq!(summary(cx, &grid), Some(("No match".into(), None, None)));
    app.click(cx, "grid-find-case");
    assert_eq!(summary(cx, &grid).unwrap().0, "11 matches");
    // Escape closes the bar; the grid has the keyboard again.
    app.press(cx, &["escape"]);
    assert!(!app.has(cx, "grid-find"));
    assert_eq!(summary(cx, &grid), None);
    assert!(app.step(cx, |window, cx| {
        grid.read(cx).focus_handle(cx).is_focused(window)
    }));
    // The menu of a cell opens it too.
    app.step(cx, |window, cx| {
        Grid::perform(&grid, GridAction::Find, window, cx)
    });
    assert!(app.has(cx, "grid-find"));
}

/// "Matching rows only" shows the matching rows in the order of the sort,
/// follows a new sort, and gives every row back when closed (Beekeeper
/// #524).
#[gpui_kit::test]
fn matching_rows_only_follows_the_sort(cx: &mut TestAppContext) {
    use gpui_kit::component::table::TableDelegate as _;

    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let grid = thirty_rows(&app, cx);
    let sort = |cx: &mut TestAppContext, order: ColumnSort| {
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| {
                table.delegate_mut().perform_sort(1, order, window, cx)
            })
        });
    };
    sort(cx, ColumnSort::Descending);
    app.step(cx, |window, cx| find::open(&grid, window, cx));
    app.type_text(cx, "item 2");
    app.click(cx, "grid-find-only");
    assert_eq!(
        summary(cx, &grid),
        Some(("11 matches".into(), Some(11), None))
    );
    let mut descending: Vec<String> = (20..=29).rev().map(|n| n.to_string()).collect();
    descending.push("2".into());
    assert_eq!(first_column(cx, &grid), descending);
    assert!(app.has(cx, "grid-find-rows"));
    assert!(paints(&app, cx, "11 of 30 rows"));
    // The rows are matched again in the new order.
    sort(cx, ColumnSort::Ascending);
    app.wait_until(cx, "the ascending matches", |_, cx| {
        first_column(cx, &grid).first().map(String::as_str) == Some("2")
    });
    let mut ascending = vec!["2".to_string()];
    ascending.extend((20..=29).map(|n| n.to_string()));
    assert_eq!(first_column(cx, &grid), ascending);
    // Next goes through the rows shown.
    app.press(cx, &["enter", "enter"]);
    assert_eq!(summary(cx, &grid).unwrap().2, Some((1, 2)));
    // Closed, every row is back, still sorted.
    app.click(cx, "grid-find-close");
    let all = first_column(cx, &grid);
    assert_eq!(all.len(), 30);
    assert_eq!(all[..2], ["1", "2"]);
}

/// "Matching rows only" keeps each row's number (the number it has
/// without the filter, in the sort shown), and the result's footer and the
/// status bar say how many rows of how many are shown, as the find bar
/// does (Beekeeper #524).
#[gpui_kit::test]
fn matching_rows_keep_their_numbers_and_every_count_says_of_how_many(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let grid = thirty_rows(&app, cx);
    let numbers = |cx: &mut TestAppContext| -> Vec<usize> {
        cx.update(|cx| {
            let grid = grid.read(cx).delegate();
            (0..grid.total_rows())
                .map(|row| grid.row_number(row))
                .collect()
        })
    };
    app.step(cx, |window, cx| find::open(&grid, window, cx));
    app.type_text(cx, "item 2");
    app.click(cx, "grid-find-only");
    let mut unsorted = vec![2];
    unsorted.extend(20..=29);
    assert_eq!(numbers(cx), unsorted);
    // The find bar, the footer and the status bar.
    let said = |cx: &mut TestAppContext| {
        let status = cx.update(|cx| app.workspace.read(cx).status_text(cx));
        (
            paints(&app, cx, "11 of 30 rows"),
            app.label_of(cx, "result-summary"),
            status.iter().any(|item| item == "11 of 30 rows"),
        )
    };
    assert_eq!(said(cx), (true, "11 of 30 rows".to_string(), true));
    // Sorted by n, descending: 30 is row 1, 29 row 2, ..., 2 row 29.
    app.step(cx, |window, cx| {
        grid.update(cx, |table, cx| {
            use gpui_kit::component::table::TableDelegate as _;
            table
                .delegate_mut()
                .perform_sort(1, ColumnSort::Descending, window, cx)
        })
    });
    app.wait_until(cx, "the descending matches", |_, cx| {
        first_column(cx, &grid).first().map(String::as_str) == Some("29")
    });
    let mut descending: Vec<usize> = (2..=11).collect();
    descending.push(29);
    assert_eq!(numbers(cx), descending);
    // Closed: every row, numbered from 1 again, and "30 rows".
    app.click(cx, "grid-find-close");
    assert_eq!(numbers(cx), (1..=30).collect::<Vec<_>>());
    assert_eq!(said(cx), (false, "30 rows".to_string(), false));
}

/// In a table's data the find works as in a result; "matching rows only"
/// refuses pending changes, and refuses edits while on: changes are kept
/// by row as fetched. The bar stays open over the page read again. An
/// integer that looks like a Unix time says when in the value panel
/// (Beekeeper #941).
#[gpui_kit::test]
fn a_tables_rows_are_found_and_edits_wait_for_every_row(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_grid_find CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_grid_find",
            "CREATE TABLE kerno_ui_grid_find.fruit (id int PRIMARY KEY, name text, at bigint)",
            "INSERT INTO kerno_ui_grid_find.fruit VALUES (1, 'apple', 1794619425000), \
             (2, 'banana', 5), (3, 'pear', NULL), (4, 'grape', 946684799)",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "kerno_ui_grid_find", "fruit", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–4 of 4");
    let grid = app.data_grid(cx);
    focus(&app, cx, &grid);
    app.press(cx, &["secondary-f"]);
    app.type_text(cx, "p");
    assert_eq!(summary(cx, &grid).unwrap().0, "3 matches");
    app.click(cx, "grid-find-only");
    assert_eq!(first_column(cx, &grid), ["1", "3", "4"]);
    // While only matching rows show, nothing is edited.
    assert!(
        cx.update(|cx| grid.read(cx).delegate().editable(0, 2))
            .is_err()
    );
    app.on_data(cx, |tab, window, cx| tab.delete_rows(Some(0), window, cx));
    assert_eq!(app.pending(cx), 0);
    // Off, an edit; on again, refused while it waits.
    app.click(cx, "grid-find-only");
    assert_eq!(first_column(cx, &grid), ["1", "2", "3", "4"]);
    app.type_in_cell(cx, 1, 2, "papaya", "enter");
    assert_eq!(app.pending(cx), 1);
    app.click(cx, "grid-find-only");
    assert_eq!(summary(cx, &grid).unwrap().1, None);
    // The pending value is what is matched: "papaya" now counts.
    assert_eq!(summary(cx, &grid).unwrap().0, "4 matches");
    app.on_data(cx, |tab, _, cx| tab.discard(cx));

    // The page read again keeps the bar, its text and its options.
    app.click(cx, "grid-find-only");
    app.on_data(cx, |tab, window, cx| tab.refresh(window, cx));
    app.wait_until(cx, "the page read again", |app, cx| {
        app.data_grid(cx) != grid && app.data_state(cx) == "Rows 1–4 of 4"
    });
    let grid = app.data_grid(cx);
    assert_eq!(
        summary(cx, &grid),
        Some(("3 matches".into(), Some(3), None))
    );

    // The Unix time hint, in the value panel of an integer.
    app.click(cx, "grid-find-close");
    app.step(cx, |window, cx| {
        Grid::open_value(&grid, 0, 3, true, window, cx)
    });
    assert!(app.has(cx, "value-editor"));
    assert!(paints(
        &app,
        cx,
        "Unix milliseconds \u{2192} 2026-11-14 01:23:45.000 UTC"
    ));
    app.click(cx, "value-cancel");
    // 946684799 is a second before 2000: no hint.
    app.step(cx, |window, cx| {
        Grid::open_value(&grid, 3, 3, false, window, cx)
    });
    assert!(app.has(cx, "value-text"));
    assert!(!app.has(cx, "value-unix-time"));
    app.click(cx, "value-close");
}

/// The viewer of a result's integer says what Unix time it is (Beekeeper
/// #941).
#[gpui_kit::test]
fn an_integer_that_looks_like_a_unix_time_says_when(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select 1794619425 as seconds, 'x' as text, '{\"a\": 1}'::jsonb as doc",
        "secondary-enter",
    );
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    app.step(cx, |window, cx| {
        Grid::open_value(&grid, 0, 1, false, window, cx)
    });
    assert!(app.has(cx, "value-unix-time"));
    assert!(paints(
        &app,
        cx,
        "Unix seconds \u{2192} 2026-11-14 01:23:45 UTC"
    ));
    // An integer opens as text: a bare number is no JSON document.
    let json_view = |cx: &mut TestAppContext| {
        use gpui_kit::test::TestQueryExt as _;
        app.step(cx, |window, _| window.try_find_by_label("JSON").is_some())
    };
    assert!(!json_view(cx), "no JSON view for an integer");
    app.click(cx, "value-close");
    app.step(cx, |window, cx| {
        Grid::open_value(&grid, 0, 2, false, window, cx)
    });
    assert!(!app.has(cx, "value-unix-time"));
    app.click(cx, "value-close");
    // A JSON column has it.
    app.step(cx, |window, cx| {
        Grid::open_value(&grid, 0, 3, false, window, cx)
    });
    assert!(json_view(cx), "the JSON view of a jsonb value");
    app.click(cx, "value-close");
}

/// A find over 1,000,000 fetched rows runs off the UI thread: the steps
/// that start it return at once, the count comes later, and "matching rows
/// only" says how many of the fetched rows match (DBeaver #38133).
#[gpui_kit::test]
fn a_million_fetched_rows_are_searched_off_the_ui_thread(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select id, username, note from analytics.big_table",
        "secondary-enter",
    );
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx) == ["1000 rows, more"]
    });
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    app.step(cx, |_, cx| tab.update(cx, |tab, cx| tab.fetch(true, cx)));
    app.wait_until(cx, "every row", |_, cx| {
        cx.update(|cx| {
            let grid = grid.read(cx).delegate();
            grid.is_complete() && grid.row_count() == 1_000_000
        })
    });
    app.step(cx, |window, cx| find::open(&grid, window, cx));

    let started = Instant::now();
    app.type_text(cx, "user_4242");
    let typing = started.elapsed();
    assert!(
        cx.update(|cx| find::scanning(grid.read(cx).delegate())),
        "the rows are matched off the UI thread"
    );
    app.wait_until(cx, "the count", |_, cx| {
        !cx.update(|cx| find::scanning(grid.read(cx).delegate()))
    });
    // user_4242 and user_42420 to user_42429, 20 rows each.
    assert_eq!(summary(cx, &grid).unwrap().0, "220 matches");

    let started = Instant::now();
    app.click(cx, "grid-find-only");
    let only = started.elapsed();
    // The rows to show are worked out off the UI thread too.
    assert!(cx.update(|cx| find::scanning(grid.read(cx).delegate())));
    app.wait_until(cx, "the matching rows", |_, cx| {
        summary(cx, &grid).unwrap().1 == Some(220)
    });
    assert!(paints(&app, cx, "220 of 1,000,000 rows"));
    let started = Instant::now();
    app.press(cx, &["enter"]);
    let next = started.elapsed();
    assert_eq!(summary(cx, &grid).unwrap().0, "1 of 220");
    // The time of each step, frames included, for the report; a bound would
    // fail on a busy machine (the suite runs beside three other tests), the
    // checks above say where the work ran.
    eprintln!("1,000,000 rows: typing {typing:?}, matching rows only {only:?}, next {next:?}");
}

/// The table key a layout is stored under, for a table of the fixture's
/// connection.
fn layout_key(engine: Engine, schema: &str, table: &str) -> String {
    let database = match engine {
        Engine::Postgres => "kerno",
        Engine::MySql | Engine::MariaDb => "",
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    };
    serde_json::to_string(&[database, schema, table]).unwrap()
}

/// The layout stored for a table of the first connection.
fn stored_layout(
    app: &Fixture,
    cx: &mut TestAppContext,
    key: &str,
) -> Option<kerno_core::storage::TableLayout> {
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    app.storage.table_layout(&id, key).unwrap()
}

/// Waits until the delayed write of a layout reached storage.
fn wait_for_layout(
    app: &Fixture,
    cx: &mut TestAppContext,
    key: &str,
    done: impl Fn(Option<kerno_core::storage::TableLayout>) -> bool,
) {
    cx.executor().advance_clock(Duration::from_secs(1));
    app.wait_until(cx, "the stored layout", |app, cx| {
        done(stored_layout(app, cx, key))
    });
}

/// The columns, pinned columns, widths and order of a table's data come
/// back when it is opened again, after a restart too; "Reset layout"
/// forgets them (DBeaver #38133).
#[gpui_kit::test]
fn a_tables_layout_comes_back_until_it_is_reset(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_grid_layout";
        let (drop_schema, create_schema) = scratch_schema(engine, schema);
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &format!(
                    "CREATE TABLE {schema}.t (id INT PRIMARY KEY, a INT, b INT, c INT, \
                     note VARCHAR(100))"
                ),
                &format!(
                    "INSERT INTO {schema}.t VALUES (1, 10, 100, 7, '{}'), (2, 20, 200, 8, 'x')",
                    "long note ".repeat(9)
                ),
            ],
        );
        let key = layout_key(engine, schema, "t");
        let app = connected(cx, engine);
        app.open_table(cx, schema, "t", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        let grid = app.data_grid(cx);
        let width = |grid: &Entity<TableState<Grid>>, column: usize, cx: &mut TestAppContext| {
            cx.update(|cx| f32::from(grid.read(cx).col_width(column).unwrap()))
        };
        // Widen `note` past the first width's cap, as its header's "Fit
        // width" does; hide `b` and pin `c` from their menus; order by `a`
        // descending (on the server).
        let before = width(&grid, 5, cx);
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| table.fit_col(5, window, cx))
        });
        let fitted = width(&grid, 5, cx);
        assert!(fitted > before + 50.0, "{before} → {fitted}");
        let on_column = |cx: &mut TestAppContext, column: usize, action: GridAction| {
            app.step(cx, |window, cx| {
                grid.update(cx, |table, cx| table.set_selected_cell(0, column, cx));
                Grid::perform(&grid, action, window, cx)
            });
        };
        on_column(cx, 3, GridAction::HideColumn);
        on_column(cx, 3, GridAction::Pin(false));
        // A column hidden or pinned leaves the width of the others alone.
        assert!((width(&grid, 4, cx) - fitted).abs() < 1.0, "{engine:?}");
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| {
                table
                    .delegate_mut()
                    .perform_sort(3, ColumnSort::Descending, window, cx)
            })
        });
        app.wait_until(cx, "the sorted page", |app, cx| {
            let tab = app.data(cx);
            cx.update(|cx| tab.read(cx).sort() == Some(("a", true)))
                && app.data_state(cx) == "Rows 1–2 of 2"
        });
        let expected = |grid: &Entity<TableState<Grid>>, cx: &mut TestAppContext| {
            cx.update(|cx| {
                let grid = grid.read(cx).delegate();
                (grid.shown_columns(), grid.is_pinned(1), grid.is_pinned(2))
            })
        };
        let laid_out = (
            vec!["c".to_string(), "id".into(), "a".into(), "note".into()],
            true,
            false,
        );
        let wide = |grid: &Entity<TableState<Grid>>, cx: &mut TestAppContext| {
            (width(grid, 4, cx) - fitted).abs() < 1.0
        };
        // The page read in the new order keeps the columns as they were.
        assert_eq!(expected(&app.data_grid(cx), cx), laid_out, "{engine:?}");
        assert_eq!(app.data_rows(cx)[0][..3], ["8", "2", "20"], "{engine:?}");
        assert!(wide(&app.data_grid(cx), cx), "{engine:?}");
        wait_for_layout(&app, cx, &key, |stored| {
            stored.is_some_and(|stored| {
                stored.hidden == ["b"]
                    && stored.pinned == ["c"]
                    && stored.sort == Some(("a".into(), true))
                    && stored
                        .widths
                        .get("note")
                        .is_some_and(|stored| (stored - fitted).abs() < 1.0)
            })
        });

        // Closed and opened again.
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
        assert_eq!(app.titles(cx), Vec::<String>::new());
        app.open_table(cx, schema, "t", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(expected(&app.data_grid(cx), cx), laid_out, "{engine:?}");
        assert_eq!(app.data_rows(cx)[0][..3], ["8", "2", "20"], "{engine:?}");
        assert!(wide(&app.data_grid(cx), cx), "{engine:?}");

        // After a restart.
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
        let restarted = open(cx, app.storage.clone());
        let id = cx.update(|cx| restarted.connections.read(cx).saved()[0].id.clone());
        let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
        kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
        restarted.step(cx, |_, cx| {
            restarted
                .connections
                .update(cx, |connections, cx| connections.connect(&id, cx))
        });
        restarted.open_table(cx, schema, "t", RelationKind::Table);
        restarted.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(
            expected(&restarted.data_grid(cx), cx),
            laid_out,
            "{engine:?}"
        );
        assert_eq!(
            restarted.data_rows(cx)[0][..3],
            ["8", "2", "20"],
            "{engine:?}"
        );
        assert!(wide(&restarted.data_grid(cx), cx), "{engine:?}");

        // "Reset layout" in the menu of a column header.
        let grid = restarted.data_grid(cx);
        restarted.step(cx, |window, cx| {
            Grid::perform(&grid, GridAction::ResetLayout, window, cx)
        });
        restarted.wait_until(cx, "the page in the table's order", |app, cx| {
            app.data_grid(cx) != grid && app.data_state(cx) == "Rows 1–2 of 2"
        });
        assert_eq!(
            expected(&restarted.data_grid(cx), cx),
            (
                vec![
                    "id".to_string(),
                    "a".into(),
                    "b".into(),
                    "c".into(),
                    "note".into()
                ],
                false,
                false
            ),
            "{engine:?}"
        );
        assert_eq!(restarted.data_rows(cx)[0][0], "1", "{engine:?}");
        assert!(width(&restarted.data_grid(cx), 5, cx) < fitted - 50.0);
        wait_for_layout(&restarted, cx, &key, |stored| stored.is_none());
        restarted.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
    }
}

/// A table of a PostgreSQL connection that names no database is the same
/// table whether the tree opened it (in the server's default database) or
/// something that names no database did: one layout for both (DBeaver
/// #38133).
#[gpui_kit::test]
fn a_table_has_one_layout_key_with_or_without_its_database(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let config = ConnectionConfig {
        database: None,
        ..testing::config(Engine::Postgres)
    };
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let key = |database: Option<&str>, cx: &mut TestAppContext| {
        let target = Target {
            connection: id.clone(),
            database: database.map(str::to_string),
            schema: None,
        };
        let tab = app.step(cx, |window, cx| {
            app.workspace.update(cx, |workspace, cx| {
                workspace.open_data(
                    target,
                    TableRef::new("kerno_ui_layout_key", "t"),
                    RelationKind::Table,
                    Filter::default(),
                    window,
                    cx,
                )
            })
        });
        cx.update(|cx| tab.read(cx).stored_layout_key(cx))
    };
    let from_the_tree = key(Some(kerno_postgres::DEFAULT_DATABASE), cx);
    assert_eq!(key(None, cx), from_the_tree);
    assert_eq!(from_the_tree, r#"["postgres","kerno_ui_layout_key","t"]"#);
    // Another database is another table.
    assert_ne!(key(Some("kerno"), cx), from_the_tree);
}
