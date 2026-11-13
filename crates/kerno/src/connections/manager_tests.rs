//! The connection manager against the real test containers: saving,
//! connecting with and without stored secrets, SSH prompts, reconnecting.
//! Tests that need a container print `SKIPPED` and return when it is down.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::{AppContext as _, Entity, TestAppContext};
use kerno_core::config::{ConnectionConfig, Engine, Secret, Secrets, SshAuth};
use kerno_core::secrets::{MemorySecretStore, SecretStore, load_secrets};
use kerno_core::storage::{AppPaths, Storage};
use kerno_core::{exec, testing};
use kerno_postgres::PostgresDriver;

use super::manager::{
    Connections, ConnectionsEvent, Drivers, InputAnswer, InputRequest, Status, TestReport,
};
use crate::bridge;
use crate::settings::Settings;
use crate::state::AppState;

struct Fixture {
    connections: Entity<Connections>,
    events: Rc<RefCell<Vec<ConnectionsEvent>>>,
    storage: Arc<Storage>,
    secrets: Arc<MemorySecretStore>,
    _dir: tempfile::TempDir,
    _subscription: gpui_kit::Subscription,
}

fn fixture(cx: &mut TestAppContext) -> Fixture {
    cx.executor().allow_parking();
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::in_memory().unwrap());
    let secrets = Arc::new(MemorySecretStore::default());
    let events = Rc::new(RefCell::new(Vec::new()));
    let (connections, subscription) = cx.update(|cx| {
        bridge::init(cx).unwrap();
        cx.set_global(AppState {
            paths: AppPaths::under(dir.path().to_path_buf()),
            storage: storage.clone(),
            secrets: secrets.clone(),
            settings: Settings::default(),
            appearance_override: None,
        });
        // No user known-hosts file: only the scratch one in the data dir.
        let connections =
            cx.new(|_| Connections::new(Vec::new(), Drivers::default().with(PostgresDriver), None));
        let seen = events.clone();
        let subscription = cx.subscribe(&connections, move |_, event: &ConnectionsEvent, _| {
            seen.borrow_mut().push(event.clone());
        });
        (connections, subscription)
    });
    Fixture {
        connections,
        events,
        storage,
        secrets,
        _dir: dir,
        _subscription: subscription,
    }
}

