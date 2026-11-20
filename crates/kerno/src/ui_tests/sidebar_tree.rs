//! The sidebar's one tree (CLAUDE.md §3, owner's test of 2026-11-13, items
//! 1, 2 and 8): folders and connections are its roots, expanding a
//! connection connects it and shows its databases under it, several
//! connections are browsed at once, a colour tag is a dot, and the default
//! database comes first.

use super::*;

impl Fixture {
    /// The rows shown under the saved connection `index`, indented by depth.
    fn outline_of(&self, cx: &mut TestAppContext, index: usize) -> Vec<String> {
        cx.update(|cx| {
            let id = self.connections.read(cx).saved()[index].id.clone();
            self.workspace
                .read(cx)
                .navigator(cx)
                .read(cx)
                .outline_of(&id, cx)
        })
    }

    /// The id of the first saved connection.
    fn first_id(&self, cx: &mut TestAppContext) -> String {
        cx.update(|cx| self.connections.read(cx).saved()[0].id.clone())
    }

    /// Saves `configs` with the test servers' secrets and opens the app.
    fn saved(cx: &mut TestAppContext, configs: &[ConnectionConfig]) -> Fixture {
        let storage = memory_storage();
        let ids: Vec<String> = configs
            .iter()
            .map(|config| storage.save_connection(None, config).unwrap())
            .collect();
        let app = open(cx, storage);
        let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
        for id in &ids {
            kerno_core::secrets::save_secrets(secrets.as_ref(), id, &testing::secrets()).unwrap();
        }
        app
    }
}

#[gpui_kit::test]
fn two_connections_are_browsed_at_once(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    let app = Fixture::saved(
        cx,
        &[
            testing::config(Engine::Postgres),
            testing::config(Engine::MySql),
        ],
    );
    app.open_connection_row(cx, 0);
    app.open_connection_row(cx, 1);
    app.wait_until(cx, "both trees", |app, cx| {
        app.outline_of(cx, 0).contains(&row(1, "shop"))
            && app.outline_of(cx, 1).contains(&row(1, "Tables"))
    });

    // The first one's tables are read again; the second stays as it was.
    app.click(cx, ("connection", 0usize));
    app.toggle(cx, &["kerno", "shop"]);
    app.wait_until(cx, "the objects of shop", |app, cx| {
        app.outline_of(cx, 0).contains(&row(2, "Tables"))
    });
    app.toggle(cx, &["kerno", "shop", "Tables"]);
    let mysql = app.outline_of(cx, 1);
    let (navigator, postgres) = cx.update(|cx| {
        (
            app.workspace.read(cx).navigator(cx),
            app.connections.read(cx).saved()[0].id.clone(),
        )
    });
    app.step(cx, |_, cx| {
        navigator.update(cx, |navigator, cx| {
            navigator.refresh_tables(&postgres, None, "shop", cx)
        })
    });
    app.wait_until(cx, "the tables of shop again", |app, cx| {
        app.outline_of(cx, 0).contains(&row(3, "orders"))
    });
    assert_eq!(app.outline_of(cx, 1), mysql);
    assert!((0..2).all(|index| matches!(app.status(cx, index), Status::Connected { .. })));
}

#[gpui_kit::test]
fn collapsing_a_connection_keeps_it_connected(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
    app.click(cx, ("connection-chevron", 0usize));
    assert_eq!(app.tree(cx), Vec::<String>::new());
    assert!(matches!(app.status(cx, 0), Status::Connected { .. }));
    // Opening it again shows what was loaded, at once.
    app.click(cx, ("connection-chevron", 0usize));
    assert!(
        app.tree(cx).contains(&row(1, "shop")),
        "{:#?}",
        app.tree(cx)
    );
}

