//! K62 (docs/plans/M12.md): a PostgreSQL connection chooses which of the
//! server's databases its tree shows, and a database the account may not
//! open is drawn as such before any click. Against the test server, as a
//! login role of its own (the superuser `kerno` may open anything);
//! scratch objects `kerno_test_k62_ui_*`, dropped by the test.

use kerno_core::config::DatabasesShown;

use super::*;
use crate::navigator::model::{NO_CONNECTIONS, NOT_ALLOWED};

const ROLE: &str = "kerno_test_k62_ui_role";
const CLOSED: &str = "kerno_test_k62_ui_closed";

/// The closed database and the role, made fresh, dropped on the way out.
fn closed_database() -> Scratch {
    let terminate = format!(
        "SELECT pg_catalog.pg_terminate_backend(pid) FROM pg_catalog.pg_stat_activity \
         WHERE usename = '{ROLE}'"
    );
    Scratch::new(
        Engine::Postgres,
        &[
            &terminate,
            &format!("DROP DATABASE IF EXISTS {CLOSED} WITH (FORCE)"),
            &format!("DROP ROLE IF EXISTS {ROLE}"),
        ],
        &[
            &format!("CREATE DATABASE {CLOSED}"),
            &format!("REVOKE CONNECT ON DATABASE {CLOSED} FROM PUBLIC"),
            // The test servers' password, which `connected_as` saves.
            &format!(
                "CREATE ROLE {ROLE} LOGIN PASSWORD '{}'",
                testing::DB_PASSWORD
            ),
        ],
    )
}

impl Fixture {
    /// The databases (rows at depth 0) under the connection.
    fn databases_shown(&self, cx: &mut TestAppContext) -> Vec<String> {
        self.tree(cx)
            .into_iter()
            .filter(|line| !line.starts_with(' '))
            .collect()
    }

    /// Opens the connection's dialog, as its Edit… entry does.
    fn edit_first(&self, cx: &mut TestAppContext) {
        let (connections, id) = (self.connections.clone(), self.first_id_k62(cx));
        self.step(cx, |window, cx| {
            crate::sidebar::Sidebar::edit(connections, id, window, cx)
        });
        self.wait_until(cx, "the connection's dialog", |app, cx| {
            app.has(cx, "conn-save")
        });
    }

    fn first_id_k62(&self, cx: &mut TestAppContext) -> String {
        cx.update(|cx| self.connections.read(cx).saved()[0].id.clone())
    }

    /// Chooses `shown` in the open dialog and saves.
    fn choose_shown(&self, cx: &mut TestAppContext, shown: DatabasesShown) {
        self.click(cx, "conn-databases-shown");
        self.click_menu_label(cx, shown.label());
        self.click(cx, "conn-save");
        self.wait_until(cx, "the saved choice", |app, cx| {
            cx.update(|cx| app.connections.read(cx).saved()[0].config.databases_shown) == shown
        });
    }

    /// The databases the active query tab offers.
    fn tab_databases(&self, cx: &mut TestAppContext) -> Vec<String> {
        let tab = self.tab(cx);
        cx.update(|cx| tab.read(cx).databases(cx))
    }
}

/// The choice is on PostgreSQL's dialog only, beside the Database field,
/// and is saved; MySQL's dialog has none, and saves "all".
#[gpui_kit::test]
fn the_dialog_offers_the_choice_on_postgresql_only_and_saves_it(cx: &mut TestAppContext) {
    let storage = memory_storage();
    storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    storage
        .save_connection(None, &testing::config(Engine::MySql))
        .unwrap();
    let app = open(cx, storage);
    app.edit_first(cx);
    let (field, picker) = app.step(cx, |window, _| {
        (
            window.find("conn-database").bounds(),
            window.find("conn-databases-shown").bounds(),
        )
    });
    // On the Database field's line: the tab is no taller (K56).
    assert!(
        picker.top() < field.bottom() && field.top() < picker.bottom(),
        "{field:?} {picker:?}"
    );
    app.click(cx, "conn-databases-shown");
    for shown in DatabasesShown::ALL {
        assert!(app.shows_label(cx, shown.label()), "{}", shown.label());
    }
    app.press(cx, &["escape"]);
    app.choose_shown(cx, DatabasesShown::DefaultOnly);
    assert_eq!(
        app.storage.connections().unwrap()[0].config.databases_shown,
        DatabasesShown::DefaultOnly
    );

    let (connections, mysql) = (
        app.connections.clone(),
        cx.update(|cx| app.connections.read(cx).saved()[1].id.clone()),
    );
    app.step(cx, |window, cx| {
        crate::sidebar::Sidebar::edit(connections, mysql, window, cx)
    });
    app.wait_until(cx, "MySQL's dialog", |app, cx| app.has(cx, "conn-save"));
    assert!(app.has(cx, "conn-database"));
    assert!(!app.has(cx, "conn-databases-shown"));
}

