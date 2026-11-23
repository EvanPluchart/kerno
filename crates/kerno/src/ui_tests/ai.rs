//! The AI assistant (CLAUDE.md §4 J17; DBeaver #36951, #20072; GOAL-2 criterion 2), against a
//! fake OpenAI-compatible endpoint on 127.0.0.1 (`kerno_core::testing::FakeAi`): nothing is
//! sent while it is off; the body is the preview byte for byte; it holds names and types and no
//! row unless "include sample rows" is ticked; the key goes in `Authorization` and nowhere
//! else; a host other than this computer asks first and plain http:// to one is refused; SQL
//! from an answer is inserted, never run. No test reaches any other host.

use super::*;
use crate::ai_view::AiPanel;
use kerno_core::ai::{self, AiSettings};
use kerno_core::testing::FakeAi;

/// An answer with a statement in it, as models give them.
const ANSWER: &str =
    "Here it is:\n```sql\nSELECT count(*) FROM shop.customers;\n```\nIt counts them.";
/// The end of every fixture customer's email: a cell value, never a name.
const ROW_VALUE: &str = "@example.com";

pub(super) fn enable(cx: &mut TestAppContext, endpoint: &str, enabled: bool) {
    let endpoint = endpoint.to_string();
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            settings.ai = AiSettings {
                enabled,
                endpoint,
                model: "fake-model".into(),
                ..AiSettings::default()
            }
        })
    });
}

pub(super) fn panel(app: &Fixture, cx: &mut TestAppContext) -> Option<Entity<AiPanel>> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).ai_panel().cloned())
}

/// The bytes Send would send now, as the preview shows them.
fn previewed(app: &Fixture, cx: &mut TestAppContext) -> Option<String> {
    let panel = panel(app, cx)?;
    cx.update(|cx| {
        panel
            .read(cx)
            .payload(cx)
            .map(|payload| ai::preview(&payload))
    })
}

pub(super) fn answer(app: &Fixture, cx: &mut TestAppContext) -> Option<String> {
    let panel = panel(app, cx)?;
    cx.update(|cx| panel.read(cx).answer().map(str::to_string))
}

pub(super) fn failure(app: &Fixture, cx: &mut TestAppContext) -> Option<String> {
    let panel = panel(app, cx)?;
    cx.update(|cx| panel.read(cx).failure().map(str::to_string))
}

/// Opens "Write SQL" and types `sentence`.
pub(super) fn write_sql(app: &Fixture, cx: &mut TestAppContext, sentence: &str) {
    app.dispatch(cx, actions::AiWriteSql);
    assert!(app.has(cx, "ai-panel"));
    app.click(cx, "ai-sentence");
    app.type_text(cx, sentence);
    wait_for_details(app, cx);
}

/// K50: the structures of the tables the request is about are read before Send.
pub(super) fn wait_for_details(app: &Fixture, cx: &mut TestAppContext) {
    app.wait_until(cx, "the tables' keys and indexes", |app, cx| {
        panel(app, cx).is_none_or(|panel| cx.update(|cx| !panel.read(cx).reading_details()))
    });
}

pub(super) fn send_and_wait(app: &Fixture, cx: &mut TestAppContext) {
    wait_for_details(app, cx);
    app.click(cx, "ai-send");
    app.wait_until(cx, "the answer", |app, cx| {
        if let Some(failure) = failure(app, cx) {
            panic!("{failure}");
        }
        answer(app, cx).is_some()
    });
}

/// Nothing ran: no statement running and the same results as before.
fn ran_nothing(app: &Fixture, cx: &mut TestAppContext, outcomes: &[String]) {
    let tab = app.tab(cx);
    assert!(!cx.update(|cx| tab.read(cx).running()));
    assert_eq!(app.outcomes(cx), outcomes);
}

fn history_len(app: &Fixture) -> usize {
    app.storage.history(None, "", 1000).unwrap().len()
}

