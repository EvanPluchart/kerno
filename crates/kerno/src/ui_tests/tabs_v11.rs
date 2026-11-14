//! J10 and J11 (CLAUDE.md §4 J, docs/plans/M11.md Part 8): the list of
//! every open tab and the tabs on several rows or at the side (DBeaver
//! #7114, #23248); the results of a script stacked in one panel (DBeaver
//! #6861). And the find bar kept over a result read again (Part 4).

use kerno_core::environment;

use super::*;
use crate::query::find;
use crate::settings::TabLayout;

/// A store holding `count` query tabs; every third works on a staging
/// connection (which is never opened: its colour is all that is drawn).
fn many_tabs(count: usize) -> Arc<Storage> {
    let storage = memory_storage();
    let staging = storage
        .save_connection(
            None,
            &ConnectionConfig {
                name: "kerno_ui_tabs staging".into(),
                environment: environment::STAGING.into(),
                ..testing::config(Engine::Postgres)
            },
        )
        .unwrap();
    let tabs: Vec<TabSnapshot> = (0..count)
        .map(|index| TabSnapshot::Query {
            title: format!("Query {}", index + 1),
            sql: String::new(),
            target: (index % 3 == 1).then(|| Target {
                connection: staging.clone(),
                database: None,
                schema: None,
            }),
            file: None,
            unsaved: false,
            source: None,
        })
        .collect();
    storage.save_tabs(&tabs).unwrap();
    storage
}

fn layout(cx: &mut TestAppContext) -> TabLayout {
    cx.update(|cx| AppState::get(cx).settings.tab_layout)
}

/// Clicks a tab in the list of every open tab, found by its title.
fn pick_from_all_tabs(app: &Fixture, cx: &mut TestAppContext, index: usize) {
    app.click(cx, "tabs-overflow");
    assert!(app.has(cx, "all-tabs-list"), "the list opens");
    app.type_text(cx, &format!("Query {}", index + 1));
    app.click(cx, ("all-tabs-item", index));
    assert!(!app.has(cx, "all-tabs-list"), "the list closes");
}

/// The button at the end of the strip lists every open tab, the active one
/// checked; past 15 a filter narrows them; a click shows the tab and
/// scrolls the strip to it. The palette opens it too (DBeaver #7114).
#[gpui_kit::test]
fn the_list_of_every_open_tab_filters_and_activates_one_of_thirty(cx: &mut TestAppContext) {
    let app = open(cx, many_tabs(30));
    assert_eq!(app.titles(cx).len(), 30);
    app.click(cx, "tabs-overflow");
    for index in 0..30usize {
        assert!(app.has(cx, ("all-tabs-item", index)), "tab {index} listed");
    }
    assert!(app.has(cx, "all-tabs-filter"), "a filter past 15 tabs");
    // The filter has the keyboard.
    app.type_text(cx, "query 7");
    assert!(app.has(cx, ("all-tabs-item", 6usize)));
    assert!(!app.has(cx, ("all-tabs-item", 26usize)));
    assert!(!app.has(cx, ("all-tabs-item", 0usize)));
    app.click(cx, ("all-tabs-item", 6usize));
    assert_eq!(app.active(cx), Some(6));
    assert!(!app.has(cx, "all-tabs-list"));
    app.step(cx, |window, _| window.refresh());
    let (inside, overflows) = cx.update(|cx| app.workspace.read(cx).active_tab_in_view());
    assert!(overflows, "30 tabs do not fit on one row");
    assert!(inside, "the strip scrolled to the tab");

    // From the palette (View ▸ Show All Open Tabs…), with an empty filter.
    app.dispatch(cx, actions::ShowAllTabs);
    assert!(app.has(cx, "all-tabs-list"));
    app.type_text(cx, "query 30");
    assert!(app.has(cx, ("all-tabs-item", 29usize)));
    app.click(cx, ("all-tabs-item", 29usize));
    assert_eq!(app.active(cx), Some(29));

    // A handful of tabs: no filter.
    let few = open(cx, many_tabs(4));
    few.click(cx, "tabs-overflow");
    assert!(few.has(cx, ("all-tabs-item", 3usize)));
    assert!(!few.has(cx, "all-tabs-filter"));
}

