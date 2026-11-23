//! K51 (D-120): the AI assistant's conversation, in a panel at the right of the window, one per
//! query tab, against fake OpenAI-compatible endpoints on 127.0.0.1 (`FakeAi`). No test reaches
//! another host, and no answer ever runs.

use super::ai::{answer, enable, panel, send_and_wait, wait_for_details, write_sql};
use super::*;
use kerno_core::testing::FakeAi;

const FIRST: &str = "Here:\n```sql\nSELECT count(*) FROM customers;\n```";
const SECOND: &str = "By city:\n```sql\nSELECT city, count(*) FROM customers GROUP BY city;\n```";

/// The messages of the request `n` the endpoint received.
fn messages(fake: &FakeAi, n: usize) -> Vec<(String, String)> {
    let body: serde_json::Value = serde_json::from_slice(&fake.requests()[n].body).unwrap();
    (body["messages"].as_array().unwrap().iter())
        .map(|m| {
            (
                m["role"].as_str().unwrap().to_string(),
                m["content"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Types `text` in the panel's message field.
fn say(app: &Fixture, cx: &mut TestAppContext, text: &str) {
    app.click(cx, "ai-sentence");
    app.type_text(cx, text);
    wait_for_details(app, cx);
}

/// The preview of what Send would send now.
fn previewed(app: &Fixture, cx: &mut TestAppContext) -> String {
    let panel = panel(app, cx).unwrap();
    cx.update(|cx| panel.read(cx).preview_text(cx))
}

fn thread(app: &Fixture, cx: &mut TestAppContext) -> Vec<(&'static str, String)> {
    match panel(app, cx) {
        Some(panel) => cx.update(|cx| panel.read(cx).thread()),
        None => Vec::new(),
    }
}

/// Nothing ran: no statement running, no result.
fn ran_nothing(app: &Fixture, cx: &mut TestAppContext) {
    let tab = app.tab(cx);
    assert!(!cx.update(|cx| tab.read(cx).running()));
    assert!(app.outcomes(cx).is_empty());
}

/// A first message and a follow-up: the follow-up goes with the first question and its answer,
/// the schema once in the system message; "What is sent…" shows those turns before Send.
#[gpui_kit::test]
fn a_follow_up_goes_with_the_earlier_turns_and_refines_the_answer(cx: &mut TestAppContext) {
    let fake = FakeAi::start_answers(&[FIRST, SECOND]);
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    write_sql(&app, cx, "count the customers");
    send_and_wait(&app, cx);
    say(&app, cx, "no, by city");
    app.click(cx, "ai-preview-toggle");
    assert!(app.has(cx, "ai-preview"));
    let preview = previewed(&app, cx);
    assert!(preview.contains("count the customers"), "{preview}");
    assert!(
        preview.contains("SELECT count(*) FROM customers;"),
        "{preview}"
    );
    assert!(preview.contains("no, by city"), "{preview}");
    send_and_wait(&app, cx);
    let sent = messages(&fake, 1);
    let roles: Vec<&str> = sent.iter().map(|(role, _)| role.as_str()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    assert!(sent[1].1.ends_with("count the customers"), "{}", sent[1].1);
    assert_eq!(sent[2].1, FIRST);
    assert_eq!(sent[3].1, "no, by city");
    let body = String::from_utf8(fake.requests()[1].body.clone()).unwrap();
    assert_eq!(body.matches("Schemas:").count(), 1, "the schema once");
    // The preview was the body, byte for byte.
    assert!(preview.ends_with(&body), "{preview}");
    // The thread, in order; the latest answer is the follow-up's.
    let shown: Vec<&str> = thread(&app, cx).iter().map(|(who, _)| *who).collect();
    assert_eq!(shown, ["you", "assistant", "you", "assistant"]);
    assert_eq!(answer(&app, cx).as_deref(), Some(SECOND));
    // The session's log keeps each send.
    assert_eq!(cx.update(|cx| crate::ai_view::sent_log(cx)).len(), 2);
}

/// Insert puts the SQL of a block at the cursor, Replace in place of the statement at the
/// cursor; each is one undo step and nothing runs.
#[gpui_kit::test]
fn insert_and_replace_put_the_sql_where_they_say_and_run_nothing(cx: &mut TestAppContext) {
    let fake = FakeAi::start("Try:\n```sql\nSELECT 3 AS c;\n```");
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    let text = "select 1 as a;\nselect 2 as b";
    app.type_text(cx, text);
    write_sql(&app, cx, "a better second query");
    send_and_wait(&app, cx);
    let tab = app.tab(cx);
    let editor = cx.update(|cx| tab.read(cx).editor().clone());
    // The cursor in the second statement.
    app.step(cx, |_, cx| {
        editor.update(cx, |editor, cx| editor.set_selected_range(20..20, cx))
    });
    app.click(cx, "ai-replace");
    assert_eq!(app.active_query(cx).1, "select 1 as a;\nSELECT 3 AS c;");
    ran_nothing(&app, cx);
    app.press(cx, &["secondary-z"]);
    assert_eq!(app.active_query(cx).1, text, "one undo step");
    // Insert: at the end, on its own line.
    let end = text.len();
    app.step(cx, |_, cx| {
        editor.update(cx, |editor, cx| editor.set_selected_range(end..end, cx))
    });
    app.click(cx, "ai-insert");
    assert_eq!(app.active_query(cx).1, format!("{text}\nSELECT 3 AS c;"));
    ran_nothing(&app, cx);
    // Copy takes the block's SQL alone.
    app.click(cx, "ai-copy-sql");
    assert_eq!(app.clipboard(cx).as_deref(), Some("SELECT 3 AS c;"));
}

/// The editor's query goes with a message as a chip; removed, it is not sent with that message;
/// the next message has it again.
#[gpui_kit::test]
fn the_editor_query_goes_with_a_message_unless_its_chip_is_removed(cx: &mut TestAppContext) {
    let fake = FakeAi::start(FIRST);
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    app.type_text(cx, "select * from orders where id = 7");
    // The panel shown from the keyboard: the keyboard goes to its field.
    app.press(cx, &["secondary-shift-j"]);
    assert!(app.has(cx, "ai-panel"));
    say(&app, cx, "add a date filter");
    assert!(app.has(cx, "ai-editor-query"));
    let preview = previewed(&app, cx);
    assert!(preview.contains("The query in my editor"), "{preview}");
    assert!(preview.contains("id = 7"), "{preview}");
    app.click(cx, ("ai-editor-query", 1usize));
    assert!(!app.has(cx, "ai-editor-query"));
    assert!(!previewed(&app, cx).contains("id = 7"));
    send_and_wait(&app, cx);
    let sent = messages(&fake, 0);
    assert_eq!(sent.last().unwrap().1, "add a date filter");
    assert!(
        !String::from_utf8(fake.requests()[0].body.clone())
            .unwrap()
            .contains("id = 7")
    );
    // The next message has the chip again.
    say(&app, cx, "and sort it");
    assert!(app.has(cx, "ai-editor-query"));
    assert!(previewed(&app, cx).contains("id = 7"));
}

/// Each query tab has its own conversation: switching tabs switches the thread; a tab never
/// shown in the panel has none.
#[gpui_kit::test]
fn switching_tabs_switches_the_conversation(cx: &mut TestAppContext) {
    let fake = FakeAi::start(FIRST);
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    write_sql(&app, cx, "count the customers");
    send_and_wait(&app, cx);
    assert_eq!(thread(&app, cx).len(), 2);
    app.press(cx, &["secondary-t"]);
    // The panel stays, on the new tab's (empty) conversation.
    assert!(app.has(cx, "ai-panel"));
    assert!(app.has(cx, "ai-empty"));
    assert!(thread(&app, cx).is_empty());
    app.press(cx, &["ctrl-tab"]);
    assert_eq!(thread(&app, cx).len(), 2);
    assert!(!app.has(cx, "ai-empty"));
    // New conversation clears this tab's thread.
    app.click(cx, "ai-new-conversation");
    assert!(thread(&app, cx).is_empty());
    assert!(app.has(cx, "ai-empty"));
}

/// The four actions open the panel with the message ready, sending nothing; Send sends it.
#[gpui_kit::test]
fn the_actions_open_the_panel_with_the_message_ready_and_send_nothing(cx: &mut TestAppContext) {
    let fake = FakeAi::start(FIRST);
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    app.type_text(cx, "select * from orders");
    assert!(!app.has(cx, "ai-panel"));
    app.dispatch(cx, actions::AiExplainQuery);
    assert!(app.has(cx, "ai-panel"));
    assert!(app.has(cx, "ai-subject"));
    cx.run_until_parked();
    assert_eq!(fake.connections(), 0, "nothing sent by itself");
    // Its message is the action's; no editor chip (the statement is the subject).
    assert!(!app.has(cx, "ai-editor-query"));
    assert!(previewed(&app, cx).contains("Explain what this"));
    send_and_wait(&app, cx);
    let sent = messages(&fake, 0);
    assert!(sent.last().unwrap().1.contains("select * from orders"));
    // Write SQL: the subject, the field empty, Send waiting for the sentence.
    app.dispatch(cx, actions::AiWriteSql);
    assert!(app.has(cx, "ai-subject"));
    app.click(cx, "ai-send");
    cx.run_until_parked();
    assert_eq!(fake.requests().len(), 1, "no sentence, nothing sent");
}

/// `ai_view::open`, the entry point other places use (K53): the panel on that tab with the
/// draft's words in its field, nothing sent; off, it opens nothing.
#[gpui_kit::test]
fn open_with_a_draft_fills_the_field_and_sends_nothing(cx: &mut TestAppContext) {
    let fake = FakeAi::start(FIRST);
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    let draft = || crate::ai_view::Draft {
        action: None,
        text: "what is in shop.orders?".into(),
    };
    enable(cx, &fake.base(), false);
    app.step(cx, |window, cx| {
        crate::ai_view::open(&tab, draft(), window, cx)
    });
    assert!(!app.has(cx, "ai-panel"), "off: not opened");
    enable(cx, &fake.base(), true);
    app.step(cx, |window, cx| {
        crate::ai_view::open(&tab, draft(), window, cx)
    });
    assert!(app.has(cx, "ai-panel"));
    let panel = panel(&app, cx).unwrap();
    assert!(previewed(&app, cx).contains("what is in shop.orders?"));
    cx.run_until_parked();
    assert_eq!(fake.connections(), 0, "nothing sent by itself");
    // The keyboard is in the field: Enter sends it.
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the answer", |app, cx| answer(app, cx).is_some());
    assert_eq!(
        messages(&fake, 0).last().unwrap().1,
        "what is in shop.orders?"
    );
    assert!(!cx.update(|cx| panel.read(cx).sending()));
}

/// The panel says what it does when its conversation is empty, what is needed with no query
/// tab, and how to turn the assistant on while it is off (Send then sends nothing); the status
/// bar's entry and the command show and hide it.
#[gpui_kit::test]
fn every_state_of_the_panel_has_its_words(cx: &mut TestAppContext) {
    let fake = FakeAi::start(FIRST);
    let app = open(cx, memory_storage());
    enable(cx, &fake.base(), true);
    app.press(cx, &["secondary-shift-j"]);
    assert!(app.has(cx, "ai-no-tab"));
    app.press(cx, &["secondary-t"]);
    assert!(app.has(cx, "ai-empty"));
    // The status bar's entry hides it, and shows it again.
    app.click(cx, "status-ai");
    assert!(!app.has(cx, "ai-panel"));
    app.click(cx, "status-ai");
    assert!(app.has(cx, "ai-panel"));
    // Off: said, and Send sends nothing.
    enable(cx, &fake.base(), false);
    assert!(app.has(cx, "ai-off"));
    say(&app, cx, "anything");
    let panel = panel(&app, cx).unwrap();
    app.step(cx, |window, cx| {
        panel.update(cx, |panel, cx| panel.send(window, cx))
    });
    cx.run_until_parked();
    assert!(super::ai::failure(&app, cx).unwrap().contains("off"));
    assert_eq!(fake.connections(), 0);
    // The width is remembered within its range.
    let width = cx.update(|cx| AppState::get(cx).settings.ai_panel_width);
    assert!(crate::ai_view::WIDTHS.contains(&width));
}

/// Escape in the message field gives the keyboard back to the editor.
#[gpui_kit::test]
fn escape_gives_the_keyboard_back_to_the_editor(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, "http://127.0.0.1:9/v1", true);
    app.press(cx, &["secondary-shift-j"]);
    say(&app, cx, "draft");
    let tab = app.tab(cx);
    let editor = cx.update(|cx| tab.read(cx).editor().clone());
    let focused = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, cx| {
            editor.read(cx).focus_handle(cx).is_focused(window)
        })
    };
    assert!(!focused(&app, cx));
    app.press(cx, &["escape"]);
    assert!(focused(&app, cx));
}

/// An answer that arrives as it is written is shown as it comes; Stop in the middle closes the
/// request, drops what came and says so; a new conversation drops a late answer.
#[gpui_kit::test]
fn a_streamed_answer_shows_as_it_comes_and_stops(cx: &mut TestAppContext) {
    let fake = FakeAi::streaming(
        &["Here:\n```sql\nSELECT ", "1;\n```"],
        Duration::from_millis(800),
    );
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    write_sql(&app, cx, "one");
    app.click(cx, "ai-send");
    let panel = panel(&app, cx).unwrap();
    app.wait_until(cx, "the first piece", |app, cx| {
        thread(app, cx)
            .last()
            .is_some_and(|(_, text)| text.starts_with("Here:"))
    });
    assert!(cx.update(|cx| panel.read(cx).sending()), "still arriving");
    assert!(
        !app.has(cx, "ai-insert"),
        "no Insert on a part of an answer"
    );
    app.wait_until(cx, "the whole answer", |app, cx| answer(app, cx).is_some());
    assert_eq!(
        answer(&app, cx).as_deref(),
        Some("Here:\n```sql\nSELECT 1;\n```")
    );
    assert!(app.has(cx, "ai-insert"));

    // Stopped in the middle: what came is dropped, the endpoint sees the client go.
    let held = FakeAi::streaming(&["SELECT", " 2;"], Duration::from_secs(120));
    enable(cx, &held.base(), true);
    say(&app, cx, "two");
    app.click(cx, "ai-send");
    app.wait_until(cx, "the first piece", |app, cx| {
        thread(app, cx)
            .last()
            .is_some_and(|(_, text)| text == "SELECT")
    });
    app.click(cx, "ai-stop");
    assert!(app.has(cx, "ai-stopped"));
    assert!(!thread(&app, cx).iter().any(|(_, text)| text == "SELECT"));
    app.wait_until(cx, "the connection closed", |_, _| held.closed() == 1);

    // A new conversation while a request waits: closed, nothing of it comes back.
    let slow = FakeAi::start_with(FIRST, Duration::from_millis(600));
    enable(cx, &slow.base(), true);
    say(&app, cx, "three");
    app.click(cx, "ai-send");
    app.wait_until(cx, "the request", |_, _| slow.requests().len() == 1);
    app.click(cx, "ai-new-conversation");
    assert!(!cx.update(|cx| panel.read(cx).sending()));
    app.wait_until(cx, "the connection closed", |_, _| slow.closed() == 1);
    for _ in 0..5 {
        app.step(cx, |window, _| window.refresh());
    }
    assert!(thread(&app, cx).is_empty());
}

/// Beyond the budget the oldest exchanges are left out of what is sent, and the thread says so
/// at that place; the newest message always goes.
#[gpui_kit::test]
fn a_long_conversation_leaves_out_its_oldest_turns_and_says_so(cx: &mut TestAppContext) {
    let long = format!("```sql\nSELECT 1;\n```\n{}", "word ".repeat(3_000));
    let fake = FakeAi::start(&long);
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    write_sql(&app, cx, "kerno-first-question");
    send_and_wait(&app, cx);
    for n in 0..2 {
        say(&app, cx, &format!("question {n}"));
        send_and_wait(&app, cx);
    }
    // Three answers of 15,000 characters: the first exchange no longer fits in 40,000.
    say(&app, cx, "the last one");
    let panel = panel(&app, cx).unwrap();
    assert_eq!(cx.update(|cx| panel.read(cx).left_out()), 2);
    assert!(app.has(cx, "ai-left-out"));
    send_and_wait(&app, cx);
    let body = String::from_utf8(fake.requests()[3].body.clone()).unwrap();
    assert!(
        !body.contains("kerno-first-question"),
        "the oldest left out"
    );
    assert!(body.contains("question 0") && body.contains("the last one"));
}
