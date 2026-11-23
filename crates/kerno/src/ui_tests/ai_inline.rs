//! K52 (D-121): the assistant asked in the editor, and a failed statement fixed in one click,
//! the proposal shown as a diff to accept or reject, against the stand-in endpoint of the J17
//! tests (`FakeAi`, 127.0.0.1). No test reaches any other host; the fixture is read only.

use std::ops::Range;

use gpui_kit::{App, Pixels};
use kerno_core::testing::FakeAi;

use super::ai::enable;
use super::*;
use crate::query::ai_inline::AiInline;

/// The statement asked about, between two others.
const SCRIPT: &str = "SELECT 1;\nSELECT id, total\nFROM shop.orders\nORDER BY total;\nSELECT 3;";
/// Where it is in [`SCRIPT`].
const STATEMENT: Range<usize> = 10..58;
/// The stand-in's answer to it.
const ANSWER: &str = "Here it is:\n```sql\nSELECT id, total\nFROM shop.orders\nWHERE ordered_at \
                      >= date_trunc('year', now())\nORDER BY id;\n```\nOnly this year's orders.";
const PROPOSED: &str = "SELECT id, total\nFROM shop.orders\nWHERE ordered_at >= \
                        date_trunc('year', now())\nORDER BY id";

/// The editor's text becomes `text`, `selected` selected (the cursor at its end), the keyboard
/// in the editor.
fn set_text(app: &Fixture, cx: &mut TestAppContext, text: &str, selected: Range<usize>) {
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.editor().update(cx, |editor, cx| {
                editor.set_value(text, window, cx);
                editor.set_selected_range(selected, cx);
                editor.focus(window, cx);
            })
        })
    });
}

fn text(app: &Fixture, cx: &mut TestAppContext) -> String {
    app.active_query(cx).1
}

fn selection(app: &Fixture, cx: &mut TestAppContext) -> Range<usize> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).editor().read(cx).selected_range())
}

fn inline(app: &Fixture, cx: &mut TestAppContext) -> Option<Entity<AiInline>> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).ai_inline().cloned())
}

fn read<R>(app: &Fixture, cx: &mut TestAppContext, f: impl FnOnce(&AiInline, &App) -> R) -> R {
    let inline = inline(app, cx).expect("the field is open");
    cx.update(|cx| f(inline.read(cx), cx))
}

/// ⌘⇧H, the sentence typed in the field, Enter once the structures are read.
fn ask(app: &Fixture, cx: &mut TestAppContext, sentence: &str) {
    app.press(cx, &["secondary-shift-h"]);
    assert!(app.has(cx, "ai-inline"), "the field opens");
    app.type_text(cx, sentence);
    app.wait_until(cx, "the tables' keys and indexes", |app, cx| {
        !read(app, cx, |inline, _| inline.reading())
    });
    app.press(cx, &["enter"]);
}

/// Waits for the answer; a failure of the request fails the test.
fn wait_for_answer(app: &Fixture, cx: &mut TestAppContext, turns: usize) {
    app.wait_until(cx, "the answer", |app, cx| {
        read(app, cx, |inline, _| {
            if let Some(note) = inline.note().filter(|_| !inline.sending()) {
                assert!(
                    !note.contains("could not") && !note.contains("Nothing was sent"),
                    "{note}"
                );
            }
            inline.turns() == turns && !inline.sending()
        })
    });
}

fn diff(app: &Fixture, cx: &mut TestAppContext) -> Option<Vec<String>> {
    read(app, cx, |inline, _| inline.diff_lines())
}

fn history_len(app: &Fixture) -> usize {
    app.storage.history(None, "", 1000).unwrap().len()
}

fn body(request: &kerno_core::testing::AiRequest) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap()
}

