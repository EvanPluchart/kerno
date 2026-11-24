//! K59 (docs/plans/M12.md): the field that filters a query's result on the
//! server proposes the result's columns and the words of a condition, as a
//! table's WHERE field does (K40); Show Completions opens the list in both.
//! Fixtures read only; scratch objects `kerno_test_k59_*`.

use super::*;

/// Whether the result filter's list is open, and its labels and details.
fn proposals(app: &Fixture, cx: &mut TestAppContext) -> (bool, Vec<(String, String)>) {
    let tab = app.tab(cx);
    cx.update(|cx| {
        let menu = tab
            .read(cx)
            .result_filter()
            .read(cx)
            .completion_menu_state();
        let items = menu
            .items
            .iter()
            .map(|item| (item.label.clone(), item.detail.clone().unwrap_or_default()))
            .collect();
        (menu.open, items)
    })
}

fn filter_text(app: &Fixture, cx: &mut TestAppContext) -> String {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).typed_result_filter(cx))
}

/// The result's filter proposes its columns with their types, an alias
/// and a name that needs quotes too (inserted quoted), then the words of a
/// condition, never Kerno's alias of the derived table; Tab and Enter take
/// a proposal, Escape closes the list, Enter with it closed applies the
/// filter; Ctrl+Space opens the list again.
#[gpui_kit::test]
fn the_result_filter_proposes_the_results_columns(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            continue;
        }
        let schema = "kerno_test_k59_filter";
        let quoted = match engine {
            Engine::Postgres => "\"full name\"",
            _ => "`full name`",
        };
        let (drop_schema, create_schema) = scratch_schema(engine, schema);
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &format!(
                    "CREATE TABLE {schema}.account (id INT PRIMARY KEY, provider_id INT, \
                     {quoted} VARCHAR(20))"
                ),
                &format!(
                    "INSERT INTO {schema}.account VALUES (1, 1, 'x'), (2, 2, 'x'), (3, 3, 'y')"
                ),
            ],
        );
        let app = connected(cx, engine);
        app.run_in_new_tab(
            cx,
            &format!(
                "select a.id, a.provider_id, a.{quoted}, a.id * 2 doubled from {schema}.account a"
            ),
            "secondary-enter",
        );
        app.wait_until(cx, "the accounts", |app, cx| {
            app.outcomes(cx) == ["3 rows"] && !app.running(cx)
        });
        app.click(cx, "result-filter");
        app.type_text(cx, "pro");
        let (open, items) = proposals(&app, cx);
        assert!(open, "{engine:?}: {items:?}");
        let provider = items
            .iter()
            .find(|(label, _)| label == "provider_id")
            .unwrap_or_else(|| panic!("{engine:?}: {items:?}"));
        assert!(
            provider.1.to_lowercase().contains("int"),
            "{engine:?}: {provider:?}"
        );
        // Escape closes the list and leaves the text.
        app.press(cx, &["escape"]);
        assert!(!proposals(&app, cx).0, "{engine:?}");
        assert_eq!(filter_text(&app, cx), "pro");
        // Tab takes the first proposal of a longer word.
        app.type_text(cx, "v");
        assert_eq!(proposals(&app, cx).1[0].0, "provider_id", "{engine:?}");
        app.press(cx, &["tab"]);
        assert_eq!(filter_text(&app, cx), "provider_id", "{engine:?}");
        // What a condition is made of; not the rest of a statement, not the
        // derived table's alias.
        let words = |app: &Fixture, cx: &mut TestAppContext, typed: &str| {
            app.type_text(cx, typed);
            let (open, items) = proposals(app, cx);
            if open {
                app.press(cx, &["escape"]);
            }
            for _ in 0..typed.len() {
                app.press(cx, &["backspace"]);
            }
            items
                .into_iter()
                .map(|(label, _)| label)
                .collect::<Vec<_>>()
        };
        app.type_text(cx, " ");
        let i = words(&app, cx, "i");
        for word in ["IN", "IS NULL", "IS NOT NULL"] {
            assert!(i.iter().any(|label| label == word), "{engine:?}: {i:?}");
        }
        assert_eq!(
            i.iter().any(|label| label == "ILIKE"),
            engine == Engine::Postgres,
            "{engine:?}: {i:?}"
        );
        let o = words(&app, cx, "o");
        assert!(o.iter().any(|label| label == "OR"), "{engine:?}: {o:?}");
        assert!(
            !o.iter().any(|label| label == "ORDER BY"),
            "{engine:?}: {o:?}"
        );
        let k = words(&app, cx, "ker");
        assert!(
            !k.iter().any(|label| label == "kerno_q"),
            "{engine:?}: {k:?}"
        );
        assert_eq!(filter_text(&app, cx), "provider_id ");
        // With the list closed, Enter applies the filter.
        app.type_text(cx, "> 1");
        if proposals(&app, cx).0 {
            app.press(cx, &["escape"]);
        }
        app.press(cx, &["enter"]);
        app.wait_until(cx, "the filtered accounts", |app, cx| {
            app.outcomes(cx) == ["2 rows"] && !app.running(cx)
        });
        assert_eq!(filter_text(&app, cx), "provider_id > 1");
        // A name that needs quotes is inserted quoted; Enter takes it. A
        // column the statement named itself is proposed too.
        app.click(cx, "result-filter");
        app.press(cx, &["end"]);
        app.type_text(cx, " and full");
        let (open, items) = proposals(&app, cx);
        assert!(open, "{engine:?}");
        assert_eq!(items[0].0, "full name", "{engine:?}: {items:?}");
        app.press(cx, &["enter"]);
        assert_eq!(
            filter_text(&app, cx),
            format!("provider_id > 1 and {quoted}"),
            "{engine:?}"
        );
        // Only that: the half-typed condition is not run (it was on MySQL).
        assert!(!app.running(cx), "{engine:?}");
        assert_eq!(app.outcomes(cx), ["2 rows"], "{engine:?}");
        app.type_text(cx, " = 'y' and dou");
        let (open, items) = proposals(&app, cx);
        assert!(open, "{engine:?}: {items:?}");
        assert_eq!(items[0].0, "doubled", "{engine:?}: {items:?}");
        // Escape closes it, Show Completions opens it again.
        app.press(cx, &["escape"]);
        assert!(!proposals(&app, cx).0, "{engine:?}");
        app.press(cx, &["ctrl-space"]);
        let (open, items) = proposals(&app, cx);
        assert!(open, "{engine:?}: Ctrl+Space");
        assert_eq!(items[0].0, "doubled", "{engine:?}: {items:?}");
        app.press(cx, &["enter"]);
        app.type_text(cx, " = 6");
        app.press(cx, &["enter"]);
        app.wait_until(cx, "the one account", |app, cx| {
            app.outcomes(cx) == ["1 rows"] && !app.running(cx)
        });
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
    }
}

