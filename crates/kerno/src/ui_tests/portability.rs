//! J15 (DBeaver #22579): connections, folder looks, environments and saved
//! tasks exported from one data directory and imported into a fresh one,
//! without any secret; passwords entered at import or linked to the secret
//! store's entry of the same id, then used to connect.
//!
//! Two apps run side by side, one per test context, each on its own data;
//! they share one secret store, as two data directories of one computer
//! share its keychain.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::BorrowAppContext as _;
use kerno_core::config::{FolderLook, PasswordSource};
use kerno_core::environment::{EnvironmentDef, EnvironmentRules, Tint};
use kerno_core::secrets::{self, SecretStore};
use kerno_core::storage::SavedTask;
use kerno_core::tools::{BackupOptions, TaskSettings};

use super::*;
use crate::portability::{ImportSetup, Part};

const SSH_SECRET: &str = "kerno-ui-ssh-secret";
const PASSPHRASE: &str = "kerno-ui-key-passphrase";
const CLIENT_DIR: &str = "/Users/kerno-ui/secret-client-dir";
const PICTURE: &str = "kerno-ui-folder-picture.png";
const LOCAL_CLIENT: &str = "/opt/kerno-ui/local-client";
/// The id of a connection the data directories never had.
const STRANGER: &str = "0b5e2a3c-4d5e-4f60-8a7b-9c0d1e2f3a4b";

impl Fixture {
    fn secret_store(&self, cx: &mut TestAppContext) -> Arc<dyn SecretStore> {
        cx.update(|cx| AppState::get(cx).secrets.clone())
    }

    /// Opens the import preview of `path`, once it is read.
    fn import_preview(&self, cx: &mut TestAppContext, path: PathBuf) -> Entity<ImportSetup> {
        let slot: Rc<RefCell<Option<Entity<ImportSetup>>>> = Rc::default();
        let (connections, filled) = (self.connections.clone(), slot.clone());
        self.step(cx, move |window, cx| {
            let opening =
                crate::portability::open_import(path, connections, Vec::new(), window, cx);
            cx.spawn(async move |_| *filled.borrow_mut() = opening.await)
                .detach();
        });
        self.wait_until(cx, "the import preview", |_, _| slot.borrow().is_some());
        slot.borrow().clone().unwrap()
    }

    fn summary(&self, cx: &mut TestAppContext, view: &Entity<ImportSetup>) -> String {
        cx.update(|cx| view.read(cx).summary())
    }

    fn connect_and_wait(&self, cx: &mut TestAppContext, id: &str) {
        let id = id.to_string();
        let connections = self.connections.clone();
        cx.update(|cx| connections.update(cx, |list, cx| list.connect(&id, cx)));
        self.wait_until(cx, "the connection", |app, cx| {
            cx.update(|cx| {
                matches!(
                    app.connections.read(cx).status(&id),
                    Status::Connected { .. } | Status::Failed { .. }
                )
            })
        });
        let status = cx.update(|cx| self.connections.read(cx).status(&id));
        assert!(matches!(status, Status::Connected { .. }), "{status:?}");
    }
}