fn last_message(request: &kerno_core::testing::AiRequest) -> String {
    let body = body(request);
    let messages = body["messages"].as_array().unwrap();
    messages.last().unwrap()["content"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A connected PostgreSQL tab with the assistant on, `text` in its editor.
fn ready(cx: &mut TestAppContext, fake: &FakeAi, text: &str, at: Range<usize>) -> Fixture {
    let app = connected(cx, Engine::Postgres);
    new_tab_with_catalog(&app, cx);
    enable(cx, &fake.base(), true);
    set_text(&app, cx, text, at);
    app
}

/// K52 points 1 and 2: Ask Here on the statement at the cursor sends the schema context, that
/// statement and the sentence (never a row); the answer is a diff against the statement, the
/// text unchanged until Accept; ⌘↩ in the field accepts, as one undo step, and runs nothing.
#[gpui_kit::test]
fn ask_here_shows_a_diff_that_accept_applies_in_one_undo_step(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(ANSWER);
    let app = ready(cx, &fake, SCRIPT, 30..30);
    let history = history_len(&app);
    let outcomes = app.outcomes(cx);
    ask(&app, cx, "only the orders of this year");
    wait_for_answer(&app, cx, 2);
    assert_eq!(
        diff(&app, cx).unwrap(),
        [
            " SELECT id, total",
            " FROM shop.orders",
            "−ORDER BY total",
            "+WHERE ordered_at >= date_trunc('year', now())",
            "+ORDER BY id"
        ]
    );
    assert!(app.has(cx, "ai-inline-diff") && app.has(cx, "ai-inline-accept"));
    // The line it removes is tinted in the text.
    let removed = SCRIPT.find("ORDER BY total").unwrap();
    let marked = read(&app, cx, |inline, cx| inline.marked(cx));
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0], removed..removed + "ORDER BY total".len());
    // Nothing of the text changed yet; the cursor stays.
    assert_eq!(text(&app, cx), SCRIPT);
    assert_eq!(selection(&app, cx), 30..30);
    // What went: the statement as "the SQL to change", the sentence; the body is the log's.
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    let message = last_message(&requests[0]);
    assert!(
        message.ends_with(
            "Request: only the orders of this year\n\nThe SQL to change:\n```sql\nSELECT id, \
             total\nFROM shop.orders\nORDER BY total\n```"
        ),
        "{message}"
    );
    let sent = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(sent.contains("- shop.orders (table)"), "{sent}");
    assert!(!sent.contains("Sample rows"), "{sent}");
    let log = cx.update(|cx| crate::ai_view::sent_log(cx));
    assert_eq!(log.last().unwrap().body, sent);
    // ⌘↩ in the field: accepted, the new text selected, the field gone.
    app.press(cx, &["secondary-enter"]);
    assert!(inline(&app, cx).is_none());
    let accepted = SCRIPT.replace(&SCRIPT[STATEMENT], PROPOSED);
    assert_eq!(text(&app, cx), accepted);
    assert_eq!(selection(&app, cx), 10..10 + PROPOSED.len());
    assert!(app.editor_has_keyboard(cx));
    // One undo step brings the statement back whole.
    app.press(cx, &["secondary-z"]);
    assert_eq!(text(&app, cx), SCRIPT);
    cx.run_until_parked();
    assert_eq!(app.outcomes(cx), outcomes, "nothing ran");
    assert_eq!(history_len(&app), history, "nothing ran");
}

/// K52 point 2: Reject (Escape, or its button) leaves the text, the cursor, the selection and
/// the undo history exactly as they were: the next undo undoes the user's own last edit.
#[gpui_kit::test]
fn reject_leaves_the_text_the_selection_and_the_undo_history_alone(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(ANSWER);
    let app = ready(cx, &fake, SCRIPT, SCRIPT.len()..SCRIPT.len());
    // The user's own last edit, then a selection inside the statement.
    app.type_text(cx, " -- mine");
    let typed = format!("{SCRIPT} -- mine");
    assert_eq!(text(&app, cx), typed);
    let tab = app.tab(cx);
    app.step(cx, |_, cx| {
        tab.update(cx, |tab, cx| {
            tab.editor()
                .update(cx, |editor, cx| editor.set_selected_range(STATEMENT, cx))
        })
    });
    for reject in ["escape", "button"] {
        ask(&app, cx, "only the orders of this year");
        let turns = 2;
        wait_for_answer(&app, cx, turns);
        assert!(diff(&app, cx).is_some());
        if reject == "escape" {
            app.press(cx, &["escape"]);
        } else {
            app.click(cx, "ai-inline-reject");
        }
        assert!(inline(&app, cx).is_none(), "{reject}");
        assert_eq!(text(&app, cx), typed, "{reject}");
        assert_eq!(selection(&app, cx), STATEMENT, "{reject}");
        assert!(app.editor_has_keyboard(cx), "{reject}");
    }
    app.press(cx, &["secondary-z"]);
    assert_eq!(text(&app, cx), SCRIPT, "the undo history is the user's");
}