/// One row, several rows, a list at the side: each draws every tab with its
/// logo, its environment's tint and its menu, the empty area answers a
/// right-click, the active tab is scrolled into view, and the layout is
/// kept after a restart (DBeaver #7114, #23248).
#[gpui_kit::test]
async fn every_tab_layout_draws_every_tab_and_comes_back_after_a_restart(cx: &mut TestAppContext) {
    let storage = many_tabs(24);
    let app = open(cx, storage.clone());
    assert_eq!(layout(cx), TabLayout::SingleRow);
    for (action, expected, container) in [
        (
            Box::new(actions::TabsOnSeveralRows) as Box<dyn gpui_kit::Action>,
            TabLayout::Rows,
            Some("tab-rows"),
        ),
        (
            Box::new(actions::TabsAtTheSide),
            TabLayout::Vertical,
            Some("tab-list"),
        ),
        (Box::new(actions::TabsOnOneRow), TabLayout::SingleRow, None),
    ] {
        app.step(cx, |window, cx| window.dispatch_action(action, cx));
        assert_eq!(layout(cx), expected);
        for drawn in ["tab-rows", "tab-list"] {
            assert_eq!(app.has(cx, drawn), container == Some(drawn), "{expected:?}");
        }
        assert_eq!(
            app.has(cx, ("tab-row", 0usize)),
            expected == TabLayout::Vertical
        );
        // The last tab, picked from the list, is scrolled into view.
        pick_from_all_tabs(&app, cx, 23);
        app.step(cx, |window, _| window.refresh());
        let (inside, _) = cx.update(|cx| app.workspace.read(cx).active_tab_in_view());
        assert!(inside, "{expected:?}: the active tab is in view");
        for index in 0..24usize {
            assert!(
                app.has(cx, ("tab-title", index)),
                "{expected:?} tab {index}"
            );
            assert!(
                app.has(cx, ("close-tab", index)),
                "{expected:?} tab {index}"
            );
            assert_eq!(
                app.has(cx, ("tab-env", index)),
                index % 3 == 1,
                "{expected:?} tab {index}: the staging tint"
            );
            assert_eq!(app.has(cx, ("tab-logo", index)), index % 3 == 1);
        }
        // A click shows a tab; the chrome takes its environment's colour.
        pick_from_all_tabs(&app, cx, 0);
        app.click(cx, ("tab-title", 1usize));
        assert_eq!(app.active(cx), Some(1), "{expected:?}");
        assert!(
            cx.update(|cx| app.workspace.read(cx).active_chrome(cx))
                .is_some()
        );
        app.click(cx, ("tab-title", 0usize));
        assert_eq!(app.active(cx), Some(0), "{expected:?}");
        assert!(
            cx.update(|cx| app.workspace.read(cx).active_chrome(cx))
                .is_none()
        );
        // The same menus.
        app.right_click(cx, ("tab-title", 2usize), None);
        assert!(
            menu_entry(&app, cx, "Close others").is_some(),
            "{expected:?}"
        );
        app.press(cx, &["escape"]);
        // On one row the empty area is past the last tab, scrolled away
        // here (`tabs_answer_a_right_click_…` right-clicks it there).
        if expected != TabLayout::SingleRow {
            app.right_click(cx, "tab-bar-empty", None);
            assert!(
                menu_entry(&app, cx, "New query tab").is_some(),
                "{expected:?}"
            );
            app.press(cx, &["escape"]);
        }
    }

    // Settings ▸ Appearance picks it too: the list at the side.
    app.press(cx, &["secondary-,"]);
    app.step(cx, |window, cx| {
        window.within("settings-tab-layout").click(2usize, cx)
    });
    assert_eq!(layout(cx), TabLayout::Vertical);
    let stored = storage.clone();
    cx.wait_for(app.window.into(), Duration::from_secs(10), move |_, _| {
        Settings::load(&stored).tab_layout == TabLayout::Vertical
    })
    .await;
    let restarted = open(cx, storage);
    assert!(restarted.has(cx, "tab-list"));
    for index in 0..24usize {
        assert!(restarted.has(cx, ("tab-row", index)));
    }
}

/// A scratch database (a schema on PostgreSQL) with a two-row table.
fn stack_scratch(engine: Engine) -> Scratch {
    let (drop, create) = scratch_schema(engine, "kerno_ui_stacked");
    Scratch::new(
        engine,
        &[&drop],
        &[
            &create,
            "CREATE TABLE kerno_ui_stacked.t (n INT)",
            "INSERT INTO kerno_ui_stacked.t VALUES (1), (2)",
        ],
    )
}

const STACK_SCRIPT: &str = "select 1 as one;\nselect 2 as two;\n\
    update kerno_ui_stacked.t set n = n + 1 where n > 0;\nselect * from kerno_no_such_table";

