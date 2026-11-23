//! K36: a table's rows keep loading as the scroll reaches the end, and the
//! size of a load is changed in the footer (a first user's report of
//! 2026-11-23, DBeaver's data viewer). Every edit and drop is on a
//! `kerno_test_*` scratch schema; fixture tables are only read.

use gpui_kit::component::table::TableSelection;

use super::*;

/// Lets the frames and the server run a while: what was going to arrive has.
fn settle(app: &Fixture, cx: &mut TestAppContext) {
    for _ in 0..5 {
        app.step(cx, |_, _| {});
        cx.run_until_parked();
    }
}

/// Fills `table` (an `id` key the server numbers, and `name`) with `rows`
/// rows named "item 1" to "item <rows>", in that order.
fn fill(engine: Engine, table: &str, rows: u32) -> String {
    match engine {
        Engine::Postgres => format!(
            "INSERT INTO {table} (name) SELECT 'item ' || n FROM generate_series(1, {rows}) n \
             ORDER BY n"
        ),
        _ => format!(
            "INSERT INTO {table} (name) WITH RECURSIVE s (n) AS (SELECT 1 UNION ALL \
             SELECT n + 1 FROM s WHERE n < {rows}) SELECT CONCAT('item ', n) FROM s ORDER BY n"
        ),
    }
}

/// The selection of the active data tab's grid.
fn selection(app: &Fixture, cx: &mut TestAppContext) -> TableSelection {
    let grid = app.data_grid(cx);
    cx.update(|cx| grid.read(cx).selection())
}

/// Whether the active data tab's grid shows one row as a record.
fn record(app: &Fixture, cx: &mut TestAppContext) -> bool {
    let grid = app.data_grid(cx);
    cx.update(|cx| grid.read(cx).delegate().record)
}

/// Scrolls the active data tab's grid to `row`.
fn scroll_to(app: &Fixture, cx: &mut TestAppContext, row: usize) {
    let grid = app.data_grid(cx);
    app.step(cx, |_, cx| {
        grid.update(cx, |table, cx| table.scroll_to_row(row, cx))
    });
}

/// The first column of every row the active data tab draws.
fn first_column(app: &Fixture, cx: &mut TestAppContext) -> Vec<String> {
    app.data_rows(cx)
        .into_iter()
        .map(|row| row[0].clone())
        .collect()
}