/// K52 point 3: a further sentence goes with the turns so far and replaces the diff.
#[gpui_kit::test]
fn a_refinement_goes_with_the_turns_and_replaces_the_diff(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let by_month = "```sql\nSELECT date_trunc('month', ordered_at) AS month, sum(total)\nFROM \
                    shop.orders\nGROUP BY 1\nORDER BY 1;\n```";
    let fake = FakeAi::start_answers(&[ANSWER, by_month]);
    let app = ready(cx, &fake, SCRIPT, 30..30);
    ask(&app, cx, "only the orders of this year");
    wait_for_answer(&app, cx, 2);
    // The field stays, emptied, the keyboard in it.
    app.type_text(cx, "no, by month");
    app.press(cx, &["enter"]);
    wait_for_answer(&app, cx, 4);
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    let second = body(&requests[1]);
    let messages = second["messages"].as_array().unwrap();
    let roles: Vec<&str> = (messages.iter())
        .map(|message| message["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    assert_eq!(messages[2]["content"], ANSWER);
    assert!(
        (messages[3]["content"].as_str().unwrap())
            .starts_with("Change your last SQL as this says: no, by month")
    );
    assert_eq!(
        diff(&app, cx).unwrap(),
        [
            "−SELECT id, total",
            "+SELECT date_trunc('month', ordered_at) AS month, sum(total)",
            " FROM shop.orders",
            "−ORDER BY total",
            "+GROUP BY 1",
            "+ORDER BY 1"
        ]
    );
    // "Open in the panel": the exchange goes on in the tab's conversation.
    app.click(cx, "ai-inline-panel");
    assert!(inline(&app, cx).is_none());
    let panel = super::ai::panel(&app, cx).expect("the panel");
    let thread = cx.update(|cx| panel.read(cx).thread());
    let said: Vec<&str> = thread.iter().map(|(who, _)| *who).collect();
    assert_eq!(said, ["you", "assistant", "you", "assistant"]);
    assert_eq!(thread[2].1, "no, by month");
    assert_eq!(cx.update(|cx| panel.read(cx).left_out()), 0);
    assert_eq!(text(&app, cx), SCRIPT);
}

/// K52 point 1: on an empty line nothing of the editor is sent; Accept writes the answer
/// there, on its own line, ended so that it does not join the next statement.
#[gpui_kit::test]
fn asking_on_an_empty_line_sends_no_sql_and_writes_at_the_cursor(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start("```sql\nSELECT c.* FROM shop.customers c\n```");
    let script = "SELECT 1;\n\nSELECT 2;";
    let app = ready(cx, &fake, script, 10..10);
    ask(&app, cx, "the customers without an order");
    wait_for_answer(&app, cx, 2);
    let message = last_message(&fake.requests()[0]);
    assert!(!message.contains("The SQL to change"), "{message}");
    assert!(!message.contains("SELECT 1"), "{message}");
    assert_eq!(
        diff(&app, cx).unwrap(),
        ["+SELECT c.* FROM shop.customers c;"]
    );
    app.click(cx, "ai-inline-accept");
    assert_eq!(
        text(&app, cx),
        "SELECT 1;\nSELECT c.* FROM shop.customers c;\nSELECT 2;"
    );
}

/// K52 point 2: an answer with no SQL in it is shown as its text, with "Open in the panel"
/// and no Accept; ⌘↩ then runs nothing of it.
#[gpui_kit::test]
fn an_answer_without_sql_is_shown_as_text_and_nothing_is_offered(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start("I cannot tell which year you mean: the table has no date column.");
    let app = ready(cx, &fake, SCRIPT, 30..30);
    ask(&app, cx, "only the orders of this year");
    wait_for_answer(&app, cx, 2);
    assert!(diff(&app, cx).is_none());
    assert!(read(&app, cx, |inline, _| inline
        .answer()
        .unwrap()
        .starts_with("I cannot tell")));
    assert!(app.has(cx, "ai-inline-text"));
    assert!(!app.has(cx, "ai-inline-accept"));
    assert!(app.has(cx, "ai-inline-panel"));
    app.click(cx, "ai-inline-close");
    assert!(inline(&app, cx).is_none());
    assert_eq!(text(&app, cx), SCRIPT);
}

/// K52 point 5: Stop (⌘. in the field) closes the request at once; an edit of the
/// statement while it waits, and leaving the tab, drop its late answer.
#[gpui_kit::test]
fn stop_an_edit_or_a_tab_switch_drop_the_answer(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start_with(ANSWER, Duration::from_secs(3));
    let app = ready(cx, &fake, SCRIPT, 30..30);
    ask(&app, cx, "only the orders of this year");
    app.wait_until(cx, "the request on its way", |app, cx| {
        app.has(cx, "ai-inline-sending") && fake.requests().len() == 1
    });
    // The quiet mark of the request: the place framed.
    let tab = app.tab(cx);
    let marked = read(&app, cx, |inline, cx| inline.marked(cx));
    assert_eq!((marked.len(), marked[0].clone()), (1, STATEMENT));
    app.press(cx, &["secondary-."]);
    assert!(!app.has(cx, "ai-inline-sending"));
    assert!(read(&app, cx, |inline, _| inline
        .note()
        .unwrap()
        .starts_with("Stopped")));
    assert!(read(&app, cx, |inline, cx| inline.marked(cx)).is_empty());
    app.wait_until(cx, "the connection closed", |_, _| fake.closed() == 1);
    assert!(diff(&app, cx).is_none());

    // An edit of the statement while it waits: dropped, said.
    app.press(cx, &["escape"]);
    ask(&app, cx, "only the orders of this year");
    app.wait_until(cx, "the second request", |_, _| fake.requests().len() == 2);
    let editor = cx.update(|cx| tab.read(cx).editor().clone());
    app.step(cx, |window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_range(20..20, cx);
            editor.replace("x", window, cx);
        })
    });
    assert!(read(&app, cx, |inline, _| inline.stale()));
    assert!(read(&app, cx, |inline, _| inline
        .note()
        .unwrap()
        .contains("text changed")));
    app.wait_until(cx, "the second connection closed", |_, _| {
        fake.closed() == 2
    });
    assert!(diff(&app, cx).is_none());
    assert!(!app.has(cx, "ai-inline-field"));
    // ⌘⇧H starts over where the cursor is.
    app.press(cx, &["secondary-shift-h"]);
    assert!(!read(&app, cx, |inline, _| inline.stale()));

    // Leaving the tab closes the request; the answer is not waited for.
    app.type_text(cx, "only the orders of this year");
    app.wait_until(cx, "the structures", |app, cx| {
        !read(app, cx, |inline, _| inline.reading())
    });
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the third request", |_, _| fake.requests().len() == 3);
    let inline_of_first = inline(&app, cx).unwrap();
    app.press(cx, &["secondary-t"]);
    app.wait_until(cx, "the third connection closed", |_, _| fake.closed() == 3);
    let note = cx.update(|cx| inline_of_first.read(cx).note().map(str::to_string));
    assert!(note.unwrap().contains("tab was left"));
    assert!(cx.update(|cx| inline_of_first.read(cx).diff_lines().is_none()));

    // Closing a tab with a request on its way closes the request.
    set_text(&app, cx, "SELECT 1;", 3..3);
    ask(&app, cx, "count the orders");
    app.wait_until(cx, "the fourth request", |_, _| fake.requests().len() == 4);
    app.press(cx, &["secondary-w"]);
    app.wait_until(cx, "the fourth connection closed", |_, _| {
        fake.closed() == 4
    });
}

