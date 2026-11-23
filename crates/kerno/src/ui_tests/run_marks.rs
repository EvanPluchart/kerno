//! K37 (D-110): where a running script stands, in the editor's left margin,
//! as DBeaver's ruler shows it. Each statement the run reached is marked
//! (running, done, failed, cancelled, refused) the moment it starts and
//! ends; the marks stay after the run until the next one or an edit; the
//! status bar says "statement k of n" and for how long; the margin's marks
//! sit beside their lines however the editor draws them.

use gpui_kit::Pixels;

use crate::query::run_marks::{self, Mark, Trace};

use super::*;

/// The editor's text becomes `text`, the cursor at `at`.
fn set_text(app: &Fixture, cx: &mut TestAppContext, text: &str, at: usize) {
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.editor().update(cx, |editor, cx| {
                editor.set_value(text, window, cx);
                editor.set_selected_range(at..at, cx);
            })
        })
    });
}

/// The marks of the active tab, in words; `-` for a statement not reached.
fn marks(app: &Fixture, cx: &mut TestAppContext) -> Vec<&'static str> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).run_marks())
        .into_iter()
        .map(|mark| mark.map_or("-", Mark::word))
        .collect()
}

fn wait_for_marks(app: &Fixture, cx: &mut TestAppContext, wanted: &[&str]) {
    app.wait_until(cx, &format!("the marks {wanted:?}"), |app, cx| {
        marks(app, cx) == wanted
    });
}

/// What the margin draws now, after a frame: each mark with the line of
/// its first row.
fn drawn(app: &Fixture, cx: &mut TestAppContext) -> Vec<(Mark, Pixels, Pixels)> {
    app.step(cx, |_, _| {});
    let tab = app.tab(cx);
    cx.update(|cx| {
        let tab = tab.read(cx);
        let trace = tab.trace_for_tests();
        run_marks::layout(tab.editor().read(cx), &trace.borrow())
            .map(|layout| {
                layout
                    .marks
                    .iter()
                    .map(|drawn| (drawn.mark, drawn.top, drawn.bottom))
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Where the editor draws the byte `at`.
fn place_of(app: &Fixture, cx: &mut TestAppContext, at: usize) -> Bounds<Pixels> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).editor().read(cx).range_to_bounds(&(at..at)))
        .expect("the byte is on screen")
}

/// The cursor, the selection and the scroll of the editor: what a run
/// must leave alone (K37 point 5).
fn hands(app: &Fixture, cx: &mut TestAppContext) -> (usize, std::ops::Range<usize>, Point<Pixels>) {
    let tab = app.tab(cx);
    cx.update(|cx| {
        let editor = tab.read(cx).editor().read(cx);
        (
            editor.cursor(),
            editor.selected_range(),
            editor.scroll_offset(),
        )
    })
}

/// Mid-run, statement 1 done, 2 running, 3 not reached; the status bar
/// says which and for how long; after the run all three done, the cursor,
/// selection and scroll untouched; an edit clears them; the next run, of
/// one statement, marks that one alone.
#[gpui_kit::test]
fn a_script_s_marks_move_as_it_runs_and_stay_until_an_edit(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let script = "select 1;\nselect pg_sleep(0.4);\nselect 3;";
    set_text(&app, cx, script, 4);
    let before = hands(&app, cx);
    app.press(cx, &["secondary-shift-enter"]);
    wait_for_marks(&app, cx, &["done", "running", "-"]);
    assert!(
        app.status_says(cx, "running statement 2 of 3 · "),
        "{:?}",
        app.status_bar(cx)
    );
    // Beside their lines: the first and second rows of the text.
    let drawn_now = drawn(&app, cx);
    let (first, second) = (place_of(&app, cx, 0), place_of(&app, cx, 10));
    assert_eq!(
        drawn_now,
        vec![
            (Mark::Done, first.top(), first.bottom()),
            (Mark::Running, second.top(), second.bottom()),
        ]
    );
    wait_for_marks(&app, cx, &["done", "done", "done"]);
    app.wait_until(cx, "the end of the run", |app, cx| !app.running(cx));
    assert!(!app.status_says(cx, "running"));
    assert!(app.status_says(cx, "3 of 3 statements done"));
    assert_eq!(hands(&app, cx), before);
    assert_eq!(drawn(&app, cx).len(), 3);

    // An edit: the marks no longer describe the text.
    app.type_text(cx, " ");
    assert_eq!(marks(&app, cx), Vec::<&str>::new());
    assert!(!app.status_says(cx, "3 of 3"));
    assert_eq!(drawn(&app, cx), vec![]);

    // The next run, of the statement at the cursor alone.
    set_text(&app, cx, script, 4);
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the statement", |app, cx| {
        !app.running(cx) && app.outcomes(cx) == ["1 rows"]
    });
    assert_eq!(marks(&app, cx), ["done"]);
}

