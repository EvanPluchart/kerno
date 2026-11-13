//! The main toolbar (CLAUDE.md §3 item 11, docs/plans/M10.md Part 12): its
//! items and their state, the engine list, the recent scripts, the
//! transaction mode with the isolation level, the pickers, its right-click
//! customisation, "Show main toolbar" and the overflow of a narrow window.

use super::*;
use crate::workspace::toolbar::{Facts, Item, why_not};
use gpui_kit::test::ElementSnapshot;
use kerno_core::IsolationLevel;

/// The element of a toolbar item, as the last frame drew it.
fn item(app: &Fixture, cx: &mut TestAppContext, item: Item) -> ElementSnapshot {
    app.step(cx, |window, _| window.find(item.element_id()))
}

/// Whether the item is greyed, and what its tooltip says.
fn state(app: &Fixture, cx: &mut TestAppContext, of: Item) -> (bool, String) {
    let element = item(app, cx, of);
    (
        element.disabled() == Some(true),
        element.description().unwrap_or_default().to_string(),
    )
}

/// The facts of the window now, as the toolbar reads them.
fn facts(app: &Fixture, cx: &mut TestAppContext) -> Facts {
    cx.update(|cx| app.workspace.read(cx).toolbar_facts(cx))
}

/// What a picker shows now.
fn picker(app: &Fixture, cx: &mut TestAppContext, of: Item) -> String {
    cx.update(|cx| app.workspace.read(cx).picker_text(of, cx))
}

/// Opens the list of a dropdown item.
fn open_list(app: &Fixture, cx: &mut TestAppContext, id: &str) {
    app.click(cx, id.to_string());
    assert!(app.has(cx, "popup-menu"), "the list of {id} did not open");
}

/// Opens the caret half of a split item.
fn open_split(app: &Fixture, cx: &mut TestAppContext, split: &'static str) {
    app.step(cx, |window, cx| window.within(split).click("popup", cx));
}

#[gpui_kit::test]
fn every_item_is_there_and_greyed_with_its_reason_when_nothing_is_open(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let now = facts(&app, cx);
    for of in Item::all() {
        let element = item(&app, cx, of);
        assert_eq!(element.label(), Some(of.name()), "{of:?}");
        let (greyed, tooltip) = state(&app, cx, of);
        match why_not(of, now) {
            Some(reason) => {
                assert!(greyed, "{of:?} should be greyed");
                assert!(
                    tooltip.ends_with(&format!("({reason})")),
                    "{of:?}: {tooltip}"
                );
            }
            None => assert!(!greyed, "{of:?} should apply"),
        }
        assert!(tooltip.starts_with(of.name()), "{of:?}: {tooltip}");
    }
    // Only the two new items apply.
    let applying: Vec<Item> = Item::all()
        .filter(|of| why_not(*of, now).is_none())
        .collect();
    assert_eq!(applying, [Item::NewConnection, Item::NewEditor]);
    // No tab: nowhere to show.
    assert_eq!(picker(&app, cx, Item::Connection), "No connection");
    assert_eq!(picker(&app, cx, Item::Target), "No database");
}

#[gpui_kit::test]
fn the_engine_list_opens_the_new_connection_dialog_on_that_engine(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    open_split(&app, cx, "toolbar-new-connection-menu");
    for engine in Engine::ALL {
        assert!(menu_entry(&app, cx, engine.label()).is_some(), "{engine:?}");
    }
    click_menu_entry(&app, cx, "MariaDB");
    for (field, text) in [
        ("conn-name", "kerno_ui_toolbar_engine"),
        ("conn-host", "localhost"),
        ("conn-user", "kerno"),
    ] {
        app.click(cx, field);
        app.type_text(cx, text);
    }
    app.click(cx, "conn-save");
    app.wait_until(cx, "the saved connection", |app, cx| {
        cx.update(|cx| !app.connections.read(cx).saved().is_empty())
    });
    let saved = app.first_stored();
    assert_eq!(
        (saved.name.as_str(), saved.engine, saved.port),
        ("kerno_ui_toolbar_engine", Engine::MariaDb, 3306)
    );
}