/// K52 point 4: "Fix with AI" on a statement the server refused (a misspelt column of
/// `shop.orders`) sends the statement and the server's error at once, from the error view and
/// from the mark in the editor; the fix comes back as a diff on the statement; nothing runs.
#[gpui_kit::test]
fn fix_with_ai_proposes_the_fix_of_a_failed_statement_and_runs_nothing(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(
        "The column is named total.\n```sql\nSELECT id, total FROM shop.orders;\n```",
    );
    let wrong = "SELECT 1;\nSELECT id, totl FROM shop.orders;";
    let app = ready(cx, &fake, wrong, wrong.len() - 3..wrong.len() - 3);
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the error", |app, cx| app.has(cx, "result-error"));
    assert!(app.has(cx, "ai-fix-error"));
    assert!(app.has(cx, "ai-fix-chip"), "beside the mark in the editor");
    let history = history_len(&app);
    let outcomes = app.outcomes(cx);
    for from in ["ai-fix-error", "ai-fix-chip"] {
        let before = fake.requests().len();
        app.click(cx, from);
        // Sent at once, no sentence asked.
        wait_for_answer(&app, cx, 2);
        let requests = fake.requests();
        assert_eq!(requests.len(), before + 1, "{from}");
        let message = last_message(requests.last().unwrap());
        assert!(
            message.contains("Statement:\n```sql\nSELECT id, totl FROM shop.orders\n```"),
            "{message}"
        );
        assert!(
            message.contains("Error:\ncolumn \"totl\" does not exist [42703]"),
            "{message}"
        );
        assert!(message.contains("points at character 12"), "{message}");
        assert_eq!(
            diff(&app, cx).unwrap(),
            [
                "−SELECT id, totl FROM shop.orders",
                "+SELECT id, total FROM shop.orders"
            ],
            "{from}"
        );
        assert_eq!(text(&app, cx), wrong);
        if from == "ai-fix-error" {
            app.press(cx, &["escape"]);
        }
    }
    app.click(cx, "ai-inline-accept");
    assert_eq!(
        text(&app, cx),
        "SELECT 1;\nSELECT id, total FROM shop.orders;"
    );
    cx.run_until_parked();
    assert_eq!(
        app.outcomes(cx),
        outcomes,
        "the fixed statement did not run"
    );
    assert_eq!(history_len(&app), history);
    // The old "Explain error with AI…" stays, in the panel.
    assert!(app.has(cx, "ai-explain-error"));
}