/// Show Completions (Ctrl+Space) opens the list of a table's WHERE field,
/// empty or not: it was the query editor's only.
#[gpui_kit::test]
fn show_completions_opens_the_list_of_a_tables_where_field(cx: &mut TestAppContext) {
    for (engine, schema) in [(Engine::Postgres, "shop"), (Engine::MySql, "kerno")] {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        app.open_table(cx, schema, "customers", RelationKind::Table);
        app.wait_until(cx, "the customers", |app, cx| {
            app.data_state(cx).starts_with("Rows 1–")
        });
        app.click(cx, "filter-raw");
        let menu = |app: &Fixture, cx: &mut TestAppContext| {
            let tab = app.data(cx);
            cx.update(|cx| {
                let menu = tab.read(cx).raw_filter().read(cx).completion_menu_state();
                let labels: Vec<String> = menu.items.iter().map(|i| i.label.clone()).collect();
                (menu.open, labels)
            })
        };
        assert!(!menu(&app, cx).0, "{engine:?}");
        app.press(cx, &["ctrl-space"]);
        let (open, labels) = menu(&app, cx);
        assert!(open, "{engine:?}: {labels:?}");
        assert!(
            labels.iter().any(|label| label == "email"),
            "{engine:?}: {labels:?}"
        );
        app.press(cx, &["escape"]);
        app.type_text(cx, "em");
        app.press(cx, &["escape"]);
        assert!(!menu(&app, cx).0, "{engine:?}");
        app.press(cx, &["ctrl-space"]);
        let (open, labels) = menu(&app, cx);
        assert!(open, "{engine:?}");
        assert_eq!(labels[0], "email", "{engine:?}: {labels:?}");
        // Enter takes it and does not apply "em".
        app.press(cx, &["enter"]);
        let tab = app.data(cx);
        assert_eq!(
            cx.update(|cx| tab.read(cx).raw_filter().read(cx).value().to_string()),
            "email"
        );
        assert_eq!(
            cx.update(|cx| tab.read(cx).applied_filter().clone()),
            kerno_core::browse::Filter::default(),
            "{engine:?}"
        );
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
    }
}