#[gpui_kit::test]
fn the_recent_scripts_list_and_reopen_a_file(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kerno_ui_toolbar.sql");
    std::fs::write(&path, "select 1").unwrap();
    open_split(&app, cx, "toolbar-new-editor-menu");
    assert!(menu_entry(&app, cx, "No recent scripts").is_some());
    app.press(cx, &["escape"]);

    let opened = path.clone();
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_sql_file_from(opened, window, cx)
        })
    });
    app.wait_until(cx, "the file's tab", |app, cx| app.titles(cx).len() == 1);
    assert_eq!(settings_now(cx).recent_scripts, std::slice::from_ref(&path));
    app.press(cx, &["secondary-w"]);
    assert!(app.titles(cx).is_empty());

    // The list names the file and its folder; picking it opens it again.
    let label = format!("kerno_ui_toolbar.sql  {}", dir.path().display());
    open_split(&app, cx, "toolbar-new-editor-menu");
    click_menu_entry(&app, cx, &label);
    app.wait_until(cx, "the file again", |app, cx| app.titles(cx).len() == 1);
    assert_eq!(app.active_query(cx).1, "select 1");

    // A file that went away leaves the list when picked.
    app.press(cx, &["secondary-w"]);
    std::fs::remove_file(&path).unwrap();
    open_split(&app, cx, "toolbar-new-editor-menu");
    click_menu_entry(&app, cx, &label);
    app.wait_until(cx, "the missing file dropped", |_, cx| {
        settings_now(cx).recent_scripts.is_empty()
    });
    assert!(app.titles(cx).is_empty());
}

/// The isolation level the active tab's own session reads, run in the tab.
fn isolation_in_tab(app: &Fixture, cx: &mut TestAppContext, engine: Engine) -> String {
    let sql = match engine {
        Engine::Postgres => "show transaction_isolation",
        _ => "select @@transaction_isolation",
    };
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, sql);
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the level", |app, cx| {
        !app.running(cx) && app.outcomes(cx) == ["1 rows"]
    });
    let (_, rows) = app.result(cx, 0, 1);
    rows[0][0].clone().unwrap_or_default()
}

#[gpui_kit::test]
fn the_mode_list_switches_the_commit_mode_and_sets_the_isolation_level(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        app.press(cx, &["secondary-t"]);
        let tab = app.tab(cx);
        // The level the server gives the new session is ticked.
        isolation_in_tab(&app, cx, engine);
        let default = match engine {
            Engine::Postgres => IsolationLevel::ReadCommitted,
            _ => IsolationLevel::RepeatableRead,
        };
        assert_eq!(cx.update(|cx| tab.read(cx).isolation()), Some(default));

        open_list(&app, cx, "toolbar-mode");
        click_menu_entry(&app, cx, "Serializable");
        app.wait_until(cx, "the new level", |_, cx| {
            cx.update(|cx| tab.read(cx).isolation()) == Some(IsolationLevel::Serializable)
        });
        // The tab's own session runs at that level.
        assert_eq!(
            IsolationLevel::parse(&isolation_in_tab(&app, cx, engine)),
            Some(IsolationLevel::Serializable),
            "{engine:?}"
        );

        open_list(&app, cx, "toolbar-mode");
        click_menu_entry(&app, cx, "Manual commit");
        assert!(cx.update(|cx| tab.read(cx).manual_commit()));
        assert!(state(&app, cx, Item::Commit).0);
        open_list(&app, cx, "toolbar-mode");
        click_menu_entry(&app, cx, "Auto-commit");
        assert!(!cx.update(|cx| tab.read(cx).manual_commit()));
    }
}

