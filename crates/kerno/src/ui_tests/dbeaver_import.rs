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
