//! K17 (the owner's request of 2026-11-18): the value panel docked beside
//! the grid, following the selected cell, in a query tab and a data tab;
//! JSON laid out, edits through the pending changes, its state and width
//! kept across a restart. Scratch objects `kerno_ui_k17_*`.

use gpui_kit::App;
use gpui_kit::component::table::TableEvent;

use super::tool_wizards::paints;
use super::*;
use crate::query::value_panel::ValuePanel;

/// The value panel of the active query tab, while shown.
fn query_panel(app: &Fixture, cx: &mut TestAppContext) -> Option<Entity<ValuePanel>> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).value_panel())
}

/// The value panel of the active data tab, while shown.
fn data_panel(app: &Fixture, cx: &mut TestAppContext) -> Entity<ValuePanel> {
    let tab = app.data(cx);
    cx.update(|cx| tab.read(cx).value_panel())
        .expect("the value panel")
}

/// Reads something of a panel.
fn read<R>(
    cx: &mut TestAppContext,
    panel: &Entity<ValuePanel>,
    read: impl FnOnce(&ValuePanel, &App) -> R,
) -> R {
    cx.update(|cx| read(panel.read(cx), cx))
}

/// A double click on a cell of the grid.
fn double_click(
    app: &Fixture,
    cx: &mut TestAppContext,
    grid: &Entity<TableState<Grid>>,
    cell: (usize, usize),
) {
    select_cell(app, cx, grid, cell);
    app.step(cx, |_, cx| {
        grid.update(cx, |_, cx| {
            cx.emit(TableEvent::DoubleClickedCell(cell.0, cell.1))
        })
    });
}

/// Whether the panel's editor has the keyboard.
fn editor_focused(app: &Fixture, cx: &mut TestAppContext, panel: &Entity<ValuePanel>) -> bool {
    app.step(cx, |window, cx| panel.read(cx).editor_focused(window, cx))
}

/// F7 shows the panel beside a result; it follows the selection and says
/// what it shows: JSON indented and highlighted, the empty string and NULL
/// apart, a Unix time, several cells, why a result takes no value; Cmd+Enter
/// in it never runs the statement; the bar's button hides it.
#[gpui_kit::test]
fn the_panel_follows_the_selected_cell_of_a_result(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select '{\"a\": [1, 2]}'::jsonb as doc, '' as blank, null::text as nothing, \
         1794619425 as at, clock_timestamp()::text as ran",
        "secondary-enter",
    );
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert!(query_panel(&app, cx).is_none());
    assert!(!app.has(cx, "value-panel"));
    app.press(cx, &["f7"]);
    let panel = query_panel(&app, cx).expect("the value panel");
    assert!(app.has(cx, "value-panel"));
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");

    select_cell(&app, cx, &grid, (0, 1));
    assert_eq!(
        read(cx, &panel, |panel, _| panel.header()).as_deref(),
        Some("doc · jsonb")
    );
    assert_eq!(
        read(cx, &panel, |panel, cx| panel.text(cx)),
        "{\n  \"a\": [\n    1,\n    2\n  ]\n}"
    );
    assert!(read(cx, &panel, |panel, cx| panel.highlights_json(cx)));
    // A result of no table: shown, not edited, and the panel says why.
    let why = read(cx, &panel, |panel, _| {
        panel.why_read_only().map(str::to_string)
    });
    assert_eq!(
        why.as_deref(),
        Some("Read-only result: no column comes from a table.")
    );
    assert!(app.has(cx, "value-panel-why"));
    assert!(!app.has(cx, "value-panel-set"));

    // The empty string and NULL are told apart.
    select_cell(&app, cx, &grid, (0, 2));
    assert_eq!(
        read(cx, &panel, |panel, _| panel.status()).as_deref(),
        Some("Empty string")
    );
    assert_eq!(read(cx, &panel, |panel, cx| panel.text(cx)), "");
    select_cell(&app, cx, &grid, (0, 3));
    assert_eq!(
        read(cx, &panel, |panel, _| panel.status()).as_deref(),
        Some("NULL")
    );
    assert!(app.has(cx, "value-panel-null"));
    // A Unix time says when.
    select_cell(&app, cx, &grid, (0, 4));
    assert!(app.has(cx, "value-unix-time"));
    assert!(paints(
        &app,
        cx,
        "Unix seconds \u{2192} 2026-11-14 01:23:45 UTC"
    ));

    // Cmd+Enter in the panel sets (nothing, here): the statement does not
    // run again, or its clock would move.
    select_cell(&app, cx, &grid, (0, 5));
    let ran = read(cx, &panel, |panel, cx| panel.text(cx));
    app.step(cx, |window, cx| {
        Grid::view_selected_value(&grid, window, cx)
    });
    assert!(editor_focused(&app, cx, &panel));
    app.press(cx, &["secondary-enter"]);
    assert!(!app.running(cx));
    let (_, rows) = app.result(cx, 0, 1);
    assert_eq!(rows[0][4].as_deref(), Some(ran.as_str()));
    // Escape gives the keyboard back to the grid.
    app.press(cx, &["escape"]);
    assert!(app.step(cx, |window, cx| {
        grid.read(cx).focus_handle(cx).is_focused(window)
    }));

    // Several cells: the panel says what to do.
    app.step(cx, |_, cx| {
        grid.update(cx, |table, cx| {
            table.delegate_mut().extend_to(0, 3);
            cx.notify();
        })
    });
    assert!(app.has(cx, "value-panel-empty"));
    assert!(paints(&app, cx, "Several cells are selected"));

    // The bar's button hides it.
    app.click(cx, "result-value-panel");
    assert!(query_panel(&app, cx).is_none());
    assert!(!app.has(cx, "value-panel"));
}

