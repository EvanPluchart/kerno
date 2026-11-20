//! K19 (docs/plans/M12.md): the tab area split in groups, by a command, a tab's menu, a key or
//! a tab dragged to an edge; the focused group is the one the commands, the toolbar and the
//! status bar act on; the layout is stored with the tabs.

use gpui_kit::test::TestEventExt as _;
use gpui_kit::{Bounds, MouseButton, Pixels, Point, point};
use kerno_core::environment;

use super::*;
use crate::workspace::panes::GroupId;

/// The groups as `Workspace::layout_text` writes them.
fn layout(app: &Fixture, cx: &mut TestAppContext) -> String {
    cx.update(|cx| app.workspace.read(cx).layout_text(cx))
}

fn groups(app: &Fixture, cx: &mut TestAppContext) -> Vec<GroupId> {
    cx.update(|cx| app.workspace.read(cx).group_ids())
}

fn bounds(
    app: &Fixture,
    cx: &mut TestAppContext,
    id: impl Into<gpui_kit::ElementId>,
) -> Bounds<Pixels> {
    let id = id.into();
    app.step(cx, |window, _| window.find(id).bounds())
}

/// The app with "Query 1" to "Query `count`" open in one group, the last shown.
fn with_tabs(cx: &mut TestAppContext, count: usize) -> Fixture {
    let app = open(cx, memory_storage());
    for _ in 0..count {
        app.press(cx, &["secondary-t"]);
    }
    app
}

/// A point of `area` at these fractions of its width and height.
fn at(area: Bounds<Pixels>, x: f32, y: f32) -> Point<Pixels> {
    point(
        area.left() + area.size.width * x,
        area.top() + area.size.height * y,
    )
}

/// Drags the title of the tab at `index` to `to`, by native pointer events.
fn drag_tab(app: &Fixture, cx: &mut TestAppContext, index: usize, to: Point<Pixels>) {
    let from = bounds(app, cx, ("tab-title", index)).center();
    app.step(cx, |window, cx| window.drag(from, to, cx));
}

/// K19: Split Right (the command), Split down (a tab's menu) and the keys of the next group;
/// a tab alone in its group is not split, and says why.
#[gpui_kit::test]
fn split_right_and_down_by_command_menu_and_keys(cx: &mut TestAppContext) {
    let app = with_tabs(cx, 3);
    assert_eq!(layout(&app, cx), ">[Query 1, Query 2, Query 3*]");
    app.dispatch(cx, actions::SplitRight);
    assert_eq!(layout(&app, cx), "([Query 1, Query 2*] | >[Query 3*])");
    assert!(
        app.editor_has_keyboard(cx),
        "the tab split off has the keyboard"
    );

    // The menu of Query 1, in the left group: Split down.
    app.right_click(cx, ("tab-title", 0usize), None);
    app.click_label(cx, "Split down");
    assert_eq!(
        layout(&app, cx),
        "(([Query 2*] / >[Query 1*]) | [Query 3*])"
    );

    // Alone in its group, Query 1 has nothing to split from: nothing moves, a notification
    // says why.
    let before = notification_count(&app, cx);
    app.dispatch(cx, actions::SplitRight);
    assert_eq!(
        layout(&app, cx),
        "(([Query 2*] / >[Query 1*]) | [Query 3*])"
    );
    assert_eq!(notification_count(&app, cx), before + 1);

    // ⌘⌥⇧M: to the next group; its own group, left empty, closes.
    app.press(cx, &["secondary-alt-shift-m"]);
    assert_eq!(layout(&app, cx), "([Query 2*] | >[Query 3, Query 1*])");
    // ⌘⌥⇧N: the next group, around; its tab has the keyboard.
    app.press(cx, &["secondary-alt-shift-n"]);
    assert_eq!(layout(&app, cx), "(>[Query 2*] | [Query 3, Query 1*])");
    assert!(app.editor_has_keyboard(cx));
    assert_eq!(app.active_query(cx).0, "Query 2");
    // ⌘⌥⇧I splits down, ⌘⌥⇧P goes back.
    app.press(cx, &["secondary-t", "secondary-alt-shift-i"]);
    assert_eq!(
        layout(&app, cx),
        "(([Query 2*] / >[Query 4*]) | [Query 3, Query 1*])"
    );
    app.press(cx, &["secondary-alt-shift-p"]);
    assert_eq!(
        layout(&app, cx),
        "((>[Query 2*] / [Query 4*]) | [Query 3, Query 1*])"
    );
    // With four groups, a fifth is refused.
    app.press(cx, &["secondary-t", "secondary-alt-shift-l"]);
    assert_eq!(groups(&app, cx).len(), 4);
    app.press(cx, &["secondary-t"]);
    let before = layout(&app, cx);
    app.press(cx, &["secondary-alt-shift-l"]);
    assert_eq!(layout(&app, cx), before);
}

