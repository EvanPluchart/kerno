//! K23, the owner's request of 2026-11-20: Settings → AI says what the assistant is for, keeps
//! several endpoints (found on this computer, imported from omp or pi, typed), tests them, and
//! picks the model from a list; the panel switches endpoint and model. Every server here is a
//! local test server on 127.0.0.1, every home directory a temporary one: nothing reads the
//! user's files or reaches another host.

use std::collections::HashMap;

use super::ai::{answer, enable, panel, send_and_wait, type_in, write_sql};
use super::*;
use crate::ai_settings_view::AiSources;
use kerno_core::ai::{self, AiSettings, Model, SavedEndpoint};
use kerno_core::testing::FakeAi;

/// Opens Settings → AI.
fn open_ai(app: &Fixture, cx: &mut TestAppContext) {
    app.press(cx, &["secondary-,"]);
    let index = crate::settings_view::SECTIONS
        .iter()
        .position(|(_, label)| *label == "AI")
        .unwrap();
    app.click(cx, ("settings-section", index));
    assert!(app.has(cx, "settings-ai"));
}

/// Where Import and Detect look, for this test.
fn sources(cx: &mut TestAppContext, home: Option<&Path>, ports: Vec<(String, u16)>) {
    let home = home.map(Path::to_path_buf);
    cx.update(|cx| {
        cx.set_global(AiSources {
            home,
            env: Some(HashMap::new()),
            ports,
        })
    });
}

/// The text painted inside the element `id`.
fn text_of(app: &Fixture, cx: &mut TestAppContext, id: impl Into<gpui_kit::ElementId>) -> String {
    let id = id.into();
    app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .iter()
            .filter(|text| text.path().contains(&id))
            .map(|text| text.text().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// A port nothing listens on: bound, then closed.
fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The port of a fake's base address.
fn port_of(fake: &FakeAi) -> u16 {
    fake.base()["http://127.0.0.1:".len()..]
        .trim_end_matches("/v1")
        .parse()
        .unwrap()
}

fn saved(name: &str, base: &str, model: &str, models: &[&str]) -> SavedEndpoint {
    SavedEndpoint {
        name: name.into(),
        base: base.into(),
        model: model.into(),
        models: models.iter().map(|id| Model::new(*id)).collect(),
    }
}

/// Saves `endpoints` (the first one active) with the assistant on.
fn with_endpoints(cx: &mut TestAppContext, endpoints: Vec<SavedEndpoint>) {
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            settings.ai = AiSettings {
                enabled: true,
                ..AiSettings::default()
            };
            for endpoint in endpoints {
                settings.ai.save_endpoint(endpoint);
            }
        })
    });
}

/// K23: the top of the page says what turning the assistant on gives, where it is found and
/// what is sent, before the switch and any field; with no endpoint, how to add one. The search
/// finds the page by the new words.
#[gpui_kit::test]
fn the_page_says_what_the_assistant_is_for_before_any_field(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    open_ai(&app, cx);
    let purpose = text_of(&app, cx, "ai-purpose");
    for words in [
        "writes SQL from a sentence (inserted in the editor, never run by itself)",
        "explains a query or its plan",
        "explains an error with a proposed fix",
        "the editor's right-click menu, the command palette and Query ▸ AI Assistant",
        "names of your schemas, tables and columns with their types",
        "rows only when you tick “Include sample rows”",
    ] {
        assert!(purpose.contains(words), "{words} missing: {purpose}");
    }
    let top = |app: &Fixture, cx: &mut TestAppContext, id: &'static str| {
        app.step(cx, |window, _| window.find(id).bounds().origin.y)
    };
    let purpose_top = top(&app, cx, "ai-purpose");
    assert!(purpose_top < top(&app, cx, "settings-ai-enabled"));
    assert!(purpose_top < top(&app, cx, "ai-no-endpoint"));
    assert!(text_of(&app, cx, "ai-no-endpoint").contains("No endpoint yet: Detect"));
    for query in ["endpoint", "detect", "import", "omp", "litellm"] {
        let found = crate::settings_view::search(query).unwrap();
        assert!(
            found
                .iter()
                .any(|(_, rows)| rows.iter().any(|row| row.key == "settings-ai")),
            "{query}"
        );
    }
}