#[gpui_kit::test]
fn commit_and_rollback_apply_while_a_transaction_is_open(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = entries_scratch("kerno_ui_toolbar_tx");
    let count = || {
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM kerno_ui_toolbar_tx.entries",
        )
    };
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    // Connected, a query tab, nothing open.
    for (of, greyed) in [
        (Item::Connect, true),
        (Item::Reconnect, false),
        (Item::Disconnect, false),
        (Item::Commit, true),
        (Item::Rollback, true),
        (Item::Mode, false),
        (Item::Log, false),
        (Item::ReadOnly, false),
        (Item::Connection, false),
        (Item::Target, false),
        (Item::Dashboard, false),
        (Item::Tools, false),
        (Item::Search, false),
    ] {
        assert_eq!(state(&app, cx, of).0, greyed, "{of:?}");
    }
    assert_eq!(
        state(&app, cx, Item::Commit).1,
        "Commit the transaction (no transaction open)"
    );
    app.press(cx, &["ctrl-alt-a"]);
    app.type_text(cx, "insert into kerno_ui_toolbar_tx.entries values (1)");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the open transaction", |app, cx| {
        !app.running(cx) && app.tx_state(cx) == kerno_core::TxState::Open
    });
    assert!(!state(&app, cx, Item::Commit).0 && !state(&app, cx, Item::Rollback).0);
    // The isolation level waits for the transaction's end.
    assert_eq!(
        {
            let tab = app.tab(cx);
            cx.update(|cx| tab.read(cx).isolation_why_not(cx))
        },
        Some("a transaction is open")
    );
    app.click(cx, Item::Rollback.element_id());
    app.wait_until(cx, "the rollback", |app, cx| {
        app.tx_state(cx) == kerno_core::TxState::Idle
    });
    assert_eq!(count(), "0");
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the open transaction", |app, cx| {
        !app.running(cx) && app.tx_state(cx) == kerno_core::TxState::Open
    });
    app.click(cx, Item::Commit.element_id());
    app.wait_until(cx, "the commit", |app, cx| {
        app.tx_state(cx) == kerno_core::TxState::Idle
    });
    assert_eq!(count(), "1");
}

#[gpui_kit::test]
fn the_pickers_change_the_active_tab_s_connection_and_schema(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    // Two saved connections to the test server, both open.
    let storage = memory_storage();
    let first = storage
        .save_connection(None, &testing::config(Engine::Postgres))
        .unwrap();
    let mut config = testing::config(Engine::Postgres);
    config.name = "kerno_ui_toolbar_second".into();
    let second = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    for id in [&first, &second] {
        kerno_core::secrets::save_secrets(secrets.as_ref(), id, &testing::secrets()).unwrap();
        let connections = app.connections.clone();
        cx.update(|cx| connections.update(cx, |connections, cx| connections.connect(id, cx)));
    }
    app.wait_until(cx, "both connections", |app, cx| {
        (0..2).all(|index| matches!(app.status(cx, index), Status::Connected { .. }))
    });
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_query(
                "Query 1".into(),
                "",
                Some(Target {
                    connection: first.clone(),
                    database: None,
                    schema: None,
                }),
                window,
                cx,
            );
        })
    });
    let tab = app.tab(cx);
    app.wait_until(cx, "the schemas", |_, cx| {
        cx.update(|cx| {
            tab.read(cx)
                .choices(cx)
                .schemas
                .contains(&"shop".to_string())
        })
    });
    open_list(&app, cx, "toolbar-target");
    assert!(menu_entry(&app, cx, "Database").is_some());
    click_menu_entry(&app, cx, "shop");
    app.wait_until(cx, "the schema", |_, cx| {
        cx.update(|cx| {
            tab.read(cx)
                .target()
                .and_then(|target| target.schema.clone())
        }) == Some("shop".into())
    });
    // The connection in use stays as it is when picked again.
    let name = app.first_stored().name;
    open_list(&app, cx, "toolbar-connection");
    click_menu_entry(&app, cx, &name);
    let target = cx.update(|cx| tab.read(cx).target().cloned()).unwrap();
    assert_eq!(target.schema.as_deref(), Some("shop"));
    // The other one moves the tab there, on its own database.
    open_list(&app, cx, "toolbar-connection");
    click_menu_entry(&app, cx, "kerno_ui_toolbar_second");
    let target = cx.update(|cx| tab.read(cx).target().cloned()).unwrap();
    assert_eq!((target.connection, target.schema), (second, None));
}

#[gpui_kit::test]
fn tools_lists_the_connection_s_tools(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    open_list(&app, cx, "toolbar-tools");
    for label in ["Back up…", "Global backup…", "Open dashboard", "Tasks"] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
    click_menu_entry(&app, cx, "Open dashboard");
    app.wait_until(cx, "the dashboard", |app, cx| {
        cx.update(|cx| app.workspace.read(cx).active_admin_tab().is_some())
    });
}

