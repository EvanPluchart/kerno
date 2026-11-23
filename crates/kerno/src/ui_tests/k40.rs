//! K40 (docs/plans/M12.md): a table's data tab keeps its columns scrolled
//! through a sort, a filter and a refresh; the order of its columns comes
//! back; its WHERE field proposes the table's columns.

use super::grid_v11::{layout_key, stored_layout, wait_for_layout};
use super::*;
use kerno_core::browse::Filter;

/// The scratch schema with what `setup` makes in it, dropped at the end.
fn wide_table(engine: Engine, schema: &str, setup: &[&str]) -> Scratch {
    let (drop_schema, create_schema) = scratch_schema(engine, schema);
    let mut statements = vec![create_schema.as_str()];
    statements.extend_from_slice(setup);
    Scratch::new(engine, &[&drop_schema], &statements)
}

fn offset_x(cx: &mut TestAppContext, grid: &Entity<TableState<Grid>>) -> f32 {
    cx.update(|cx| f32::from(grid.read(cx).horizontal_scroll_handle.offset().x))
}

/// Scrolled to the right, a sort from a header, a filter and a refresh read
/// the rows again from the first; the columns stay where they were, the
/// sorted one in view.
#[gpui_kit::test]
fn a_sort_a_filter_and_a_refresh_keep_the_columns_scrolled(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_test_k40_scroll";
        let columns: Vec<String> = (1..=40).map(|n| format!("c{n} INT")).collect();
        let create = format!(
            "CREATE TABLE {schema}.t (id INT PRIMARY KEY, {})",
            columns.join(", ")
        );
        let values: Vec<String> = (1..=3)
            .map(|row| {
                let cells: Vec<String> = (0..=40).map(|n| (row * 100 + n).to_string()).collect();
                format!("({})", cells.join(", "))
            })
            .collect();
        let insert = format!("INSERT INTO {schema}.t VALUES {}", values.join(", "));
        let _scratch = wide_table(engine, schema, &[&create, &insert]);
        let app = connected(cx, engine);
        app.open_table(cx, schema, "t", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–3 of 3");
        let grid = app.data_grid(cx);
        app.step(cx, |_, cx| {
            grid.update(cx, |table, cx| table.scroll_to_col(30, cx))
        });
        app.step(cx, |_, _| {});
        let scrolled = offset_x(cx, &grid);
        assert!(scrolled < -500.0, "{engine:?}: {scrolled}");

        // A sort from the header of `c30` (the row numbers are column 0).
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| {
                table
                    .delegate_mut()
                    .perform_sort(31, ColumnSort::Descending, window, cx)
            })
        });
        app.wait_until(cx, "the sorted rows", |app, cx| {
            let tab = app.data(cx);
            cx.update(|cx| tab.read(cx).sort() == Some(("c30", true)))
                && app.data_grid(cx) != grid
                && app.data_state(cx) == "Rows 1–3 of 3"
        });
        app.step(cx, |_, _| {});
        let sorted = app.data_grid(cx);
        assert_eq!(app.data_rows(cx)[0][0], "300", "{engine:?}");
        assert!(
            (offset_x(cx, &sorted) - scrolled).abs() < 1.0,
            "{engine:?}: {} after the sort, {scrolled} before",
            offset_x(cx, &sorted)
        );

        // A filter, then a refresh.
        let tab = app.data(cx);
        app.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| {
                tab.set_filter(
                    Filter {
                        conditions: Vec::new(),
                        raw: "c30 > 200".into(),
                    },
                    window,
                    cx,
                )
            })
        });
        app.wait_until(cx, "the filtered rows", |app, cx| {
            app.data_grid(cx) != sorted && app.data_state(cx) == "Rows 1–2 of 2"
        });
        app.step(cx, |_, _| {});
        let filtered = app.data_grid(cx);
        assert!(
            (offset_x(cx, &filtered) - scrolled).abs() < 1.0,
            "{engine:?}"
        );
        app.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| tab.reload(window, cx))
        });
        app.wait_until(cx, "the rows read again", |app, cx| {
            app.data_grid(cx) != filtered && app.data_state(cx) == "Rows 1–2 of 2"
        });
        app.step(cx, |_, _| {});
        let refreshed = app.data_grid(cx);
        assert!(
            (offset_x(cx, &refreshed) - scrolled).abs() < 1.0,
            "{engine:?}"
        );
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
    }
}