// Part 2: foreign keys marked and followed in more results (K59). -----------

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

/// The tooltip an arrow leading to `label` has.
fn tip(cx: &mut TestAppContext, label: &str) -> String {
    cx.update(|cx| {
        let key = crate::query::grid::follow_key(cx);
        crate::query::grid::arrow_tip(label, cx.platform_info().is_mac(), key.as_deref())
    })
}

/// Waits until column `column` of the result on screen carries `expected`
/// as its mark's tooltip (the keys come once the catalog and, on
/// PostgreSQL, the names of the result's tables are read).
fn wait_mark(app: &Fixture, cx: &mut TestAppContext, column: usize, expected: &str) {
    app.wait_until(cx, &format!("the mark {expected:?}"), |app, cx| {
        mark(app, cx, column).as_deref() == Some(expected)
    });
}

/// The active data tab's table and filter.
fn shown(app: &Fixture, cx: &mut TestAppContext) -> (TableRef, kerno_core::browse::Filter) {
    let tab = app.data(cx);
    cx.update(|cx| {
        let tab = tab.read(cx);
        (tab.table().clone(), tab.applied_filter().clone())
    })
}

/// Waits for the `count`-th tab, a data tab, to read its rows.
fn opened(app: &Fixture, cx: &mut TestAppContext, count: usize) {
    app.wait_until(cx, "the referenced rows", |app, cx| {
        app.titles(cx).len() == count && app.data_state(cx).starts_with("Rows 1–")
    });
}

fn rows(app: &Fixture, cx: &mut TestAppContext) {
    app.wait_until(cx, "the rows", |app, cx| {
        !app.running(cx)
            && app
                .outcomes(cx)
                .iter()
                .any(|outcome| outcome.contains("rows"))
    });
}

/// Scratch tables: a key of one column held by a binary column, a column
/// in two keys, a key of two columns. The binary keys are `bytea` on
/// PostgreSQL, `BINARY(16)` on MySQL and MariaDB.
fn keys_scratch(engine: Engine) -> Scratch {
    let (drop_schema, create_schema) = scratch_schema(engine, "kerno_test_k59");
    let binary = match engine {
        Engine::Postgres => "BYTEA",
        _ => "BINARY(16)",
    };
    let bytes = match engine {
        Engine::Postgres => "'\\x00112233445566778899aabbccddeeff'::bytea",
        _ => "X'00112233445566778899AABBCCDDEEFF'",
    };
    Scratch::new(
        engine,
        &[&drop_schema],
        &[
            &create_schema,
            "CREATE TABLE kerno_test_k59.parents (id INT PRIMARY KEY, name VARCHAR(20))",
            "CREATE TABLE kerno_test_k59.others (id INT PRIMARY KEY, name VARCHAR(20))",
            "CREATE TABLE kerno_test_k59.pairs (a INT, b INT, name VARCHAR(20), PRIMARY KEY (a, b))",
            &format!(
                "CREATE TABLE kerno_test_k59.bins (id {binary} PRIMARY KEY, name VARCHAR(20))"
            ),
            &format!(
                "CREATE TABLE kerno_test_k59.kids (id INT PRIMARY KEY, ref_id INT, pa INT, pb INT, \
                 bin {binary}, \
                 FOREIGN KEY (ref_id) REFERENCES kerno_test_k59.parents (id), \
                 FOREIGN KEY (ref_id) REFERENCES kerno_test_k59.others (id), \
                 FOREIGN KEY (pa, pb) REFERENCES kerno_test_k59.pairs (a, b), \
                 FOREIGN KEY (bin) REFERENCES kerno_test_k59.bins (id))"
            ),
            "INSERT INTO kerno_test_k59.parents VALUES (1, 'parent one'), (2, 'parent two')",
            "INSERT INTO kerno_test_k59.others VALUES (1, 'other one'), (2, 'other two')",
            "INSERT INTO kerno_test_k59.pairs VALUES (1, 2, 'one-two')",
            &format!("INSERT INTO kerno_test_k59.bins VALUES ({bytes}, 'the bytes')"),
            &format!("INSERT INTO kerno_test_k59.kids VALUES (1, 2, 1, 2, {bytes})"),
        ],
    )
}

