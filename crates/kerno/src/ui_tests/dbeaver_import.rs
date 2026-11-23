//! K41 (D-114): a DBeaver project export previewed, then imported once:
//! folders, the connections Kerno can take (the others greyed with why),
//! their scripts as saved queries, and their passwords only when asked.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use kerno_core::dbeaver;
use kerno_core::secrets;

use super::*;
use crate::dbeaver_import::DbeaverImport;

impl Fixture {
    /// Opens the DBeaver import preview of `path`, once it is read.
    fn dbeaver_preview(&self, cx: &mut TestAppContext, path: PathBuf) -> Entity<DbeaverImport> {
        let slot: Rc<RefCell<Option<Entity<DbeaverImport>>>> = Rc::default();
        let (connections, filled) = (self.connections.clone(), slot.clone());
        self.step(cx, move |window, cx| {
            let opening = crate::dbeaver_import::open(path, connections, window, cx);
            cx.spawn(async move |_| *filled.borrow_mut() = opening.await)
                .detach();
        });
        self.wait_until(cx, "the DBeaver preview", |_, _| slot.borrow().is_some());
        slot.borrow().clone().unwrap()
    }

    fn dbeaver_import(&self, cx: &mut TestAppContext, view: &Entity<DbeaverImport>) -> String {
        self.step(cx, |window, cx| {
            view.update(cx, |view, cx| view.import(window, cx))
        });
        self.wait_until(cx, "the DBeaver import", |_, cx| {
            cx.update(|cx| view.read(cx).summary()).starts_with("done")
        });
        cx.update(|cx| view.read(cx).summary())
    }

    fn by_name(&self, name: &str) -> (String, ConnectionConfig) {
        let saved = self.storage.connections().unwrap();
        let found = saved
            .iter()
            .find(|saved| saved.config.name == name)
            .unwrap();
        (found.id.clone(), found.config.clone())
    }
}

fn project_file(dir: &tempfile::TempDir) -> PathBuf {
    let path = dir.path().join("General.dbp");
    std::fs::write(&path, dbeaver::sample_project()).unwrap();
    path
}

#[gpui_kit::test]
fn a_dbeaver_project_is_previewed_then_imported_once(cx: &mut TestAppContext) {
    let storage = memory_storage();
    // The MariaDB server of the file is saved here already.
    let mut legacy = ConnectionConfig::new(Engine::MariaDb);
    legacy.name = "Mine".into();
    legacy.host = "maria.example.com".into();
    legacy.user = "maria".into();
    let legacy_id = storage.save_connection(None, &legacy).unwrap();
    let app = open(cx, storage);
    let store = cx.update(|cx| AppState::get(cx).secrets.clone());
    let dir = tempfile::tempdir().unwrap();
    let path = project_file(&dir);

    // The preview: ticked, already here, refused; Cancel writes nothing.
    let view = app.dbeaver_preview(cx, path.clone());
    let summary = cx.update(|cx| view.read(cx).summary());
    for part in [
        "preview |",
        "Shop Import(true)",
        "Blog Import(true)",
        &format!("Legacy Exists(\"{legacy_id}\")"),
        "Notes Import(true)",
        "ERP Refused(\"Oracle: not an engine Kerno has.\")",
        "folders [\"Empty\", \"Work\", \"Work/Clients\"]",
        "3 scripts",
        "passwords false",
    ] {
        assert!(summary.contains(part), "{part} in {summary}");
    }
    assert!(app.has(cx, ("dbeaver-connection", 4usize)));
    app.click(cx, "dbeaver-cancel");
    assert_eq!(app.storage.connections().unwrap().len(), 1);
    assert!(app.storage.saved_queries().unwrap().is_empty());

    // Imported with the passwords, Blog left out.
    let view = app.dbeaver_preview(cx, path.clone());
    cx.update(|cx| {
        view.update(cx, |view, cx| {
            view.set_ticked(1, false, cx);
            view.set_passwords(true, cx);
        })
    });
    let done = app.dbeaver_import(cx, &view);
    assert!(
        done.contains("Imported 2 connections and 3 saved queries"),
        "{done}"
    );
    let names: Vec<String> = (app.storage.connections().unwrap().into_iter())
        .map(|saved| saved.config.name)
        .collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(!names.contains(&"Blog".to_string()) && !names.contains(&"ERP".to_string()));
    let (shop_id, shop) = app.by_name("Shop");
    assert_eq!(
        (
            shop.group.as_deref(),
            shop.environment.as_str(),
            shop.save_password
        ),
        (Some("Work/Clients"), "production", true)
    );
    assert_eq!(shop.ssh.as_ref().unwrap().user, "ops");
    let kept = secrets::load_secrets(store.as_ref(), &shop_id).unwrap();
    assert_eq!(
        kept.password.as_ref().map(Secret::expose),
        Some("pg-secret")
    );
    assert_eq!(
        kept.ssh_key_passphrase.as_ref().map(Secret::expose),
        Some("ssh-secret")
    );
    let (notes_id, _) = app.by_name("Notes");
    assert!(!secrets::has_secrets(store.as_ref(), &notes_id).unwrap());
    let mut queries: Vec<(Option<String>, String, String)> = (app.storage.saved_queries())
        .unwrap()
        .into_iter()
        .map(|q| (q.connection_id, q.folder, q.name))
        .collect();
    queries.sort();
    assert_eq!(
        queries,
        [
            (None, String::new(), "oracle".to_string()),
            (None, String::new(), "scratch".to_string()),
            (
                Some(shop_id.clone()),
                "reports".to_string(),
                "monthly".to_string()
            ),
        ]
    );
    let folders = cx.update(|cx| AppState::get(cx).settings.connection_folders.clone());
    for folder in ["Empty", "Work", "Work/Clients"] {
        assert!(folders.contains(folder), "{folder} in {folders:?}");
    }

    // Again: only Blog is new; nothing else twice.
    let view = app.dbeaver_preview(cx, path);
    let summary = cx.update(|cx| view.read(cx).summary());
    assert!(
        summary.contains(&format!("Shop Exists(\"{shop_id}\")")),
        "{summary}"
    );
    let done = app.dbeaver_import(cx, &view);
    assert!(
        done.contains("Imported 1 connection and 0 saved queries"),
        "{done}"
    );
    assert_eq!(app.storage.connections().unwrap().len(), 4);
    assert_eq!(app.storage.saved_queries().unwrap().len(), 3);
    // Not asked this time: Blog has no password.
    let (blog_id, _) = app.by_name("Blog");
    assert!(!secrets::has_secrets(store.as_ref(), &blog_id).unwrap());
}