#[gpui_kit::test]
fn everything_moves_to_a_fresh_data_directory_without_a_secret(
    cx: &mut TestAppContext,
    fresh: &mut TestAppContext,
) {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    // The first data directory: three connections, an environment of the
    // user's, an edited built-in, a folder look with a picture, a task.
    let storage = memory_storage();
    let mut linked = testing::config(Engine::Postgres);
    linked.name = "kerno_ui linked".into();
    linked.environment = "kerno-ui-qa".into();
    linked.group = Some("Work".into());
    linked.client_dir = Some(CLIENT_DIR.into());
    let linked_id = storage.save_connection(None, &linked).unwrap();
    let mut entered = testing::config(Engine::MySql);
    entered.name = "kerno_ui entered".into();
    let entered_id = storage.save_connection(None, &entered).unwrap();
    let mut command = testing::config(Engine::Postgres);
    command.name = "kerno_ui command".into();
    command.password_source = PasswordSource::Command {
        program: "kerno-ui-vault".into(),
        args: vec!["read".into()],
        timeout_secs: 10,
    };
    // Confirmed here; never in the file.
    command.password_command_unconfirmed = false;
    let command_id = storage.save_connection(None, &command).unwrap();
    let mut task = SavedTask::new(
        "kerno_ui nightly",
        &linked_id,
        TaskSettings::Backup(BackupOptions::new(Engine::Postgres, "kerno")),
    );
    task.description = "kerno_ui backup".into();
    storage.save_task(&task).unwrap();
    let app = open(cx, storage);
    let store = app.secret_store(cx);
    secrets::save_secrets(
        store.as_ref(),
        &linked_id,
        &Secrets {
            password: Some(Secret::new(testing::DB_PASSWORD)),
            ssh_password: Some(Secret::new(SSH_SECRET)),
            ssh_key_passphrase: Some(Secret::new(PASSPHRASE)),
        },
    )
    .unwrap();
    let qa = EnvironmentDef {
        id: "kerno-ui-qa".into(),
        name: "kerno_ui QA".into(),
        color_light: Some("#336699".into()),
        color_dark: Some("#6699CC".into()),
        tint: Tint::Soft,
        rules: EnvironmentRules {
            confirm_execute: true,
            ..EnvironmentRules::default()
        },
    };
    cx.update(|cx| {
        let qa = qa.clone();
        AppState::update_settings(cx, move |settings| {
            settings.environments.push(qa);
            settings.environments[1].name = "kerno_ui Staging".into();
            settings.folder_looks.insert(
                "Work".into(),
                FolderLook {
                    color: Some("blue".into()),
                    glyph: Some("briefcase".into()),
                    image: Some(PICTURE.into()),
                },
            );
        })
    });

    // Export: what to include, then the file.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("kerno-connections.json");
    let connections = app.connections.clone();
    let dialog = app.step(cx, |window, cx| {
        crate::portability::open_export(connections, window, cx)
    });
    assert!(app.has(cx, ("portable-include", 3usize)));
    let target = file.clone();
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| dialog.export_to(target, window, cx))
    });
    app.wait_until(cx, "the export", |app, cx| app.has(cx, "portable-done"));
    let state = cx.update(|cx| dialog.read(cx).state());
    assert!(
        state.starts_with("done: Exported 3 connections, 1 folder look, 2 environments and 1 task"),
        "{state}"
    );
    let bytes = std::fs::read(&file).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    for leak in [
        testing::DB_PASSWORD,
        SSH_SECRET,
        PASSPHRASE,
        CLIENT_DIR,
        PICTURE,
        "password_command_unconfirmed",
    ] {
        assert!(!text.contains(leak), "{leak} in {text}");
    }
    assert!(text.contains("kerno-ui-vault") && text.contains(&linked_id));

    // Import into a fresh data directory of the same computer.
    let other = open(fresh, memory_storage());
    fresh.update(|cx| cx.update_global::<AppState, _>(|state, _| state.secrets = store.clone()));
    let preview = other.import_preview(fresh, file.clone());
    assert_eq!(
        other.summary(fresh, &preview),
        "preview | kerno_ui linked Add Ask, kerno_ui entered Add Ask, \
         kerno_ui command Add Ask | environments [Replace, Add] | tasks [Add]"
    );
    assert!(other.has(fresh, ("import-connection", 2usize)));
    other.click(fresh, "import-next");
    // The command needs no password: two rows. The entry found under the
    // first one's id is offered, naming where it would go, but not chosen:
    // this data directory does not know that connection yet.
    let link = other.label_of(fresh, ("import-linked", 0usize));
    assert!(
        link.contains("database password → kerno@127.0.0.1:55432"),
        "{link}"
    );
    other.click(fresh, ("import-linked", 0usize));
    assert!(
        other
            .summary(fresh, &preview)
            .starts_with("passwords | kerno_ui linked Add Linked,"),
        "{}",
        other.summary(fresh, &preview)
    );
    assert!(!other.has(fresh, ("import-linked", 1usize)));
    assert!(!other.has(fresh, ("import-password-row", 2usize)));
    // "Enter now" with nothing typed is refused.
    other.click(fresh, ("import-enter", 1usize));
    other.click(fresh, "import-confirm");
    assert!(other.has(fresh, "import-problem"));
    other.click(fresh, ("import-password", 1usize));
    other.type_text(fresh, testing::DB_PASSWORD);
    other.click(fresh, "import-confirm");
    other.wait_until(fresh, "the import", |app, cx| app.has(cx, "import-done"));
    assert_eq!(
        other.summary(fresh, &preview).split(" | ").next(),
        Some("done: Imported 3 connections, 2 environments and 1 task.")
    );
    other.click(fresh, "import-close");

    // Everything is there, under the same ids, without this computer's
    // choices; the command waits for a confirmation.
    other.wait_until(fresh, "the list", |app, cx| {
        cx.update(|cx| app.connections.read(cx).saved().len() == 3)
    });
    let stored = other.storage.connections().unwrap();
    let ids: Vec<&str> = stored.iter().map(|saved| saved.id.as_str()).collect();
    assert_eq!(ids, [&linked_id, &entered_id, &command_id]);
    assert_eq!(stored[0].config.client_dir, None);
    assert_eq!(stored[0].config.environment, "kerno-ui-qa");
    assert!(stored[2].config.password_command_unconfirmed);
    let tasks = other.storage.tasks().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        (
            &tasks[0].name,
            &tasks[0].description,
            &tasks[0].connection_id
        ),
        (&task.name, &task.description, &linked_id)
    );
    fresh.update(|cx| {
        let settings = &AppState::get(cx).settings;
        assert!(settings.environments.contains(&qa));
        assert_eq!(settings.environments[1].name, "kerno_ui Staging");
        assert_eq!(settings.environments.len(), 4);
        assert_eq!(
            settings.folder_looks["Work"],
            FolderLook {
                color: Some("blue".into()),
                glyph: Some("briefcase".into()),
                image: None,
            }
        );
    });
    // The entered password went to the secret store; the linked one was
    // left as it was.
    assert_eq!(
        secrets::load_secrets(store.as_ref(), &entered_id)
            .unwrap()
            .password,
        Some(Secret::new(testing::DB_PASSWORD))
    );
    assert_eq!(
        secrets::load_secrets(store.as_ref(), &linked_id)
            .unwrap()
            .ssh_key_passphrase,
        Some(Secret::new(PASSPHRASE))
    );
    other.connect_and_wait(fresh, &linked_id);
    other.connect_and_wait(fresh, &entered_id);

    // Importing the same file again changes nothing.
    let again = other.import_preview(fresh, file.clone());
    assert!(
        other
            .summary(fresh, &again)
            .contains("kerno_ui linked Unchanged"),
        "{}",
        other.summary(fresh, &again)
    );
    other.click(fresh, "import-cancel");

    // This data directory's own local client stays through a replace.
    let mut local = other.storage.connections().unwrap()[0].config.clone();
    local.client_dir = Some(LOCAL_CLIENT.into());
    other
        .storage
        .save_connection(Some(&linked_id), &local)
        .unwrap();
    let list = other.connections.clone();
    fresh.update(|cx| list.update(cx, |list, cx| list.reload(cx)));
    other.wait_until(fresh, "the reload", |app, cx| {
        cx.update(|cx| {
            app.connections.read(cx).saved()[0]
                .config
                .client_dir
                .is_some()
        })
    });

    // A file reusing a known id for another server never gets that id's
    // password without asking: "Ask when connecting" forgets it. A new
    // connection whose id has an entry elsewhere starts on "Ask" too, and
    // then gets an id of its own, leaving that entry (and its owner) alone;
    // its task follows it.
    let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
    json["connections"][0]["host"] = "kerno-ui-elsewhere.invalid".into();
    let mut stranger = json["connections"][1].clone();
    stranger["id"] = STRANGER.into();
    stranger["name"] = "kerno_ui stranger".into();
    json["connections"].as_array_mut().unwrap().push(stranger);
    let mut stranger_task = json["tasks"][0].clone();
    stranger_task["name"] = "kerno_ui stranger task".into();
    stranger_task["connection_id"] = STRANGER.into();
    json["tasks"].as_array_mut().unwrap().push(stranger_task);
    secrets::save_password(store.as_ref(), STRANGER, &Secret::new("kerno-ui-other")).unwrap();
    let moved = dir.path().join("moved.json");
    std::fs::write(&moved, json.to_string()).unwrap();
    let preview = other.import_preview(fresh, moved);
    let summary = other.summary(fresh, &preview);
    assert!(
        summary.contains("kerno_ui linked Replace Ask moved")
            && summary.contains("kerno_ui stranger Add Ask"),
        "{summary}"
    );
    other.click(fresh, "import-next");
    other.click(fresh, "import-confirm");
    other.wait_until(fresh, "the import", |app, cx| app.has(cx, "import-done"));
    assert!(!secrets::has_secrets(store.as_ref(), &linked_id).unwrap());
    assert!(secrets::has_secrets(store.as_ref(), STRANGER).unwrap());
    let stored = other.storage.connections().unwrap();
    assert_eq!(stored[0].config.host, "kerno-ui-elsewhere.invalid");
    assert_eq!(
        stored[0].config.client_dir.as_deref(),
        Some(std::path::Path::new(LOCAL_CLIENT))
    );
    let stranger = stored
        .iter()
        .find(|saved| saved.config.name == "kerno_ui stranger")
        .unwrap();
    assert_ne!(stranger.id, STRANGER);
    let tasks = other.storage.tasks().unwrap();
    let task = tasks
        .iter()
        .find(|task| task.name == "kerno_ui stranger task")
        .unwrap();
    assert_eq!(task.connection_id, stranger.id);
}

