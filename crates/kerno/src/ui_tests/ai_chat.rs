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
    // The last statement had no `;`: it gets one, so the two run each on its own (the
    // real-app pass after K51), and one undo step takes both back.
    let after = app.active_query(cx).1;
    assert_eq!(after, format!("{text};\nSELECT 3 AS c;"));
    let statements = kerno_core::sql::split_statements(&after, kerno_core::sql::LexRules::POSTGRES);
    let statements: Vec<&str> = statements.iter().map(|s| s.text(&after)).collect();
    assert_eq!(
        statements,
        ["select 1 as a", "select 2 as b", "SELECT 3 AS c"]
    );
    ran_nothing(&app, cx);
    app.press(cx, &["secondary-z"]);
    assert_eq!(app.active_query(cx).1, text, "one undo step");
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

/// Whether the active query tab's editor has the keyboard.
fn editor_focused(app: &Fixture, cx: &mut TestAppContext) -> bool {
    let tab = cx.update(|cx| app.workspace.read(cx).active_query_tab().cloned());
    let Some(tab) = tab else {
        return false;
    };
    let editor = cx.update(|cx| tab.read(cx).editor().clone());
    app.step(cx, |window, cx| {
        editor.read(cx).focus_handle(cx).is_focused(window)
    })
}

/// The keyboard is in the panel's message field.
fn in_the_panel(app: &Fixture, cx: &mut TestAppContext) {
    app.click(cx, "ai-sentence");
    assert!(!editor_focused(app, cx));
}

/// The keyboard reached the editor, and the window's shortcuts work: ⇧⌘J shows the panel
/// again, typed text lands in the editor.
fn keyboard_back(app: &Fixture, cx: &mut TestAppContext, how: &str) {
    for _ in 0..3 {
        app.step(cx, |window, _| window.refresh());
    }
    assert!(
        editor_focused(app, cx),
        "{how}: the editor has the keyboard"
    );
    let before = app.active_query(cx).1;
    app.type_text(cx, "x");
    assert_eq!(
        app.active_query(cx).1,
        format!("{before}x"),
        "{how}: typing"
    );
    // ⇧⌘J works: the panel shows (or hides, when its tab closed under it left it shown).
    let shown = app.has(cx, "ai-panel");
    app.press(cx, &["secondary-shift-j"]);
    assert_eq!(app.has(cx, "ai-panel"), !shown, "{how}: ⇧⌘J");
    if shown {
        app.press(cx, &["secondary-shift-j"]);
    }
}

/// The real-app pass after K51: with the assistant off, Close left the keyboard in a panel no
/// longer drawn: no shortcut worked and typed text went nowhere. Each way the panel goes, in
/// each state, the keyboard comes back to the editor (or the workspace with no query tab).
#[gpui_kit::test]
fn the_keyboard_comes_back_whatever_takes_the_panel_away(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    for on in [false, true] {
        enable(cx, "http://127.0.0.1:9/v1", on);
        // Close.
        app.press(cx, &["secondary-shift-j"]);
        in_the_panel(&app, cx);
        app.click(cx, "ai-close");
        assert!(!app.has(cx, "ai-panel"));
        keyboard_back(&app, cx, &format!("Close, on: {on}"));
        // ⇧⌘J (the panel is shown by `keyboard_back`).
        in_the_panel(&app, cx);
        app.press(cx, &["secondary-shift-j"]);
        assert!(!app.has(cx, "ai-panel"));
        keyboard_back(&app, cx, &format!("⇧⌘J, on: {on}"));
        // The status bar's entry (there while the panel shows).
        in_the_panel(&app, cx);
        app.click(cx, "status-ai");
        assert!(!app.has(cx, "ai-panel"));
        keyboard_back(&app, cx, &format!("the status bar, on: {on}"));
        // The panel stays, the assistant turned off under it: the keyboard still works.
        in_the_panel(&app, cx);
        enable(cx, "http://127.0.0.1:9/v1", false);
        assert!(app.has(cx, "ai-off"));
        app.press(cx, &["secondary-shift-j"]);
        assert!(!app.has(cx, "ai-panel"), "assistant off under it, on: {on}");
        keyboard_back(&app, cx, &format!("turned off under it, on: {on}"));
        app.press(cx, &["secondary-shift-j"]);
    }
    // Its tab closed with ⌘W, another tab there: that tab's editor.
    enable(cx, "http://127.0.0.1:9/v1", true);
    app.press(cx, &["secondary-t"]);
    app.press(cx, &["secondary-shift-j"]);
    in_the_panel(&app, cx);
    app.press(cx, &["secondary-w"]);
    assert_eq!(app.titles(cx).len(), 1);
    keyboard_back(&app, cx, "its tab closed");
    // The last tab closed: the workspace has the keyboard, ⌘T opens a tab.
    in_the_panel(&app, cx);
    app.press(cx, &["secondary-w"]);
    assert!(app.titles(cx).is_empty());
    for _ in 0..3 {
        app.step(cx, |window, _| window.refresh());
    }
    let workspace = app.workspace.clone();
    assert!(app.step(cx, |window, cx| {
        workspace.focus_handle(cx).contains_focused(window, cx)
    }));
    app.press(cx, &["secondary-t"]);
    assert_eq!(app.titles(cx).len(), 1, "⌘T after the last tab closed");
}

