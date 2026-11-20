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