/// K23: Import lists the OpenAI-compatible providers of omp's file in a home directory it is
/// given, with what it skipped; the ticked ones become endpoints with their models' names;
/// the key goes to the secret store, never to the settings; a key that cannot be resolved is
/// said.
#[gpui_kit::test]
fn import_lists_a_tools_providers_and_keeps_the_key_in_the_secret_store(cx: &mut TestAppContext) {
    const KEY: &str = "sk-kerno-import-7c1f";
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".omp/agent")).unwrap();
    std::fs::write(
        home.path().join(".omp/agent/models.yml"),
        format!(
            "providers:\n  work:\n    name: Work models\n    baseUrl: https://llm.example.lan/v1/\n    \
             apiKey: {KEY}\n    api: openai-completions\n    models:\n      - id: qwen-low\n        \
             name: \"Qwen · low\"\n      - id: glm-4\n  spare:\n    baseUrl: https://spare.example.lan\n    \
             apiKey: SPARE_LLM_KEY\n    api: openai-completions\n  claude:\n    baseUrl: https://x.example.lan\n    \
             api: anthropic-messages\n"
        ),
    )
    .unwrap();
    let app = open(cx, memory_storage());
    sources(cx, Some(home.path()), Vec::new());
    open_ai(&app, cx);
    app.tap(cx, "ai-import");
    app.wait_until(cx, "the providers found", |app, cx| {
        app.has(cx, "ai-import-list")
    });
    let list = text_of(&app, cx, "ai-import-list");
    assert!(
        list.contains("Work models (omp) · https://llm.example.lan/v1 · 2 models · has a key"),
        "{list}"
    );
    assert!(
        list.contains("SPARE_LLM_KEY, which is not set for Kerno"),
        "{list}"
    );
    assert!(list.contains("anthropic-messages"), "{list}");
    assert!(!list.contains(KEY), "{list}");
    app.tap(cx, "ai-import-ticked");
    let settings = settings_now(cx).ai;
    assert_eq!(settings.endpoints.len(), 2);
    assert!(settings.active().is_some());
    let work = (settings.endpoints.iter())
        .find(|saved| saved.base == "https://llm.example.lan/v1")
        .unwrap();
    assert_eq!(work.name, "Work models");
    assert_eq!(work.model, "qwen-low");
    assert_eq!(work.models[0].label.as_deref(), Some("Qwen · low"));
    let name = ai::key_name("https://llm.example.lan/v1");
    app.wait_until(cx, "the key in the secret store", |_, cx| {
        cx.update(|cx| AppState::get(cx).secrets.get(&name).unwrap())
            .is_some_and(|key| key.expose() == KEY)
    });
    assert!(
        cx.update(|cx| AppState::get(cx)
            .secrets
            .get(&ai::key_name("https://spare.example.lan/v1"))
            .unwrap())
            .is_none()
    );
    app.wait_until(cx, "the stored settings", |app, _| {
        app.storage
            .setting::<serde_json::Value>("settings")
            .unwrap()
            .is_some_and(|stored| {
                stored["ai"]["endpoints"]
                    .as_array()
                    .is_some_and(|e| e.len() == 2)
            })
    });
    let stored: serde_json::Value = app.storage.setting("settings").unwrap().unwrap();
    assert!(!stored.to_string().contains(KEY), "{stored}");
    let note = text_of(&app, cx, "ai-key-note");
    assert!(
        note.contains(
            "spare was imported without a key: its key is the environment variable SPARE_LLM_KEY"
        ),
        "{note}"
    );
}