/// The scroll reaching the end reads the next load and adds it: the rows
/// there, the pending changes (an edit, a deletion, a row added), the
/// selection and the record view stay. A row being added holds the loads
/// of the scroll back; Fetch more adds the rows above it and the selection
/// follows it. An apply reads back as many rows as were loaded.
#[gpui_kit::test]
fn a_load_adds_rows_under_those_there_and_keeps_what_was_pending(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            continue;
        }
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_test_loads");
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &scratch_table(
                    engine,
                    "kerno_test_loads.items",
                    "name VARCHAR(50) NOT NULL",
                ),
                &fill(engine, "kerno_test_loads.items", 250),
            ],
        );
        let count = |condition: &str| {
            on_server(
                engine,
                &format!("SELECT count(*) FROM kerno_test_loads.items WHERE {condition}"),
            )
        };
        let app = connected(cx, engine);
        cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 100));
        app.open_table(cx, "kerno_test_loads", "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–100 of 250");
        let grid = app.data_grid(cx);

        // Pending: an edited cell and a row marked for deletion.
        app.type_in_cell(cx, 3, 2, "changed", "enter");
        app.on_data(cx, |tab, window, cx| tab.delete_rows(Some(20), window, cx));
        assert_eq!(app.pending(cx), 2, "{engine:?}");
        app.step(cx, |_, cx| {
            grid.update(cx, |table, cx| table.set_selected_cell(10, 2, cx))
        });

        // The end of the rows reads the next load, once.
        scroll_to(&app, cx, 99);
        app.wait_for_rows(cx, "Rows 1–200 of 250");
        settle(&app, cx);
        assert_eq!(app.data_state(cx), "Rows 1–200 of 250", "{engine:?}");
        let rows = app.data_rows(cx);
        assert_eq!(rows[3][..2], ["4", "changed"], "{engine:?}");
        assert_eq!(rows[100][..2], ["101", "item 101"], "{engine:?}");
        // Row numbers go on.
        let number = cx.update(|cx| grid.read(cx).delegate().row_number(150));
        assert_eq!(number, 151);
        // The same grid: nothing was rebuilt.
        assert_eq!(app.data_grid(cx).entity_id(), grid.entity_id());
        assert_eq!(app.pending(cx), 2, "{engine:?}");
        assert_eq!(selection(&app, cx), TableSelection::Cell(10, 2));

        // A row added: the scroll does not read under it.
        app.click(cx, "data-add-row");
        assert_eq!(app.data_rows(cx).len(), 201);
        app.type_in_cell(cx, 200, 2, "new one", "enter");
        assert_eq!(app.pending(cx), 3, "{engine:?}");
        app.step(cx, |_, cx| {
            grid.update(cx, |table, cx| table.set_selected_cell(200, 2, cx))
        });
        scroll_to(&app, cx, 200);
        settle(&app, cx);
        assert_eq!(app.data_state(cx), "Rows 1–200 of 250", "{engine:?}");

        // Fetch more, in the record view: the rows go above the added one,
        // which keeps the selection; the record view stays.
        app.step(cx, |window, cx| Grid::toggle_record(&grid, window, cx));
        assert!(record(&app, cx));
        app.click(cx, "data-fetch-more");
        app.wait_for_rows(cx, "Rows 1–250 of 250");
        assert!(record(&app, cx), "{engine:?}");
        assert_eq!(selection(&app, cx), TableSelection::Cell(250, 2));
        let rows = app.data_rows(cx);
        assert_eq!(rows.len(), 251);
        assert_eq!(rows[249][..2], ["250", "item 250"], "{engine:?}");
        assert_eq!(rows[250][1], "new one", "{engine:?}");
        assert_eq!(app.pending(cx), 3, "{engine:?}");
        app.step(cx, |window, cx| Grid::toggle_record(&grid, window, cx));

        // The statements address the rows they were made on.
        let tab = app.data(cx);
        let (_, statements) = cx.update(|cx| tab.read(cx).pending_sql(cx)).unwrap();
        assert_eq!(statements.len(), 3, "{statements:?}");
        assert!(statements[0].starts_with("DELETE") && statements[0].ends_with("= 21"));
        assert!(statements[1].starts_with("UPDATE") && statements[1].ends_with("= 4"));
        assert!(statements[2].starts_with("INSERT") && statements[2].contains("'new one'"));

        // Apply: the rows are read again as far as they were loaded, as the
        // server has them now.
        app.click(cx, "data-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the applied changes", |app, cx| {
            app.pending(cx) == 0 && app.data_state(cx) == "Rows 1–250 of 250"
        });
        assert_eq!(count("id = 21"), "0", "{engine:?}");
        assert_eq!(count("name = 'changed' AND id = 4"), "1");
        assert_eq!(count("name = 'new one'"), "1");
        let names: Vec<String> = app
            .data_rows(cx)
            .into_iter()
            .map(|row| row[1].clone())
            .collect();
        assert_eq!(names[3], "changed");
        assert_eq!(names[249], "new one");
        assert!(!names.contains(&"item 21".to_string()));
    }
}

/// The fetch size is changed in the footer, from its list or typed, for
/// this tab only: the next load reads that many, what is there stays,
/// Settings keep theirs.
#[gpui_kit::test]
fn a_fetch_size_changed_in_the_footer_is_read_by_the_next_load(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 100));
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–100 of 5,000");

    // From the list.
    app.click(cx, "data-fetch-size");
    click_menu_entry(&app, cx, "500");
    assert_eq!(app.label_of(cx, "data-fetch-size"), "Fetch size: 500");
    assert_eq!(app.label_of(cx, "data-fetch-more"), "Fetch 500 more");
    assert_eq!(app.data_state(cx), "Rows 1–100 of 5,000");
    app.click(cx, "data-fetch-more");
    app.wait_for_rows(cx, "Rows 1–600 of 5,000");

    // Typed: a size out of bounds is not taken.
    app.click(cx, "data-fetch-size");
    click_menu_entry(&app, cx, "Other size…");
    assert!(app.has(cx, "data-fetch-size-input"));
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "50");
    app.click(cx, "data-fetch-size-set");
    assert!(
        app.has(cx, "data-fetch-size-input"),
        "50 is under the least"
    );
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "2,50");
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the dialog to close", |app, cx| {
        !app.has(cx, "data-fetch-size-input")
    });
    assert_eq!(app.label_of(cx, "data-fetch-size"), "Fetch size: 250");
    app.click(cx, "data-fetch-more");
    app.wait_for_rows(cx, "Rows 1–850 of 5,000");
    let tab = app.data(cx);
    assert_eq!(cx.update(|cx| tab.read(cx).load_size()), 250);
    assert_eq!(cx.update(|cx| AppState::get(cx).settings.page_size), 100);
}