/// The parts left out of an export are not in the file, except the
/// environments its connections use.
#[gpui_kit::test]
fn an_export_holds_only_what_was_ticked(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let mut config = testing::config(Engine::Postgres);
    config.environment = "kerno-ui-used".into();
    let id = storage.save_connection(None, &config).unwrap();
    storage
        .save_task(&SavedTask::new(
            "kerno_ui task",
            &id,
            TaskSettings::Backup(BackupOptions::new(Engine::Postgres, "kerno")),
        ))
        .unwrap();
    let app = open(cx, storage);
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            for id in ["kerno-ui-used", "kerno-ui-unused"] {
                settings.environments.push(EnvironmentDef {
                    id: id.into(),
                    name: id.into(),
                    color_light: None,
                    color_dark: None,
                    tint: Tint::None,
                    rules: EnvironmentRules::default(),
                });
            }
        })
    });
    let connections = app.connections.clone();
    let dialog = app.step(cx, |window, cx| {
        crate::portability::open_export(connections, window, cx)
    });
    app.step(cx, |_, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_included(Part::Environments, false, cx);
            dialog.set_included(Part::Tasks, false, cx);
        })
    });
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("out.json");
    let target = file.clone();
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| dialog.export_to(target, window, cx))
    });
    app.wait_until(cx, "the export", |app, cx| app.has(cx, "portable-done"));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("kerno-ui-used"), "{text}");
    assert!(!text.contains("kerno-ui-unused"), "{text}");
    assert!(!text.contains("kerno_ui task"), "{text}");
}