/// GOAL-2 criterion 2, first half: off, no entry runs and the endpoint gets no connection at
/// all, whatever is asked (the commands, the editor menu, Send, List models).
#[gpui_kit::test]
fn nothing_is_sent_while_the_assistant_is_off(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(ANSWER);
    let app = connected(cx, Engine::Postgres);
    new_tab_with_catalog(&app, cx);
    enable(cx, &fake.base(), false);
    app.type_text(cx, "select * from shop.customers");
    for action in crate::ai_view::menu_entries(false) {
        assert!(!action.2 && action.0.contains("turn on in Settings → AI"));
        app.step(cx, |window, cx| window.dispatch_action((action.1)(), cx));
        assert!(panel(&app, cx).is_none(), "{} opened a panel", action.0);
    }
    // An error on screen offers no AI button while off.
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "select * from shop.kerno_ui_ai_missing");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the error", |app, cx| app.has(cx, "result-error"));
    assert!(!app.has(cx, "ai-explain-error"));
    // A panel opened while on, then the assistant turned off: Send refuses.
    enable(cx, &fake.base(), true);
    write_sql(&app, cx, "how many customers");
    enable(cx, &fake.base(), false);
    let panel = panel(&app, cx).unwrap();
    app.step(cx, |window, cx| {
        panel.update(cx, |panel, cx| panel.send(window, cx))
    });
    assert!(failure(&app, cx).unwrap().contains("off"));
    // Settings → AI: List models asks nothing while off.
    app.press(cx, &["secondary-,"]);
    let index = crate::settings_view::SECTIONS
        .iter()
        .position(|(_, label)| *label == "AI")
        .unwrap();
    app.click(cx, ("settings-section", index));
    assert!(app.has(cx, "settings-ai"));
    app.tap(cx, "ai-list-models");
    assert!(!app.has(cx, ("ai-model", 0usize)));
    cx.run_until_parked();
    assert_eq!(fake.connections(), 0);
    assert!(fake.requests().is_empty());
    assert!(cx.update(|cx| crate::ai_view::sent_log(cx).is_empty()));
}