/// K19: a tab dragged onto the left, right, top or bottom edge of a group's content splits
/// there; while it is dragged, the half it will take is shown with what a drop does.
#[gpui_kit::test]
fn a_tab_dropped_on_each_edge_of_a_group_splits_there(cx: &mut TestAppContext) {
    for (x, y, indicator, expected) in [
        (0.1, 0.5, "Split to the left", "(>[Query 1*] | [Query 2*])"),
        (0.9, 0.5, "Split to the right", "([Query 2*] | >[Query 1*])"),
        (0.5, 0.1, "Split up", "(>[Query 1*] / [Query 2*])"),
        (0.5, 0.9, "Split down", "([Query 2*] / >[Query 1*])"),
    ] {
        let app = with_tabs(cx, 2);
        let group = groups(&app, cx)[0];
        let content = bounds(&app, cx, ("tab-content", group));
        let from = bounds(&app, cx, ("tab-title", 0usize)).center();
        let to = at(content, x, y);
        // Pressed and moved, not released: the indicator.
        app.step(cx, |window, cx| {
            window.pointer_down(from, MouseButton::Left, cx);
            for step in 1..=8 {
                let f = step as f32 / 8.;
                let between = point(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f);
                window.pointer_move(between, Some(MouseButton::Left), cx);
            }
        });
        let shown = app.step(cx, |window, _| {
            gpui_kit::base::test_support::texts(window)
                .into_iter()
                .filter(|text| text.path().contains(&("tab-drop", group).into()))
                .map(|text| text.text().to_string())
                .collect::<Vec<_>>()
        });
        assert_eq!(shown, [indicator]);
        let half = bounds(&app, cx, ("tab-drop", group));
        assert!(
            half.contains(&to)
                && f32::from(half.size.width) * f32::from(half.size.height)
                    < f32::from(content.size.width) * f32::from(content.size.height) * 0.6,
            "{indicator}: {half:?} in {content:?}"
        );
        app.step(cx, |window, cx| {
            window.pointer_up(to, MouseButton::Left, cx)
        });
        assert_eq!(layout(&app, cx), expected, "{indicator}");
        assert!(
            !app.has(cx, ("tab-drop", group)),
            "the indicator goes with the drop"
        );
    }

    // Dragged to an edge of the group it is alone in, a tab changes nothing.
    let app = with_tabs(cx, 2);
    app.dispatch(cx, actions::SplitRight);
    let (left, right) = (groups(&app, cx)[0], groups(&app, cx)[1]);
    let edge = at(bounds(&app, cx, ("tab-content", right)), 0.9, 0.5);
    drag_tab(&app, cx, 1, edge);
    assert_eq!(layout(&app, cx), "([Query 1*] | >[Query 2*])");
    // Into the middle of the other group's content: it joins that group.
    let middle = at(bounds(&app, cx, ("tab-content", left)), 0.5, 0.5);
    drag_tab(&app, cx, 1, middle);
    assert_eq!(layout(&app, cx), ">[Query 1, Query 2*]");
}

/// K19: a tab dropped on another group's strip goes there, before the tab it is dropped on;
/// on its own strip it moves within it; after the tabs, it goes last.
#[gpui_kit::test]
fn a_tab_dropped_on_a_strip_moves_there_and_within_a_strip_reorders(cx: &mut TestAppContext) {
    let app = with_tabs(cx, 3);
    app.dispatch(cx, actions::SplitRight);
    assert_eq!(layout(&app, cx), "([Query 1, Query 2*] | >[Query 3*])");
    // Query 1 onto Query 3's title.
    app.step(cx, |window, cx| {
        window.drag_to(("tab-title", 0usize), ("tab-title", 2usize), cx)
    });
    assert_eq!(layout(&app, cx), "([Query 2*] | >[Query 1*, Query 3])");
    // Query 3 onto Query 1's: before it.
    app.step(cx, |window, cx| {
        window.drag_to(("tab-title", 2usize), ("tab-title", 1usize), cx)
    });
    assert_eq!(layout(&app, cx), "([Query 2*] | >[Query 3*, Query 1])");
    // Query 3 after the tabs of the left strip.
    let left = groups(&app, cx)[0];
    let empty = app.step(cx, |window, _| {
        window
            .within(("tab-group", left))
            .find("tab-bar-empty")
            .bounds()
            .center()
    });
    drag_tab(&app, cx, 1, empty);
    assert_eq!(layout(&app, cx), "(>[Query 2, Query 3*] | [Query 1*])");
}

/// K19: the last tab of a group closed, the group closes and its neighbour takes its place; the
/// last tab of the last group gives the empty state.
#[gpui_kit::test]
fn closing_the_last_tab_of_a_group_closes_it_then_the_empty_state(cx: &mut TestAppContext) {
    let app = with_tabs(cx, 3);
    app.dispatch(cx, actions::SplitRight);
    app.dispatch(cx, actions::SplitDown);
    assert_eq!(groups(&app, cx).len(), 2, "Query 3 alone: not split down");
    app.press(cx, &["secondary-w"]);
    assert_eq!(layout(&app, cx), ">[Query 1, Query 2*]");
    assert!(
        app.editor_has_keyboard(cx),
        "the neighbour's tab has the keyboard"
    );
    app.press(cx, &["secondary-w", "secondary-w"]);
    assert!(app.has(cx, "tabs-empty"));
    app.press(cx, &["secondary-t"]);
    assert_eq!(layout(&app, cx), ">[Query 4*]");
}

/// K19: Close others, to the right, to the left and all act within the tab's group; the other
/// groups keep their tabs.
#[gpui_kit::test]
fn closing_others_and_all_stays_within_the_group(cx: &mut TestAppContext) {
    let app = with_tabs(cx, 4);
    app.dispatch(cx, actions::SplitRight);
    app.press(cx, &["secondary-t", "secondary-t"]);
    assert_eq!(
        layout(&app, cx),
        "([Query 1, Query 2, Query 3*] | >[Query 4, Query 5, Query 6*])"
    );
    app.press(cx, &["secondary-alt-shift-n"]);
    app.dispatch(cx, actions::CloseTabsToTheLeft);
    assert_eq!(
        layout(&app, cx),
        "(>[Query 3*] | [Query 4, Query 5, Query 6*])"
    );
    app.press(cx, &["secondary-alt-shift-n"]);
    app.dispatch(cx, actions::CloseOtherTabs);
    assert_eq!(layout(&app, cx), "([Query 3*] | >[Query 6*])");
    app.press(cx, &["secondary-t", "secondary-t"]);
    app.dispatch(cx, actions::CloseAllTabs);
    assert_eq!(layout(&app, cx), ">[Query 3*]");
}

