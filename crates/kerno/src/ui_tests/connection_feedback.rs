//! What a connection tells the user (CLAUDE.md §3, owner's test of
//! 2026-11-13, items 5, 8 and 9): failures in plain words with the raw
//! message below, the Database field's list of the server's databases, and
//! each secret read from the store once per run.

use std::collections::HashMap;
use std::sync::Mutex;

use kerno_core::config::Secret;
use kerno_core::secrets::{CachedSecretStore, SecretStore};

use super::*;

/// The text painted in the window's last frame.
fn painted(app: &Fixture, cx: &mut TestAppContext) -> Vec<String> {
    app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .into_iter()
            .map(|text| text.text().to_string())
            .collect()
    })
}

fn paints(app: &Fixture, cx: &mut TestAppContext, part: &str) -> bool {
    painted(app, cx).iter().any(|text| text.contains(part))
}

/// A URL for the test server of `engine` as user `kerno`, without TLS.
fn url(engine: Engine, password: &str, host: &str, port: u16, database: &str) -> String {
    match engine {
        Engine::Postgres => {
            format!("postgres://kerno:{password}@{host}:{port}/{database}?sslmode=disable")
        }
        Engine::MySql | Engine::MariaDb => {
            format!("mysql://kerno:{password}@{host}:{port}/{database}?ssl-mode=DISABLED")
        }
        kerno_core::Engine::Sqlite => {
            unreachable!("SQLite has no test server: these tests run on the server engines")
        }
    }
}

/// Opens the new-connection dialog filled from `url`.
fn dialog_from(app: &Fixture, cx: &mut TestAppContext, url: &str) {
    app.click(cx, "empty-new-connection");
    app.click(cx, "conn-url");
    app.type_text(cx, url);
    app.press(cx, &["enter"]);
}

#[gpui_kit::test]
fn a_refused_connection_says_so_in_plain_words(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = open(cx, memory_storage());
    let port = testing::port(Engine::Postgres) + 1;
    dialog_from(
        &app,
        cx,
        &url(
            Engine::Postgres,
            testing::DB_PASSWORD,
            "127.0.0.1",
            port,
            "kerno",
        ),
    );
    app.click(cx, "conn-test");
    // The system's own words, shown below Kerno's: Windows says them its way (K68).
    let (refused, lookup) = if cfg!(windows) {
        ("actively refused", "No such host is known")
    } else {
        ("Connection refused", "lookup")
    };
    app.wait_until(cx, "the failed test", |app, cx| {
        app.has(cx, "conn-test-failed") && paints(app, cx, refused)
    });
    // The plain words first, the raw message of the driver below.
    assert!(paints(
        &app,
        cx,
        &format!("Nothing answers on 127.0.0.1:{port}.")
    ));
    assert!(paints(
        &app,
        cx,
        "Check that the server is running and listens on this port."
    ));

    // A second failure replaces both parts, raw message included.
    app.click(cx, "conn-url");
    app.type_text(
        cx,
        &url(
            Engine::Postgres,
            testing::DB_PASSWORD,
            "kerno-no-such-host.invalid",
            port,
            "kerno",
        ),
    );
    app.press(cx, &["enter"]);
    app.click(cx, "conn-test");
    app.wait_until(cx, "the second failed test", |app, cx| {
        paints(app, cx, "Host “kerno-no-such-host.invalid” was not found.")
    });
    assert!(paints(
        &app,
        cx,
        "Check the host name; for a server on this computer, use localhost."
    ));
    assert!(paints(&app, cx, lookup), "{:?}", painted(&app, cx));
    assert!(!paints(&app, cx, refused));
}

#[gpui_kit::test]
fn the_database_field_lists_the_servers_databases(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let app = open(cx, memory_storage());
        // A mistyped database: the list is what rescues it.
        dialog_from(
            &app,
            cx,
            &url(
                engine,
                testing::DB_PASSWORD,
                "127.0.0.1",
                testing::port(engine),
                "kerno_ui_no_such_database",
            ),
        );
        // Nothing is sent to the server until the list is asked for.
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        for id in [
            "conn-database-list",
            "conn-database-loading",
            "conn-database-failed",
        ] {
            assert!(!app.has(cx, id), "{engine:?}: {id}");
        }
        app.click(cx, "conn-database-load");
        app.wait_until(cx, "the list of databases", |app, cx| {
            app.has(cx, "conn-database-list")
        });
        assert!(!app.has(cx, "conn-database-failed"));
        app.click(cx, "conn-database-list");
        assert!(app.shows_label(cx, "kerno"), "{engine:?}");
        app.click_label(cx, "kerno");

        // The field took the name, and it is what is saved.
        app.click(cx, "conn-name");
        app.type_text(cx, "picked");
        app.click(cx, "conn-save");
        app.wait_until(cx, "the saved connection", |app, _| {
            app.storage.connections().unwrap().len() == 1
        });
        assert_eq!(app.first_stored().database.as_deref(), Some("kerno"));
    }
}