/// A join's columns carry their tables' keys whatever the statement:
/// `customer_id` of PostgreSQL's fixture join, under an alias too, its
/// arrow opening the customer; the result is not editable (a join) and
/// follows all the same.
#[gpui_kit::test]
fn a_join_and_an_alias_are_marked_and_followed(cx: &mut TestAppContext) {
    for (engine, schema) in [(Engine::Postgres, "shop"), (Engine::MySql, "kerno")] {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        let prefix = if engine == Engine::Postgres {
            "shop."
        } else {
            ""
        };
        app.run_in_new_tab(
            cx,
            &format!(
                "select o.id, o.customer_id, c.full_name from {prefix}orders o \
                 join {prefix}customers c on c.id = o.customer_id order by o.id"
            ),
            "secondary-enter",
        );
        rows(&app, cx);
        let customers = format!("→ {schema}.customers (id)");
        wait_mark(&app, cx, 2, &customers);
        // The mark came after the first layout: its column was widened for
        // it, the type beside it not cut.
        let (width, wanted) = {
            let grid = app.result_grid(cx, 0);
            cx.update(|cx| {
                let table = grid.read(cx);
                (
                    table.col_width(2).unwrap(),
                    table.delegate().content_width(1, cx),
                )
            })
        };
        assert!(width >= wanted, "{engine:?}: {width:?} for {wanted:?}");
        assert_eq!(mark(&app, cx, 1), None, "{engine:?}");
        assert_eq!(mark(&app, cx, 3), None, "{engine:?}");
        assert!(
            app.result_verdict(cx).is_some(),
            "{engine:?}: a join is not editable"
        );
        let grid = app.result_grid(cx, 0);
        let customer = cx.update(|cx| {
            let grid = grid.read(cx).delegate();
            String::from_utf8_lossy(grid.cell(0, 1).unwrap()).into_owned()
        });
        let label = format!("Open {schema}.customers where id = {customer}");
        hover(&app, cx, cell(0, 2));
        assert_eq!(
            said(&app, cx, arrow(0, 2)),
            Some(tip(cx, &label)),
            "{engine:?}"
        );
        app.click(cx, arrow(0, 2));
        opened(&app, cx, 2);
        let (table, filter) = shown(&app, cx);
        assert_eq!(table, TableRef::new(schema, "customers"), "{engine:?}");
        assert_eq!(filter.conditions.len(), 1, "{engine:?}");
        assert_eq!(app.data_rows(cx).len(), 1, "{engine:?}");

        // Read again filtered, the result keeps its marks and asks no names
        // again (PostgreSQL asked once; MySQL names its tables itself).
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
        let tab = app.tab(cx);
        let asked = cx.update(|cx| tab.read(cx).names_asked());
        assert_eq!(asked, usize::from(engine == Engine::Postgres), "{engine:?}");
        app.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| tab.filter_result("id > 0", window, cx))
        });
        app.wait_until(cx, "the filtered rows", |app, cx| {
            let filtered = cx.update(|cx| {
                tab.read(cx)
                    .result_requery()
                    .0
                    .is_some_and(|requery| requery.filter == "id > 0")
            });
            filtered && !app.running(cx)
        });
        wait_mark(&app, cx, 2, &customers);
        assert_eq!(
            cx.update(|cx| tab.read(cx).names_asked()),
            asked,
            "{engine:?}"
        );

        // An alias keeps the mark: the column is the table's.
        app.run_in_new_tab(
            cx,
            &format!("select customer_id as who, 1 + 1 as two from {prefix}orders"),
            "secondary-enter",
        );
        rows(&app, cx);
        wait_mark(&app, cx, 1, &customers);
        assert_eq!(mark(&app, cx, 2), None, "{engine:?}: an expression");
    }
}