/// A table's JSON edited in the panel: invalid JSON is said and not
/// written, the text waits while the selection moves on, Cmd+Enter makes it
/// a pending change, and Apply writes it (PostgreSQL `jsonb`, MySQL `JSON`).
#[gpui_kit::test]
fn json_edited_in_the_panel_is_pending_then_applied(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_k17_edit";
        let (cleanup, create) = scratch_schema(engine, schema);
        let json = if engine == Engine::Postgres {
            "jsonb"
        } else {
            "json"
        };
        let _scratch = Scratch::new(
            engine,
            &[&cleanup],
            &[
                &create,
                &format!(
                    "CREATE TABLE {schema}.items (id INT PRIMARY KEY, doc {json}, note VARCHAR(20))"
                ),
                &format!(
                    "INSERT INTO {schema}.items VALUES (1, '{{\"a\": 1}}', 'x'), (2, NULL, NULL)"
                ),
            ],
        );
        let app = connected(cx, engine);
        app.dispatch(cx, actions::ToggleValuePanel);
        app.open_table(cx, schema, "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–2 of 2");
        let grid = app.data_grid(cx);
        let panel = data_panel(&app, cx);

        // A double click puts the keyboard in the panel's editor.
        double_click(&app, cx, &grid, (0, 2));
        assert_eq!(
            read(cx, &panel, |panel, cx| panel.text(cx)),
            "{\n  \"a\": 1\n}",
            "{engine:?}"
        );
        assert!(editor_focused(&app, cx, &panel), "{engine:?}");

        // Broken JSON: said with where it breaks, nothing pending.
        app.press(cx, &["secondary-a", "backspace"]);
        app.type_text(cx, "{\"b\": ");
        app.press(cx, &["secondary-enter"]);
        let problem = read(cx, &panel, |panel, _| panel.problem().map(str::to_string));
        assert!(
            problem
                .as_deref()
                .is_some_and(|problem| problem.contains("line 1 column")),
            "{problem:?}"
        );
        assert!(app.has(cx, "value-panel-problem"));
        assert_eq!(app.pending(cx), 0);

        // The selection moves on: the panel keeps the text and its cell.
        select_cell(&app, cx, &grid, (1, 3));
        assert!(read(cx, &panel, |panel, _| panel.has_draft()));
        let header = read(cx, &panel, |panel, _| panel.header()).unwrap_or_default();
        assert!(header.starts_with("doc · "), "{header}");
        assert!(paints(&app, cx, "this text is for row 1"));

        // Mended, and set with Cmd+Enter: one pending change; the panel
        // follows the selection again.
        double_click(&app, cx, &grid, (1, 3));
        assert!(editor_focused(&app, cx, &panel));
        app.press(cx, &["secondary-a", "backspace"]);
        app.type_text(cx, "{\"b\": [true, null]}");
        app.press(cx, &["secondary-enter"]);
        assert_eq!(app.pending(cx), 1, "{engine:?}");
        assert!(!read(cx, &panel, |panel, _| panel.has_draft()));
        let header = read(cx, &panel, |panel, _| panel.header()).unwrap_or_default();
        assert!(header.starts_with("note · "), "{header}");
        let tab = app.data(cx);
        let (_, statements) = cx.update(|cx| tab.read(cx).pending_sql(cx)).unwrap();
        assert_eq!(statements.len(), 1);
        assert!(
            statements[0].contains("'{\"b\": [true, null]}'"),
            "{statements:?}"
        );

        app.click(cx, "data-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the applied change", |app, cx| {
            app.pending(cx) == 0 && app.data_state(cx) == "Rows 1–2 of 2"
        });
        let stored = match engine {
            Engine::Postgres => "SELECT doc::text FROM kerno_ui_k17_edit.items WHERE id = 1",
            _ => "SELECT CAST(doc AS CHAR) FROM kerno_ui_k17_edit.items WHERE id = 1",
        };
        assert_eq!(
            on_server(engine, stored),
            "{\"b\": [true, null]}",
            "{engine:?}"
        );
    }
}

/// The bar's button shows the panel; its width, once dragged, and its
/// shown state are there again after a restart.
#[gpui_kit::test]
async fn the_panel_and_its_width_come_back_after_a_restart(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(cx, "select 1 as n", "secondary-enter");
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    app.click(cx, "result-value-panel");
    let panel = query_panel(&app, cx).expect("the value panel");
    let split = read(cx, &panel, |panel, _| panel.split());
    // What the end of a drag of its edge does.
    app.step(cx, |window, cx| {
        split.update(cx, |split, cx| split.resize_panel(1, px(500.), window, cx))
    });
    let stored = app.storage.clone();
    cx.wait_for(app.window.into(), Duration::from_secs(10), move |_, _| {
        let settings = Settings::load(&stored);
        settings.value_panel && settings.value_panel_width == 500.0
    })
    .await;

    // The next launch shows it, at that width, beside a result.
    let restarted = open(cx, app.storage.clone());
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let id = cx.update(|cx| restarted.connections.read(cx).saved()[0].id.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    restarted.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    restarted.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    restarted.run_in_new_tab(cx, "select 2 as n", "secondary-enter");
    restarted.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert!(restarted.has(cx, "value-panel"));
    let panel = query_panel(&restarted, cx).expect("the value panel");
    let split = read(cx, &panel, |panel, _| panel.split());
    assert_eq!(
        cx.update(|cx| split.read(cx).sizes().get(1).copied()),
        Some(px(500.))
    );
}