#[gpui_kit::test]
async fn a_right_click_hides_an_item_and_the_choice_survives_a_restart(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    assert!(app.has(cx, "toolbar-dashboard"));
    // A real right-click on the toolbar, in its empty end.
    let width = app.step(cx, |window, _| {
        window.find("main-toolbar").bounds().size.width
    });
    app.right_click(
        cx,
        "main-toolbar",
        Some(gpui_kit::point(width - px(4.), px(17.))),
    );
    for of in Item::all() {
        assert!(menu_entry(&app, cx, of.name()).is_some(), "{of:?}");
    }
    assert!(menu_entry(&app, cx, "Reset toolbar").is_some());
    click_menu_entry(&app, cx, Item::Dashboard.name());
    assert!(!app.has(cx, "toolbar-dashboard"));

    let stored = storage.clone();
    cx.wait_for(app.window.into(), Duration::from_secs(10), move |_, _| {
        Settings::load(&stored).toolbar_hidden.contains("dashboard")
    })
    .await;
    let restarted = open(cx, storage);
    assert!(!restarted.has(cx, "toolbar-dashboard"));
    assert!(restarted.has(cx, "toolbar-search"));

    // Reset toolbar brings it back.
    let width = restarted.step(cx, |window, _| {
        window.find("main-toolbar").bounds().size.width
    });
    restarted.right_click(
        cx,
        "main-toolbar",
        Some(gpui_kit::point(width - px(4.), px(17.))),
    );
    click_menu_entry(&restarted, cx, "Reset toolbar");
    assert!(restarted.has(cx, "toolbar-dashboard"));
}

#[gpui_kit::test]
fn show_main_toolbar_hides_and_shows_the_row(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    assert!(app.has(cx, "main-toolbar"));
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::ToggleMainToolbar), cx)
    });
    assert!(!app.has(cx, "main-toolbar"));
    assert!(!settings_now(cx).toolbar_visible);
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::ToggleMainToolbar), cx)
    });
    assert!(app.has(cx, "main-toolbar"));
}

#[gpui_kit::test]
fn a_narrow_window_puts_the_rightmost_items_under_more(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    assert!(app.has(cx, "toolbar-search") && !app.has(cx, "toolbar-more"));
    cx.simulate_window_resize(app.window.into(), size(px(520.), px(800.)));
    assert!(!app.has(cx, "toolbar-search"));
    // The first groups stay; the rest is under "»", greyed as on the row.
    assert!(app.has(cx, "toolbar-new-connection"));
    app.click(cx, "toolbar-more");
    assert!(menu_entry(&app, cx, "Find a table to open (no connection)").is_some());
    app.press(cx, &["escape"]);
    // Whatever the width, nothing is cut at the row's end, and "»" shows
    // only when an item is missing.
    for width in (400..=1200).step_by(20) {
        cx.simulate_window_resize(app.window.into(), size(px(width as f32), px(800.)));
        let (end, shown, more) = app.step(cx, |window, _| {
            let end = window.find("main-toolbar").bounds().right();
            let shown: Vec<_> = Item::all()
                .filter_map(|of| window.try_find(of.element_id()).map(|found| (of, found)))
                .collect();
            (end, shown, window.try_find("toolbar-more"))
        });
        for (of, found) in &shown {
            assert!(found.bounds().right() <= end, "{of:?} cut at {width} px");
        }
        if let Some(more) = &more {
            assert!(more.bounds().right() <= end, "» cut at {width} px");
        }
        assert_eq!(
            more.is_some(),
            shown.len() < Item::all().count(),
            "{width} px"
        );
    }
    assert!(app.has(cx, "toolbar-search") && !app.has(cx, "toolbar-more"));
}