/// A table without a key has no order the server keeps between two
/// statements: a row updated between two loads moves in its heap, and
/// reads by offset would show rows twice and skip others. Three loads of
/// three sizes show every row once.
#[gpui_kit::test]
fn loads_of_a_keyless_table_neither_repeat_nor_skip_a_row(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_test_keyless CASCADE"],
        &[
            "CREATE SCHEMA kerno_test_keyless",
            "CREATE TABLE kerno_test_keyless.items (n integer, doc json)",
            "INSERT INTO kerno_test_keyless.items \
             SELECT n, json_build_object('n', n) FROM generate_series(1, 300) n ORDER BY n",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 100));
    app.open_table(cx, "kerno_test_keyless", "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–100 of 300");
    let tab = app.data(cx);
    assert!(
        cx.update(|cx| tab.read(cx).read_only_reason(cx))
            .is_some_and(|why| why.contains("no primary key"))
    );
    let first: Vec<String> = first_column(&app, cx);
    assert_eq!(first[0], "1");

    // Rows already read move to the end of the table's heap.
    on_server(
        Engine::Postgres,
        "UPDATE kerno_test_keyless.items SET n = n WHERE n <= 50",
    );
    // The last load reads exactly the rows left: the count ends the way on.
    for (size, shown) in [(120, "Rows 1–220 of 300"), (80, "Rows 1–300 of 300")] {
        app.on_data(cx, |tab, _, cx| tab.set_load_size(size, cx));
        app.click(cx, "data-fetch-more");
        app.wait_for_rows(cx, shown);
    }
    assert_eq!(app.label_of(cx, "data-fetch-more"), "All rows fetched");
    let mut seen: Vec<u32> = first_column(&app, cx)
        .iter()
        .map(|n| n.parse().unwrap())
        .collect();
    seen.sort_unstable();
    assert_eq!(seen, (1..=300).collect::<Vec<u32>>());
}

/// After several loads: a refresh reads as many rows again and keeps the
/// selection; a new sort or a new filter starts again from one load.
#[gpui_kit::test]
fn refresh_sort_and_filter_after_several_loads(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 100));
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–100 of 5,000");
    for shown in ["Rows 1–200 of 5,000", "Rows 1–300 of 5,000"] {
        app.click(cx, "data-fetch-more");
        app.wait_for_rows(cx, shown);
    }
    let grid = app.data_grid(cx);
    app.step(cx, |_, cx| {
        grid.update(cx, |table, cx| table.set_selected_cell(250, 2, cx))
    });

    // Refresh: the 300 rows, the selection where it was.
    app.click(cx, "data-refresh");
    app.wait_until(cx, "the rows read again", |app, cx| {
        app.data_grid(cx).entity_id() != grid.entity_id()
            && app.data_state(cx) == "Rows 1–300 of 5,000"
    });
    assert_eq!(selection(&app, cx), TableSelection::Cell(250, 2));
    assert_eq!(app.data_rows(cx)[250][0], "251");

    // A sort starts again from the first load.
    let grid = app.data_grid(cx);
    app.step(cx, |window, cx| {
        grid.update(cx, |table, cx| {
            table
                .delegate_mut()
                .perform_sort(1, ColumnSort::Descending, window, cx)
        })
    });
    app.wait_until(cx, "the sorted rows", |app, cx| {
        let tab = app.data(cx);
        cx.update(|cx| tab.read(cx).sort() == Some(("id", true)))
            && app.data_state(cx) == "Rows 1–100 of 5,000"
    });
    assert_eq!(app.data_rows(cx)[0][0], "5000");
    app.click(cx, "data-fetch-more");
    app.wait_for_rows(cx, "Rows 1–200 of 5,000");
    // The next load follows the order.
    assert_eq!(app.data_rows(cx)[100][0], "4900");

    // So does a filter.
    let paid = on_server(
        Engine::Postgres,
        "SELECT count(*) FROM shop.orders WHERE status = 'paid'",
    );
    let paid = format!(
        "Rows 1–100 of {}",
        crate::query::plan_view::number(paid.parse::<f64>().unwrap())
    );
    app.on_data(cx, |tab, window, cx| {
        tab.set_filter(
            Filter {
                conditions: Vec::new(),
                raw: "status = 'paid'".into(),
            },
            window,
            cx,
        )
    });
    app.wait_for_rows(cx, &paid);
}