/// K19: a click anywhere in a group (here the empty end of its strip, which takes no keyboard)
/// makes it the focused group, so the next key acts there.
#[gpui_kit::test]
fn a_click_in_a_group_makes_it_the_one_keys_act_on(cx: &mut TestAppContext) {
    let app = with_tabs(cx, 2);
    app.dispatch(cx, actions::SplitRight);
    let left = groups(&app, cx)[0];
    app.step(cx, |window, cx| {
        window
            .within(("tab-group", left))
            .click("tab-bar-empty", cx)
    });
    assert_eq!(layout(&app, cx), "(>[Query 1*] | [Query 2*])");
    app.press(cx, &["secondary-w"]);
    assert_eq!(layout(&app, cx), ">[Query 2*]");
}

/// K19: a click on a part of a group that takes no keyboard and keeps the click to itself (a
/// greyed button, a scrollbar) still makes that group the one the keys act on: with the keyboard
/// left in the other group's editor, ⌘. would stop nothing there while this group's statement
/// runs on.
#[gpui_kit::test]
fn a_click_that_takes_no_keyboard_still_moves_the_keys_to_its_group(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 1 as kerno_ui_k19_left");
    app.press(cx, &["secondary-t"]);
    let bottom_tab = app.tab(cx);
    app.type_text(cx, "select pg_sleep(60) as kerno_ui_k19_sleep");
    app.dispatch(cx, actions::SplitDown);
    app.press(cx, &["secondary-enter"]);
    let running = |cx: &mut TestAppContext| {
        cx.update(|cx| matches!(bottom_tab.read(cx).activity(), Activity::Running { .. }))
    };
    app.wait_until(cx, "the sleep", |_, cx| running(cx));
    // The keyboard into the top group's editor.
    let (top, bottom) = (groups(&app, cx)[0], groups(&app, cx)[1]);
    let content = bounds(&app, cx, ("tab-content", top));
    app.step(cx, |window, cx| {
        window.click_at(
            ("tab-content", top),
            point(content.size.width / 2., px(60.)),
            cx,
        )
    });
    assert!(app.editor_has_keyboard(cx));
    // The bottom group's Run button, greyed while its statement runs.
    let run = app.step(cx, |window, _| {
        let run = window.within(("tab-group", bottom)).find("tab-run");
        assert_eq!(run.disabled(), Some(true));
        run.bounds().center()
    });
    app.step(cx, |window, cx| {
        window.pointer_move(run, None, cx);
        window.pointer_down(run, MouseButton::Left, cx);
        window.pointer_up(run, MouseButton::Left, cx);
    });
    assert_eq!(layout(&app, cx), "([Query 1*] / >[Query 2*])");
    app.press(cx, &["secondary-."]);
    app.wait_until(cx, "the sleep stopped", |_, cx| !running(cx));
}

/// K19, CLAUDE.md §3 item 7: a production tab keeps its tint (its title and its frame) in its
/// group while a local tab of another group is focused; the toolbar and the status bar follow
/// the focused group.
#[gpui_kit::test]
fn a_production_tab_keeps_its_tint_while_a_local_tab_is_focused(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let save = |name: &str, environment: &str| {
        storage
            .save_connection(
                None,
                &ConnectionConfig {
                    name: name.into(),
                    environment: environment.into(),
                    ..testing::config(Engine::Postgres)
                },
            )
            .unwrap()
    };
    let production = save("kerno_ui_k19 production", environment::PRODUCTION);
    let local = save("kerno_ui_k19 local", environment::LOCAL);
    let tab = |title: &str, connection: &str| TabSnapshot::Query {
        title: title.into(),
        sql: String::new(),
        target: Some(Target {
            connection: connection.into(),
            database: None,
            schema: None,
        }),
        file: None,
        unsaved: false,
        source: None,
    };
    storage
        .save_tabs(&[tab("Prod", &production), tab("Local", &local)])
        .unwrap();
    storage
        .set_setting(crate::state::ACTIVE_TAB_KEY, &1usize)
        .unwrap();
    let app = open(cx, storage);
    app.dispatch(cx, actions::SplitRight);
    assert_eq!(layout(&app, cx), "([Prod*] | >[Local*])");
    let chrome = |cx: &mut TestAppContext| cx.update(|cx| app.workspace.read(cx).active_chrome(cx));
    assert!(
        chrome(cx).is_none(),
        "the local tab is focused: no tint on the chrome"
    );
    let left = groups(&app, cx)[0];
    let in_left = |cx: &mut TestAppContext, id: &'static str| {
        app.step(cx, |window, _| {
            window.within(("tab-group", left)).try_find(id).is_some()
        })
    };
    assert!(
        app.has(cx, ("tab-env", 0usize)),
        "the production tab's title keeps its tint"
    );
    assert!(!app.has(cx, ("tab-env", 1usize)));
    assert!(in_left(cx, "env-frame"), "and its frame");
    let picker = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            app.workspace
                .read(cx)
                .picker_text(crate::workspace::toolbar::Item::Connection, cx)
        })
    };
    assert_eq!(picker(cx), "kerno_ui_k19 local");
    assert!(
        app.status_says(cx, "kerno_ui_k19 local"),
        "{:?}",
        app.status_bar(cx)
    );
    app.press(cx, &["secondary-alt-shift-p"]);
    assert!(
        chrome(cx).is_some(),
        "the production tab focused: the chrome takes its colour"
    );
    assert_eq!(picker(cx), "kerno_ui_k19 production");
    assert!(
        app.status_says(cx, "kerno_ui_k19 production"),
        "{:?}",
        app.status_bar(cx)
    );
}