#[gpui_kit::test]
fn a_failed_connection_shows_its_error_and_try_again_under_it(cx: &mut TestAppContext) {
    // Port 1 of this machine: nothing listens there, the refusal is quick.
    let config = ConnectionConfig {
        port: 1,
        ..testing::config(Engine::Postgres)
    };
    let app = Fixture::saved(cx, std::slice::from_ref(&config));
    app.open_connection_row(cx, 0);
    app.wait_until(cx, "the failure", |app, cx| {
        matches!(app.status(cx, 0), Status::Failed { .. })
    });
    let Status::Failed { message } = app.status(cx, 0) else {
        unreachable!()
    };
    let explained = kerno_core::error::explain(&message, &config);
    // In plain words, what to do, the server's own words, and the button.
    for label in [&explained.headline, &explained.next_step, &explained.raw] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    assert!(app.has(cx, ("connection-retry", 0usize)));
    assert!(!app.has(cx, "navigator-error"), "no panel of its own");

    let (connections, id) = (app.connections.clone(), app.first_id(cx));
    let status = app.step(cx, |window, cx| {
        window.click(("connection-retry", 0usize), cx);
        connections.read(cx).status(&id)
    });
    assert_eq!(status, Status::Connecting);
    app.wait_until(cx, "the second failure", |app, cx| {
        matches!(app.status(cx, 0), Status::Failed { .. })
    });
}

#[gpui_kit::test]
fn the_filter_matches_connection_names_and_objects(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let idle = ConnectionConfig {
        name: "kerno_ui_dormant".into(),
        ..testing::config(Engine::MySql)
    };
    let app = Fixture::saved(cx, &[testing::config(Engine::Postgres), idle]);
    app.open_connection_row(cx, 0);
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
    app.toggle(cx, &["kerno", "shop"]);
    app.wait_until(cx, "the objects of shop", |app, cx| {
        app.tree(cx).contains(&row(2, "Tables"))
    });
    let filter = |app: &Fixture, cx: &mut TestAppContext, text: &str| {
        app.click(cx, "navigator-filter");
        app.press(cx, &["secondary-a", "backspace"]);
        app.type_text(cx, text);
    };

    // A connection's name.
    filter(&app, cx, "DORMANT");
    assert!(app.has(cx, ("connection", 1usize)));
    assert!(!app.has(cx, ("connection", 0usize)));

    // An object: the way to it, under its connection.
    filter(&app, cx, "order_items");
    assert!(app.has(cx, ("connection", 0usize)));
    assert!(!app.has(cx, ("connection", 1usize)));
    assert_eq!(
        app.tree(cx),
        [
            row(0, "kerno"),
            row(1, "shop"),
            row(2, "Tables"),
            row(3, "order_items")
        ]
    );

    // The name of an open connection: all it holds.
    let name = app.first_stored().name.to_lowercase();
    filter(&app, cx, &name);
    assert!(
        app.tree(cx).contains(&row(1, "analytics")),
        "{:#?}",
        app.tree(cx)
    );
    assert!(!app.has(cx, ("connection", 1usize)));

    app.press(cx, &["secondary-a", "backspace"]);
    assert!(app.has(cx, ("connection", 0usize)) && app.has(cx, ("connection", 1usize)));
}

/// Real-app pass: a filter that matched nothing loaded left the sidebar blank. It says so, and
/// where the rest is.
#[gpui_kit::test]
fn a_filter_that_matches_nothing_loaded_says_so(cx: &mut TestAppContext) {
    let app = Fixture::saved(cx, &[testing::config(Engine::Postgres)]);
    assert!(!app.has(cx, "navigator-filter-empty"));
    app.click(cx, "navigator-filter");
    app.type_text(cx, "kerno_ui_Nowhere");
    assert!(!app.has(cx, ("connection", 0usize)));
    let said = app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .iter()
            .filter(|text| text.path().contains(&"navigator-filter-empty".into()))
            .map(|text| text.text().to_string())
            .collect::<String>()
    });
    assert!(
        said.starts_with("Nothing loaded matches “kerno_ui_Nowhere”.")
            && said.contains("expand a schema, or use Quick Open"),
        "{said}"
    );
    app.press(cx, &["secondary-a", "backspace"]);
    assert!(app.has(cx, ("connection", 0usize)));
    assert!(!app.has(cx, "navigator-filter-empty"));
}