/// Lets real threads (network, storage) make progress until `done`.
fn wait_until(
    cx: &mut TestAppContext,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl Fixture {
    fn status(&self, cx: &mut TestAppContext, id: &str) -> Status {
        cx.update(|cx| self.connections.read(cx).status(id))
    }

    /// Saves a connection and returns its id.
    fn save(&self, cx: &mut TestAppContext, config: ConnectionConfig, secrets: Secrets) -> String {
        let before = cx.update(|cx| self.connections.read(cx).saved().len());
        cx.update(|cx| {
            self.connections
                .update(cx, |c, cx| c.save(None, config, secrets, None, false, cx))
        });
        wait_until(cx, "the connection to be saved", |cx| {
            cx.update(|cx| self.connections.read(cx).saved().len()) > before
        });
        cx.update(|cx| self.connections.read(cx).saved().last().unwrap().id.clone())
    }

    fn connect(&self, cx: &mut TestAppContext, id: &str) {
        cx.update(|cx| self.connections.update(cx, |c, cx| c.connect(id, cx)));
    }

    fn answer(&self, cx: &mut TestAppContext, id: &str, answer: InputAnswer) {
        cx.update(|cx| {
            self.connections
                .update(cx, |c, cx| c.answer(id, answer, cx))
        });
    }

    /// Waits until the connection leaves `Connecting`, and returns where it landed.
    fn settle(&self, cx: &mut TestAppContext, id: &str) -> Status {
        wait_until(cx, "the connection attempt to finish", |cx| {
            self.status(cx, id) != Status::Connecting
        });
        self.status(cx, id)
    }

    /// The last thing the manager asked the user for.
    fn last_request(&self) -> Option<InputRequest> {
        self.events
            .borrow()
            .iter()
            .rev()
            .find_map(|event| match event {
                ConnectionsEvent::NeedsInput { request, .. } => Some(request.clone()),
                _ => None,
            })
    }

    /// Runs a query on a new session of the connection.
    fn scalar(&self, cx: &mut TestAppContext, id: &str, sql: &str) -> String {
        let opening = cx.update(|cx| self.connections.read(cx).open_session(id, None, cx));
        let result = Rc::new(RefCell::new(None));
        let slot = result.clone();
        let sql = sql.to_string();
        cx.update(|cx| {
            let query = bridge::spawn(cx, async move {
                let session = opening.await?;
                let collected = exec::collect(session, sql).await?;
                Ok::<_, kerno_core::Error>(
                    collected.results[0].rows()[0][0]
                        .clone()
                        .unwrap_or_default(),
                )
            });
            cx.spawn(async move |_| *slot.borrow_mut() = Some(query.await))
                .detach();
        });
        wait_until(cx, "the query", |_| result.borrow().is_some());
        result.take().unwrap().expect("query on a new session")
    }

    /// Runs a query on the connection's own (navigator) session.
    fn scalar_on_meta(
        &self,
        cx: &mut TestAppContext,
        id: &str,
        sql: &str,
    ) -> kerno_core::Result<String> {
        let session = cx
            .update(|cx| self.connections.read(cx).meta_session(id))
            .expect("the connection is open");
        let result = Rc::new(RefCell::new(None));
        let slot = result.clone();
        let sql = sql.to_string();
        cx.update(|cx| {
            let query = bridge::spawn(cx, async move {
                let collected = exec::collect(session, sql).await?;
                Ok::<_, kerno_core::Error>(
                    collected.results[0].rows()[0][0]
                        .clone()
                        .unwrap_or_default(),
                )
            });
            cx.spawn(async move |_| *slot.borrow_mut() = Some(query.await))
                .detach();
        });
        wait_until(cx, "the query", |_| result.borrow().is_some());
        result.take().unwrap()
    }
}

fn pg() -> ConnectionConfig {
    let mut config = testing::config(Engine::Postgres);
    config.name = "Test PostgreSQL".into();
    config
}

fn db_password() -> Secrets {
    Secrets {
        password: Some(Secret::new(testing::DB_PASSWORD)),
        ..Secrets::default()
    }
}

fn connected(status: &Status) -> bool {
    matches!(status, Status::Connected { version } if version.starts_with("PostgreSQL 17"))
}

#[gpui_kit::test]
fn saving_stores_settings_and_secrets_separately(cx: &mut TestAppContext) {
    let app = fixture(cx);
    let id = app.save(cx, pg(), db_password());

    let stored = app.storage.connections().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, id);
    assert_eq!(stored[0].config.name, "Test PostgreSQL");
    assert_eq!(
        load_secrets(app.secrets.as_ref(), &id).unwrap(),
        db_password()
    );
    assert!(app.events.borrow().contains(&ConnectionsEvent::ListChanged));

    // "Ask each time": nothing goes to the secret store, and what was
    // stored before is removed.
    let mut ask = pg();
    ask.save_password = false;
    cx.update(|cx| {
        app.connections.update(cx, |c, cx| {
            c.save(Some(id.clone()), ask, db_password(), None, false, cx)
        })
    });
    wait_until(cx, "the stored password to be removed", |_| {
        load_secrets(app.secrets.as_ref(), &id).unwrap() == Secrets::default()
    });
    assert_eq!(
        app.storage.connections().unwrap().len(),
        1,
        "an update, not a second connection"
    );
    assert!(!app.storage.connections().unwrap()[0].config.save_password);
}

#[gpui_kit::test]
fn connects_queries_and_disconnects(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = fixture(cx);
    let id = app.save(cx, pg(), db_password());
    assert_eq!(app.status(cx, &id), Status::Disconnected);

    app.connect(cx, &id);
    assert_eq!(app.status(cx, &id), Status::Connecting);
    let status = app.settle(cx, &id);
    assert!(connected(&status), "{status:?}");
    assert_eq!(
        app.scalar(cx, &id, "SELECT count(*) FROM shop.categories"),
        "10"
    );
    assert!(
        cx.update(|cx| app.connections.read(cx).meta_session(&id))
            .is_some()
    );
    // The password came from the secret store, not from a prompt.
    assert_eq!(app.last_request(), None);

    cx.update(|cx| app.connections.update(cx, |c, cx| c.disconnect(&id, cx)));
    assert_eq!(app.status(cx, &id), Status::Disconnected);
    assert!(
        cx.update(|cx| app.connections.read(cx).meta_session(&id))
            .is_none()
    );
}