/// K19: the groups, which tab is where, each group's shown tab, the focused group and the
/// sizes a divider was dragged to are stored with the tabs and come back after a restart.
#[gpui_kit::test]
async fn the_layout_and_its_sizes_survive_a_restart(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    app.press(
        cx,
        &["secondary-t", "secondary-t", "secondary-t", "secondary-t"],
    );
    app.dispatch(cx, actions::SplitRight);
    app.dispatch(cx, actions::FocusPreviousGroup);
    app.dispatch(cx, actions::SplitDown);
    let before = layout(&app, cx);
    assert_eq!(before, "(([Query 1, Query 2*] / >[Query 3*]) | [Query 4*])");
    // The divider between the columns, dragged to about a third.
    let area = bounds(&app, cx, ("tab-split", 1usize));
    let divider = bounds(&app, cx, ("tab-divider", 9usize)).center();
    let to = point(area.left() + area.size.width * 0.3, divider.y);
    app.step(cx, |window, cx| window.drag(divider, to, cx));
    let shares = cx.update(|cx| app.workspace.read(cx).root_shares());
    assert!((shares[0] - 0.3).abs() < 0.03, "{shares:?}");
    let stored = storage.clone();
    let wanted = shares.clone();
    cx.wait_for(app.window.into(), Duration::from_secs(10), move |_, _| {
        stored
            .setting::<crate::workspace::panes::SavedPane>(crate::state::TAB_LAYOUT_KEY)
            .unwrap()
            .is_some_and(|saved| match saved {
                crate::workspace::panes::SavedPane::Split { shares, .. } => shares == wanted,
                _ => false,
            })
    })
    .await;

    let restarted = open(cx, storage.clone());
    assert_eq!(layout(&restarted, cx), before);
    let again = cx.update(|cx| restarted.workspace.read(cx).root_shares());
    assert_eq!(again, shares);
    assert_eq!(
        restarted.active_query(cx).0,
        "Query 3",
        "the focused group's tab is shown"
    );
}

/// K19: tabs stored before K19 (no layout) open in one group; a stored layout that cannot be
/// read, or that names tabs no longer there, opens what it can, never an error.
#[gpui_kit::test]
fn an_old_or_broken_stored_layout_restores_as_one_group(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let query = |title: &str| TabSnapshot::Query {
        title: title.into(),
        sql: String::new(),
        target: None,
        file: None,
        unsaved: false,
        source: None,
    };
    storage
        .save_tabs(&[query("Query 1"), query("Query 2"), query("Query 3")])
        .unwrap();
    storage
        .set_setting(crate::state::ACTIVE_TAB_KEY, &1usize)
        .unwrap();
    let app = open(cx, storage.clone());
    assert_eq!(layout(&app, cx), ">[Query 1, Query 2*, Query 3]");
    // Not a layout at all.
    storage
        .set_setting(crate::state::TAB_LAYOUT_KEY, &"not a layout")
        .unwrap();
    let app = open(cx, storage.clone());
    assert_eq!(layout(&app, cx), ">[Query 1, Query 2*, Query 3]");
    // Two groups naming a tab twice and a tab that is gone; Query 3 unplaced.
    storage
        .set_setting(
            crate::state::TAB_LAYOUT_KEY,
            &serde_json::json!({"split": {"axis": "row", "shares": [0.5, 0.5], "children": [
                {"tabs": {"tabs": [0, 9], "active": 9}},
                {"tabs": {"tabs": [1, 0], "active": 1}},
            ]}}),
        )
        .unwrap();
    let app = open(cx, storage);
    assert_eq!(layout(&app, cx), "([Query 1*, Query 3] | >[Query 2*])");
}

/// K19, C15: the quit question lists the transactions of every group, the focused one's and
/// the others'.
#[gpui_kit::test]
fn quitting_lists_the_transactions_of_both_groups(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = k19_table("kerno_ui_k19_quit");
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    app.open_transaction(cx, "insert into kerno_ui_k19_quit.t values (1)");
    app.press(cx, &["secondary-t"]);
    app.open_transaction(cx, "insert into kerno_ui_k19_quit.t values (2)");
    app.dispatch(cx, actions::SplitRight);
    assert_eq!(layout(&app, cx), "([Query 1*] | >[Query 2*])");
    app.dispatch(cx, actions::Quit);
    assert!(app.has(cx, "end-question"));
    for line in [
        "“Query 1”: a transaction is open.",
        "“Query 2”: a transaction is open.",
    ] {
        assert!(app.shows_label(cx, line), "{line}");
    }
    app.click(cx, "end-cancel");
    for _ in 0..2 {
        app.dispatch(cx, actions::RollbackTransaction);
        app.wait_until(cx, "the rollback", |app, cx| {
            app.tx_state(cx) == kerno_core::TxState::Idle
        });
        app.press(cx, &["secondary-alt-shift-n"]);
    }
}

