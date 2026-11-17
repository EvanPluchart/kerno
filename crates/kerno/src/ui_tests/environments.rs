//! Environments are the user's (CLAUDE.md §3, owner's test of 2026-11-13,
//! items 6 and 7): Settings → Environments, the picker of the connection
//! dialog, each rule at work, and the chrome a connection's environment
//! tints.

use gpui_kit::component::WindowExt as _;
use kerno_core::environment::{self, EnvironmentDef, EnvironmentRules, PRODUCTION, Tint};

use super::*;

/// A store holding `settings`.
fn storage_with(settings: &Settings) -> Arc<Storage> {
    let storage = memory_storage();
    settings.save(&storage).unwrap();
    storage
}

/// Settings with one environment of the user's added after the built-ins.
fn with_environment(def: EnvironmentDef) -> Settings {
    let mut settings = Settings::default();
    settings.environments.push(def);
    settings
}

fn own_environment(id: &str) -> EnvironmentDef {
    EnvironmentDef {
        id: id.into(),
        name: id.into(),
        color_light: Some("#7445C7".into()),
        color_dark: Some("#A98BEF".into()),
        tint: Tint::Soft,
        rules: EnvironmentRules::default(),
    }
}

/// Opens Settings → Environments.
fn open_environments(app: &Fixture, cx: &mut TestAppContext) {
    app.press(cx, &["secondary-,"]);
    let index = crate::settings_view::SECTIONS
        .iter()
        .position(|(_, label)| *label == "Environments")
        .unwrap();
    app.click(cx, ("settings-section", index));
    assert!(app.has(cx, "environments"));
}

fn environments_now(cx: &mut TestAppContext) -> Vec<EnvironmentDef> {
    settings_now(cx).environments
}

/// Waits for the connections saved by the dialog to reach the store.
fn stored_after(app: &Fixture, cx: &mut TestAppContext, count: usize) -> Vec<ConnectionConfig> {
    app.wait_until(cx, "the saved connection", |app, cx| {
        cx.update(|cx| app.connections.read(cx).saved().len()) == count
    });
    cx.update(|cx| {
        app.connections
            .read(cx)
            .saved()
            .iter()
            .map(|saved| saved.config.clone())
            .collect()
    })
}

#[gpui_kit::test]
fn a_user_environment_is_added_edited_and_picked_for_a_connection(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    open_environments(&app, cx);
    // The built-ins are listed and marked.
    for index in 0..3usize {
        assert!(app.has(cx, ("env-builtin", index)));
    }
    app.click(cx, "env-add");
    let added = environments_now(cx);
    assert_eq!(added.len(), 4);
    assert_eq!(added[3].id, "new-environment");

    // Its name, colours, tint and rules, each stored at once.
    app.click(cx, "env-name");
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "kerno_ui_env QA");
    // The palette's purple in the light theme, a typed colour in the dark.
    app.click(cx, "env-light-7");
    app.click(cx, "env-color-dark");
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "#12ab9f");
    app.click(cx, ("env-tint", 2usize));
    app.click(cx, "env-confirm-execute");
    app.click(cx, "env-auto-commit");
    app.click(cx, "env-read-only");
    app.click(cx, "env-close-idle");
    let def = environments_now(cx)[3].clone();
    assert_eq!(def.id, "new-environment", "the id never follows the name");
    assert_eq!(def.name, "kerno_ui_env QA");
    assert_eq!(def.color_light.as_deref(), Some("#7445C7"));
    assert_eq!(def.color_dark.as_deref(), Some("#12AB9F"));
    assert_eq!(def.tint, Tint::Strong);
    let rules = &def.rules;
    assert!(rules.confirm_execute && !rules.auto_commit && rules.read_only);
    assert_eq!(rules.close_idle_transactions, Some(600));
    // Stored, not only held in memory.
    app.wait_until(cx, "the stored settings", |app, _| {
        Settings::load(&app.storage).environments.get(3) == Some(&def)
    });

    // A duplicate is a new environment with the same settings.
    app.click(cx, "env-duplicate");
    let copy = environments_now(cx)[4].clone();
    assert_eq!(copy.name, "kerno_ui_env QA copy");
    assert_eq!((copy.tint, &copy.rules), (def.tint, &def.rules));
    assert_ne!(copy.id, def.id);
    app.step(cx, |window, cx| window.close_dialog(cx));

    // The connection dialog lists it; picking it for a new connection
    // makes it read-only, as its rule says.
    app.press(cx, &["secondary-n"]);
    assert!(app.has(cx, ("conn-env", 4usize)));
    app.click(cx, ("conn-env", 3usize));
    app.click(cx, "conn-name");
    app.type_text(cx, "kerno_ui_env picked");
    app.click(cx, "conn-save");
    let stored = stored_after(&app, cx, 1);
    assert_eq!(stored[0].environment, "new-environment");
    assert!(stored[0].read_only);
    // Its tabs start in manual commit, as the rule says.
    let environments = environments_now(cx);
    assert!(stored[0].starts_in_manual_commit(&environments));
}