/// Security review of J15: the secrets of a connection id are one set. A
/// file that keeps a connection's id but moves its tunnel to another SSH
/// host starts on "Ask", and the import removes the SSH password and the
/// key passphrase with the database password; the same server keeps them
/// all.
#[gpui_kit::test]
fn a_tunnel_moved_to_another_ssh_host_loses_every_saved_secret(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let mut config = testing::config(Engine::Postgres);
    config.name = "kerno_ui tunnel".into();
    config.ssh = Some(testing::ssh(kerno_core::config::SshAuth::KeyFile {
        path: "/nonexistent/kerno-ui/id_ed25519".into(),
    }));
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let store = app.secret_store(cx);
    let all = Secrets {
        password: Some(Secret::new(testing::DB_PASSWORD)),
        ssh_password: Some(Secret::new(SSH_SECRET)),
        ssh_key_passphrase: Some(Secret::new(PASSPHRASE)),
    };
    secrets::save_secrets(store.as_ref(), &id, &all).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("out.json");
    let (workspace, target) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.export_connections_to(target, window, cx)
        })
    });
    app.wait_until(cx, "the export", |app, cx| app.has(cx, "portable-done"));
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let import =
        |app: &Fixture, cx: &mut TestAppContext, change: &dyn Fn(&mut serde_json::Value)| {
            let mut changed = json.clone();
            change(&mut changed["connections"][0]);
            let path = dir.path().join("changed.json");
            std::fs::write(&path, changed.to_string()).unwrap();
            let preview = app.import_preview(cx, path);
            let summary = app.summary(cx, &preview);
            app.click(cx, "import-next");
            if app.has(cx, "import-confirm") {
                app.click(cx, "import-confirm");
            }
            app.wait_until(cx, "the import", |app, cx| app.has(cx, "import-done"));
            app.click(cx, "import-close");
            summary
        };

    // The same server, renamed: linked by default, every secret kept.
    let summary = import(&app, cx, &|connection| {
        connection["name"] = "kerno_ui tunnel renamed".into()
    });
    assert!(summary.contains("Replace Linked"), "{summary}");
    assert_eq!(secrets::load_secrets(store.as_ref(), &id).unwrap(), all);

    // Another SSH host: "Ask" by default, and nothing is left to read.
    let summary = import(&app, cx, &|connection| {
        connection["ssh"]["host"] = "kerno-ui-attacker.invalid".into()
    });
    assert!(summary.contains("Replace Ask moved"), "{summary}");
    assert_eq!(
        secrets::load_secrets(store.as_ref(), &id).unwrap(),
        Secrets::default()
    );
}