/// K19: Commit, ⌘Enter and the keys act on the focused group's tab only: a click in the left
/// group's editor makes it the one Commit ends; the other group's transaction stays open.
#[gpui_kit::test]
fn commit_and_run_act_on_the_focused_group_only(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = k19_table("kerno_ui_k19_commit");
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let left = app.tab(cx);
    app.open_transaction(cx, "insert into kerno_ui_k19_commit.t values (1)");
    app.press(cx, &["secondary-t"]);
    let right = app.tab(cx);
    app.open_transaction(cx, "insert into kerno_ui_k19_commit.t values (2)");
    app.dispatch(cx, actions::SplitRight);
    let state = |tab: &Entity<crate::query::tab::QueryTab>, cx: &mut TestAppContext| {
        cx.update(|cx| tab.read(cx).tx_state())
    };
    // A click in the left group's editor, then ⌘Enter: the left text runs again. (Below the
    // toolbar, which takes two lines in a group this narrow.)
    let group = groups(&app, cx)[0];
    let content = bounds(&app, cx, ("tab-content", group));
    app.step(cx, |window, cx| {
        window.click_at(
            ("tab-content", group),
            point(content.size.width / 2., px(120.)),
            cx,
        )
    });
    assert!(app.editor_has_keyboard(cx));
    assert_eq!(app.tab(cx), left);
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "select 'left' as kerno_ui_k19_side");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the left run", |app, cx| {
        !app.running(cx) && !app.outcomes(cx).is_empty()
    });
    let (columns, _) = app.result(cx, 0, 1);
    assert_eq!(columns, ["kerno_ui_k19_side"]);
    assert!(
        cx.update(|cx| right.read(cx).sql(cx))
            .contains("values (2)"),
        "the right tab's text is untouched"
    );
    app.dispatch(cx, actions::CommitTransaction);
    app.wait_until(cx, "the commit", |_, cx| {
        state(&left, cx) == kerno_core::TxState::Idle
    });
    assert_eq!(state(&right, cx), kerno_core::TxState::Open);
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT string_agg(id::text, ',') FROM kerno_ui_k19_commit.t"
        ),
        "1"
    );
    // The right group focused, Roll back ends its own.
    app.press(cx, &["secondary-alt-shift-n"]);
    app.dispatch(cx, actions::RollbackTransaction);
    app.wait_until(cx, "the rollback", |_, cx| {
        state(&right, cx) == kerno_core::TxState::Idle
    });
}

/// A scratch schema `name` with a table `t`, for one transaction test.
fn k19_table(name: &str) -> Scratch {
    Scratch::new(
        Engine::Postgres,
        &[&format!("DROP SCHEMA IF EXISTS {name} CASCADE")],
        &[
            &format!("CREATE SCHEMA {name}"),
            &format!("CREATE TABLE {name}.t (id INT PRIMARY KEY)"),
        ],
    )
}

/// The real-app pass of 2026-11-20: beside a table in a narrow group, the value panel kept its
/// remembered width and was drawn past the group's edge ("Set" half visible, "Open the row" out
/// of reach). It takes the width the grid leaves, and its own width again once there is room,
/// without forgetting the width the user gave it. Fails without `ValuePanel::fit`.
#[gpui_kit::test]
fn the_value_panel_stays_inside_a_narrow_group(cx: &mut TestAppContext) {
    use gpui_kit::size;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.simulate_window_resize(app.window.into(), size(px(1400.), px(800.)));
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_until(cx, "the first page", |app, cx| {
        app.data_state(cx).starts_with("Rows 1")
    });
    app.press(cx, &["f7"]);
    app.wait_until(cx, "the value panel", |app, cx| app.has(cx, "value-panel"));
    let wide = bounds(&app, cx, "value-panel").size.width;
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    let left = groups(&app, cx)[0];
    app.wait_until(cx, "the panel inside its group", |app, cx| {
        let (group, panel) = (
            bounds(app, cx, ("tab-group", left)),
            bounds(app, cx, "value-panel"),
        );
        panel.right() <= group.right() + px(1.) && panel.size.width >= px(240.)
    });
    assert!(bounds(&app, cx, "value-panel").size.width < wide);
    // The width the user chose is not replaced by the narrow one.
    let remembered = cx.update(|cx| AppState::get(cx).settings.value_panel_width);
    assert_eq!(px(remembered), wide);
}

/// Real-app pass: a group narrowed by its divider squeezed the value panel and the squeezed
/// width replaced the user's (450 became 300, then 241), so the panel never grew back. Only
/// a drag of the panel's own divider changes the width kept; the group widened again, the
/// panel is back at it.
#[gpui_kit::test]
fn a_squeezed_value_panel_grows_back_and_keeps_the_users_width(cx: &mut TestAppContext) {
    use gpui_kit::size;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.simulate_window_resize(app.window.into(), size(px(1400.), px(800.)));
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.value_panel_width = 450.));
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_until(cx, "the first page", |app, cx| {
        app.data_state(cx).starts_with("Rows 1")
    });
    app.press(cx, &["f7"]);
    app.wait_until(cx, "the value panel", |app, cx| app.has(cx, "value-panel"));
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    let remembered =
        |cx: &mut TestAppContext| cx.update(|cx| AppState::get(cx).settings.value_panel_width);
    let drag_divider = |cx: &mut TestAppContext, share: f32| {
        let area = bounds(&app, cx, ("tab-split", 1usize));
        let divider = bounds(&app, cx, ("tab-divider", 9usize)).center();
        let to = point(area.left() + area.size.width * share, divider.y);
        app.step(cx, |window, cx| window.drag(divider, to, cx));
        for _ in 0..5 {
            app.step(cx, |window, _| window.refresh());
        }
    };
    // Narrowed in steps, as a hand drags.
    for share in [0.65, 0.6, 0.55, 0.5] {
        drag_divider(cx, share);
    }
    let width = bounds(&app, cx, "value-panel").size.width;
    assert!(width < px(450.), "squeezed: {width:?}");
    assert_eq!(remembered(cx), 450., "a squeeze is not the user's width");
    drag_divider(cx, 0.8);
    app.wait_until(cx, "the panel back at its width", |app, cx| {
        (bounds(app, cx, "value-panel").size.width - px(450.)).abs() < px(2.)
    });
    assert_eq!(remembered(cx), 450.);
}