#[gpui_kit::test]
fn built_in_environments_cannot_be_deleted(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    open_environments(&app, cx);
    for index in 0..3usize {
        app.click(cx, ("env-row", index));
        app.click(cx, "env-delete");
        assert_eq!(environments_now(cx).len(), 3);
        assert!(app.has(cx, "env-delete-reason"));
    }
    // An unused environment of the user's goes.
    app.click(cx, "env-add");
    assert!(!app.has(cx, "env-delete-reason"));
    app.click(cx, "env-delete");
    assert_eq!(
        environments_now(cx)
            .iter()
            .map(|def| def.id.as_str())
            .collect::<Vec<_>>(),
        [environment::LOCAL, environment::STAGING, PRODUCTION]
    );
}

#[gpui_kit::test]
fn an_environment_in_use_is_not_deleted(cx: &mut TestAppContext) {
    let storage = storage_with(&with_environment(own_environment("kerno_ui_env_used")));
    let config = ConnectionConfig {
        name: "kerno_ui_env user".into(),
        environment: "kerno_ui_env_used".into(),
        ..testing::config(Engine::Postgres)
    };
    storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    open_environments(&app, cx);
    app.click(cx, ("env-row", 3usize));
    app.click(cx, "env-delete");
    assert_eq!(environments_now(cx).len(), 4, "in use: kept");
    // The page says why (the wording is `deletion_refused`'s, tested in
    // core).
    assert!(app.has(cx, "env-delete-reason"));
    // Once nothing uses it, it goes.
    cx.update(|cx| {
        let id = app.connections.read(cx).saved()[0].id.clone();
        app.connections
            .update(cx, |connections, cx| connections.delete(&id, cx))
    });
    app.wait_until(cx, "the deleted connection", |app, cx| {
        cx.update(|cx| app.connections.read(cx).saved().is_empty())
    });
    assert!(!app.has(cx, "env-delete-reason"));
    app.click(cx, "env-delete");
    assert_eq!(environments_now(cx).len(), 3);
}

#[gpui_kit::test]
fn a_read_only_environment_makes_new_connections_read_only(cx: &mut TestAppContext) {
    // Local itself is made read-only by default.
    let mut settings = Settings::default();
    settings.environments[0].rules.read_only = true;
    let app = open(cx, storage_with(&settings));

    // A new connection starts as local says.
    app.press(cx, &["secondary-n"]);
    app.click(cx, "conn-name");
    app.type_text(cx, "kerno_ui_env first");
    app.click(cx, "conn-save");
    let stored = stored_after(&app, cx, 1);
    assert!(stored[0].read_only);

    // Staging is not read-only: picking it for a new one clears the flag.
    app.press(cx, &["secondary-n"]);
    app.click(cx, ("conn-env", 1usize));
    app.click(cx, "conn-name");
    app.type_text(cx, "kerno_ui_env second");
    app.click(cx, "conn-save");
    let stored = stored_after(&app, cx, 2);
    let second = stored
        .iter()
        .find(|config| config.name == "kerno_ui_env second")
        .unwrap();
    assert_eq!(
        (second.environment.as_str(), second.read_only),
        (environment::STAGING, false)
    );

    // An existing connection keeps its own flag whatever it is moved to.
    let id = cx.update(|cx| {
        app.connections
            .read(cx)
            .saved()
            .iter()
            .find(|saved| saved.config.name == "kerno_ui_env first")
            .unwrap()
            .id
            .clone()
    });
    app.sidebar_does(cx, Subject::Connection(id.clone()), SidebarAction::Edit);
    app.wait_until(cx, "the connection dialog", |app, cx| {
        app.has(cx, "conn-name")
    });
    app.click(cx, ("conn-env", 1usize));
    app.click(cx, "conn-save");
    app.wait_until(cx, "the edited connection", |app, cx| {
        cx.update(|cx| {
            app.connections
                .read(cx)
                .get(&id)
                .is_some_and(|saved| saved.config.environment == environment::STAGING)
        })
    });
    assert!(cx.update(|cx| app.connections.read(cx).get(&id).unwrap().config.read_only));
}