/// K52 point 2: while a proposal is shown, running the statement from the editor runs the
/// editor's own text, never the proposal.
#[gpui_kit::test]
fn running_while_a_proposal_is_shown_runs_the_editor_text(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start("```sql\nSELECT 2 AS proposal;\n```");
    let app = ready(cx, &fake, "SELECT 1 AS original;", 5..5);
    ask(&app, cx, "use two");
    wait_for_answer(&app, cx, 2);
    assert_eq!(
        diff(&app, cx).unwrap(),
        ["−SELECT 1 AS original", "+SELECT 2 AS proposal"]
    );
    // The keyboard back in the editor, ⌘↩ runs what it holds.
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        let focus = tab.read(cx).editor().read(cx).focus_handle(cx);
        focus.focus(window, cx);
    });
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the rows", |app, cx| !app.outcomes(cx).is_empty());
    assert_eq!(app.columns_of(cx, 0), ["original"]);
    // The proposal is still there to accept.
    assert!(diff(&app, cx).is_some());
    assert_eq!(text(&app, cx), "SELECT 1 AS original;");
}

/// K52 point 5: off, ⌘⇧H and Fix with AI say how to turn the assistant on and open nothing;
/// the field of a request opened while on refuses to send once it is off.
#[gpui_kit::test]
fn off_nothing_opens_and_nothing_is_sent(cx: &mut TestAppContext) {
    let fake = FakeAi::start(ANSWER);
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    // ⌘⇧H is no key of the kit's text fields or editor.
    assert_eq!(
        cx.update(|cx| actions::kit_binding_of("secondary-shift-h", cx)),
        None
    );
    enable(cx, &fake.base(), false);
    set_text(&app, cx, SCRIPT, 30..30);
    app.press(cx, &["secondary-shift-h"]);
    assert!(inline(&app, cx).is_none());
    app.dispatch(cx, actions::AiFixError);
    assert!(inline(&app, cx).is_none());
    // Opened while on, then turned off: Enter sends nothing and says why.
    enable(cx, &fake.base(), true);
    app.press(cx, &["secondary-shift-h"]);
    assert!(inline(&app, cx).is_some());
    enable(cx, &fake.base(), false);
    assert!(app.has(cx, "ai-inline-off"));
    app.type_text(cx, "only the orders of this year");
    app.press(cx, &["enter"]);
    assert!(read(&app, cx, |inline, _| inline
        .note()
        .unwrap()
        .contains("off")));
    cx.run_until_parked();
    assert_eq!(fake.connections(), 0);
    assert!(cx.update(|cx| crate::ai_view::sent_log(cx).is_empty()));
    // Escape closes it, nothing changed.
    app.press(cx, &["escape"]);
    assert!(inline(&app, cx).is_none());
    assert_eq!(text(&app, cx), SCRIPT);
}