#[gpui_kit::test]
fn f2_and_delete_act_on_the_selected_connection(cx: &mut TestAppContext) {
    let config = ConnectionConfig {
        group: Some("Work".into()),
        ..testing::config(Engine::Postgres)
    };
    let app = Fixture::saved(cx, &[config]);

    app.click(cx, ("connection", 0usize));
    app.press(cx, &["f2"]);
    assert!(app.has(cx, "name-input"), "F2 renames the connection");
    app.click(cx, "name-cancel");

    app.click(cx, ("connection", 0usize));
    app.press(cx, &["delete"]);
    assert!(app.has(cx, "ok"), "Delete asks first");
    app.click(cx, "cancel");
    assert_eq!(app.storage.connections().unwrap().len(), 1);

    // The same keys on a folder.
    app.click(cx, ("conn-folder", 0usize));
    app.press(cx, &["f2"]);
    let name = app.step(cx, |window, _| {
        window
            .find("name-input")
            .value()
            .map(|value| value.to_string())
    });
    assert_eq!(name.as_deref(), Some("Work"));
    app.click(cx, "name-cancel");
}

#[gpui_kit::test]
fn reveal_opens_a_collapsed_folder_and_its_connection(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected_as(
        cx,
        ConnectionConfig {
            group: Some("Work".into()),
            ..testing::config(Engine::Postgres)
        },
    );
    app.wait_until(cx, "the schemas", |app, cx| {
        app.tree(cx).contains(&row(1, "shop"))
    });
    app.sidebar_does(cx, Subject::List, SidebarAction::CollapseAll);
    assert!(
        !app.has(cx, ("connection", 0usize)),
        "inside a closed folder"
    );

    let navigator = cx.update(|cx| app.workspace.read(cx).navigator(cx));
    let target = Target {
        connection: app.first_id(cx),
        database: None,
        schema: None,
    };
    app.step(cx, |_, cx| {
        navigator.update(cx, |navigator, cx| {
            navigator.reveal(
                &target,
                &TableRef::new("shop", "orders"),
                RelationKind::Table,
                cx,
            )
        })
    });
    app.wait_until(cx, "orders selected", |app, cx| {
        let orders = cx.update(|cx| {
            navigator
                .read(cx)
                .row_of(&["kerno", "shop", "Tables", "orders"], cx)
        });
        orders.is_some() && app.selected_row(cx) == orders
    });
    assert!(cx.update(|cx| {
        !AppState::get(cx)
            .settings
            .collapsed_folders
            .contains("Work")
    }));
}

#[gpui_kit::test]
fn a_connection_without_colour_has_no_dot(cx: &mut TestAppContext) {
    let tagged = ConnectionConfig {
        name: "tagged".into(),
        color: Some("blue".into()),
        ..testing::config(Engine::Postgres)
    };
    let app = Fixture::saved(cx, &[testing::config(Engine::Postgres), tagged]);
    assert!(app.has(cx, ("connection", 0usize)));
    assert!(!app.has(cx, ("connection-dot", 0usize)));
    assert!(app.has(cx, ("connection-dot", 1usize)));
}

#[gpui_kit::test]
fn the_default_database_comes_first_and_is_marked(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected_as(
        cx,
        ConnectionConfig {
            database: Some("kerno_other".into()),
            ..testing::config(Engine::Postgres)
        },
    );
    app.wait_until(cx, "the databases", |app, cx| {
        let tree = app.tree(cx);
        tree.contains(&row(0, "kerno")) && tree.contains(&row(0, "postgres"))
    });
    assert_eq!(app.tree(cx)[0], "kerno_other", "{:#?}", app.tree(cx));
    assert!(app.is_open(cx, &["kerno_other"]));
    let default = app.tree_row(cx, &["kerno_other"]);
    assert!(app.has(cx, ("node-default", default)));
    for other in ["kerno", "postgres"] {
        assert!(!app.is_open(cx, &[other]), "{other} is closed");
        let row = app.tree_row(cx, &[other]);
        assert!(!app.has(cx, ("node-default", row)));
    }
}