/// A column moved by its header (the kit's drop calls the grid's
/// `move_column`) keeps its place when the tab is opened again and after a
/// restart, with the order on the server.
#[gpui_kit::test]
fn a_column_moved_keeps_its_place_after_closing_and_a_restart(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_test_k40_layout";
        let _scratch = wide_table(
            engine,
            schema,
            &[
                &format!("CREATE TABLE {schema}.m (id INT PRIMARY KEY, a INT, b INT, c INT)"),
                &format!("INSERT INTO {schema}.m VALUES (1, 10, 100, 7), (2, 20, 200, 8)"),
            ],
        );
        let key = layout_key(engine, schema, "m");
        let app = connected(cx, engine);
        app.open_table(cx, schema, "m", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        let grid = app.data_grid(cx);
        // `b` dragged before `id`.
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| {
                table.delegate_mut().move_column(3, 1, window, cx);
                table.refresh(cx);
            })
        });
        let moved = vec!["b".to_string(), "id".into(), "a".into(), "c".into()];
        let shown = |app: &Fixture, cx: &mut TestAppContext| {
            let grid = app.data_grid(cx);
            cx.update(|cx| grid.read(cx).delegate().shown_columns())
        };
        assert_eq!(shown(&app, cx), moved, "{engine:?}");
        wait_for_layout(&app, cx, &key, |stored| {
            stored.is_some_and(|stored| stored.order == moved)
        });

        // Closed and opened again.
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
        app.open_table(cx, schema, "m", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(shown(&app, cx), moved, "{engine:?}");
        assert_eq!(app.data_rows(cx)[0][0], "100", "{engine:?}");
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });

        // After a restart: a new `Storage` on the same store.
        let restarted = open(cx, app.storage.clone());
        let id = cx.update(|cx| restarted.connections.read(cx).saved()[0].id.clone());
        let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
        kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
        restarted.step(cx, |_, cx| {
            restarted
                .connections
                .update(cx, |connections, cx| connections.connect(&id, cx))
        });
        restarted.open_table(cx, schema, "m", RelationKind::Table);
        restarted.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(shown(&restarted, cx), moved, "{engine:?}");
        assert_eq!(
            stored_layout(&restarted, cx, &key).map(|stored| stored.order),
            Some(moved.clone())
        );
        restarted.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
    }
}

/// The open state and the labels and details of the WHERE field's list.
fn proposals(app: &Fixture, cx: &mut TestAppContext) -> (bool, Vec<(String, String)>) {
    let tab = app.data(cx);
    cx.update(|cx| {
        let menu = tab.read(cx).raw_filter().read(cx).completion_menu_state();
        let items = menu
            .items
            .iter()
            .map(|item| (item.label.clone(), item.detail.clone().unwrap_or_default()))
            .collect();
        (menu.open, items)
    })
}

fn where_text(app: &Fixture, cx: &mut TestAppContext) -> String {
    let tab = app.data(cx);
    cx.update(|cx| tab.read(cx).raw_filter().read(cx).value().to_string())
}

/// Typing in the WHERE field of a table proposes its columns with their
/// types; Tab or Enter takes one (quoted where the engine needs it), Escape
/// closes the list, Enter with the list closed applies the filter.
#[gpui_kit::test]
fn the_where_field_proposes_the_tables_columns(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_test_k40_where";
        let quoted = match engine {
            Engine::Postgres => "\"full name\"",
            _ => "`full name`",
        };
        let _scratch = wide_table(
            engine,
            schema,
            &[
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
        app.open_table(cx, schema, "account", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–3 of 3");
        app.click(cx, "filter-raw");
        app.type_text(cx, "pro");
        let (open, items) = proposals(&app, cx);
        assert!(open, "{engine:?}: {items:?}");
        let provider = items
            .iter()
            .find(|(label, _)| label == "provider_id")
            .unwrap_or_else(|| panic!("{engine:?}: {items:?}"));
        assert!(provider.1.contains("int"), "{engine:?}: {provider:?}");
        // Escape closes the list and leaves the text.
        app.press(cx, &["escape"]);
        assert!(!proposals(&app, cx).0, "{engine:?}");
        assert_eq!(where_text(&app, cx), "pro");
        // Tab takes the first proposal of a longer word.
        app.type_text(cx, "v");
        assert_eq!(proposals(&app, cx).1[0].0, "provider_id", "{engine:?}");
        app.press(cx, &["tab"]);
        assert_eq!(where_text(&app, cx), "provider_id", "{engine:?}");
        // What a condition is made of, after a column; not the rest of a
        // statement.
        let words = |app: &Fixture, cx: &mut TestAppContext, typed: &str| {
            app.type_text(cx, typed);
            let (_, items) = proposals(app, cx);
            app.press(cx, &["escape", "backspace"]);
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
        assert_eq!(where_text(&app, cx), "provider_id ");
        // With the list closed, Enter applies the filter.
        app.type_text(cx, "> 1");
        if proposals(&app, cx).0 {
            app.press(cx, &["escape"]);
        }
        app.press(cx, &["enter"]);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        assert_eq!(where_text(&app, cx), "provider_id > 1");

        // A name that needs quotes is inserted quoted; Enter takes it.
        app.type_text(cx, " and full");
        let (open, items) = proposals(&app, cx);
        assert!(open, "{engine:?}");
        assert_eq!(items[0].0, "full name", "{engine:?}: {items:?}");
        app.press(cx, &["enter"]);
        assert_eq!(
            where_text(&app, cx),
            format!("provider_id > 1 and {quoted}"),
            "{engine:?}"
        );
        app.type_text(cx, " = 'y'");
        app.press(cx, &["enter"]);
        app.wait_for_rows(cx, "Rows 1–1 of 1");
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
    }
}