/// With "Stack results", a script's results are one under the other in one
/// panel: two grids, the update count, the error in its place; a click
/// gives a grid the focus; "Open in its own tab" and the results menu go
/// back to result tabs (DBeaver #6861).
#[gpui_kit::test]
fn the_results_of_a_script_stack_in_one_panel_on_every_engine(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            continue;
        }
        let _scratch = stack_scratch(engine);
        let app = connected(cx, engine);
        cx.update(|cx| AppState::update_settings(cx, |settings| settings.stack_results = true));
        app.run_in_new_tab(cx, STACK_SCRIPT, "secondary-shift-enter");
        app.wait_until(cx, "the script", |app, cx| {
            app.outcomes(cx).len() == 4 && !app.running(cx)
        });
        let outcomes = app.outcomes(cx);
        assert_eq!(
            outcomes[..3],
            ["1 rows", "1 rows", "done, 2 affected"],
            "{engine:?}"
        );
        assert!(outcomes[3].starts_with("failed"), "{engine:?} {outcomes:?}");
        assert!(app.has(cx, "stacked-results"), "{engine:?}");
        assert!(!app.has(cx, ("result-tab", 0usize)), "no result tabs");
        for index in 0..4usize {
            assert!(app.has(cx, ("stacked-header", index)), "{engine:?} {index}");
        }
        assert!(app.has(cx, ("stacked-grid", 0usize)));
        assert!(app.has(cx, ("stacked-grid", 1usize)));
        assert!(app.has(cx, ("stacked-status", 2usize)));
        assert!(app.has(cx, ("stacked-status", 3usize)));
        assert!(app.has(cx, ("stacked-export", 0usize)));
        // The error has the focus, as in result tabs, but does not take
        // the panel: the grids stay in view above it.
        let tab = app.tab(cx);
        assert_eq!(
            cx.update(|cx| tab.read(cx).active_result()),
            3,
            "{engine:?}"
        );

        // A click in the second grid gives it the focus: Cmd+C copies it.
        app.click(cx, ("stacked-grid", 1usize));
        assert_eq!(cx.update(|cx| tab.read(cx).active_result()), 1);
        app.click(cx, ("stacked-copy", 1usize));
        assert_eq!(
            app.clipboard(cx).as_deref().map(str::trim_end),
            Some("two\n2"),
            "{engine:?}"
        );

        // "Open in its own tab": result tabs, this one on screen.
        app.click(cx, ("stacked-own-tab", 2usize));
        assert!(app.has(cx, ("result-tab", 3usize)));
        assert!(!app.has(cx, "stacked-results"));
        assert_eq!(cx.update(|cx| tab.read(cx).active_result()), 2);
        assert!(cx.update(|cx| AppState::get(cx).settings.stack_results));
    }
}

/// The results menu stacks the results and puts them back in tabs; so
/// does the palette (DBeaver #6861).
#[gpui_kit::test]
fn the_results_menu_stacks_and_unstacks(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select 1 as one; select 2 as two",
        "secondary-shift-enter",
    );
    app.wait_until(cx, "the script", |app, cx| {
        app.outcomes(cx).len() == 2 && !app.running(cx)
    });
    assert!(app.has(cx, ("result-tab", 1usize)));
    app.right_click(cx, ("result-tab", 0usize), None);
    click_menu_entry(&app, cx, "Stack results in one panel");
    assert!(app.has(cx, ("stacked-header", 1usize)));
    assert!(!app.has(cx, ("result-tab", 0usize)));
    // A grid's lower edge drags it taller.
    let bounds = |cx: &mut TestAppContext, id: (&'static str, usize)| {
        app.step(cx, |window, _| {
            window.try_find(id).map(|found| found.bounds())
        })
        .unwrap_or_else(|| panic!("{id:?} is drawn"))
    };
    let before = bounds(cx, ("stacked-grid", 0)).size.height;
    let edge = bounds(cx, ("stacked-resize", 0)).center();
    app.step(cx, |window, cx| {
        window.drag(edge, gpui_kit::point(edge.x, edge.y + px(80.)), cx)
    });
    let after = bounds(cx, ("stacked-grid", 0)).size.height;
    assert!(after > before + px(60.), "{before:?} → {after:?}");
    // The headers carry the same menu.
    app.right_click(cx, ("stacked-header", 0usize), None);
    click_menu_entry(&app, cx, "Stack results in one panel");
    assert!(app.has(cx, ("result-tab", 1usize)));
    assert!(!app.has(cx, "stacked-results"));
    assert!(!cx.update(|cx| AppState::get(cx).settings.stack_results));
    app.dispatch(cx, actions::ToggleStackedResults);
    assert!(app.has(cx, "stacked-results"));
    app.dispatch(cx, actions::ToggleStackedResults);
    assert!(!app.has(cx, "stacked-results"));
}