#[gpui_kit::test]
fn without_the_box_no_password_is_kept(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let store = cx.update(|cx| AppState::get(cx).secrets.clone());
    let dir = tempfile::tempdir().unwrap();
    let view = app.dbeaver_preview(cx, project_file(&dir));
    let done = app.dbeaver_import(cx, &view);
    assert!(done.contains("Imported 4 connections"), "{done}");
    for saved in app.storage.connections().unwrap() {
        assert!(
            !secrets::has_secrets(store.as_ref(), &saved.id).unwrap(),
            "{}",
            saved.config.name
        );
    }
    // The user name, read from DBeaver's credentials, still comes.
    assert_eq!(app.by_name("Shop").1.user, "shop_owner");
}

/// K46: each row shows its environment, a row's is changed from its tag,
/// and DBeaver's own type becomes a Kerno environment with its rules, once.
#[gpui_kit::test]
fn environments_are_shown_chosen_and_created_once(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data-sources.json");
    let mysql = |name: &str, host: &str, kind: &str| {
        serde_json::json!({"provider": "mysql", "driver": "mysql8", "name": name,
            "configuration": {"host": host, "user": "u", "type": kind}})
    };
    let json = serde_json::json!({
        "connection-types": {
            "pp": {"name": "Preprod", "color": "214,250,207", "auto-commit": false,
                "confirm-execute": true, "confirm-data-change": true}
        },
        "connections": {
            "a": mysql("Pick", "a", "dev"),
            "b": mysql("Pre", "b", "pp"),
            "c": mysql("Live", "c", "prod")
        }
    });
    std::fs::write(&path, json.to_string()).unwrap();
    let environments =
        |cx: &mut TestAppContext| cx.update(|cx| AppState::get(cx).settings.environments.clone());
    let before = environments(cx).len();

    let view = app.dbeaver_preview(cx, path.clone());
    let summary = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).summary());
    let shown = summary(cx);
    for part in [
        "Pick Import(true) local",
        "Pre Import(true) preprod",
        "Live Import(true) production",
        "new environments [\"Preprod\"] created true",
    ] {
        assert!(shown.contains(part), "{part} in {shown}");
    }
    for index in 0..3usize {
        assert!(app.has(cx, ("dbeaver-env", index)), "tag {index}");
    }
    assert!(app.has(cx, "dbeaver-new-environments"));
    // Unticked, the new type's connection goes in local; ticked again, back.
    cx.update(|cx| view.update(cx, |view, cx| view.set_create_environments(false, cx)));
    assert!(
        summary(cx).contains("Pre Import(true) local"),
        "{}",
        summary(cx)
    );
    cx.update(|cx| view.update(cx, |view, cx| view.set_create_environments(true, cx)));
    // A row's tag opens Kerno's environments, the new one among them.
    app.click(cx, ("dbeaver-env", 0usize));
    assert!(super::menu_entry(&app, cx, "Preprod (new)").is_some());
    super::click_menu_entry(&app, cx, "Production");
    assert!(
        summary(cx).contains("Pick Import(true) production"),
        "{}",
        summary(cx)
    );
    let done = app.dbeaver_import(cx, &view);
    assert!(done.contains("Imported 3 connections"), "{done}");

    let all = environments(cx);
    assert_eq!(all.len(), before + 1);
    let preprod = all.iter().find(|def| def.name == "Preprod").unwrap();
    assert_eq!(preprod.color_light.as_deref(), Some("#D6FACF"));
    let rules = &preprod.rules;
    assert!(!rules.auto_commit && rules.confirm_execute && rules.confirm_edits);
    assert_eq!(app.by_name("Pre").1.environment, preprod.id);
    assert_eq!(app.by_name("Pick").1.environment, "production");
    assert_eq!(app.by_name("Live").1.environment, "production");

    // Again: the type is Kerno's now; nothing is created twice.
    let view = app.dbeaver_preview(cx, path);
    let shown = cx.update(|cx| view.read(cx).summary());
    assert!(shown.contains("new environments []"), "{shown}");
    assert!(!app.has(cx, "dbeaver-new-environments"));
    app.dbeaver_import(cx, &view);
    assert_eq!(environments(cx).len(), before + 1);
}