/// On a read-only connection, whose results are never editable, a result
/// of one table follows its keys all the same: following only reads.
#[gpui_kit::test]
fn a_read_only_connection_follows_too(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    app.run_in_new_tab(
        cx,
        "select * from shop.orders order by id",
        "secondary-enter",
    );
    rows(&app, cx);
    wait_mark(&app, cx, 2, "→ shop.customers (id)");
    assert!(
        app.result_verdict(cx)
            .is_some_and(|why| why.contains("read-only connection")),
        "{:?}",
        app.result_verdict(cx)
    );
    hover(&app, cx, cell(0, 2));
    app.click(cx, arrow(0, 2));
    opened(&app, cx, 2);
    assert_eq!(shown(&app, cx).0, TableRef::new("shop", "customers"));
    assert_eq!(app.data_rows(cx).len(), 1);
}

/// A column in two keys names both in its header and opens a menu of the
/// two from its arrow; a key of two columns with one missing names it and
/// has no arrow; a binary key is followed with the engine's binary literal,
/// shortened in the tooltip. PostgreSQL and MySQL, on a join.
#[gpui_kit::test]
fn two_keys_a_missing_column_and_a_binary_key(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            continue;
        }
        let _scratch = keys_scratch(engine);
        let app = connected(cx, engine);
        app.run_in_new_tab(
            cx,
            "select k.id, k.ref_id, k.pa, k.bin, b.name from kerno_test_k59.kids k \
             join kerno_test_k59.bins b on b.id = k.bin",
            "secondary-enter",
        );
        rows(&app, cx);
        // Two keys: both in the header, one arrow, a menu of the two.
        app.wait_until(cx, "the two keys", |app, cx| {
            mark(app, cx, 2).is_some_and(|both| {
                let mut lines: Vec<&str> = both.lines().collect();
                lines.sort();
                lines
                    == [
                        "→ kerno_test_k59.others (id)",
                        "→ kerno_test_k59.parents (id)",
                    ]
            })
        });
        hover(&app, cx, cell(0, 2));
        let both = said(&app, cx, arrow(0, 2)).expect("an arrow");
        assert!(
            both.contains("Open kerno_test_k59.parents where id = 2")
                && both.contains("Open kerno_test_k59.others where id = 2"),
            "{engine:?}: {both}"
        );
        app.click(cx, arrow(0, 2));
        // The arrow stays while its menu is open, the pointer gone from it.
        hover(&app, cx, cell(0, 1));
        assert!(said(&app, cx, arrow(0, 2)).is_some(), "{engine:?}");
        click_menu_entry(&app, cx, "Open kerno_test_k59.others where id = 2");
        opened(&app, cx, 2);
        assert_eq!(
            shown(&app, cx).0,
            TableRef::new("kerno_test_k59", "others"),
            "{engine:?}"
        );
        assert_eq!(app.data_rows(cx).len(), 1, "{engine:?}");
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });

        // A key of two columns, `pb` missing: named, not followed.
        wait_mark(
            &app,
            cx,
            3,
            "(pa, pb) → kerno_test_k59.pairs (a, b) · not followed: pb is not in the result",
        );
        hover(&app, cx, cell(0, 3));
        assert_eq!(said(&app, cx, arrow(0, 3)), None, "{engine:?}");

        // A binary key: its literal, shortened in the tooltip.
        hover(&app, cx, cell(0, 4));
        let binary = said(&app, cx, arrow(0, 4)).expect("an arrow on a binary key");
        assert!(
            binary.starts_with("Open kerno_test_k59.bins where id = ") && binary.contains('…'),
            "{engine:?}: {binary}"
        );
        app.click(cx, arrow(0, 4));
        opened(&app, cx, 2);
        let (table, filter) = shown(&app, cx);
        assert_eq!(table, TableRef::new("kerno_test_k59", "bins"), "{engine:?}");
        assert!(filter.conditions.is_empty(), "{engine:?}: {filter:?}");
        assert!(
            filter
                .raw
                .to_lowercase()
                .contains("00112233445566778899aabbccddeeff"),
            "{engine:?}: {filter:?}"
        );
        assert_eq!(app.data_rows(cx).len(), 1, "{engine:?}");
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
    }
}