/// The field sits under its statement (else above it) and inside the editor's view, however
/// the editor draws it: soft wrap on and off, zoomed, a long statement, vim mode.
#[gpui_kit::test]
fn the_field_sits_at_its_statement_however_the_editor_draws_it(cx: &mut TestAppContext) {
    let fake = FakeAi::start(ANSWER);
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    let tab = app.tab(cx);
    let view =
        |cx: &mut TestAppContext| cx.update(|cx| tab.read(cx).editor().read(cx).input_bounds());
    let place_of = |app: &Fixture, cx: &mut TestAppContext, at: usize| -> Bounds<Pixels> {
        let tab = app.tab(cx);
        cx.update(|cx| tab.read(cx).editor().read(cx).range_to_bounds(&(at..at)))
            .expect("the byte is on screen")
    };
    let check = |app: &Fixture, cx: &mut TestAppContext, last_byte: usize, what: &str| {
        app.press(cx, &["secondary-shift-h"]);
        let card = app.step(cx, |window, _| window.find("ai-inline").bounds());
        let view = view(cx);
        assert!(
            card.top() >= view.top() && card.bottom() <= view.bottom(),
            "{what}"
        );
        assert!(
            card.left() >= view.left() && card.right() <= view.right(),
            "{what}"
        );
        let last = place_of(app, cx, last_byte);
        assert!(card.top() >= last.bottom(), "{what}: under its last row");
        assert!(
            card.top() <= last.bottom() + px(4.),
            "{what}: right under it"
        );
        app.press(cx, &["escape"]);
        assert!(inline(app, cx).is_none(), "{what}");
    };
    set_text(&app, cx, SCRIPT, 30..30);
    check(&app, cx, STATEMENT.end, "plain");
    app.press(cx, &["secondary-=", "secondary-="]);
    set_text(&app, cx, SCRIPT, 30..30);
    check(&app, cx, STATEMENT.end, "zoomed");
    app.press(cx, &["secondary-0"]);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.soft_wrap = true));
    let long = format!("SELECT '{}';\nSELECT 2;", "wrapped ".repeat(80));
    let end = long.find(';').unwrap();
    set_text(&app, cx, &long, 5..5);
    check(&app, cx, end, "soft-wrapped");
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.soft_wrap = false));
    // A statement taller than the view: the field stays in the view.
    let tall = format!("SELECT\n{}1;\nSELECT 2;", "1 +\n".repeat(299));
    set_text(&app, cx, &tall, 3..3);
    app.press(cx, &["secondary-shift-h"]);
    let card = app.step(cx, |window, _| window.find("ai-inline").bounds());
    let shown = view(cx);
    assert!(card.top() >= shown.top() && card.bottom() <= shown.bottom());
    app.press(cx, &["escape"]);
    // Vim mode: typed keys go to the field, Escape closes it, vim keeps its mode.
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.vim_mode = true));
    cx.run_until_parked();
    set_text(&app, cx, SCRIPT, 30..30);
    let mode = cx.update(|cx| tab.read(cx).vim_mode());
    app.press(cx, &["secondary-shift-h"]);
    app.type_text(cx, "dd only this year");
    let typed = read(&app, cx, |inline, cx| inline.typed(cx));
    assert_eq!(typed, "dd only this year");
    assert_eq!(text(&app, cx), SCRIPT, "vim took no key");
    app.press(cx, &["escape"]);
    assert!(inline(&app, cx).is_none());
    assert_eq!(cx.update(|cx| tab.read(cx).vim_mode()), mode);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.vim_mode = false));
    cx.run_until_parked();

    // A narrow editor (the assistant's panel shown at its widest): every part of the card, its
    // hint and its links included, stays inside the card, and the card inside the editor; in the
    // diff, the answer without SQL, the request on its way and the failed request.
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            settings.ai_panel = true;
            settings.ai_panel_width = 720.;
        })
    });
    cx.run_until_parked();
    let inside = |app: &Fixture, cx: &mut TestAppContext, what: &str| {
        let card = app.step(cx, |window, _| window.find("ai-inline").bounds());
        let editor = view(cx);
        assert!(editor.size.width < px(480.), "{what}: narrow, {editor:?}");
        assert!(
            card.left() >= editor.left() && card.right() <= editor.right(),
            "{what}: {card:?} in {editor:?}"
        );
        for id in [
            "ai-inline-diff",
            "ai-inline-text",
            "ai-inline-words",
            "ai-inline-sending",
            "ai-inline-stop",
            "ai-inline-note",
            "ai-inline-field",
            "ai-inline-accept",
            "ai-inline-reject",
            "ai-inline-close",
            "ai-inline-foot",
            "ai-inline-hint",
            "ai-inline-panel",
            "ai-inline-what",
        ] {
            let Some(part) = app.step(cx, |window, _| window.try_find(id).map(|e| e.bounds()))
            else {
                continue;
            };
            assert!(
                part.left() >= card.left() - px(0.5)
                    && part.right() <= card.right() + px(0.5)
                    && part.top() >= card.top() - px(0.5)
                    && part.bottom() <= card.bottom() + px(0.5),
                "{what}: {id} {part:?} outside the card {card:?}"
            );
        }
    };
    set_text(&app, cx, SCRIPT, 30..30);
    ask(&app, cx, "only the orders of this year");
    wait_for_answer(&app, cx, 2);
    assert!(diff(&app, cx).is_some());
    assert!(app.has(cx, "ai-inline-panel") && app.has(cx, "ai-inline-accept"));
    inside(&app, cx, "the diff");
    app.press(cx, &["escape"]);
    let prose = FakeAi::start("I cannot tell which year you mean: the table has no date column.");
    enable(cx, &prose.base(), true);
    ask(&app, cx, "only the orders of this year");
    wait_for_answer(&app, cx, 2);
    assert!(app.has(cx, "ai-inline-text"));
    inside(&app, cx, "the answer without SQL");
    app.press(cx, &["escape"]);
    let slow = FakeAi::start_with(ANSWER, Duration::from_secs(30));
    enable(cx, &slow.base(), true);
    ask(&app, cx, "only the orders of this year");
    app.wait_until(cx, "the request on its way", |app, cx| {
        app.has(cx, "ai-inline-sending") && slow.requests().len() == 1
    });
    inside(&app, cx, "the request on its way");
    app.press(cx, &["escape"]);
    // Nothing listens there: the request fails at once, said in the card.
    enable(cx, "http://127.0.0.1:1/v1", true);
    ask(&app, cx, "only the orders of this year");
    app.wait_until(cx, "the failure", |app, cx| {
        !read(app, cx, |inline, _| inline.sending()) && app.has(cx, "ai-inline-note")
    });
    inside(&app, cx, "the failed request");
    app.press(cx, &["escape"]);
}
