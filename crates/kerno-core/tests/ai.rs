//! The AI assistant's one sender against a fake OpenAI-compatible endpoint on 127.0.0.1
//! (CLAUDE.md §4 J17, DBeaver #36951, #20072; GOAL-2 criterion 2). No test reaches any other
//! host.

use std::collections::BTreeSet;

use kerno_core::Secret;
use kerno_core::ai::{self, Action, AiError, AiSettings, Call, Context};
use kerno_core::testing::FakeAi;

fn settings(fake: &FakeAi, enabled: bool) -> AiSettings {
    AiSettings {
        enabled,
        endpoint: fake.base(),
        model: "fake-model".into(),
        confirmed: BTreeSet::new(),
        endpoints: Vec::new(),
    }
}

fn payload() -> serde_json::Value {
    let action = Action::WriteSql {
        sentence: "count the orders".into(),
    };
    ai::build_payload(&action, &Context::default(), "fake-model")
}

#[test]
fn nothing_is_sent_while_the_assistant_is_off() {
    let fake = FakeAi::start("SELECT 1");
    let off = settings(&fake, false);
    let key = Secret::new("sk-kerno-test-off");
    assert_eq!(ai::chat(&off, Some(&key), &payload()), Err(AiError::Off));
    assert_eq!(ai::list_models(&off, Some(&key)), Err(AiError::Off));
    assert_eq!(
        ai::request(&off, None, &Call::Chat(payload())),
        Err(AiError::Off)
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(fake.connections(), 0);
    assert!(fake.requests().is_empty());
}

#[test]
fn the_body_is_the_preview_byte_for_byte_and_the_key_goes_in_authorization() {
    let fake = FakeAi::start("Here it is:\n```sql\nSELECT count(*) FROM shop.orders;\n```");
    let on = settings(&fake, true);
    let key = Secret::new("sk-kerno-test-7f3a");
    let payload = payload();
    let answer = ai::chat(&on, Some(&key), &payload).unwrap();
    assert_eq!(ai::sql_block(&answer), "SELECT count(*) FROM shop.orders;");
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(requests[0].body, ai::preview(&payload).into_bytes());
    assert_eq!(requests[0].body, ai::body(&payload));
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer sk-kerno-test-7f3a")
    );
    assert_eq!(requests[0].header("content-type"), Some("application/json"));
    // Without a key, no Authorization header at all.
    ai::chat(&on, None, &payload).unwrap();
    assert_eq!(fake.requests()[1].header("authorization"), None);
    assert_eq!(fake.connections(), 2);
}

#[test]
fn list_models_asks_the_same_endpoint() {
    let fake = FakeAi::start("");
    let on = settings(&fake, true);
    assert_eq!(
        ai::list_models(&on, None).unwrap(),
        ["fake-model", "other-model"]
    );
    assert_eq!(fake.requests()[0].line, "GET /v1/models HTTP/1.1");
}

#[test]
fn a_refused_address_sends_nothing() {
    let fake = FakeAi::start("");
    // The fake's port, behind a host name that is not this computer over plain http.
    let plain = AiSettings {
        endpoint: fake.base().replace("127.0.0.1", "kerno-ai.invalid"),
        ..settings(&fake, true)
    };
    assert!(matches!(
        ai::chat(&plain, None, &payload()),
        Err(AiError::Endpoint(_))
    ));
    let remote = AiSettings {
        endpoint: "https://kerno-ai.invalid/v1".into(),
        ..settings(&fake, true)
    };
    assert_eq!(
        ai::list_models(&remote, None),
        Err(AiError::NotConfirmed("kerno-ai.invalid".into()))
    );
    assert_eq!(fake.connections(), 0);
}

#[test]
fn an_endpoint_that_is_down_is_said_in_plain_words() {
    // A port nothing listens on: bound, then closed.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let down = AiSettings {
        enabled: true,
        endpoint: format!("http://127.0.0.1:{port}/v1"),
        ..AiSettings::default()
    };
    let error = ai::chat(&down, Some(&Secret::new("sk-kerno-down")), &payload()).unwrap_err();
    let text = error.to_string();
    assert!(matches!(error, AiError::Network(_)), "{error:?}");
    assert!(text.contains("127.0.0.1"), "{text}");
    assert!(!text.contains("sk-kerno-down"), "{text}");
}