#[gpui_kit::test]
fn a_database_list_that_fails_leaves_the_field_usable(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = open(cx, memory_storage());
    dialog_from(
        &app,
        cx,
        &url(
            Engine::Postgres,
            "not-the-password",
            "127.0.0.1",
            testing::port(Engine::Postgres),
            "kerno",
        ),
    );
    app.click(cx, "conn-database-load");
    app.wait_until(cx, "the failed list", |app, cx| {
        app.has(cx, "conn-database-failed")
    });
    // In plain words, under the field, never a dialog of its own.
    assert!(paints(
        &app,
        cx,
        "The databases could not be listed. The server refused the login of user “kerno”."
    ));
    assert!(app.has(cx, "conn-save"));

    // The field is still free text.
    app.click(cx, "conn-database");
    app.type_text(cx, "_typed");
    app.click(cx, "conn-name");
    app.type_text(cx, "typed");
    app.click(cx, "conn-save");
    app.wait_until(cx, "the saved connection", |app, _| {
        app.storage.connections().unwrap().len() == 1
    });
    assert_eq!(app.first_stored().database.as_deref(), Some("kerno_typed"));
}

/// A store that counts the reads reaching it.
#[derive(Clone, Default)]
struct Counting {
    store: Arc<MemorySecretStore>,
    reads: Arc<Mutex<HashMap<String, usize>>>,
}

impl SecretStore for Counting {
    fn get(&self, key: &str) -> kerno_core::Result<Option<Secret>> {
        *self.reads.lock().unwrap().entry(key.into()).or_default() += 1;
        self.store.get(key)
    }
    fn set(&self, key: &str, secret: &Secret) -> kerno_core::Result<()> {
        self.store.set(key, secret)
    }
    fn delete(&self, key: &str) -> kerno_core::Result<()> {
        self.store.delete(key)
    }
    fn is_persistent(&self) -> bool {
        true
    }
}

#[gpui_kit::test]
async fn each_secret_is_read_once_per_run(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let storage = memory_storage();
    let id = storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let app = open(cx, storage);
    // The secrets are in the store from an earlier run; this run reads
    // them through the cache the app puts over the system store.
    let counting = Counting::default();
    kerno_core::secrets::save_secrets(&counting, &id, &testing::secrets()).unwrap();
    let cached: Arc<dyn SecretStore> = Arc::new(CachedSecretStore::new(counting.clone()));
    cx.update(|cx| cx.global_mut::<AppState>().secrets = cached);

    let secrets_of =
        |cx: &mut TestAppContext| cx.update(|cx| app.connections.read(cx).secrets_of(&id, cx));
    // Edit, then back up, before the connection is opened: each reads them.
    let edited = secrets_of(cx).await.unwrap();
    assert_eq!(edited, testing::secrets());
    secrets_of(cx).await.unwrap();
    // Connect, disconnect, connect again.
    for _ in 0..2 {
        cx.update(|cx| {
            app.connections
                .update(cx, |connections, cx| connections.connect(&id, cx))
        });
        app.wait_until(cx, "the connection", |app, cx| {
            matches!(app.status(cx, 0), Status::Connected { .. })
        });
        cx.update(|cx| {
            app.connections
                .update(cx, |connections, cx| connections.disconnect(&id, cx))
        });
    }
    // A test from the dialog uses the form's secrets, not the store.
    let config = testing::config(Engine::Postgres);
    cx.update(|cx| {
        app.connections
            .read(cx)
            .test(config, testing::secrets(), None, false, cx)
    })
    .await
    .unwrap();

    let reads = counting.reads.lock().unwrap().clone();
    assert_eq!(reads.len(), 3, "{reads:?}");
    assert!(reads.values().all(|count| *count == 1), "{reads:?}");
}

#[gpui_kit::test]
fn a_postgres_user_without_a_database_of_its_name_lists_through_postgres(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    // A login with no database named after it: PostgreSQL refuses a session
    // without a database name, so the list is read from `postgres`.
    const ROLE: &str = "kerno_ui_list_nodb";
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[&format!("DROP ROLE IF EXISTS {ROLE}")],
        &[&format!("CREATE ROLE {ROLE} LOGIN PASSWORD '{ROLE}_pw'")],
    );
    let app = open(cx, memory_storage());
    dialog_from(
        &app,
        cx,
        &format!(
            "postgres://{ROLE}:{ROLE}_pw@127.0.0.1:{}/?sslmode=disable",
            testing::port(Engine::Postgres)
        ),
    );
    app.click(cx, "conn-database-load");
    app.wait_until(cx, "the list of databases", |app, cx| {
        assert!(
            !app.has(cx, "conn-database-failed"),
            "{:?}",
            painted(app, cx)
        );
        app.has(cx, "conn-database-list")
    });
    app.click(cx, "conn-database-list");
    assert!(app.shows_label(cx, "kerno"));
    app.press(cx, &["escape"]);
}