/// Real-app pass: at 960 px with two groups the value panel had no room and was not drawn,
/// while its "Value" button stayed pressed and nothing said why. The button says it: another
/// look, the reason in its tooltip; with room again it is the plain pressed button.
#[gpui_kit::test]
fn a_value_panel_without_room_says_so_on_its_button(cx: &mut TestAppContext) {
    use gpui_kit::size;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.simulate_window_resize(app.window.into(), size(px(960.), px(800.)));
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_until(cx, "the first page", |app, cx| {
        app.data_state(cx).starts_with("Rows 1")
    });
    app.press(cx, &["f7"]);
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    let tip = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, _| {
            let node = window.find("data-value-panel");
            (
                node.description().unwrap_or_default().to_string(),
                node.selected(),
            )
        })
    };
    app.wait_until(cx, "the button to say there is no room", |app, cx| {
        tip(app, cx).0.contains(crate::query::value_panel::NO_ROOM)
    });
    assert!(!app.has(cx, "value-panel"));
    assert_ne!(tip(&app, cx).1, Some(true), "not drawn as shown");
    cx.simulate_window_resize(app.window.into(), size(px(1800.), px(800.)));
    app.wait_until(cx, "the panel drawn", |app, cx| app.has(cx, "value-panel"));
    app.wait_until(cx, "the plain button", |app, cx| {
        !tip(app, cx).0.contains(crate::query::value_panel::NO_ROOM)
    });
}

/// The owner's test of the build (2026-11-20): a table's toolbar in a narrow group drew over the
/// group beside it ("Set NULL", "Structure", "Export" on top of the other tab's connection
/// picker). It wraps inside its own group, as a query tab's does. Fails without `flex_wrap` on
/// the data tab's toolbar.
#[gpui_kit::test]
fn the_toolbar_of_a_table_stays_inside_its_narrow_group(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.open_table(cx, "shop", "customers", RelationKind::Table);
    app.wait_until(cx, "the first page", |app, cx| {
        app.data_state(cx).starts_with("Rows 1")
    });
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    let ids = groups(&app, cx);
    assert_eq!(ids.len(), 2);
    let left = bounds(&app, cx, ("tab-group", ids[0]));
    for id in ["data-add-row", "data-export"] {
        let button = bounds(&app, cx, id);
        assert!(
            button.left() >= left.left()
                && button.right() <= left.right()
                && button.bottom() <= left.bottom(),
            "{id} is drawn outside its group: {button:?} in {left:?}"
        );
    }
}

/// K19, review fix: two groups side by side in the default window are each narrower than a
/// query tab's toolbar; its buttons wrap to a second line instead of being cut off at the
/// group's edge, so Run stays in reach in both. Fails without `flex_wrap` on the toolbar.
#[gpui_kit::test]
fn the_toolbar_of_a_narrow_group_keeps_run_in_reach(cx: &mut TestAppContext) {
    use gpui_kit::test::TestQueryExt as _;
    let app = with_tabs(cx, 2);
    app.dispatch(cx, actions::SplitRight);
    let ids = groups(&app, cx);
    assert_eq!(ids.len(), 2);
    let areas: Vec<Bounds<Pixels>> = ids
        .iter()
        .map(|gid| bounds(&app, cx, ("tab-group", *gid)))
        .collect();
    let run = gpui_kit::ElementId::from("tab-run");
    let buttons: Vec<Bounds<Pixels>> = app.step(cx, |window, _| {
        window
            .elements()
            .into_iter()
            .filter(|element| element.path().last() == Some(&run))
            .map(|element| element.bounds())
            .collect()
    });
    assert_eq!(buttons.len(), 2, "one Run button per shown query tab");
    let holding = areas
        .iter()
        .filter(|area| {
            buttons.iter().any(|button| {
                button.left() >= area.left()
                    && button.right() <= area.right()
                    && button.top() >= area.top()
                    && button.bottom() <= area.bottom()
            })
        })
        .count();
    assert_eq!(
        holding, 2,
        "a Run button is cut off: {buttons:?} in {areas:?}"
    );
}

/// Whether `inner` lies within `outer` (a pixel of rounding allowed).
fn lies_in(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.left() >= outer.left() - px(1.)
        && inner.right() <= outer.right() + px(1.)
        && inner.top() >= outer.top() - px(1.)
        && inner.bottom() <= outer.bottom() + px(1.)
}

/// The figures of the selection of `grid` shown in its footer.
fn figures(cx: &mut TestAppContext, grid: &Entity<TableState<Grid>>) -> Option<String> {
    cx.update(|cx| {
        crate::query::grid_view::SelectionStats::of(grid, cx)
            .read(cx)
            .text()
    })
}