/// The real-app pass after K53: Enter in "Rows to generate" sent nothing. It sends, as Enter in
/// the message field does; Escape there gives the keyboard back to the editor.
#[gpui_kit::test]
fn enter_in_the_rows_field_sends_and_escape_leaves_it(cx: &mut TestAppContext) {
    use kerno_core::ai::ask::{Ask, Subject};
    use kerno_core::schema::TableRef;
    let fake = FakeAi::start(FIRST);
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    let ask = || Ask::TestData {
        subject: Subject {
            table: TableRef::new("shop", "orders"),
            view: false,
            columns: vec![("id".into(), "bigint".into())],
            references: Vec::new(),
            referenced_by: Vec::new(),
        },
        rows: 10,
    };
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        crate::ai_view::open(&tab, crate::ai_view::Draft::default(), window, cx)
    });
    let panel = panel(&app, cx).unwrap();
    app.step(cx, |window, cx| {
        panel.update(cx, |panel, cx| panel.set_ask(ask(), None, window, cx))
    });
    // Escape in the field: the editor.
    app.click(cx, "ai-ask-rows-input");
    assert!(!editor_focused(&app, cx));
    app.press(cx, &["escape"]);
    assert!(editor_focused(&app, cx));
    // Enter in the field: sent.
    app.click(cx, "ai-ask-rows-input");
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "3");
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the answer", |app, cx| answer(app, cx).is_some());
    assert_eq!(fake.requests().len(), 1);
    let sent = messages(&fake, 0);
    assert!(
        sent.last().unwrap().1.contains("3 rows"),
        "{}",
        sent.last().unwrap().1
    );
}

/// The real-app pass after K52: at its widest (720) in a 1,280-point window with the sidebar,
/// the panel ran past the window's right edge. It is drawn no wider than what the sidebar and
/// an editor of 360 points leave, follows a window made narrower, and comes back to the width
/// the user chose once there is room; that width stays remembered.
#[gpui_kit::test]
fn the_panel_always_fits_in_the_window(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, "http://127.0.0.1:9/v1", true);
    cx.update(|cx| AppState::update_settings(cx, |s| s.ai_panel_width = 720.));
    app.press(cx, &["secondary-shift-j"]);
    let inside = |app: &Fixture, cx: &mut TestAppContext, width: f32, what: &str| {
        for _ in 0..4 {
            app.step(cx, |window, _| window.refresh());
        }
        let (panel, editor) = app.step(cx, |window, _| {
            (
                window.find("ai-panel").bounds(),
                window.find("query-editor").bounds(),
            )
        });
        let right = f32::from(panel.origin.x + panel.size.width);
        assert!(
            right <= width + 0.5,
            "{what}: the panel ends at {right} of {width}"
        );
        for id in [
            "ai-new-conversation",
            "ai-close",
            "ai-endpoint-picker",
            "ai-sentence",
        ] {
            let part = app.step(cx, |window, _| window.find(id).bounds());
            let end = f32::from(part.origin.x + part.size.width);
            assert!(end <= width + 0.5, "{what}: {id} ends at {end} of {width}");
        }
        assert!(
            f32::from(editor.size.width) >= 300.,
            "{what}: the editor keeps its room ({})",
            f32::from(editor.size.width)
        );
        f32::from(panel.size.width)
    };
    cx.simulate_window_resize(app.window.into(), size(px(1280.), px(800.)));
    let wide = inside(&app, cx, 1280., "1280 × 800");
    assert!(wide < 720., "narrower than the width chosen: {wide}");
    cx.simulate_window_resize(app.window.into(), size(px(900.), px(600.)));
    let narrow = inside(&app, cx, 900., "900 × 600");
    assert!(narrow < wide, "follows a narrower window: {narrow} {wide}");
    // Room again: the width chosen, which stayed remembered.
    cx.simulate_window_resize(app.window.into(), size(px(1900.), px(900.)));
    let back = inside(&app, cx, 1900., "1900 × 900");
    assert!((back - 720.).abs() < 1., "the width chosen again: {back}");
    assert_eq!(
        cx.update(|cx| AppState::get(cx).settings.ai_panel_width),
        720.
    );
}