/// GOAL-2 criterion 2, second half: the body is the preview byte for byte; it holds the
/// schema, table and column names and types and no cell value of the fixture until "include
/// sample rows" is ticked; the tick is off at every new request; the SQL of the answer is
/// inserted as one undoable edit and never reaches the database.
#[gpui_kit::test]
fn the_body_is_the_preview_and_holds_rows_only_with_the_tick(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(ANSWER);
    let app = connected(cx, Engine::Postgres);
    new_tab_with_catalog(&app, cx);
    enable(cx, &fake.base(), true);
    let history = history_len(&app);
    write_sql(&app, cx, "how many customers are there");
    app.click(cx, "ai-preview-toggle");
    assert!(app.has(cx, "ai-preview"));
    let preview = previewed(&app, cx).unwrap();
    // The example of the report: "Write SQL" on the fixture's `shop` schema.
    println!("--- Write SQL payload ---\n{preview}\n---");
    for name in [
        "shop.customers",
        "email shop.email",
        "full_name",
        "Schemas:",
        "shop",
    ] {
        assert!(preview.contains(name), "{name} missing:\n{preview}");
    }
    assert!(!preview.contains(ROW_VALUE), "{preview}");
    // K50: the table the sentence names, with its keys and indexes, read when the request
    // opened; no default or comment of its structure.
    for line in [
        r"- shop.customers (table)\n  id bigint not null\n  email shop.email not null",
        r"\n  phone character varying(32)\n",
        r"\n  primary key (id)",
        r"\n  unique (email)",
    ] {
        assert!(preview.contains(line), "{line} missing:\n{preview}");
    }
    for never in ["now()", "People who can place orders", "Free-form labels"] {
        assert!(!preview.contains(never), "{never}:\n{preview}");
    }
    send_and_wait(&app, cx);
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(requests[0].body, preview.as_bytes());
    let sent = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(
        !sent.contains(ROW_VALUE) && !sent.contains("Lovelace"),
        "{sent}"
    );
    // The log of the session holds the same bytes.
    let log = cx.update(|cx| crate::ai_view::sent_log(cx));
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].body, preview);
    assert_eq!(log[0].endpoint, format!("{}/chat/completions", fake.base()));

    // Insert: at the cursor, selected, one undo step; nothing runs.
    let tab = app.tab(cx);
    let editor = cx.update(|cx| tab.read(cx).editor().clone());
    app.step(cx, |window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_value("", window, cx);
        })
    });
    let outcomes = app.outcomes(cx);
    app.click(cx, "ai-insert");
    ran_nothing(&app, cx, &outcomes);
    let sql = "SELECT count(*) FROM shop.customers;";
    assert_eq!(app.active_query(cx).1, sql);
    assert_eq!(
        cx.update(|cx| editor.read(cx).selected_range()),
        0..sql.len()
    );
    app.press(cx, &["secondary-z"]);
    assert_eq!(app.active_query(cx).1, "");
    cx.run_until_parked();
    assert_eq!(history_len(&app), history, "an inserted statement ran");

    // A new request starts without rows; ticked, the rows are read and sent.
    write_sql(&app, cx, "list the customers");
    let panel = panel(&app, cx).unwrap();
    assert!(!cx.update(|cx| panel.read(cx).include_samples()));
    app.click(cx, "ai-sample-rows");
    assert!(cx.update(|cx| panel.read(cx).include_samples()));
    app.wait_until(cx, "the sample rows", |app, cx| {
        previewed(app, cx).is_some()
    });
    let preview = previewed(&app, cx).unwrap();
    assert!(preview.contains("Sample rows the user chose to include"));
    assert!(preview.contains(ROW_VALUE), "{preview}");
    send_and_wait(&app, cx);
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].body, preview.as_bytes());
    // Sent, the tick goes back off: the next message carries no row (K51: the earlier turns
    // go with it, the rows of the system message do not).
    assert!(!cx.update(|cx| panel.read(cx).include_samples()));
    app.click(cx, "ai-sentence");
    app.type_text(cx, "and by city");
    wait_for_details(&app, cx);
    let next = previewed(&app, cx).unwrap();
    assert!(next.contains("list the customers"), "{next}");
    assert!(!next.contains(ROW_VALUE), "{next}");
    // The sample rows were read off the history.
    assert_eq!(history_len(&app), history);
}

/// The key goes in `Authorization: Bearer` and nowhere else: not in the settings, not in
/// Kerno's storage file, not in the preview or the log (`Bearer ***`), not in a log line
/// written on the UI thread.
#[gpui_kit::test]
fn the_key_goes_in_authorization_and_nowhere_else(cx: &mut TestAppContext) {
    use std::io::Write as _;
    const KEY: &str = "sk-kerno-ui-key-5d81c2";
    #[derive(Clone, Default)]
    struct Lines(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Lines {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let lines = Lines::default();
    let writer = lines.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);

    let fake = FakeAi::start(ANSWER);
    let data = tempfile::tempdir().unwrap();
    let database = data.path().join("kerno.db");
    let app = open(cx, Arc::new(Storage::open(&database).unwrap()));
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    // Typed in Settings → AI and saved for this endpoint.
    app.press(cx, &["secondary-,"]);
    let index = crate::settings_view::SECTIONS
        .iter()
        .position(|(_, label)| *label == "AI")
        .unwrap();
    app.click(cx, ("settings-section", index));
    type_in(&app, cx, "ai-key", KEY);
    app.tap(cx, "ai-save-key");
    let name = ai::key_name(&fake.base());
    app.wait_until(cx, "the saved key", |_, cx| {
        cx.update(|cx| AppState::get(cx).secrets.get(&name).unwrap())
            .is_some_and(|key| key.expose() == KEY)
    });
    // List models sends it too.
    app.tap(cx, "ai-list-models");
    app.wait_until(cx, "the models", |app, cx| {
        app.has(cx, ("ai-model", 0usize))
    });
    app.tap(cx, ("ai-model", 0usize));
    assert_eq!(settings_now(cx).ai.model, "fake-model");
    app.press(cx, &["escape"]);
    write_sql(&app, cx, "count the orders");
    app.click(cx, "ai-preview-toggle");
    let panel = panel(&app, cx).unwrap();
    let preview_text = cx.update(|cx| panel.read(cx).preview_text(cx));
    assert!(
        preview_text.contains("Authorization: Bearer ***"),
        "{preview_text}"
    );
    send_and_wait(&app, cx);
    let requests = fake.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].line, "GET /v1/models HTTP/1.1");
    for request in &requests {
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );
        assert!(!String::from_utf8_lossy(&request.body).contains(KEY));
    }
    let log = cx.update(|cx| crate::ai_view::sent_log(cx));
    assert!(log[0].key && !log[0].body.contains(KEY));

    app.wait_until(cx, "the stored settings", |app, _| {
        app.storage
            .setting::<serde_json::Value>("settings")
            .unwrap()
            .is_some_and(|stored| stored["ai"]["model"] == "fake-model")
    });
    let stored: serde_json::Value = app.storage.setting("settings").unwrap().unwrap();
    assert!(!stored.to_string().contains(KEY), "{stored}");
    assert_eq!(stored["ai"]["endpoint"], fake.base());
    let contains = |bytes: &[u8]| bytes.windows(KEY.len()).any(|w| w == KEY.as_bytes());
    for file in files_under(data.path())
        .into_iter()
        .chain(files_under(app._data_dir.path()))
    {
        assert!(!contains(&std::fs::read(&file).unwrap()), "{file:?}");
    }
    lines.clone().flush().unwrap();
    assert!(!contains(&lines.0.lock().unwrap()));
}