/// "Run again" on a result reads it again without its place in the text:
/// the marks of the run stay as they were, its statement not marked running.
#[gpui_kit::test]
fn a_result_read_again_leaves_the_marks_alone(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    set_text(
        &app,
        cx,
        "select pg_sleep(0.4) as kerno_k37_again;\nselect 2;",
        0,
    );
    app.press(cx, &["secondary-shift-enter"]);
    wait_for_marks(&app, cx, &["done", "done"]);
    app.wait_until(cx, "the end of the run", |app, cx| !app.running(cx));
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.perform_on_active_result(crate::tabs::TabAction::RunAgain, window, cx)
        })
    });
    app.wait_until(cx, "the result read again", |app, cx| app.running(cx));
    assert_eq!(marks(&app, cx), ["done", "done"]);
    app.wait_until(cx, "its end", |app, cx| !app.running(cx));
    assert_eq!(marks(&app, cx), ["done", "done"]);
}

/// A failure without "continue on error": done, done, failed, and the
/// statement after it bare, where the run stopped.
#[gpui_kit::test]
fn a_failure_marks_where_the_run_stopped(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    set_text(
        &app,
        cx,
        "select 1;\nselect pg_sleep(0.4);\nselect 1/0;\nselect 4;",
        0,
    );
    app.press(cx, &["secondary-shift-enter"]);
    wait_for_marks(&app, cx, &["done", "running", "-", "-"]);
    wait_for_marks(&app, cx, &["done", "done", "failed", "-"]);
    app.wait_until(cx, "the end of the run", |app, cx| !app.running(cx));
    // In words too, in the status bar.
    assert!(
        app.status_says(cx, "statement 3 of 4 failed"),
        "{:?}",
        app.status_bar(cx)
    );
    // The failure is painted over nothing; three marks drawn.
    let kinds: Vec<Mark> = drawn(&app, cx).into_iter().map(|(mark, ..)| mark).collect();
    assert_eq!(kinds, [Mark::Done, Mark::Done, Mark::Failed]);
}

/// One target of a run on several connections (J14): with "continue on
/// error" the statements after a failure run and are marked. (On a
/// read-only connection, below, a write is refused in its place.)
#[gpui_kit::test]
fn with_continue_on_error_the_statements_after_a_failure_are_marked(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    set_text(&app, cx, "select 1;\nselect 1/0;\nselect pg_sleep(0.4);", 0);
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| tab.run_script(true, window, cx))
    });
    wait_for_marks(&app, cx, &["done", "failed", "running"]);
    wait_for_marks(&app, cx, &["done", "failed", "done"]);
}

#[gpui_kit::test]
fn a_refused_write_on_a_read_only_connection_is_marked(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let config = ConnectionConfig {
        read_only: true,
        ..testing::config(Engine::Postgres)
    };
    let app = connected_as(cx, config);
    app.press(cx, &["secondary-t"]);
    // Never sent: the table need not exist.
    set_text(
        &app,
        cx,
        "select 1;\ninsert into kerno_test_k37_never values (1);\nselect 2;",
        0,
    );
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| tab.run_script(false, window, cx))
    });
    wait_for_marks(&app, cx, &["done", "refused", "done"]);
    app.wait_until(cx, "the end of the run", |app, cx| !app.running(cx));
    assert!(app.status_says(cx, "statement 2 of 3 refused"));
}

/// Cancel (⌘.) while statement 2 waits on the server: it is marked
/// cancelled, the one after it left bare.
#[gpui_kit::test]
fn a_cancelled_statement_is_marked_and_the_rest_left_bare(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    set_text(
        &app,
        cx,
        "select 1;\nselect pg_sleep(30) as kerno_k37_cancel;\nselect 3;",
        0,
    );
    app.press(cx, &["secondary-shift-enter"]);
    wait_for_marks(&app, cx, &["done", "running", "-"]);
    app.wait_until(cx, "the sleep on the server", |_, _| {
        running_on_server(Engine::Postgres, "kerno_k37_cancel")
    });
    app.press(cx, &["secondary-."]);
    wait_for_marks(&app, cx, &["done", "cancelled", "-"]);
    app.wait_until(cx, "the end of the run", |app, cx| !app.running(cx));
    assert!(app.status_says(cx, "statement 2 of 3 cancelled"));
}