/// K23: Detect asks only the ports it is given, on 127.0.0.1, only once clicked and only while
/// the assistant is on; it lists what answered with its models, and Add saves it.
#[gpui_kit::test]
fn detect_lists_a_server_on_this_computer_and_adds_it(cx: &mut TestAppContext) {
    let fake = FakeAi::start("");
    let app = open(cx, memory_storage());
    sources(
        cx,
        None,
        vec![
            ("Fake server".into(), port_of(&fake)),
            ("Nothing".into(), closed_port()),
        ],
    );
    open_ai(&app, cx);
    // Off: nothing is asked.
    app.tap(cx, "ai-detect");
    app.wait_until(cx, "the refusal", |app, cx| app.has(cx, "ai-detect-error"));
    assert_eq!(fake.connections(), 0);
    enable(cx, "", true);
    app.tap(cx, "ai-detect");
    app.wait_until(cx, "the server found", |app, cx| {
        app.has(cx, ("ai-detected", 0usize))
    });
    assert!(!app.has(cx, ("ai-detected", 1usize)));
    let line = text_of(&app, cx, ("ai-detected", 0usize));
    assert!(line.contains("Fake server · 2 models"), "{line}");
    app.tap(cx, ("ai-detected-add", 0usize));
    let settings = settings_now(cx).ai;
    assert_eq!(settings.endpoint, fake.base());
    assert_eq!(settings.model, "fake-model");
    assert_eq!(
        settings.active().unwrap().name,
        "Fake server on this computer"
    );
    assert_eq!(fake.requests()[0].line, "GET /v1/models HTTP/1.1");
}

/// K23: Test asks `/models` and says the latency and the number of models, or the failure in
/// plain words first: refused, a key refused (401).
#[gpui_kit::test]
fn test_says_the_latency_or_the_failure_in_plain_words(cx: &mut TestAppContext) {
    let fake = FakeAi::start("");
    let closed = closed_port();
    // A server that refuses the key.
    let locked = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let locked_port = locked.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read as _, Write as _};
        for mut stream in locked.incoming().flatten() {
            let mut buffer = [0; 2048];
            let _ = stream.read(&mut buffer);
            let body = r#"{"error":"invalid api key"}"#;
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        }
    });
    let app = open(cx, memory_storage());
    with_endpoints(
        cx,
        vec![
            saved("Fake", &fake.base(), "", &[]),
            saved("Down", &format!("http://127.0.0.1:{closed}/v1"), "", &[]),
            saved(
                "Locked",
                &format!("http://127.0.0.1:{locked_port}/v1"),
                "",
                &[],
            ),
        ],
    );
    open_ai(&app, cx);
    for index in 0..3usize {
        app.tap(cx, ("ai-test", index));
    }
    app.wait_until(cx, "the three tests", |app, cx| {
        (0..3usize).all(|index| app.has(cx, ("ai-tested", index)))
    });
    let fine = text_of(&app, cx, ("ai-tested", 0usize));
    assert!(
        fine.starts_with("Answered in ") && fine.ends_with(" ms · 2 models."),
        "{fine}"
    );
    let down = text_of(&app, cx, ("ai-tested", 1usize));
    assert!(
        down.starts_with(&format!(
            "127.0.0.1 refused the connection on port {closed}: is the server running on that port?"
        )),
        "{down}"
    );
    let locked = text_of(&app, cx, ("ai-tested", 2usize));
    assert!(
        locked.starts_with("The endpoint refused the request (401): check the API key"),
        "{locked}"
    );
    assert!(locked.contains("invalid api key"), "{locked}");
    // Test kept the models of the endpoint for the picker.
    assert_eq!(settings_now(cx).ai.endpoints[0].models.len(), 2);
}