/// A host other than this computer is shown and must be allowed before anything goes there;
/// plain http:// to it is refused. Never sent: no test reaches another host.
#[gpui_kit::test]
fn a_remote_endpoint_asks_first_and_plain_http_to_it_is_refused(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, "", true);
    app.press(cx, &["secondary-,"]);
    let index = crate::settings_view::SECTIONS
        .iter()
        .position(|(_, label)| *label == "AI")
        .unwrap();
    app.click(cx, ("settings-section", index));
    // Added by hand: plain http:// to another host is refused and not saved.
    app.tap(cx, "ai-add");
    type_in(&app, cx, "ai-add-address", "http://kerno-ai.invalid/v1");
    assert!(app.has(cx, "ai-add-error"));
    app.tap(cx, "ai-add-save");
    assert!(settings_now(cx).ai.endpoints.is_empty());
    // Without a scheme or /v1: https and /v1, saved, active, and asking first.
    type_in(&app, cx, "ai-add-address", "kerno-ai.invalid");
    app.tap(cx, "ai-add-save");
    assert_eq!(settings_now(cx).ai.endpoint, "https://kerno-ai.invalid/v1");
    assert!(app.has(cx, ("ai-remote-warning", 0usize)));
    assert!(
        crate::ai_settings_view::remote_warning("kerno-ai.invalid", ai::Reach::Internet)
            .starts_with(
                "Requests will leave this computer and go to kerno-ai.invalid, on the internet"
            )
    );
    assert!(
        crate::ai_settings_view::remote_warning("llm.example.lan", ai::Reach::Network)
            .contains("llm.example.lan, on your network")
    );
    // Not allowed yet: List models and Send refuse before connecting.
    app.tap(cx, "ai-list-models");
    app.wait_until(cx, "the refusal", |app, cx| app.has(cx, "ai-models-error"));
    app.step(cx, |window, cx| {
        gpui_kit::component::WindowExt::close_dialog(window, cx)
    });
    write_sql(&app, cx, "anything");
    app.click(cx, "ai-send");
    let refused = failure(&app, cx).unwrap();
    assert!(refused.contains("allow them in Settings → AI"), "{refused}");
    assert!(cx.update(|cx| crate::ai_view::sent_log(cx).is_empty()));
    // Allowed once per endpoint, and forgotten on demand.
    app.press(cx, &["secondary-,"]);
    app.click(cx, ("settings-section", index));
    app.tap(cx, ("ai-allow-host", 0usize));
    assert!(
        settings_now(cx)
            .ai
            .confirmed
            .contains("https://kerno-ai.invalid/v1")
    );
    assert!(!app.has(cx, ("ai-remote-warning", 0usize)));
    app.tap(cx, ("ai-forget-host", 0usize));
    assert!(settings_now(cx).ai.confirmed.is_empty());
}