/// The tree under each choice, against the real server: a closed database
/// dimmed that opens on a plain sentence without any session in it, the
/// count of what is hidden that opens the dialog (click and Enter), and
/// the query tab's database picker offering what the tree shows.
#[gpui_kit::test]
fn the_tree_shows_what_the_connection_chose_and_says_what_is_closed(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = closed_database();
    let app = connected_as(
        cx,
        ConnectionConfig {
            user: ROLE.into(),
            database: Some("kerno".into()),
            ..testing::config(Engine::Postgres)
        },
    );
    app.wait_until(cx, "the databases", |app, cx| {
        app.tree(cx).contains(&row(0, CLOSED))
    });
    let id = app.first_id_k62(cx);
    let navigator = cx.update(|cx| app.workspace.read(cx).navigator(cx));
    let sessions = |cx: &mut TestAppContext| cx.update(|cx| navigator.read(cx).sessions_of(&id));

    // All: every database, the closed ones too, nothing said hidden.
    let all = app.databases_shown(cx);
    for name in [
        "kerno",
        "kerno_other",
        "postgres",
        CLOSED,
        "template0",
        "template1",
    ] {
        assert!(all.contains(&name.to_string()), "{name}: {all:#?}");
    }
    assert!(!all.iter().any(|line| line.ends_with("hidden")), "{all:#?}");

    // Opened: the reason at once, muted, and no session in it ever.
    app.toggle(cx, &[CLOSED]);
    assert!(
        app.tree(cx).contains(&row(1, NOT_ALLOWED)),
        "{:#?}",
        app.tree(cx)
    );
    app.toggle(cx, &["template0"]);
    assert!(app.tree(cx).contains(&row(1, NO_CONNECTIONS)));
    // Another database read meanwhile: the closed one is still not tried.
    app.toggle(cx, &["kerno_other"]);
    app.wait_until(cx, "the schemas of kerno_other", |app, cx| {
        app.tree(cx).contains(&row(1, "public"))
    });
    let open = sessions(cx);
    assert!(open.contains(&"kerno_other".to_string()), "{open:?}");
    assert!(!open.contains(&CLOSED.to_string()), "{open:?}");
    assert!(!open.contains(&"template0".to_string()), "{open:?}");
    assert!(app.tree(cx).contains(&row(1, NOT_ALLOWED)));
    assert!(!app.has(cx, ("node-failure", 0usize)));
    // Its menu keeps what needs no session in it.
    let closed_row = app.tree_row(cx, &[CLOSED]);
    app.right_click(cx, ("node", closed_row), None);
    assert!(menu_entry(&app, cx, Action::CopyName.label()).is_some());
    assert!(menu_entry(&app, cx, Action::Drop.label()).is_some());
    for gone in [
        Action::NewQueryHere,
        Action::CreateSchema,
        Action::Properties,
    ] {
        assert!(menu_entry(&app, cx, gone.label()).is_none(), "{gone:?}");
    }
    assert!(menu_entry(&app, cx, Action::Tools.label()).is_none());
    app.press(cx, &["escape"]);
    // Its Refresh lists the databases again; still no session in it.
    app.act(cx, Action::Refresh, &[CLOSED]);
    app.wait_until(cx, "the databases again", |app, cx| {
        let tree = app.tree(cx);
        tree.contains(&row(0, CLOSED)) && !tree.contains(&row(1, NOT_ALLOWED))
    });
    assert!(!sessions(cx).contains(&CLOSED.to_string()));

    // A query tab on the connection: its picker offers the databases the
    // tree shows, never a closed one or a template.
    app.act(cx, Action::NewQueryHere, &["kerno"]);
    app.wait_until(cx, "the tab's databases", |app, cx| {
        !app.tab_databases(cx).is_empty()
    });
    let offered = app.tab_databases(cx);
    assert!(offered.contains(&"kerno_other".to_string()), "{offered:?}");
    for closed in [CLOSED, "template0", "template1"] {
        assert!(!offered.contains(&closed.to_string()), "{offered:?}");
    }

    // Those this account can open: the closed and the templates hidden,
    // said after the last database, at once (no reconnect).
    app.edit_first(cx);
    app.choose_shown(cx, DatabasesShown::Available);
    app.wait_until(cx, "the databases it can open", |app, cx| {
        !app.databases_shown(cx).contains(&CLOSED.to_string())
    });
    let available = app.databases_shown(cx);
    assert_eq!(
        available.last().map(String::as_str),
        Some("3 databases hidden")
    );
    for name in [CLOSED, "template0", "template1"] {
        assert!(!available.contains(&name.to_string()), "{available:#?}");
    }
    // Other tests' scratch databases come and go between two listings.
    let fixture = |names: &[String]| -> Vec<String> {
        let mut names: Vec<String> = (names.iter())
            .filter(|name| !name.starts_with("kerno_test_") && !name.starts_with("kerno_ui_"))
            .cloned()
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        fixture(&app.tab_databases(cx)),
        fixture(&available[..available.len() - 1])
    );
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));

    // Only the default: the rest hidden, and the count opens the dialog.
    app.edit_first(cx);
    app.choose_shown(cx, DatabasesShown::DefaultOnly);
    app.wait_until(cx, "the default database alone", |app, cx| {
        app.databases_shown(cx).len() == 2
    });
    let only = app.databases_shown(cx);
    assert_eq!(only[0], "kerno");
    let hidden = only[1].clone();
    assert_eq!(hidden, format!("{} databases hidden", all.len() - 1));
    assert_eq!(app.tab_databases(cx), ["kerno"]);
    // The filter box and Collapse all keep working with the line there.
    app.click(cx, "navigator-filter");
    app.type_text(cx, "kern");
    assert!(
        app.tree(cx).contains(&row(0, "kerno")),
        "{:#?}",
        app.tree(cx)
    );
    app.press(cx, &["secondary-a", "backspace"]);
    app.step(cx, |_, cx| {
        navigator.update(cx, |navigator, cx| navigator.collapse_all(cx))
    });
    assert_eq!(app.databases_shown(cx), only);

    // A click on its words opens the connection's dialog...
    assert!(!app.has(cx, "conn-save"));
    app.click(cx, ("databases-hidden", 0usize));
    app.wait_until(cx, "the dialog from the click", |app, cx| {
        app.has(cx, "conn-databases-shown")
    });
    // Its Database list offers only what this account can open.
    app.click(cx, "conn-database-load");
    app.wait_until(cx, "the server's databases", |app, cx| {
        app.has(cx, "conn-database-list")
    });
    app.click(cx, "conn-database-list");
    assert!(menu_entry(&app, cx, "kerno_other").is_some());
    assert!(menu_entry(&app, cx, CLOSED).is_none());
    assert!(menu_entry(&app, cx, "template0").is_none());
    app.press(cx, &["escape"]);
    app.press(cx, &["escape"]);
    app.wait_until(cx, "the dialog closed", |app, cx| !app.has(cx, "conn-save"));
    // ...and so does Enter on the line, selected by a click beside them.
    let line = app.tree_row(cx, &[&hidden]);
    app.step(cx, |window, cx| {
        let bounds = window.find(("node", line)).bounds();
        let options = gpui_kit::test::ClickOptions::new()
            .with_offset(gpui_kit::point(bounds.size.width - px(6.), px(6.)));
        window.click_with_options(("node", line), options, cx)
    });
    assert!(!app.has(cx, "conn-save"));
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the dialog from Enter", |app, cx| {
        app.has(cx, "conn-databases-shown")
    });
    app.press(cx, &["escape"]);
    app.wait_until(cx, "the dialog closed", |app, cx| !app.has(cx, "conn-save"));

    // The global backup neither offers it nor tries it: pg_dumpall would
    // stop there.
    let dialog = app
        .wizard(cx, "kerno", crate::backup_dialog::Mode::GlobalBackup)
        .expect("the global backup");
    let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
    if !state.starts_with("ready") {
        eprintln!("SKIPPED the global backup part: no usable pg_dumpall: {state}");
        return;
    }
    app.wait_until(cx, "the databases to back up", |_, cx| {
        cx.update(|cx| dialog.read(cx).objects_loaded())
    });
    assert!(!app.shows_label(cx, CLOSED));
    // Said where the databases are listed, without a hover.
    let said = format!("Left out, this account may not open it: {CLOSED}");
    let painted = app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .into_iter()
            .map(|text| text.text().to_string())
            .collect::<Vec<_>>()
    });
    assert!(painted.contains(&said), "{painted:#?}");
    let now = app.wizard_state(cx, &dialog);
    assert!(
        now.contains("--exclude-database") && now.contains(CLOSED),
        "{now}"
    );
    // A task keeps only what the user left out: the closed ones are read
    // again from the server at each run.
    let kept = cx.update(|cx| dialog.read(cx).task_settings(cx)).unwrap();
    let kerno_core::tools::TaskSettings::GlobalBackup(kept) = kept else {
        panic!("{kept:?}")
    };
    assert!(
        !kept.exclude_databases.contains(&CLOSED.to_string()),
        "{kept:?}"
    );

    // The run says it first in its log: one small database, into a
    // temporary folder.
    let dir = tempfile::tempdir().unwrap();
    app.tap(cx, "global-databases-none");
    app.click_label(cx, "postgres");
    app.tap(cx, "wizard-next");
    app.write_to(cx, dir.path(), Some("k62"));
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done"), "{state}");
    let log = cx.update(|cx| dialog.read(cx).log());
    assert_eq!(log.first(), Some(&said), "{log:#?}");
}