#[gpui_kit::test]
fn the_chrome_takes_the_environment_colour(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let mut ids = Vec::new();
    for (name, environment) in [
        ("kerno_ui_env local", environment::LOCAL),
        ("kerno_ui_env staging", environment::STAGING),
        ("kerno_ui_env production", PRODUCTION),
    ] {
        let config = ConnectionConfig {
            name: name.into(),
            environment: environment.into(),
            ..testing::config(Engine::Postgres)
        };
        ids.push(storage.save_connection(None, &config).unwrap());
    }
    let tabs: Vec<TabSnapshot> = ids
        .iter()
        .enumerate()
        .map(|(index, id)| TabSnapshot::Query {
            title: format!("Query {}", index + 1),
            sql: String::new(),
            target: Some(Target {
                connection: id.clone(),
                database: None,
                schema: None,
            }),
            file: None,
            unsaved: false,
            source: None,
        })
        .collect();
    storage.save_tabs(&tabs).unwrap();
    let app = open(cx, storage);
    let builtins = environment::builtin_environments();
    let chrome = |app: &Fixture, cx: &mut TestAppContext| {
        cx.update(|cx| app.workspace.read(cx).active_chrome(cx))
    };

    for appearance in [Appearance::Light, Appearance::Dark] {
        cx.update(|cx| crate::theme::apply(appearance, None, cx));
        let dark = appearance == Appearance::Dark;
        // Each tab title of a tinted connection carries its colour; local's
        // does not.
        assert!(!app.has(cx, ("tab-env", 0usize)));
        assert!(app.has(cx, ("tab-env", 1usize)));
        assert!(app.has(cx, ("tab-env", 2usize)));

        // Local: no frame, a plain status bar.
        app.click(cx, ("tab-title", 0usize));
        assert_eq!(chrome(&app, cx), None);
        assert!(!app.has(cx, "env-frame"));

        // Staging and production frame the editor, tint the status bar and
        // the toolbar, in this theme's colour of their environment.
        for (tab, id) in [(1usize, environment::STAGING), (2, PRODUCTION)] {
            app.click(cx, ("tab-title", tab));
            assert!(app.has(cx, "env-frame"), "{id} {appearance:?}");
            let shown = chrome(&app, cx).expect("a tinted chrome");
            let color = environment::environment(&builtins, id).color(dark).unwrap();
            assert_eq!(
                shown.frame,
                gpui_kit::rgb(color).into(),
                "{id} {appearance:?}"
            );
            assert_ne!(
                shown.bar,
                cx.update(|cx| cx.theme().status_bar),
                "{id} {appearance:?}"
            );
        }
        // Production is stronger than staging: its bar is the colour itself
        // in light; in dark a deep fill of the same hue under the light text.
        let production = chrome(&app, cx).unwrap();
        if dark {
            assert!(production.bar.l < production.frame.l, "{production:?}");
            let turn = (production.bar.h - production.frame.h).abs();
            assert!(turn.min(1.0 - turn) < 0.02, "{production:?}");
        } else {
            assert_eq!(production.bar, production.frame);
        }
    }
}

#[gpui_kit::test]
fn confirm_execute_asks_before_every_run(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    // Production confirms every run, as DBeaver's production type does.
    let config = ConnectionConfig {
        environment: PRODUCTION.into(),
        // About confirming, not about transactions.
        auto_commit: Some(true),
        ..testing::config(Engine::Postgres)
    };
    let app = connected_as(cx, config);
    app.run_in_new_tab(
        cx,
        "select 41 + 1 as kerno_ui_env_answer",
        "secondary-enter",
    );
    assert!(app.has(cx, "sql-preview"), "a read is confirmed too");
    // A read asks once: only writes ask twice.
    assert!(!app.has(cx, "sql-production"));
    assert!(app.outcomes(cx).is_empty(), "nothing ran before the click");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert_eq!(app.result(cx, 0, 1).1[0][0].as_deref(), Some("42"));

    // A user environment without the rule runs at once.
    let mut settings = settings_now(cx);
    settings.environments[2].rules.confirm_execute = false;
    cx.update(|cx| AppState::update_settings(cx, |stored| *stored = settings));
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "select 43");
    app.press(cx, &["secondary-enter"]);
    assert!(!app.has(cx, "sql-preview"));
    app.wait_until(cx, "the result", |app, cx| {
        app.outcomes(cx) == ["1 rows"] && app.result(cx, 0, 1).1[0][0].as_deref() == Some("43")
    });
}