/// The connection closed under the run (Disconnect, a server gone): the
/// statement it cut off is marked failed, not cancelled.
#[gpui_kit::test]
fn a_statement_cut_off_by_a_disconnect_is_marked_failed(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    set_text(
        &app,
        cx,
        "select 1;\nselect pg_sleep(30) as kerno_k37_cut;\nselect 3;",
        0,
    );
    app.press(cx, &["secondary-shift-enter"]);
    wait_for_marks(&app, cx, &["done", "running", "-"]);
    app.wait_until(cx, "the sleep on the server", |_, _| {
        running_on_server(Engine::Postgres, "kerno_k37_cut")
    });
    let id = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    app.step(cx, |_, cx| {
        app.connections
            .update(cx, |connections, cx| connections.disconnect(&id, cx))
    });
    wait_for_marks(&app, cx, &["done", "failed", "-"]);
    assert!(!app.running(cx));
}

/// The same on MySQL, its SLEEP the slow statement.
#[gpui_kit::test]
fn on_mysql_the_running_statement_is_marked_too(cx: &mut TestAppContext) {
    if !testing::available(Engine::MySql) {
        return;
    }
    let app = connected(cx, Engine::MySql);
    app.press(cx, &["secondary-t"]);
    set_text(&app, cx, "select 1;\nselect sleep(0.4);\nselect 3;", 0);
    app.press(cx, &["secondary-shift-enter"]);
    wait_for_marks(&app, cx, &["done", "running", "-"]);
    assert!(app.status_says(cx, "running statement 2 of 3 · "));
    wait_for_marks(&app, cx, &["done", "done", "done"]);
}

/// A trace of `marks` for the statements of the active tab's text.
fn set_trace(app: &Fixture, cx: &mut TestAppContext, marks: &[Option<Mark>]) {
    let tab = app.tab(cx);
    cx.update(|cx| {
        let tab = tab.read(cx);
        let editor = tab.editor().read(cx);
        // The statements of these texts: each from its first letter to its
        // `;`, as the run reads them.
        let text = editor.text().to_string();
        let mut places = Vec::new();
        let mut from = 0;
        for (end, _) in text.match_indices(';') {
            let start = from + (text[from..end].len() - text[from..end].trim_start().len());
            places.push(start..end);
            from = end + 1;
        }
        let mut trace = Trace::new(editor.text(), places.into_iter().enumerate());
        for (statement, mark) in marks.iter().enumerate() {
            if let Some(mark) = mark {
                trace.set(statement, *mark);
            }
        }
        *tab.trace_for_tests().borrow_mut() = trace;
    });
}