#[gpui_kit::test]
fn a_file_that_is_not_an_export_says_so(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.dbp");
    std::fs::write(&path, b"PK\x03\x04 damaged").unwrap();
    let opened: Rc<RefCell<Option<bool>>> = Rc::default();
    let (connections, filled) = (app.connections.clone(), opened.clone());
    app.step(cx, move |window, cx| {
        let opening = crate::dbeaver_import::open(path, connections, window, cx);
        cx.spawn(async move |_| *filled.borrow_mut() = Some(opening.await.is_some()))
            .detach();
    });
    app.wait_until(cx, "the refusal", |_, _| opened.borrow().is_some());
    assert_eq!(*opened.borrow(), Some(false));
    assert!(app.storage.connections().unwrap().is_empty());
}

/// K58 (D-124): under each connection the preview says what was left out and
/// what to check, once each; the import's report says the same, connection by
/// connection. A tunnel's jump server comes as an OpenSSH jump host.
#[gpui_kit::test]
fn the_preview_and_the_report_say_what_to_check_for_each_connection(cx: &mut TestAppContext) {
    use super::tool_wizards::paints;
    let app = open(cx, memory_storage());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data-sources.json");
    let json = serde_json::json!({
        "connection-types": {"pp": {"name": "Preprod", "color": "214,250,207"}},
        "connections": {
            "a": {"provider": "postgresql", "driver": "postgres-jdbc", "name": "Hop",
                "configuration": {"host": "db.internal", "user": "u", "type": "pp",
                    "handlers": {
                        "ssh_tunnel": {"enabled": true, "user": "ops", "properties": {
                            "host": "bastion.example.com", "port": 22.0, "authType": "AGENT",
                            "jumpServer.count": 1.0, "jumpServer0.enabled": true,
                            "jumpServer0.host": "jump.example.com", "jumpServer0.port": 2222.0,
                            "jumpServer0.name": "hop", "jumpServer0.authType": "PUBLIC_KEY",
                            "jumpServer0.keyPath": "/keys/hop"}},
                        "postgre_ssl": {"enabled": true, "properties": {"sslMode": "require",
                            "ssl.ca.cert.value": "-----BEGIN", "ssl.client.cert.value": "-----BEGIN"}}
                    }}},
            "b": {"provider": "mysql", "driver": "mysql8", "name": "Plain",
                "configuration": {"host": "b", "user": "u"}}
        }
    });
    std::fs::write(&path, json.to_string()).unwrap();
    let key = "jump server “jump.example.com”: its key file is not passed to it: load it with \
               ssh-add /keys/hop, or name it for that host in ~/.ssh/config";
    let pasted = "certificate pasted in DBeaver, not in a file: save it to a file and choose it \
                  on the SSL tab";
    let not_created = "connection type “Preprod” not created: put in Local";

    let view = app.dbeaver_preview(cx, path);
    // Each item once, under its connection; nothing under the other.
    assert_eq!(
        cx.update(|cx| view.read(cx).to_check(0)),
        [pasted.to_string(), key.to_string()]
    );
    assert!(app.has(cx, ("dbeaver-note", 0usize)));
    assert!(paints(&app, cx, &format!("To check: {pasted}; {key}.")));
    assert!(!app.has(cx, ("dbeaver-note", 1usize)));
    // DBeaver's type not created: said there too.
    cx.update(|cx| view.update(cx, |view, cx| view.set_create_environments(false, cx)));
    assert!(paints(
        &app,
        cx,
        &format!("To check: {pasted}; {key}; {not_created}.")
    ));

    let done = app.dbeaver_import(cx, &view);
    assert!(
        done.contains(&format!(
            "Imported 2 connections and 0 saved queries.\nTo check:\n• Hop: {pasted}; {key}; \
             {not_created}."
        )),
        "{done}"
    );
    assert!(!done.contains("Plain:"), "{done}");
    let ssh = app.by_name("Hop").1.ssh.unwrap();
    assert_eq!(
        (ssh.client, ssh.jump.as_deref()),
        (
            kerno_core::config::SshClient::OpenSsh,
            Some("hop@jump.example.com:2222")
        )
    );
}