/// K23: the endpoint and model the panel's picker or Settings switch to are where the next
/// request goes, with that endpoint's model.
#[gpui_kit::test]
fn switching_the_active_endpoint_switches_where_the_next_request_goes(cx: &mut TestAppContext) {
    let first = FakeAi::start("first");
    let second = FakeAi::start("second");
    let app = open(cx, memory_storage());
    with_endpoints(
        cx,
        vec![
            saved("First", &first.base(), "fake-model", &["fake-model"]),
            saved(
                "Second",
                &second.base(),
                "other-model",
                &["fake-model", "other-model"],
            ),
        ],
    );
    app.press(cx, &["secondary-t"]);
    write_sql(&app, cx, "count the orders");
    send_and_wait(&app, cx);
    assert_eq!((first.requests().len(), second.requests().len()), (1, 0));
    // The panel's picker: "endpoint · model".
    app.click(cx, "ai-endpoint-picker");
    app.click_label(cx, "Second · other-model");
    assert_eq!(settings_now(cx).ai.endpoint, second.base());
    // K51: the next message of the conversation.
    app.click(cx, "ai-sentence");
    app.type_text(cx, "count them again");
    send_and_wait(&app, cx);
    assert_eq!((first.requests().len(), second.requests().len()), (1, 1));
    let body = String::from_utf8(second.requests()[0].body.clone()).unwrap();
    assert!(body.contains(r#""model": "other-model""#), "{body}");
    assert_eq!(answer(&app, cx).as_deref(), Some("second"));
    // Its models are in the picker too.
    app.click(cx, "ai-endpoint-picker");
    app.click_label(cx, "fake-model");
    assert_eq!(settings_now(cx).ai.model, "fake-model");
    // Settings: Use.
    open_ai(&app, cx);
    app.tap(cx, ("ai-use", 0usize));
    assert_eq!(settings_now(cx).ai.endpoint, first.base());
    app.press(cx, &["escape"]);
    app.click(cx, "ai-sentence");
    app.type_text(cx, "and once more");
    let panel = panel(&app, cx).unwrap();
    app.step(cx, |window, cx| {
        panel.update(cx, |panel, cx| panel.send(window, cx))
    });
    app.wait_until(cx, "the answer", |_, _| first.answered() == 2);
    assert_eq!((first.requests().len(), second.requests().len()), (2, 1));
    // The second endpoint kept the model chosen for it.
    assert_eq!(settings_now(cx).ai.endpoints[1].model, "fake-model");
}

/// K23: the model is picked from the endpoint's list, by its name or its id, filtered by what
/// is typed; a typed id is taken as is; each endpoint keeps its own.
#[gpui_kit::test]
fn the_model_is_picked_from_a_filtered_list_and_kept_per_endpoint(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let mut work = saved("Work models", "https://llm.example.lan/v1", "", &[]);
    work.models = vec![
        Model {
            id: "qwen-low".into(),
            label: Some("Qwen · gpu1 · low".into()),
        },
        Model {
            id: "qwen-high".into(),
            label: Some("Qwen · gpu1 · high".into()),
        },
        Model::new("glm-4"),
    ];
    with_endpoints(
        cx,
        vec![
            work,
            saved(
                "Ollama on this computer",
                "http://localhost:11434/v1",
                "llama3.2",
                &[],
            ),
        ],
    );
    open_ai(&app, cx);
    for index in 0..3usize {
        assert!(app.has(cx, ("ai-model", index)), "{index}");
    }
    assert!(text_of(&app, cx, "ai-model-list").contains("Qwen · gpu1 · high (qwen-high)"));
    type_in(&app, cx, "ai-model", "high");
    // Typed, it filters; it is not the model (real-app pass: "qwen" became the model).
    assert_eq!(settings_now(cx).ai.model, "");
    assert!(app.has(cx, ("ai-model", 1usize)));
    assert!(!app.has(cx, ("ai-model", 0usize)) && !app.has(cx, ("ai-model", 2usize)));
    app.tap(cx, ("ai-model", 1usize));
    assert_eq!(settings_now(cx).ai.model, "qwen-high");
    // Another endpoint, then back: each keeps its model.
    app.tap(cx, ("ai-use", 1usize));
    assert_eq!(settings_now(cx).ai.model, "llama3.2");
    app.tap(cx, ("ai-use", 0usize));
    assert_eq!(settings_now(cx).ai.model, "qwen-high");
    // A model the list does not hold, typed: the model once Enter confirms it.
    type_in(&app, cx, "ai-model", "deepseek-r1-local");
    assert_eq!(settings_now(cx).ai.model, "qwen-high");
    app.press(cx, &["enter"]);
    assert_eq!(settings_now(cx).ai.model, "deepseek-r1-local");
    assert_eq!(settings_now(cx).ai.endpoints[0].model, "deepseek-r1-local");
}

/// Real-app pass: "Add by hand" with an address already saved replaced that endpoint ("Fake
/// local" became "Fake two") without a word. Add refuses and names the endpoint; changing it
/// is the user's choice.
#[gpui_kit::test]
fn adding_an_address_already_saved_names_its_endpoint_first(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    with_endpoints(
        cx,
        vec![saved("Fake local", "http://127.0.0.1:9/v1", "", &[])],
    );
    open_ai(&app, cx);
    app.tap(cx, "ai-add");
    type_in(&app, cx, "ai-add-name", "Fake two");
    type_in(&app, cx, "ai-add-address", "http://127.0.0.1:9");
    app.tap(cx, "ai-add-save");
    let names = |cx: &mut TestAppContext| {
        (settings_now(cx).ai.endpoints.iter())
            .map(|saved| saved.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(cx), ["Fake local"]);
    assert!(
        text_of(&app, cx, "ai-add-refused").contains("“Fake local” already has this address"),
        "{}",
        text_of(&app, cx, "ai-add-refused")
    );
    app.tap(cx, "ai-add-change");
    assert_eq!(names(cx), ["Fake two"]);
    assert!(!app.has(cx, "ai-add-refused"));
}

/// K23: the settings an earlier Kerno stored (one address, its model) open as one saved
/// endpoint, active, with its model.
#[gpui_kit::test]
fn the_settings_of_an_earlier_kerno_open_as_one_endpoint(cx: &mut TestAppContext) {
    let storage = memory_storage();
    storage
        .set_setting(
            "settings",
            &serde_json::json!({"ai": {"enabled": true, "endpoint": "http://localhost:11434/v1/",
                "model": "llama3.2", "confirmed": []}}),
        )
        .unwrap();
    let app = open(cx, storage);
    let settings = settings_now(cx).ai;
    assert_eq!(settings.endpoints.len(), 1);
    assert_eq!(settings.endpoint, "http://localhost:11434/v1");
    let saved = settings.active().unwrap();
    assert_eq!(
        (saved.name.as_str(), saved.model.as_str()),
        ("Ollama on this computer", "llama3.2")
    );
    open_ai(&app, cx);
    let row = text_of(&app, cx, ("ai-endpoint-row", 0usize));
    assert!(
        row.contains("Ollama on this computer · llama3.2")
            && row.contains("http://localhost:11434/v1 · this computer"),
        "{row}"
    );
}

/// K23: an answer whose model thought aloud (`<think>…</think>`, with SQL in it) shows and
/// inserts only the answer's SQL.
#[gpui_kit::test]
fn an_answer_with_reasoning_inserts_only_its_sql(cx: &mut TestAppContext) {
    let fake = FakeAi::start(
        "<think>Maybe:\n```sql\nDELETE FROM shop.customers;\n```\nNo.</think>\nHere:\n```sql\nSELECT 1 AS kerno_ui_ai_reasoned;\n```",
    );
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    enable(cx, &fake.base(), true);
    write_sql(&app, cx, "anything");
    send_and_wait(&app, cx);
    let shown = answer(&app, cx).unwrap();
    assert!(
        !shown.contains("DELETE") && !shown.contains("think"),
        "{shown}"
    );
    let tab = app.tab(cx);
    let editor = cx.update(|cx| tab.read(cx).editor().clone());
    app.step(cx, |window, cx| {
        editor.update(cx, |editor, cx| editor.set_value("", window, cx))
    });
    app.click(cx, "ai-insert");
    assert_eq!(app.active_query(cx).1, "SELECT 1 AS kerno_ui_ai_reasoned;");
}