/// The real-app pass of 2026-11-20: with an editable result and its value panel in a half-width
/// group, "Apply" was cut to "A" at the panel's edge, Delete and Set NULL dropped out of the edit
/// bar, the WHERE field read "Filter tl" and Export in the footer was cut. Every bar of the
/// result wraps inside the result's own width: each control lies within it, the condition keeps
/// a usable width. Fails without `flex_wrap` on the edit bar (Apply at the panel's edge).
#[gpui_kit::test]
fn the_bars_of_a_result_stay_inside_a_half_width_group(cx: &mut TestAppContext) {
    use gpui_kit::size;
    if !testing::available(Engine::Postgres) {
        return;
    }
    // A long name: "Rows of …" is longer than the result is wide.
    let schema = "kerno_ui_narrow_result_of_a_table_with_a_long_name";
    let _scratch = g8_table(Engine::Postgres, schema);
    let app = connected(cx, Engine::Postgres);
    cx.simulate_window_resize(app.window.into(), size(px(1280.), px(800.)));
    app.run_in_new_tab(
        cx,
        &format!("select id, name, qty from {schema}.t order by id"),
        "secondary-enter",
    );
    app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["4 rows"]);
    assert_eq!(app.result_verdict(cx), None);
    // A change waiting (Discard, Review SQL, Apply), a range selected (Delete, Set NULL, the
    // figures of the selection), the value panel open.
    app.type_in_result(cx, 0, 2, "uno");
    let grid = app.result_grid(cx, 0);
    select_cell(&app, cx, &grid, (0, 1));
    cx.update(|cx| grid.update(cx, |table, _| table.delegate_mut().extend_to(3, 3)));
    app.click(cx, "result-value-panel");
    app.wait_until(cx, "the value panel", |app, cx| app.has(cx, "value-panel"));
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    let left = groups(&app, cx)[0];
    let group = bounds(&app, cx, ("tab-group", left));
    let results = bounds(&app, cx, "results");
    assert!(lies_in(results, group), "{results:?} in {group:?}");
    let panel = app.step(cx, |window, _| {
        window
            .try_find("value-panel")
            .filter(|panel| panel.visible())
            .map(|panel| panel.bounds())
    });
    assert!(panel.is_some(), "the value panel beside the result");
    for id in [
        "result-filter",
        "result-filter-history",
        "result-filter-apply",
        "result-edit-table",
        "result-add-row",
        "result-delete",
        "result-set-null",
        "result-pending",
        "result-discard",
        "result-review",
        "result-apply",
        "result-summary",
        "result-record",
        "result-value-panel",
        "selection-stats",
        "result-export",
    ] {
        let control = bounds(&app, cx, id);
        assert!(
            lies_in(control, results),
            "{id} is drawn outside the result: {control:?} in {results:?}"
        );
    }
    assert!(bounds(&app, cx, "result-filter").size.width >= px(150.));
    assert!(figures(cx, &grid).is_some_and(|line| line.starts_with("12 cells")));
}

/// The real-app pass of 2026-11-20: in a narrow group, the WHERE field of a table squeezed to a
/// few letters and the figures of a selection ran past the group's edge. The filter bar and the
/// footer wrap; the figures wrap inside the footer. Fails without the field's least width (the
/// condition squeezed) or without `min_w_0` on the figures (their line past the edge).
#[gpui_kit::test]
fn the_filter_and_figures_of_a_table_stay_inside_a_narrow_group(cx: &mut TestAppContext) {
    use gpui_kit::size;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.simulate_window_resize(app.window.into(), size(px(1000.), px(800.)));
    app.open_table(cx, "shop", "customers", RelationKind::Table);
    app.wait_until(cx, "the first page", |app, cx| {
        app.data_state(cx).starts_with("Rows 1")
    });
    let grid = app.data_grid(cx);
    select_cell(&app, cx, &grid, (0, 1));
    cx.update(|cx| grid.update(cx, |table, _| table.delegate_mut().extend_to(5, 3)));
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    let left = groups(&app, cx)[0];
    let group = bounds(&app, cx, ("tab-group", left));
    for id in [
        "filter-add",
        "filter-raw",
        "filter-history",
        "filter-apply",
        "data-footer",
        "data-record",
        "data-next",
    ] {
        let control = bounds(&app, cx, id);
        assert!(
            lies_in(control, group),
            "{id} is drawn outside its group: {control:?} in {group:?}"
        );
    }
    let raw = bounds(&app, cx, "filter-raw");
    assert!(
        raw.size.width >= px(150.),
        "the condition is squeezed: {raw:?}"
    );
    let line = bounds(&app, cx, "selection-stats");
    assert!(
        lies_in(line, group),
        "the figures past the edge: {line:?} in {group:?}"
    );
    assert!(figures(cx, &grid).is_some_and(|line| line.starts_with("18 cells")));
}