/// Replaces what the input `id` holds with `text`, as typed, scrolled into view first.
pub(super) fn type_in(
    app: &Fixture,
    cx: &mut TestAppContext,
    id: impl Into<gpui_kit::ElementId>,
    text: &str,
) {
    app.tap(cx, id);
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, text);
}

/// "Explain error" sends the statement and the server's message and offers the fix as a new
/// statement; "Explain plan" sends the plan; "Explain query" the statement at the cursor.
/// Inserted, the fix does not run.
#[gpui_kit::test]
fn the_error_the_plan_and_the_query_are_explained(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(ANSWER);
    let app = connected(cx, Engine::Postgres);
    new_tab_with_catalog(&app, cx);
    enable(cx, &fake.base(), true);
    let bad = "select * from shop.kerno_ui_ai_missing";
    app.type_text(cx, bad);
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the error", |app, cx| app.has(cx, "result-error"));
    app.wait_until(cx, "the failed statement in the history", |app, _| {
        (app.storage.history(None, "", 1000).unwrap().iter())
            .any(|entry| entry.sql.contains("kerno_ui_ai_missing"))
    });
    let history = history_len(&app);
    app.click(cx, "ai-explain-error");
    send_and_wait(&app, cx);
    let body = String::from_utf8(fake.requests()[0].body.clone()).unwrap();
    assert!(
        body.contains("kerno_ui_ai_missing") && body.contains("does not exist"),
        "{body}"
    );
    assert!(body.contains("propose a fix"));
    let outcomes = app.outcomes(cx);
    app.click(cx, "ai-insert");
    ran_nothing(&app, cx, &outcomes);
    // K51: at the cursor, on its own line, as every Insert (Replace takes the statement's place).
    assert_eq!(
        app.active_query(cx).1,
        format!("{bad}\nSELECT count(*) FROM shop.customers;")
    );
    cx.run_until_parked();
    assert_eq!(history_len(&app), history);

    // The plan on screen.
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "select * from shop.orders where id = 1");
    app.press(cx, &["secondary-e"]);
    app.wait_until(cx, "the plan", |app, cx| app.has(cx, "ai-explain-plan"));
    app.click(cx, "ai-explain-plan");
    send_and_wait(&app, cx);
    let body = String::from_utf8(fake.requests()[1].body.clone()).unwrap();
    assert!(
        body.contains("Plan:") && body.contains("shop.orders"),
        "{body}"
    );

    // The statement at the cursor, among two (the keyboard back in the editor: K51's panel
    // keeps it after a send).
    app.click(cx, "query-editor");
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(
        cx,
        "select 1 as kerno_ui_ai_first;\nselect 2 as kerno_ui_ai_second",
    );
    app.dispatch(cx, actions::AiExplainQuery);
    send_and_wait(&app, cx);
    let body = String::from_utf8(fake.requests()[2].body.clone()).unwrap();
    assert!(
        body.contains("kerno_ui_ai_second") && !body.contains("kerno_ui_ai_first"),
        "{body}"
    );
    app.click(cx, "ai-copy");
    assert_eq!(app.clipboard(cx), Some(ai::unfenced(ANSWER)));
    // K51: Close hides the panel; the conversation stays with its tab.
    app.click(cx, "ai-close");
    assert!(!app.has(cx, "ai-panel"));
    assert!(panel(&app, cx).is_some());
}