#[gpui_kit::test]
fn the_toolbar_follows_a_table_s_tab_and_acts_on_its_connection(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    // Two saved connections; only the first is open, the sidebar is on the
    // second: an item that followed the sidebar would be greyed.
    let storage = memory_storage();
    let config = testing::config(Engine::Postgres);
    let first = storage.save_connection(None, &config).unwrap();
    let mut other = testing::config(Engine::Postgres);
    other.name = "kerno_ui_toolbar_second".into();
    let second = storage.save_connection(None, &other).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    for id in [&first, &second] {
        kerno_core::secrets::save_secrets(secrets.as_ref(), id, &testing::secrets()).unwrap();
    }
    let connections = app.connections.clone();
    cx.update(|cx| connections.update(cx, |connections, cx| connections.connect(&first, cx)));
    let status = |app: &Fixture, cx: &mut TestAppContext, id: &str| {
        cx.update(|cx| app.connections.read(cx).status(id))
    };
    app.wait_until(cx, "the first connection", |app, cx| {
        matches!(status(app, cx, &first), Status::Connected { .. })
    });
    let on_first = Target {
        connection: first.clone(),
        database: None,
        schema: None,
    };
    let items = TableRef::new("shop", "customers");
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_data(
                on_first.clone(),
                items.clone(),
                RelationKind::Table,
                Filter::default(),
                window,
                cx,
            );
        })
    });
    let sidebar = cx.update(|cx| app.workspace.read(cx).sidebar());
    let row = cx.update(|cx| {
        app.connections
            .read(cx)
            .saved()
            .iter()
            .position(|saved| saved.id == second)
            .unwrap()
    });
    app.click(cx, ("connection", row));
    assert_eq!(
        cx.update(|cx| sidebar.read(cx).selected(cx)),
        Some(second.clone())
    );
    assert!(cx.update(|cx| app.workspace.read(cx).active_data_tab().is_some()));

    // The pickers show the tab's place, greyed: only a query tab chooses.
    let shop = format!("shop@{}", config.name);
    assert_eq!(picker(&app, cx, Item::Connection), config.name);
    assert_eq!(picker(&app, cx, Item::Target), shop);
    for of in [Item::Connection, Item::Target] {
        let (greyed, tooltip) = state(&app, cx, of);
        assert!(greyed, "{of:?}");
        assert!(tooltip.ends_with("(query tabs only)"), "{tooltip}");
    }
    assert!(state(&app, cx, Item::Connection).1.contains(&config.name));
    // The connection items act on the tab's connection, open.
    assert_eq!(
        state(&app, cx, Item::Connect).1,
        "Connect to the server (connected)"
    );
    for of in [
        Item::Reconnect,
        Item::Disconnect,
        Item::ReadOnly,
        Item::Dashboard,
        Item::Tools,
        Item::Search,
    ] {
        assert!(!state(&app, cx, of).0, "{of:?}");
    }
    open_list(&app, cx, "toolbar-tools");
    assert!(menu_entry(&app, cx, "Back up…").is_some());
    app.press(cx, &["escape"]);
    app.click(cx, Item::Search.element_id());
    assert!(app.has(cx, "quick-open"));
    app.press(cx, &["escape"]);
    app.click(cx, Item::Dashboard.element_id());
    let admin = cx.update(|cx| app.workspace.read(cx).active_admin_tab().cloned());
    let admin = admin.expect("the dashboard");
    assert_eq!(
        cx.update(|cx| admin.read(cx).target().connection.clone()),
        first
    );
    // An admin tab shows its database.
    assert_eq!(
        picker(&app, cx, Item::Target),
        format!("kerno@{}", config.name)
    );
    assert!(state(&app, cx, Item::Target).0);

    // A structure tab, like the data tab.
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_structure(on_first.clone(), items, RelationKind::Table, window, cx);
        })
    });
    assert_eq!(picker(&app, cx, Item::Target), shop);
    assert!(state(&app, cx, Item::Connection).0);

    // Reconnect reopens the tab's connection, not the sidebar's.
    app.click(cx, Item::Reconnect.element_id());
    app.wait_until(cx, "the reconnection", |app, cx| {
        matches!(status(app, cx, &first), Status::Connected { .. })
    });
    assert_eq!(status(&app, cx, &second), Status::Disconnected);

    // The Tasks view is on no connection.
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_tasks(window, cx);
        })
    });
    assert_eq!(picker(&app, cx, Item::Connection), "No connection");
    assert_eq!(picker(&app, cx, Item::Target), "No database");

    // A query tab on a closed connection: both rows say the same.
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_query(
                "Query 1".into(),
                "",
                Some(Target {
                    connection: second.clone(),
                    database: None,
                    schema: None,
                }),
                window,
                cx,
            );
        })
    });
    assert_eq!(picker(&app, cx, Item::Target), "No database");
    assert_eq!(
        app.step(cx, |window, _| window.find("tab-database"))
            .label(),
        Some("No database")
    );
}