/// The real-app pass of 2026-11-20: in a narrow group the sub-tabs of a table's structure
/// ("Properties", "Columns", … "DDL") and of the administration ("Settings (27") were cut at the
/// group's edge with no way to the hidden ones. Each bar has the kit's overflow menu, inside the
/// group, listing every sub-tab. Fails without `.menu(true)` on the bars.
#[gpui_kit::test]
fn the_sub_tabs_of_a_narrow_group_stay_in_reach(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    // Opens the menu of the sub-tabs of the tab in the left group.
    let more = |app: &Fixture, cx: &mut TestAppContext, bar: &'static str| {
        let left = groups(app, cx)[0];
        let group = bounds(app, cx, ("tab-group", left));
        let button = app.step(cx, |window, _| window.within(bar).find("more").bounds());
        assert!(
            lies_in(button, group),
            "the sub-tabs' menu of {bar} is outside its group: {button:?} in {group:?}"
        );
        app.step(cx, |window, cx| window.within(bar).click("more", cx));
    };

    app.open_structure(cx, "shop", "customers", RelationKind::Table);
    app.wait_for_structure(cx);
    let structure = app.structure(cx);
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    more(&app, cx, "structure-sections");
    click_menu_entry(&app, cx, "DDL");
    assert_eq!(
        cx.update(|cx| structure.read(cx).section()),
        crate::structure::tab::DDL_SECTION
    );

    // The structure goes; the administration takes the left group.
    app.press(cx, &["secondary-alt-shift-n"]);
    app.press(cx, &["secondary-w"]);
    app.dispatch(cx, actions::OpenAdmin);
    let admin = app.admin(cx);
    app.wait_admin(cx, Pane::Sessions);
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    more(&app, cx, "admin-sections");
    let last = cx
        .update(|cx| admin.read(cx).section_titles(cx))
        .pop()
        .unwrap();
    click_menu_entry(&app, cx, &last);
    assert_eq!(cx.update(|cx| admin.read(cx).section()), SETTINGS);
}

/// The real-app pass of 2026-11-20: an empty administration list said why on one unwrapped
/// line centred over the whole list, cut at both ends in a half-width group ("k. A session
/// shows here…"). It wraps inside the list. Fails with the empty state as a flex row.
#[gpui_kit::test]
fn an_empty_admin_list_wraps_inside_a_narrow_group(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.dispatch(cx, actions::OpenLockManager);
    app.wait_admin(cx, Pane::Locks);
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    let left = groups(&app, cx)[0];
    let group = bounds(&app, cx, ("tab-group", left));
    assert!(app.has(cx, ("admin-empty", Pane::Locks as usize)));
    let text = bounds(&app, cx, ("admin-empty-text", Pane::Locks as usize));
    assert!(lies_in(text, group), "{text:?} in {group:?}");
    // The bar above it ("Filter l" beside the buttons): the buttons go to a second line.
    for id in ["admin-filter", "admin-cancel", "admin-terminate"] {
        let control = bounds(&app, cx, id);
        assert!(lies_in(control, group), "{id}: {control:?} in {group:?}");
    }
    let filter = bounds(&app, cx, "admin-filter");
    assert!(
        filter.size.width >= px(150.),
        "the filter is squeezed: {filter:?}"
    );
}

/// The real-app pass of 2026-11-20 read the commit-mode menu as cut ("…in a transaction you
/// end"): the menu is drawn above the groups and kept inside the window, not cut by its group,
/// but its sentence seemed to stop short. It now says what ends the transaction, and every menu
/// of a query tab's toolbar in the right-hand group of a narrow window shows each entry whole,
/// inside the window. The kit caps a menu at 31.25 rem (406 px with a 13 px interface): fails
/// with an entry longer than that ("…until you commit or roll back", cut at 400 of 519 px).
#[gpui_kit::test]
fn the_menus_of_a_query_tab_in_the_right_group_show_their_whole_text(cx: &mut TestAppContext) {
    use gpui_kit::size;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.simulate_window_resize(app.window.into(), size(px(960.), px(800.)));
    app.press(cx, &["secondary-t", "secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    let right = groups(&app, cx)[1];
    let popup = gpui_kit::ElementId::Name("popup-menu".into());
    // How wide an entry's label is, in the menu's text size.
    let needs = |label: &str, cx: &mut TestAppContext| -> Pixels {
        app.step(cx, |window, cx| {
            let text = cx.text_system();
            let font = text.resolve_font(&gpui_kit::font(cx.theme().font_family.clone()));
            let size = window.rem_size() * 0.875;
            label
                .chars()
                .map(|c| {
                    text.advance(font, size, c)
                        .map_or(px(0.), |advance| advance.width)
                })
                .sum()
        })
    };
    for id in [
        "tab-connection",
        "tab-database",
        "tab-schema",
        "tab-commit-mode",
        "tab-explain",
        "tab-run-menu",
    ] {
        app.step(cx, |window, cx| {
            window.within(("tab-group", right)).click(id, cx)
        });
        let (screen, menu, entries) = app.step(cx, |window, _| {
            use gpui_kit::test::TestQueryExt as _;
            let menu = window.find("popup-menu").bounds();
            let entries: Vec<(String, Bounds<Pixels>)> = window
                .elements()
                .into_iter()
                .filter(|entry| entry.path().contains(&popup))
                .filter_map(|entry| Some((entry.label()?.to_string(), entry.bounds())))
                .filter(|(label, _)| !label.is_empty())
                .collect();
            (
                Bounds::new(Point::default(), window.viewport_size()),
                menu,
                entries,
            )
        });
        assert!(lies_in(menu, screen), "{id}: {menu:?} past the window");
        assert!(!entries.is_empty(), "{id}: no entry");
        for (label, at) in entries {
            let needed = needs(&label, cx);
            assert!(
                lies_in(at, menu) && at.size.width >= needed,
                "{id}: “{label}” cut: {at:?} (needs {needed:?}) in {menu:?}"
            );
        }
        app.press(cx, &["escape"]);
    }
}