/// C3: Stop closes the request's connection at once ("Stopped"): the endpoint, holding the
/// answer, sees the client go and never answers; the next request goes without waiting for
/// the first; nothing is shown or inserted; a request after them is answered.
#[gpui_kit::test]
fn a_request_is_stopped(cx: &mut TestAppContext) {
    // Holds every answer longer than the test lasts: only Stop can end these requests.
    let fake = FakeAi::start_with(ANSWER, Duration::from_secs(120));
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    write_sql(&app, cx, "anything");
    let panel = panel(&app, cx).unwrap();
    let send_and_stop = |cx: &mut TestAppContext, sent: usize| {
        // K51: a sent message leaves the field empty.
        if sent > 1 {
            app.click(cx, "ai-sentence");
            app.type_text(cx, "anything again");
        }
        app.click(cx, "ai-send");
        assert!(cx.update(|cx| panel.read(cx).sending()));
        app.wait_until(cx, "the request", |_, _| fake.requests().len() == sent);
        app.click(cx, "ai-stop");
        assert!(!cx.update(|cx| panel.read(cx).sending()));
        assert!(app.has(cx, "ai-stopped"), "says so at once");
        assert!(!app.has(cx, "ai-stop"));
        app.wait_until(cx, "the endpoint to see the connection close", |_, _| {
            fake.closed() == sent
        });
    };
    send_and_stop(cx, 1);
    // The next request goes while nothing of the first is left.
    send_and_stop(cx, 2);
    assert_eq!(fake.answered(), 0);

    // Nothing is shown, Insert has nothing to insert.
    let tab = app.tab(cx);
    let editor = cx.update(|cx| tab.read(cx).editor().clone());
    let before = cx.update(|cx| editor.read(cx).text().to_string());
    for _ in 0..5 {
        app.step(cx, |window, _| window.refresh());
    }
    app.step(cx, |window, cx| {
        panel.update(cx, |panel, cx| panel.insert(window, cx))
    });
    assert!(app.has(cx, "ai-stopped"));
    assert!(answer(&app, cx).is_none() && failure(&app, cx).is_none());
    assert_eq!(cx.update(|cx| editor.read(cx).text().to_string()), before);

    // A request after them is answered (by an endpoint that answers at once).
    let quick = FakeAi::start(ANSWER);
    enable(cx, &quick.base(), true);
    app.click(cx, "ai-sentence");
    app.type_text(cx, "once more");
    send_and_wait(&app, cx);
    assert_eq!(answer(&app, cx).as_deref(), Some(ANSWER));
    assert_eq!((fake.answered(), quick.answered()), (0, 1));
}

/// Real-app pass: Insert put the SQL right after the editor's last word
/// (`…restart_check_oneSELECT …`), and the panel showed the answer's fences. The SQL goes on
/// its own line at the caret, replaces a selection, and the panel shows no fence.
#[gpui_kit::test]
fn inserted_sql_is_never_glued_to_the_text(cx: &mut TestAppContext) {
    let fake = FakeAi::start(ANSWER);
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    let line = "select 1 as restart_check_one";
    app.type_text(cx, line);
    write_sql(&app, cx, "how many customers");
    send_and_wait(&app, cx);
    let shown = app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .iter()
            .filter(|text| text.path().contains(&"ai-answer".into()))
            .map(|text| text.text().to_string())
            .collect::<String>()
    });
    assert!(shown.contains("SELECT count(*)"), "{shown}");
    assert!(!shown.contains("```"), "{shown}");
    let tab = app.tab(cx);
    let editor = cx.update(|cx| tab.read(cx).editor().clone());
    let end = line.len();
    app.step(cx, |_, cx| {
        editor.update(cx, |editor, cx| editor.set_selected_range(end..end, cx))
    });
    app.click(cx, "ai-insert");
    let sql = "SELECT count(*) FROM shop.customers;";
    assert_eq!(app.active_query(cx).1, format!("{line}\n{sql}"));
    // A selection is replaced, nothing added around it.
    app.step(cx, |_, cx| {
        editor.update(cx, |editor, cx| editor.set_selected_range(0..end, cx))
    });
    app.click(cx, "ai-insert");
    assert_eq!(app.active_query(cx).1, format!("{sql}\n{sql}"));
}