/// The marks sit beside their lines however the editor draws them: with
/// line numbers on and off (left of the text either way), soft-wrapped
/// (every row of the line), a statement of 300 lines, folded (on the fold's
/// row, the next statement's mark moved up with its text), zoomed, in vim
/// mode, and in a script of 5,000 statements (only those on screen).
#[gpui_kit::test]
fn the_marks_sit_beside_their_lines_however_the_editor_draws_them(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    let script = "select 1;\nselect 2;\nselect 3;";
    set_text(&app, cx, script, 0);
    set_trace(&app, cx, &[Some(Mark::Done), Some(Mark::Failed), None]);
    let beside = |app: &Fixture, cx: &mut TestAppContext| {
        let drawn = drawn(app, cx);
        let (first, second) = (place_of(app, cx, 0), place_of(app, cx, 10));
        assert_eq!(
            drawn,
            vec![
                (Mark::Done, first.top(), first.bottom()),
                (Mark::Failed, second.top(), second.bottom()),
            ]
        );
        // The strip is left of the text, the line numbers and the fold
        // marks (the kit's 18 px before the text).
        let tab = app.tab(cx);
        let strip = cx.update(|cx| tab.read(cx).editor().read(cx).input_bounds().left());
        assert!(
            strip + run_marks::STRIP <= first.left() - px(18.),
            "{strip:?} {first:?}"
        );
    };
    beside(&app, cx);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.line_numbers = false));
    beside(&app, cx);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.line_numbers = true));
    app.press(cx, &["secondary-=", "secondary-="]);
    beside(&app, cx);
    app.press(cx, &["secondary-0"]);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.vim_mode = true));
    cx.run_until_parked();
    beside(&app, cx);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.vim_mode = false));
    cx.run_until_parked();

    // Soft wrap: a long statement takes several rows, all marked.
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.soft_wrap = true));
    let long = format!("select '{}';\nselect 2;", "wrapped ".repeat(80));
    set_text(&app, cx, &long, 0);
    set_trace(&app, cx, &[Some(Mark::Running), Some(Mark::Done)]);
    let drawn_now = drawn(&app, cx);
    let end = long.find(';').unwrap();
    let (start, last) = (place_of(&app, cx, 0), place_of(&app, cx, end));
    let line_height = start.size.height;
    assert!(last.top() >= start.top() + line_height * 2., "not wrapped");
    assert_eq!(
        drawn_now[0],
        (Mark::Done, last.bottom(), last.bottom() + line_height)
    );
    assert_eq!(drawn_now[1], (Mark::Running, start.top(), last.bottom()));
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.soft_wrap = false));

    // A statement of 300 lines: its mark spans them all.
    let tall = format!("select\n{}1;\nselect 2;", "1 +\n".repeat(299));
    set_text(&app, cx, &tall, 0);
    set_trace(&app, cx, &[Some(Mark::Running), None]);
    let drawn_now = drawn(&app, cx);
    let top = place_of(&app, cx, 0).top();
    assert_eq!(
        drawn_now,
        vec![(Mark::Running, top, top + line_height * 301.)]
    );

    // Folded: the statement of lines 0 to 6 shows on its fold's row, the
    // next one's mark moved up with its text.
    let folding = "select g,\n  g * 2 as twice\nfrom (\n  select g\n  from generate_series(1, 3) g\n) s\norder by g;\nselect 2;";
    set_text(&app, cx, folding, 0);
    set_trace(&app, cx, &[Some(Mark::Failed), Some(Mark::Done)]);
    let tab = app.tab(cx);
    let next = folding.rfind("select 2").unwrap();
    let unfolded = place_of(&app, cx, next);
    // A click on the fold mark of line 0, left of its text (the cursor's
    // line, where the kit draws it).
    let text_start = place_of(&app, cx, 0);
    let mark = gpui_kit::point(text_start.left() - px(13.), text_start.center().y);
    app.step(cx, |window, cx| window.drag(mark, mark, cx));
    let folded = cx.update(|cx| {
        tab.read(cx)
            .editor()
            .read(cx)
            .display_map()
            .folded_ranges()
            .len()
    });
    assert_eq!(folded, 1);
    let (fold_row, after) = (place_of(&app, cx, 0), place_of(&app, cx, next));
    assert!(after.top() < unfolded.top(), "folded");
    let drawn_now = drawn(&app, cx);
    assert_eq!(drawn_now.len(), 2);
    // The text after the fold is marked where it went; the folded
    // statement on the fold's row, its failure painted last.
    assert_eq!(drawn_now[0], (Mark::Done, after.top(), after.bottom()));
    assert_eq!(drawn_now[1].0, Mark::Failed);
    assert_eq!(drawn_now[1].1, fold_row.top());
    assert!(drawn_now[1].2 <= after.top());

    // 5,000 statements, all done: only those on screen are drawn, there
    // and far down the script.
    let many: String = (0..5000).map(|n| format!("insert {n};\n")).collect();
    set_text(&app, cx, &many, 0);
    set_trace(&app, cx, &[Some(Mark::Done); 5000]);
    let on_screen = |app: &Fixture, cx: &mut TestAppContext| {
        let tab = app.tab(cx);
        let viewport = cx.update(|cx| tab.read(cx).editor().read(cx).input_bounds());
        let drawn = drawn(app, cx);
        assert!(!drawn.is_empty() && drawn.len() < 100, "{}", drawn.len());
        // On screen, or the few rows the kit lays out past each edge.
        for (_, top, bottom) in drawn {
            let rows = (bottom - top) * 3.;
            assert!(
                bottom > viewport.top() - rows && top < viewport.bottom() + rows,
                "{top:?} {bottom:?} {viewport:?}"
            );
        }
    };
    on_screen(&app, cx);
    let far = many.find("insert 4000;").unwrap();
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.editor().update(cx, |editor, cx| {
                editor.set_cursor_position(
                    gpui_kit::component::input::Position {
                        line: 4000,
                        character: 0,
                    },
                    window,
                    cx,
                )
            })
        })
    });
    on_screen(&app, cx);
    assert!(
        drawn(&app, cx)
            .iter()
            .any(|(_, top, _)| *top == place_of(&app, cx, far).top())
    );
}