/// A load that fails (the table dropped since) keeps the rows and the
/// pending edit, says what happened in the footer, and is tried again.
#[gpui_kit::test]
fn a_failed_load_keeps_the_rows_and_the_edits_and_is_tried_again(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let create = scratch_table(
        Engine::Postgres,
        "kerno_test_failed_load.items",
        "name VARCHAR(50) NOT NULL",
    );
    let fill = fill(Engine::Postgres, "kerno_test_failed_load.items", 250);
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_test_failed_load CASCADE"],
        &["CREATE SCHEMA kerno_test_failed_load", &create, &fill],
    );
    let app = connected(cx, Engine::Postgres);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 100));
    app.open_table(cx, "kerno_test_failed_load", "items", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–100 of 250");
    app.type_in_cell(cx, 3, 2, "changed", "enter");
    assert_eq!(app.pending(cx), 1);

    on_server(Engine::Postgres, "DROP TABLE kerno_test_failed_load.items");
    app.click(cx, "data-fetch-more");
    app.wait_until(cx, "the failure", |app, cx| app.has(cx, "data-fetch-error"));
    assert!(
        app.label_of(cx, "data-fetch-error")
            .starts_with("The next rows could not be read."),
        "{}",
        app.label_of(cx, "data-fetch-error")
    );
    assert_eq!(app.data_state(cx), "Rows 1–100 of 250");
    assert_eq!(app.pending(cx), 1);
    assert_eq!(app.data_rows(cx)[3][1], "changed");
    assert_eq!(app.label_of(cx, "data-fetch-more"), "Try again");

    // The table back: Try again reads the next load.
    on_server(Engine::Postgres, &create);
    on_server(Engine::Postgres, &fill);
    app.click(cx, "data-fetch-more");
    app.wait_for_rows(cx, "Rows 1–200 of 250");
    assert!(!app.has(cx, "data-fetch-error"));
    assert_eq!(app.pending(cx), 1);
}

/// The keyboard: Fetch More Rows and Fetch All Rows reach a table's data;
/// Fetch all reads load after load, the count growing, and Cancel stops it.
#[gpui_kit::test]
fn the_fetch_keys_reach_a_table_and_fetch_all_grows_until_cancelled(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 1_000));
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–1,000 of 5,000");
    // The keys themselves, with the grid holding the keyboard.
    let grid = app.data_grid(cx);
    app.step(cx, |window, cx| {
        grid.update(cx, |table, cx| table.set_selected_cell(0, 1, cx));
        grid.read(cx).focus_handle(cx).focus(window, cx);
    });
    app.press(cx, &["ctrl-alt-n"]);
    app.wait_for_rows(cx, "Rows 1–2,000 of 5,000");
    app.press(cx, &["ctrl-+"]);
    app.wait_for_rows(cx, "Rows 1–5,000 of 5,000");
    assert_eq!(app.label_of(cx, "data-fetch-more"), "All rows fetched");

    // A million rows: Cancel stops Fetch all on its way.
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 10_000));
    app.open_table(cx, "analytics", "big_table", RelationKind::Table);
    app.wait_until(cx, "the first load", |app, cx| {
        app.data_state(cx).starts_with("Rows 1–10,000 of")
    });
    let fetched = |app: &Fixture, cx: &mut TestAppContext| {
        let grid = app.data_grid(cx);
        cx.update(|cx| grid.read(cx).delegate().row_count())
    };
    app.dispatch(cx, actions::FetchAllRows);
    app.wait_until(cx, "the count to grow", |app, cx| {
        fetched(app, cx) >= 20_000
    });
    app.click(cx, "data-cancel");
    app.wait_until(cx, "the stop", |app, cx| !app.has(cx, "data-cancel"));
    let stopped = fetched(&app, cx);
    settle(&app, cx);
    assert_eq!(fetched(&app, cx), stopped, "nothing more after Cancel");
    assert!(stopped < 1_000_000, "{stopped}");
    // Stopped between two loads, or its load cancelled.
    let way_on = app.label_of(cx, "data-fetch-more");
    assert!(
        way_on == "Fetch 10,000 more"
            || (way_on == "Try again"
                && app.label_of(cx, "data-fetch-error") == "Cancelled: no more rows were read."),
        "{way_on}"
    );
}