#[gpui_kit::test]
fn confirm_edits_asks_before_applying(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_env_edits");
    let _scratch = Scratch::new(
        engine,
        &[&drop_schema],
        &[
            &create_schema,
            "CREATE TABLE kerno_ui_env_edits.t (id int PRIMARY KEY, name text)",
            "INSERT INTO kerno_ui_env_edits.t VALUES (1, 'a')",
        ],
    );
    // Even an environment stored with the rule off: Kerno always shows the
    // changes as SQL first (CLAUDE.md §1).
    let mut def = own_environment("kerno_ui_env_lax");
    def.rules.confirm_edits = false;
    let storage = storage_with(&with_environment(def));
    let config = ConnectionConfig {
        environment: "kerno_ui_env_lax".into(),
        ..testing::config(engine)
    };
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    app.open_table(cx, "kerno_ui_env_edits", "t", RelationKind::Table);
    app.wait_for_rows(cx, "Rows 1–1 of 1");
    // The environment's colour frames the grid of the table's data too.
    assert!(app.has(cx, "env-frame"));
    assert!(
        cx.update(|cx| app.workspace.read(cx).active_chrome(cx))
            .is_some()
    );
    app.type_in_cell(cx, 0, 2, "b", "enter");
    assert_eq!(app.pending(cx), 1);
    app.click(cx, "data-apply");
    assert!(app.has(cx, "sql-preview"), "the changes are shown first");
    assert_eq!(
        on_server(engine, "SELECT name FROM kerno_ui_env_edits.t"),
        "a"
    );
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the applied change", |_, _| {
        on_server(engine, "SELECT name FROM kerno_ui_env_edits.t") == "b"
    });
}

#[gpui_kit::test]
fn an_idle_transaction_is_rolled_back_after_the_period(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let (drop_schema, create_schema) = scratch_schema(engine, "kerno_ui_env_idle");
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                "CREATE TABLE kerno_ui_env_idle.t (id INT PRIMARY KEY)",
            ],
        );
        let count = || on_server(engine, "SELECT count(*) FROM kerno_ui_env_idle.t");
        // Production: manual commit, idle transactions ended after 600 s.
        let config = ConnectionConfig {
            environment: PRODUCTION.into(),
            ..testing::config(engine)
        };
        let app = connected_as(cx, config);
        let open = |app: &Fixture, cx: &mut TestAppContext| {
            app.status_bar(cx).contains(&"transaction open".to_string())
        };
        let rolled_back = |app: &Fixture, cx: &mut TestAppContext| {
            let tab = app.tab(cx);
            cx.update(|cx| tab.read(cx).messages())
                .iter()
                .any(|message| message.contains("left idle for 10 minutes: it was rolled back"))
        };
        let wait = |cx: &mut TestAppContext, seconds: u64| {
            cx.executor().advance_clock(Duration::from_secs(seconds));
            cx.run_until_parked();
        };

        // A transaction that only read is left alone.
        app.run_in_new_tab(cx, "select 1", "secondary-enter");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the read", |app, cx| {
            app.outcomes(cx) == ["1 rows"] && !app.running(cx) && open(app, cx)
        });
        wait(cx, 601);
        assert!(open(&app, cx) && !rolled_back(&app, cx), "{engine:?}");

        // One that wrote is rolled back once idle for the period.
        app.press(cx, &["secondary-a"]);
        app.type_text(cx, "insert into kerno_ui_env_idle.t values (1)");
        app.press(cx, &["secondary-enter"]);
        // Production asks twice before a write.
        app.click(cx, "sql-confirm");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the insert", |app, cx| {
            app.outcomes(cx) == ["done, 1 affected"] && !app.running(cx) && open(app, cx)
        });
        // Each statement restarts the wait.
        wait(cx, 300);
        app.press(cx, &["secondary-a"]);
        app.type_text(cx, "select count(*) from kerno_ui_env_idle.t");
        app.press(cx, &["secondary-enter"]);
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the count", |app, cx| {
            app.outcomes(cx) == ["1 rows"] && !app.running(cx)
        });
        wait(cx, 400);
        assert!(
            open(&app, cx) && !rolled_back(&app, cx),
            "{engine:?}: restarted"
        );
        wait(cx, 201);
        app.wait_until(cx, "the rollback", |app, cx| {
            !open(app, cx) && rolled_back(app, cx)
        });
        // The row was never committed, and is gone for the tab too.
        assert_eq!(count(), "0", "{engine:?}");
        app.press(cx, &["secondary-a"]);
        app.type_text(cx, "select count(*) from kerno_ui_env_idle.t");
        app.press(cx, &["secondary-enter"]);
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the count", |app, cx| {
            app.outcomes(cx) == ["1 rows"] && !app.running(cx)
        });
        assert_eq!(app.result(cx, 0, 1).1[0][0].as_deref(), Some("0"));
        // End what this read opened before the scratch objects are dropped.
        let tab = app.tab(cx);
        cx.update(|cx| {
            tab.update(cx, |tab, cx| {
                tab.finish_transaction(false, None, cx).detach()
            })
        });
        app.wait_until(cx, "the end of the transaction", |app, cx| !open(app, cx));
    }
}