/// A SQLite result of a join is marked and followed: the bundled SQLite
/// says where each column comes from.
#[gpui_kit::test]
fn a_sqlite_join_is_marked_and_followed(cx: &mut TestAppContext) {
    use super::sqlite::{file_config, fixture_file};
    let dir = tempfile::tempdir().unwrap();
    let file = fixture_file(dir.path());
    let app = connected_as(cx, file_config(&file));
    app.run_in_new_tab(
        cx,
        "select o.id, o.customer_id, c.email from orders o \
         join customers c on c.id = o.customer_id order by o.id",
        "secondary-enter",
    );
    rows(&app, cx);
    wait_mark(&app, cx, 2, "→ main.customers (id)");
    hover(&app, cx, cell(0, 2));
    app.click(cx, arrow(0, 2));
    opened(&app, cx, 2);
    assert_eq!(shown(&app, cx).0, TableRef::new("main", "customers"));
    assert_eq!(app.data_rows(cx).len(), 1);
    // A result of one table, marked, says why its rows are not edited
    // there: not "no column comes from a table" under its marks.
    app.run_in_new_tab(cx, "select id, customer_id from orders", "secondary-enter");
    rows(&app, cx);
    wait_mark(&app, cx, 2, "→ main.customers (id)");
    let why = app.result_verdict(cx).expect("read-only");
    assert!(why.contains("open the table from the tree"), "{why}");
}

/// A result of 300 columns over 20 tables (PostgreSQL): its 19 keys are
/// found (the names in one query, timed by the driver test
/// `three_hundred_columns_over_twenty_tables_are_named_in_one_query`).
#[gpui_kit::test]
fn three_hundred_columns_over_twenty_tables_are_marked(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (drop_schema, create_schema) = scratch_schema(Engine::Postgres, "kerno_test_k59_wide");
    let mut setup = vec![create_schema];
    for table in 1..=20 {
        let columns: Vec<String> = (1..=13).map(|n| format!("c{n} INT")).collect();
        let parent = if table == 1 {
            String::new()
        } else {
            format!(
                ", parent INT REFERENCES kerno_test_k59_wide.t{} (id)",
                table - 1
            )
        };
        setup.push(format!(
            "CREATE TABLE kerno_test_k59_wide.t{table} (id INT PRIMARY KEY, {}{parent})",
            columns.join(", ")
        ));
    }
    for table in 1..=20 {
        let parent = if table == 1 { "" } else { ", 1" };
        let values: Vec<&str> = vec!["1"; 13];
        setup.push(format!(
            "INSERT INTO kerno_test_k59_wide.t{table} VALUES (1, {}{parent})",
            values.join(", ")
        ));
    }
    let statements: Vec<&str> = setup.iter().map(String::as_str).collect();
    let _scratch = Scratch::new(Engine::Postgres, &[&drop_schema], &statements);
    let app = connected(cx, Engine::Postgres);
    // The tab reads its catalog first: the time measured is the marks'.
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    app.wait_until(cx, "the catalog", |_, cx| {
        cx.update(|cx| tab.read(cx).catalog_ready())
    });
    // Fifteen columns of each table: its key, thirteen values and its
    // parent (the first: its key twice).
    let mut columns = Vec::new();
    let mut from = String::from("kerno_test_k59_wide.t1 t1");
    for table in 1..=20 {
        columns.push(format!("t{table}.id"));
        for n in 1..=13 {
            columns.push(format!("t{table}.c{n}"));
        }
        columns.push(if table == 1 {
            "t1.id as again".to_string()
        } else {
            format!("t{table}.parent")
        });
        if table > 1 {
            from.push_str(&format!(
                " join kerno_test_k59_wide.t{table} t{table} on t{table}.parent = t{}.id",
                table - 1
            ));
        }
    }
    assert_eq!(columns.len(), 300);
    app.type_text(cx, &format!("select {} from {from}", columns.join(", ")));
    app.press(cx, &["secondary-enter"]);
    rows(&app, cx);
    let grid = app.result_grid(cx, 0);
    app.wait_until(cx, "the 19 keys", |_, cx| {
        cx.update(|cx| grid.read(cx).delegate().keys().len()) == 19
    });
    // Each table's `parent` column leads to the table before (the 30th
    // column of the result, out of view: its header's tooltip).
    let link = cx.update(|cx| {
        grid.read(cx)
            .delegate()
            .link(29)
            .map(|link| link.to_string())
    });
    assert_eq!(link.as_deref(), Some("→ kerno_test_k59_wide.t1 (id)"));
}