/// The context is built from the tab's catalog on MySQL and MariaDB too: names and types,
/// rows only with the tick.
#[gpui_kit::test]
fn the_context_is_built_on_mysql_and_mariadb(cx: &mut TestAppContext) {
    for engine in [Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        new_tab_with_catalog(&app, cx);
        // Never sent: the endpoint is a closed port.
        enable(cx, "http://127.0.0.1:9/v1", true);
        write_sql(&app, cx, "list the customers");
        let preview = previewed(&app, cx).unwrap();
        assert!(
            preview.contains(&format!("Database engine: {}", engine.label())),
            "{preview}"
        );
        assert!(preview.contains("kerno.customers (table)"), "{preview}");
        // K50: its keys and indexes, read on MySQL and MariaDB too.
        assert!(preview.contains(r"\n  primary key (id)"), "{preview}");
        assert!(preview.contains(r"\n  unique (email)"), "{preview}");
        // Only the tab's database: MySQL's schemas are databases.
        assert!(preview.contains(r"Schemas: kerno\nTables"), "{preview}");
        assert!(!preview.contains("kerno_analytics"), "{preview}");
        assert!(preview.contains("email"), "{preview}");
        assert!(!preview.contains(ROW_VALUE), "{preview}");
        app.click(cx, "ai-sample-rows");
        app.wait_until(cx, "the sample rows", |app, cx| {
            previewed(app, cx).is_some()
        });
        assert!(previewed(&app, cx).unwrap().contains(ROW_VALUE));
        assert!(cx.update(|cx| crate::ai_view::sent_log(cx).is_empty()));
    }
}

/// The editor's right-click menu ends with the AI entries: off, each says how to turn the
/// assistant on and does nothing; on, Write SQL opens the panel.
#[gpui_kit::test]
fn the_editor_menu_offers_the_assistant_and_says_when_it_is_off(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 1");
    let on_text = Some(gpui_kit::point(px(60.), px(10.)));
    app.right_click(cx, "query-editor", on_text);
    for label in [
        "AI: Write SQL… (turn on in Settings → AI)",
        "AI: Explain query (turn on in Settings → AI)",
        "AI: Explain plan (turn on in Settings → AI)",
        "AI: Explain error (turn on in Settings → AI)",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.click_label(cx, "AI: Write SQL… (turn on in Settings → AI)");
    assert!(panel(&app, cx).is_none());
    app.press(cx, &["escape"]);
    enable(cx, "http://127.0.0.1:9/v1", true);
    app.right_click(cx, "query-editor", on_text);
    app.click_label(cx, "AI: Write SQL…");
    app.wait_until(cx, "the panel", |app, cx| panel(app, cx).is_some());
    assert!(app.has(cx, "ai-sentence"));
}

/// An "AI: …" command run with the keyboard outside a query tab is not lost: off, it says how
/// to turn the assistant on, as in a query tab; on with no query tab, it says to open one; on
/// with a query tab active, that tab takes it.
#[gpui_kit::test]
fn an_ai_command_outside_a_query_tab_says_what_it_needs(cx: &mut TestAppContext) {
    // The notifications' text is not observable here; their count is.
    let notifications = |app: &Fixture, cx: &mut TestAppContext| {
        use gpui_kit::component::WindowExt as _;
        app.step(cx, |window, cx| window.notifications(cx).len())
    };
    let app = open(cx, memory_storage());
    app.dispatch(cx, actions::AiExplainQuery);
    assert_eq!(notifications(&app, cx), 1, "off: says so");
    // Never sent: the endpoint is a closed port.
    enable(cx, "http://127.0.0.1:9/v1", true);
    app.dispatch(cx, actions::AiWriteSql);
    assert_eq!(notifications(&app, cx), 2, "no query tab: says so");
    assert!(app.titles(cx).is_empty(), "no tab was opened");
    // A query tab active, the keyboard elsewhere in the window.
    app.press(cx, &["secondary-t"]);
    let workspace = app.workspace.clone();
    app.step(cx, |window, cx| {
        workspace.focus_handle(cx).focus(window, cx)
    });
    app.dispatch(cx, actions::AiWriteSql);
    assert!(panel(&app, cx).is_some());
    assert!(app.has(cx, "ai-sentence"));
}