#[gpui_kit::test]
fn a_failed_connection_says_why(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = fixture(cx);
    let wrong = Secrets {
        password: Some(Secret::new("not the password")),
        ..Secrets::default()
    };
    let id = app.save(cx, pg(), wrong);
    app.connect(cx, &id);
    match app.settle(cx, &id) {
        Status::Failed { message } => {
            assert!(
                message.contains("password authentication failed"),
                "{message}"
            );
            assert!(!message.contains("not the password"), "{message}");
        }
        other => panic!("expected a failure, got {other:?}"),
    }

    // An engine this build has no driver for.
    let id = app.save(cx, testing::config(Engine::MySql), db_password());
    app.connect(cx, &id);
    match app.settle(cx, &id) {
        Status::Failed { message } => assert!(message.contains("no driver for MySQL"), "{message}"),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[gpui_kit::test]
fn an_unsaved_password_is_asked_for(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = fixture(cx);
    let mut config = pg();
    config.save_password = false;
    let id = app.save(cx, config, Secrets::default());

    app.connect(cx, &id);
    assert_eq!(app.settle(cx, &id), Status::WaitingForInput);
    assert_eq!(app.last_request(), Some(InputRequest::DatabasePassword));

    // Giving up leaves the connection closed.
    cx.update(|cx| app.connections.update(cx, |c, cx| c.cancel_input(&id, cx)));
    assert_eq!(app.status(cx, &id), Status::Disconnected);

    app.connect(cx, &id);
    assert_eq!(app.settle(cx, &id), Status::WaitingForInput);
    app.answer(
        cx,
        &id,
        InputAnswer::DatabasePassword(Secret::new(testing::DB_PASSWORD)),
    );
    let status = app.settle(cx, &id);
    assert!(connected(&status), "{status:?}");
    // Still nothing in the secret store.
    assert_eq!(
        load_secrets(app.secrets.as_ref(), &id).unwrap(),
        Secrets::default()
    );
}

#[gpui_kit::test]
fn a_saved_connection_without_its_password_asks_and_then_keeps_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = fixture(cx);
    // "Save password" is on, but the store has none: an imported connection.
    let id = app.save(cx, pg(), Secrets::default());

    app.connect(cx, &id);
    assert_eq!(app.settle(cx, &id), Status::WaitingForInput);
    assert_eq!(app.last_request(), Some(InputRequest::DatabasePassword));

    // A wrong password fails, and is not what the next attempt uses.
    app.answer(
        cx,
        &id,
        InputAnswer::DatabasePassword(Secret::new("not the password")),
    );
    let status = app.settle(cx, &id);
    assert!(
        matches!(&status, Status::Failed { message } if message.contains("28P01")),
        "{status:?}"
    );
    assert_eq!(
        load_secrets(app.secrets.as_ref(), &id).unwrap(),
        Secrets::default()
    );

    app.connect(cx, &id);
    assert_eq!(app.settle(cx, &id), Status::WaitingForInput);
    app.answer(
        cx,
        &id,
        InputAnswer::DatabasePassword(Secret::new(testing::DB_PASSWORD)),
    );
    let status = app.settle(cx, &id);
    assert!(connected(&status), "{status:?}");
    // The password that worked is saved, as the connection asks.
    wait_until(cx, "the password to be saved", |_| {
        load_secrets(app.secrets.as_ref(), &id).unwrap() == db_password()
    });
}

fn through_bastion(auth: SshAuth) -> ConnectionConfig {
    let mut config = pg();
    config.host = testing::service_name(Engine::Postgres).into();
    config.port = 5432;
    config.ssh = Some(testing::ssh(auth));
    config
}

#[gpui_kit::test]
fn an_unknown_ssh_host_is_shown_before_it_is_trusted(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::ssh_available() {
        return;
    }
    let app = fixture(cx);
    let secrets = Secrets {
        ssh_password: Some(Secret::new(testing::SSH_PASSWORD)),
        ..db_password()
    };
    let id = app.save(cx, through_bastion(SshAuth::Password), secrets);

    app.connect(cx, &id);
    assert_eq!(app.settle(cx, &id), Status::WaitingForInput);
    let fingerprint = match app.last_request() {
        Some(InputRequest::TrustHostKey {
            host,
            port,
            fingerprint,
            ..
        }) => {
            assert_eq!((host.as_str(), port), ("127.0.0.1", testing::SSH_PORT));
            assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
            fingerprint
        }
        other => panic!("expected a host key question, got {other:?}"),
    };
    let known_hosts = cx.update(|cx| AppState::get(cx).paths.known_hosts());
    assert!(
        !known_hosts.exists(),
        "nothing is recorded before the user agrees"
    );

    app.answer(cx, &id, InputAnswer::TrustHostKey(fingerprint));
    let status = app.settle(cx, &id);
    assert!(connected(&status), "{status:?}");
    assert_ne!(
        app.scalar(cx, &id, "SELECT host(inet_client_addr())"),
        "127.0.0.1"
    );
    assert!(
        std::fs::read_to_string(&known_hosts)
            .unwrap()
            .contains("127.0.0.1")
    );

    // Known from now on: reconnecting asks nothing.
    cx.update(|cx| app.connections.update(cx, |c, cx| c.disconnect(&id, cx)));
    app.events.borrow_mut().clear();
    app.connect(cx, &id);
    let status = app.settle(cx, &id);
    assert!(connected(&status), "{status:?}");
    assert_eq!(app.last_request(), None);
}

#[gpui_kit::test]
fn an_encrypted_ssh_key_asks_for_its_passphrase(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::ssh_available() {
        return;
    }
    let app = fixture(cx);
    let key = testing::generated("ssh/id_ed25519_enc");
    let id = app.save(
        cx,
        through_bastion(SshAuth::KeyFile { path: key }),
        db_password(),
    );

    app.connect(cx, &id);
    assert_eq!(app.settle(cx, &id), Status::WaitingForInput);
    let Some(InputRequest::TrustHostKey { fingerprint, .. }) = app.last_request() else {
        panic!(
            "expected the host key question first, got {:?}",
            app.last_request()
        );
    };
    app.answer(cx, &id, InputAnswer::TrustHostKey(fingerprint));

    assert_eq!(app.settle(cx, &id), Status::WaitingForInput);
    assert!(
        matches!(
            app.last_request(),
            Some(InputRequest::SshKeyPassphrase { retry: false, .. })
        ),
        "{:?}",
        app.last_request()
    );
    app.answer(
        cx,
        &id,
        InputAnswer::SshKeyPassphrase(Secret::new("not the passphrase")),
    );
    assert_eq!(app.settle(cx, &id), Status::WaitingForInput);
    assert!(
        matches!(
            app.last_request(),
            Some(InputRequest::SshKeyPassphrase { retry: true, .. })
        ),
        "{:?}",
        app.last_request()
    );
    app.answer(
        cx,
        &id,
        InputAnswer::SshKeyPassphrase(Secret::new(testing::SSH_KEY_PASSPHRASE)),
    );
    let status = app.settle(cx, &id);
    assert!(connected(&status), "{status:?}");
}

#[gpui_kit::test]
fn test_connection_reports_version_and_latency(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = fixture(cx);
    let run = |cx: &mut TestAppContext, config: ConnectionConfig, secrets: Secrets| {
        let outcome: Rc<RefCell<Option<kerno_core::Result<TestReport>>>> =
            Rc::new(RefCell::new(None));
        let slot = outcome.clone();
        cx.update(|cx| {
            let test = app.connections.read(cx).test(config, secrets, None, cx);
            cx.spawn(async move |_| *slot.borrow_mut() = Some(test.await))
                .detach();
        });
        wait_until(cx, "the connection test", |_| outcome.borrow().is_some());
        outcome.take().unwrap()
    };

    let report = run(cx, pg(), db_password()).expect("the test connection");
    assert!(
        report.version.starts_with("PostgreSQL 17"),
        "{}",
        report.version
    );
    assert!(report.latency > Duration::ZERO && report.latency < Duration::from_secs(5));

    let mut unreachable = pg();
    unreachable.port = 1;
    let error = run(cx, unreachable, db_password()).unwrap_err();
    assert!(error.to_string().contains("could not connect"), "{error}");

    // Testing saves nothing.
    assert!(app.storage.connections().unwrap().is_empty());
    assert!(cx.update(|cx| app.connections.read(cx).saved().is_empty()));
}

#[gpui_kit::test]
fn duplicate_import_and_delete(cx: &mut TestAppContext) {
    let app = fixture(cx);
    let id = app.save(cx, pg(), db_password());

    cx.update(|cx| app.connections.update(cx, |c, cx| c.duplicate(&id, cx)));
    wait_until(cx, "the copy", |cx| {
        cx.update(|cx| app.connections.read(cx).saved().len()) == 2
    });
    let copy = cx.update(|cx| app.connections.read(cx).saved()[1].clone());
    assert_eq!(copy.config.name, "Test PostgreSQL copy");
    assert_ne!(copy.id, id);
    assert_eq!(
        load_secrets(app.secrets.as_ref(), &copy.id).unwrap(),
        db_password()
    );

    let mut imported = testing::config(Engine::Postgres);
    imported.name = "Imported".into();
    cx.update(|cx| {
        app.connections
            .update(cx, |c, cx| c.import(vec![imported], cx))
    });
    wait_until(cx, "the import", |cx| {
        cx.update(|cx| app.connections.read(cx).saved().len()) == 3
    });
    let imported_id = cx.update(|cx| app.connections.read(cx).saved()[2].id.clone());
    assert_eq!(
        load_secrets(app.secrets.as_ref(), &imported_id).unwrap(),
        Secrets::default()
    );

    cx.update(|cx| app.connections.update(cx, |c, cx| c.delete(&id, cx)));
    wait_until(cx, "the deletion", |cx| {
        cx.update(|cx| app.connections.read(cx).saved().len()) == 2
    });
    assert_eq!(app.storage.connections().unwrap().len(), 2);
    assert_eq!(
        load_secrets(app.secrets.as_ref(), &id).unwrap(),
        Secrets::default()
    );
    assert!(
        app.secrets
            .get(&format!("connection/{id}/password"))
            .unwrap()
            .is_none()
    );
    assert_eq!(app.status(cx, &id), Status::Disconnected);
}

#[gpui_kit::test]
fn a_lost_connection_is_re_established(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = fixture(cx);
    let id = app.save(cx, pg(), db_password());
    app.connect(cx, &id);
    assert!(connected(&app.settle(cx, &id)));
    let before = app
        .scalar_on_meta(cx, &id, "SELECT pg_backend_pid()")
        .unwrap();

    // The server drops exactly this connection's session (other tests run
    // against the same server at the same time).
    let killed = app.scalar(
        cx,
        &id,
        &format!("SELECT pg_terminate_backend({before})::text"),
    );
    assert_eq!(killed, "true");
    wait_until(cx, "the session to notice it was dropped", |cx| {
        app.scalar_on_meta(cx, &id, "SELECT 1").is_err()
    });

    // A tab would report the loss when its statement fails.
    cx.update(|cx| {
        app.connections
            .update(cx, |c, cx| c.connection_lost(&id, cx))
    });
    let status = app.settle(cx, &id);
    assert!(connected(&status), "{status:?}");
    // The connection has a new, working session.
    let after = app
        .scalar_on_meta(cx, &id, "SELECT pg_backend_pid()")
        .unwrap();
    assert_ne!(before, after);
    // No prompt: the secrets of this run were reused.
    assert_eq!(app.last_request(), None);
}

#[gpui_kit::test]
fn folders_are_made_renamed_and_removed_with_what_they_hold(cx: &mut TestAppContext) {
    let app = fixture(cx);
    let in_folder = |name: &str, folder: &str| {
        let mut config = pg();
        config.name = name.into();
        config.group = Some(folder.into());
        config
    };
    let a = app.save(cx, in_folder("A", "Work"), db_password());
    let b = app.save(cx, in_folder("B", "Work/Clients"), db_password());
    let c = app.save(cx, in_folder("C", "Workshop"), db_password());
    let folders = |cx: &mut TestAppContext| cx.update(|cx| app.connections.read(cx).folders(cx));
    let group = |cx: &mut TestAppContext, id: &str| {
        cx.update(|cx| {
            app.connections
                .read(cx)
                .get(id)
                .unwrap()
                .config
                .group
                .clone()
        })
    };
    let stored = |id: &str| {
        app.storage
            .connections()
            .unwrap()
            .into_iter()
            .find(|saved| saved.id == id)
            .unwrap()
            .config
    };
    assert_eq!(folders(cx), ["Work", "Work/Clients", "Workshop"]);

    // A folder without connections is kept, in the stored settings too.
    let made = cx.update(|cx| {
        app.connections
            .update(cx, |c, cx| c.create_folder(Some("Work"), " Archive ", cx))
    });
    assert_eq!(made, Ok("Work/Archive".to_string()));
    for (parent, name, problem) in [
        (None, "Work", "already a folder"),
        (None, "a/b", "“/”"),
        (Some("Work"), "  ", "needs a name"),
    ] {
        let refused = cx.update(|cx| {
            app.connections
                .update(cx, |c, cx| c.create_folder(parent, name, cx))
        });
        assert!(refused.unwrap_err().contains(problem), "{name}");
    }
    assert_eq!(
        folders(cx),
        ["Work", "Work/Archive", "Work/Clients", "Workshop"]
    );
    wait_until(cx, "the empty folder to be stored", |_| {
        Settings::load(&app.storage)
            .connection_folders
            .contains("Work/Archive")
    });

    // Renaming rewrites what is inside, at any depth, and nothing that only
    // starts with the same letters.
    let renamed = cx.update(|cx| {
        app.connections
            .update(cx, |c, cx| c.rename_folder("Work", "Job", cx))
    });
    assert_eq!(renamed, Ok(()));
    assert_eq!(
        folders(cx),
        ["Job", "Job/Archive", "Job/Clients", "Workshop"]
    );
    assert_eq!(group(cx, &a).as_deref(), Some("Job"));
    assert_eq!(group(cx, &b).as_deref(), Some("Job/Clients"));
    assert_eq!(group(cx, &c).as_deref(), Some("Workshop"));
    let clash = cx.update(|cx| {
        app.connections
            .update(cx, |c, cx| c.rename_folder("Job/Clients", "Archive", cx))
    });
    assert!(clash.unwrap_err().contains("already a folder"));
    wait_until(cx, "the renamed folders to be stored", |_| {
        stored(&b).group.as_deref() == Some("Job/Clients")
            && Settings::load(&app.storage)
                .connection_folders
                .contains("Job/Archive")
    });

    // Deleting a folder moves what it holds up one level, intact.
    assert_eq!(cx.update(|cx| app.connections.read(cx).count_in("Job")), 2);
    cx.update(|cx| {
        app.connections
            .update(cx, |c, cx| c.delete_folder("Job", cx))
    });
    assert_eq!(folders(cx), ["Archive", "Clients", "Workshop"]);
    assert_eq!(group(cx, &a), None);
    assert_eq!(group(cx, &b).as_deref(), Some("Clients"));

    // Moving and renaming a connection.
    cx.update(|cx| {
        app.connections.update(cx, |manager, cx| {
            manager.move_connection(&a, Some("Archive"), cx);
            manager.move_connection(&c, None, cx);
        })
    });
    assert_eq!(group(cx, &a).as_deref(), Some("Archive"));
    assert_eq!(group(cx, &c), None);
    // The folder it left stays, empty.
    assert_eq!(folders(cx), ["Archive", "Clients", "Workshop"]);
    let refused = cx.update(|cx| {
        app.connections
            .update(cx, |c, cx| c.rename_connection(&a, "  ", cx))
    });
    assert!(refused.unwrap_err().contains("needs a name"));
    cx.update(|cx| {
        app.connections
            .update(cx, |c, cx| c.rename_connection(&a, " Orders ", cx))
    })
    .unwrap();

    // What is stored is what is shown; the secrets were not touched.
    wait_until(cx, "the changes to be stored", |_| {
        let config = stored(&a);
        config.name == "Orders" && config.group.as_deref() == Some("Archive")
    });
    assert_eq!(stored(&b).group.as_deref(), Some("Clients"));
    assert_eq!(stored(&c).group, None);
    wait_until(cx, "the folders to be stored", |cx| {
        Settings::load(&app.storage) == cx.update(|cx| AppState::get(cx).settings.clone())
    });
    for id in [&a, &b, &c] {
        assert_eq!(
            load_secrets(app.secrets.as_ref(), id).unwrap(),
            db_password()
        );
    }

    // An imported connection brings its folder.
    let mut imported = testing::config(Engine::Postgres);
    imported.group = Some("Import/Old".into());
    cx.update(|cx| {
        app.connections
            .update(cx, |c, cx| c.import(vec![imported], cx))
    });
    wait_until(cx, "the import", |cx| folders(cx).len() == 5);
    assert_eq!(
        folders(cx),
        ["Archive", "Clients", "Import", "Import/Old", "Workshop"]
    );
}