/// An import that replaces a connection that is open does what saving an edited one does
/// (`saving_an_open_connection_asks_before_it_reconnects`): the window asks about what its
/// tabs hold, then the connection opens again with the imported settings (DBeaver #22579).
#[gpui_kit::test]
fn an_import_that_replaces_an_open_connection_reconnects_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = entries_scratch("kerno_ui_import_open");
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    app.run_for_row(cx, "show transaction_read_only", &["off"]);
    app.open_transaction(cx, "insert into kerno_ui_import_open.entries values (1)");

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("out.json");
    let (workspace, target) = (app.workspace.clone(), file.clone());
    app.step(cx, move |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.export_connections_to(target, window, cx)
        })
    });
    app.wait_until(cx, "the export", |app, cx| app.has(cx, "portable-done"));
    app.click(cx, "portable-close");
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    json["connections"][0]["read_only"] = true.into();
    let changed = dir.path().join("changed.json");
    std::fs::write(&changed, json.to_string()).unwrap();
    let preview = app.import_preview(cx, changed);
    let summary = app.summary(cx, &preview);
    assert!(summary.contains("Replace Linked"), "{summary}");
    app.click(cx, "import-next");
    if app.has(cx, "import-confirm") {
        app.click(cx, "import-confirm");
    }
    app.wait_until(cx, "the question", |app, cx| app.has(cx, "end-question"));
    // Nothing reconnects before the answer.
    let reconnects = Rc::new(std::cell::Cell::new(0));
    let _watch = cx.update(|cx| {
        let reconnects = reconnects.clone();
        cx.subscribe(&app.connections, move |connections, event, cx| {
            if let crate::connections::manager::ConnectionsEvent::StatusChanged { id } = event
                && connections.read(cx).status(id) == Status::Connecting
            {
                reconnects.set(reconnects.get() + 1);
            }
        })
    });
    app.click(cx, "end-rollback");
    app.wait_until(cx, "the reconnection", |app, cx| {
        let read_only = cx.update(|cx| app.connections.read(cx).saved()[0].config.read_only);
        read_only
            && reconnects.get() > 0
            && matches!(app.status(cx, 0), Status::Connected { .. })
            && app.tx_state(cx) == kerno_core::TxState::Idle
    });
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM kerno_ui_import_open.entries"
        ),
        "0"
    );
    app.click(cx, "import-close");
    assert!(!app.dialog_open(cx));
    let tab = app.tab(cx);
    cx.update(|cx| tab.update(cx, |tab, cx| tab.set_manual_commit(false, cx)));
    app.click(cx, "query-editor");
    app.run_for_row(cx, "show transaction_read_only", &["on"]);
}