/// Fifty stacked results: only those in view are laid out, each grid
/// virtual inside its bounded height, so the panel scrolls as smoothly as
/// one result (CLAUDE.md §5).
#[gpui_kit::test]
fn fifty_stacked_results_lay_out_only_those_in_view(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.stack_results = true));
    let script: Vec<String> = (1..=50)
        .map(|n| format!("select g as n{n} from generate_series(1, 100) g"))
        .collect();
    app.run_in_new_tab(cx, &script.join(";\n"), "secondary-shift-enter");
    app.wait_until(cx, "the script", |app, cx| {
        app.outcomes(cx).len() == 50 && !app.running(cx)
    });
    assert!(app.has(cx, ("stacked-result", 0usize)));
    assert!(
        !app.has(cx, ("stacked-result", 49usize)),
        "laid out out of view"
    );
    // Each grid is bounded: twelve of its hundred rows at most.
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    let height = app.step(cx, |window, _| {
        window
            .try_find(("stacked-grid", 0usize))
            .map(|found| found.bounds().size.height)
    });
    let row = cx.update(|cx| crate::query::grid::table_size(cx).table_row_height());
    let height = height.expect("the first grid is drawn");
    assert!(
        height <= row * 14. + px(20.),
        "{height:?} for rows of {row:?}"
    );
    assert_eq!(cx.update(|cx| grid.read(cx).delegate().row_count()), 100);
}

/// A header sort on a result not wholly fetched reads it again on the
/// server, into a new grid: its find bar stays open with its text (the
/// follow-up of Part 4, DBeaver #38133).
#[gpui_kit::test]
fn a_server_sort_keeps_the_find_bar_of_a_result(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g as n, 'item ' || g as label from generate_series(1, 3000) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx) == ["1000 rows, more"]
    });
    let tab = app.tab(cx);
    let old = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    app.step(cx, |window, cx| find::open(&old, window, cx));
    app.type_text(cx, "item 2");
    app.step(cx, |window, cx| {
        old.update(cx, |table, cx| {
            table
                .delegate_mut()
                .perform_sort(1, ColumnSort::Descending, window, cx)
        })
    });
    app.wait_until(cx, "the result read again", |app, cx| {
        let new = cx.update(|cx| tab.read(cx).grid_of(0));
        new.is_some_and(|new| new != old) && app.outcomes(cx) == ["1000 rows, more"]
    });
    let new = cx.update(|cx| tab.read(cx).grid_of(0)).unwrap();
    assert_eq!(
        cx.update(|cx| find::carried(&new, cx)),
        Some(find::Carried {
            text: "item 2".into(),
            case: false,
            only: false,
        })
    );
    assert!(app.has(cx, "grid-find"));
    // Sorted by the server: the largest first.
    assert_eq!(
        cx.update(|cx| new.read(cx).delegate().shown())[0][0],
        "3000"
    );
}

/// Among stacked results, the menu of a cell of a result that has no focus
/// edits that result, which takes the focus, never the other one (DBeaver
/// #6861).
#[gpui_kit::test]
fn a_cell_menu_edits_its_own_stacked_result(cx: &mut TestAppContext) {
    use crate::query::grid::GridAction;

    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_stacked_edit CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_stacked_edit",
            "CREATE TABLE kerno_ui_stacked_edit.t (id integer PRIMARY KEY, n integer)",
            "INSERT INTO kerno_ui_stacked_edit.t VALUES (1, 10), (2, 20)",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.stack_results = true));
    app.run_in_new_tab(
        cx,
        "select id, n from kerno_ui_stacked_edit.t order by id;\n\
         select id, n from kerno_ui_stacked_edit.t where id = 2",
        "secondary-shift-enter",
    );
    let tab = app.tab(cx);
    app.wait_until(cx, "two editable results", |app, cx| {
        app.outcomes(cx) == ["2 rows", "1 rows"]
            && cx.update(|cx| tab.read(cx).edit_decided(0) && tab.read(cx).edit_decided(1))
    });
    assert_eq!(cx.update(|cx| tab.read(cx).active_result()), 0);
    let (first, second) = cx.update(|cx| (tab.read(cx).grid_of(0), tab.read(cx).grid_of(1)));
    let (first, second) = (first.unwrap(), second.unwrap());
    // Set NULL from the menu of the second result's cell `n`.
    app.step(cx, |window, cx| {
        second.update(cx, |table, cx| table.set_selected_cell(0, 2, cx));
        Grid::perform(&second, GridAction::SetNull, window, cx)
    });
    assert_eq!(cx.update(|cx| tab.read(cx).active_result()), 1);
    let pending = |cx: &mut TestAppContext, grid: &Entity<TableState<Grid>>| {
        cx.update(|cx| grid.read(cx).delegate().pending().map_or(0, |p| p.count()))
    };
    assert_eq!(pending(cx, &second), 1);
    assert_eq!(pending(cx, &first), 0);
    tab.update(cx, |tab, cx| tab.discard(cx));
    assert_eq!(pending(cx, &second), 0);
}
