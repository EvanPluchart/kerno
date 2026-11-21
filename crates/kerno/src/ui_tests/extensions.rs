//! K27 (docs/plans/M12.md): the extensions manager of a PostgreSQL database. The tree lists
//! installed extensions only; the manager lists every one the server has, with a search,
//! filters, details read from the catalog, and Install, Update, Move and Drop, each through its
//! SQL. Every test works in a scratch database of its own: the fixture database's extensions
//! never change.

use gpui_kit::{Bounds, Context, Pixels};
use kerno_core::schema::ExtensionState;

use super::*;
use crate::extensions_tab::{ExtensionsTab, Filter, Op};
use crate::navigator::actions::ToolEntry;
use crate::workspace::panes::GroupId;

impl Fixture {
    /// The extensions manager opened last.
    pub(super) fn extensions_manager(&self, cx: &mut TestAppContext) -> Entity<ExtensionsTab> {
        cx.update(|cx| self.workspace.read(cx).extensions_tabs())
            .pop()
            .expect("an extensions manager")
    }

    /// The manager opened last, once its list is read.
    fn ready_manager(&self, cx: &mut TestAppContext) -> Entity<ExtensionsTab> {
        let tab = self.extensions_manager(cx);
        self.wait_until(cx, "the extensions", |_, cx| {
            cx.update(|cx| tab.read(cx).state_text()) == "ready"
        });
        tab
    }

    /// Opens the manager of `database` as the workspace does from the tree.
    fn open_manager(&self, cx: &mut TestAppContext, database: &str) -> Entity<ExtensionsTab> {
        let connection = cx.update(|cx| self.connections.read(cx).saved()[0].id.clone());
        let target = Target {
            connection,
            database: Some(database.into()),
            schema: None,
        };
        self.step(cx, |window, cx| {
            self.workspace.update(cx, |workspace, cx| {
                workspace.open_extensions(target, None, window, cx)
            })
        });
        self.ready_manager(cx)
    }

    fn on_manager<R>(
        &self,
        cx: &mut TestAppContext,
        tab: &Entity<ExtensionsTab>,
        f: impl FnOnce(&mut ExtensionsTab, &mut gpui_kit::Window, &mut Context<ExtensionsTab>) -> R,
    ) -> R {
        self.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| f(tab, window, cx))
        })
    }

    /// The state of `name` in the manager's list.
    fn state_of(
        &self,
        cx: &mut TestAppContext,
        tab: &Entity<ExtensionsTab>,
        name: &str,
    ) -> Option<ExtensionState> {
        cx.update(|cx| {
            tab.read(cx)
                .shown()
                .iter()
                .find(|extension| extension.name == name)
                .map(|extension| extension.state())
        })
    }

    /// The texts drawn in the window's last frame.
    fn texts(&self, cx: &mut TestAppContext) -> Vec<String> {
        self.step(cx, |window, _| {
            gpui_kit::base::test_support::texts(window)
                .into_iter()
                .map(|text| text.text().to_string())
                .collect()
        })
    }

    /// The SQL the open confirmation shows.
    fn previewed_sql(&self, cx: &mut TestAppContext) -> String {
        self.texts(cx)
            .into_iter()
            .find(|text| {
                text.starts_with("CREATE EXTENSION")
                    || text.starts_with("ALTER EXTENSION")
                    || text.starts_with("DROP EXTENSION")
            })
            .unwrap_or_default()
    }
}

fn scratch_database(name: &str) -> Scratch {
    Scratch::new(
        Engine::Postgres,
        &[&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)")],
        &[&format!("CREATE DATABASE {name}")],
    )
}

/// K27: the "Extensions" folder lists installed extensions with their count; the manager opens
/// for the right database from the folder (double click, Enter, its menu), from an installed
/// extension (on it), from the database's Tools ▸ and from the palette; one tab per database.
#[gpui_kit::test]
fn the_tree_lists_installed_extensions_and_every_way_in_opens_the_manager(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = scratch_database("kerno_ui_ext_ways");
    let app = connected(cx, Engine::Postgres);
    let folder = ["kerno_ui_ext_ways", "Extensions"];
    app.open_path(cx, &["kerno_ui_ext_ways", "Extensions", "plpgsql"]);
    // Only what is installed, and how many.
    assert!(!app.shows(cx, &["kerno_ui_ext_ways", "Extensions", "citext"]));
    let hint = cx.update(|cx| {
        app.workspace
            .read(cx)
            .navigator(cx)
            .read(cx)
            .hint_of("db:kerno_ui_ext_ways\u{1f}extensions")
    });
    assert_eq!(hint.as_deref(), Some("1"));

    let opened = |app: &Fixture, cx: &mut TestAppContext| {
        cx.update(|cx| {
            app.workspace
                .read(cx)
                .extensions_tabs()
                .iter()
                .map(|tab| tab.read(cx).database().to_string())
                .collect::<Vec<_>>()
        })
    };
    // A double click on the folder.
    app.click_row(cx, &folder, true);
    assert_eq!(opened(&app, cx), ["kerno_ui_ext_ways"]);
    let tab = app.ready_manager(cx);
    assert_eq!(app.titles(cx), ["kerno_ui_ext_ways · extensions"]);
    // Enter on it, its menu, Tools ▸ on the database, the palette: the same tab.
    app.click_row(cx, &folder, false);
    app.press(cx, &["enter"]);
    app.act(cx, Action::ManageExtensions, &folder);
    let row = app.tree_row(cx, &["kerno_ui_ext_ways"]);
    let navigator = cx.update(|cx| app.workspace.read(cx).navigator(cx));
    app.step(cx, |window, cx| {
        navigator.update(cx, |navigator, cx| {
            let id = navigator.id_at(row, cx);
            navigator.perform_tool(ToolEntry::Extensions, &id, window, cx)
        })
    });
    app.click_row(cx, &["kerno_ui_ext_ways"], false);
    app.dispatch(cx, actions::ManageExtensions);
    assert_eq!(opened(&app, cx), ["kerno_ui_ext_ways"]);
    assert_eq!(app.titles(cx).len(), 1);
    // On an installed extension: the manager selects it.
    app.act(
        cx,
        Action::ShowInExtensions,
        &["kerno_ui_ext_ways", "Extensions", "plpgsql"],
    );
    assert_eq!(
        cx.update(|cx| tab.read(cx).selected().map(|e| e.name.clone())),
        Some("plpgsql".into())
    );
    assert!(app.has(cx, "extension-details"));
    // Another database: a tab of its own.
    app.click_row(cx, &["kerno"], false);
    app.dispatch(cx, actions::ManageExtensions);
    assert_eq!(opened(&app, cx), ["kerno_ui_ext_ways", "kerno"]);
}

/// K27: the search goes over names and descriptions, each filter counts what it lets through,
/// a selected extension shows where it goes, its versions, what it requires and who may
/// install it; an empty search says what to do.
#[gpui_kit::test]
fn the_manager_searches_filters_and_shows_what_an_extension_needs(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = scratch_database("kerno_ui_ext_list");
    let app = connected(cx, Engine::Postgres);
    let tab = app.open_manager(cx, "kerno_ui_ext_list");
    let counts = |cx: &mut TestAppContext| {
        cx.update(|cx| Filter::ALL.map(|filter| tab.read(cx).count(filter)))
    };
    let [all, installed, available, to_update] = counts(cx);
    assert_eq!((installed, to_update), (1, 0), "a new database has plpgsql");
    assert_eq!(all, installed + available);
    assert!(available > 20, "{available}");
    // Each filter shows what it counts.
    for (filter, count) in Filter::ALL
        .into_iter()
        .zip([all, installed, available, to_update])
    {
        app.on_manager(cx, &tab, |tab, _, cx| tab.set_filter(filter, cx));
        assert_eq!(
            cx.update(|cx| tab.read(cx).shown().len()),
            count,
            "{filter:?}"
        );
    }
    app.click(cx, ("extensions-filter", 0usize));

    // The search finds earthdistance by its description.
    app.on_manager(cx, &tab, |tab, window, cx| {
        tab.search_for("great-circle", window, cx)
    });
    let shown = cx.update(|cx| {
        tab.read(cx)
            .shown()
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(shown, ["earthdistance"]);
    assert_eq!(counts(cx), [1, 0, 1, 0]);
    app.click(cx, ("extension-row", 0usize));
    assert!(app.has(cx, "extension-details"));
    let texts = app.texts(cx);
    let has = |part: &str| texts.iter().any(|text| text.contains(part));
    assert!(has("cube (not installed: installed with it)"), "{texts:?}");
    assert!(has("Only a superuser can install it"), "{texts:?}");
    assert!(has("Can be moved to another schema."), "{texts:?}");
    assert!(has("(default)"), "{texts:?}");
    assert!(has("install its package on the server's machine first"));

    // Nothing matches: the empty state says the extension may not be on the server yet.
    app.on_manager(cx, &tab, |tab, window, cx| {
        tab.search_for("kerno_no_such_ext", window, cx)
    });
    assert!(app.has(cx, "extensions-empty"));
    assert!(
        app.texts(cx)
            .iter()
            .any(|text| text.contains("No extension matches") && text.contains("package")),
    );
    // Keys: Down selects the first row, Enter offers to install it.
    app.on_manager(cx, &tab, |tab, window, cx| {
        tab.search_for("citext", window, cx)
    });
    app.step(cx, |window, cx| {
        tab.read(cx).focus_handle(cx).focus(window, cx)
    });
    app.press(cx, &["down"]);
    assert_eq!(
        cx.update(|cx| tab.read(cx).selected().map(|e| e.name.clone())),
        Some("citext".into())
    );
    app.press(cx, &["enter"]);
    assert_eq!(app.previewed_sql(cx), "CREATE EXTENSION \"citext\"");
    app.click(cx, "sql-cancel");

    // ⌘F reaches the search from the list.
    app.on_manager(cx, &tab, |tab, window, cx| tab.search_for("", window, cx));
    app.step(cx, |window, cx| {
        tab.read(cx).focus_handle(cx).focus(window, cx)
    });
    app.press(cx, &["secondary-f"]);
    app.type_text(cx, "cube");
    let shown = cx.update(|cx| {
        tab.read(cx)
            .shown()
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
    });
    assert!(
        shown.contains(&"cube".to_string()) && shown.len() < all,
        "{shown:?}"
    );

    // A right-click on a row selects it and offers its actions and Copy name.
    let row = shown.iter().position(|name| name == "cube").unwrap();
    app.right_click(cx, ("extension-row", row), None);
    assert_eq!(
        cx.update(|cx| tab.read(cx).selected().map(|e| e.name.clone())),
        Some("cube".into())
    );
    assert!(menu_entry(&app, cx, "Install…").is_some());
    click_menu_entry(&app, cx, "Copy name");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("cube".into())
    );
}

/// K27: earthdistance installs with cube (CASCADE, said before), in the schema picked; citext
/// installs at an older version, updates to the default one and moves; a plain drop of cube
/// lists what refuses it (K14), and the tree follows each change.
#[gpui_kit::test]
fn install_with_a_requirement_update_move_and_drop_go_through_their_sql(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = scratch_database("kerno_ui_ext_ops");
    for schema in ["geo", "moved"] {
        on_database(
            Engine::Postgres,
            Some("kerno_ui_ext_ops"),
            &format!("CREATE SCHEMA {schema}"),
        );
    }
    let app = connected(cx, Engine::Postgres);
    app.open_path(cx, &["kerno_ui_ext_ops", "Extensions", "plpgsql"]);
    let tab = app.open_manager(cx, "kerno_ui_ext_ops");

    app.on_manager(cx, &tab, |tab, window, cx| {
        tab.select("earthdistance", window, cx);
        tab.choose_schema(Some("geo".into()), cx);
    });
    app.click(cx, "extension-install");
    assert_eq!(
        app.previewed_sql(cx),
        "CREATE EXTENSION \"earthdistance\" SCHEMA \"geo\" CASCADE"
    );
    assert!(
        app.texts(cx)
            .iter()
            .any(|text| text.contains("It requires “cube”") && text.contains("CASCADE")),
        "the requirement is said before"
    );
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the install", |app, cx| {
        app.state_of(cx, &tab, "cube") == Some(ExtensionState::Installed)
    });
    assert_eq!(
        app.state_of(cx, &tab, "earthdistance"),
        Some(ExtensionState::Installed)
    );
    app.wait_until(cx, "the tree", |app, cx| {
        app.shows(cx, &["kerno_ui_ext_ops", "Extensions", "earthdistance"])
            && app.shows(cx, &["kerno_ui_ext_ops", "Extensions", "cube"])
    });
    assert_eq!(
        on_database(
            Engine::Postgres,
            Some("kerno_ui_ext_ops"),
            "SELECT string_agg(e.extname || ':' || n.nspname, ',' ORDER BY e.extname) \
             FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace \
             WHERE e.extname IN ('cube', 'earthdistance')"
        ),
        "cube:geo,earthdistance:geo"
    );

    // An older citext, then the update to the default version.
    let (oldest, default) = cx.update(|cx| {
        let tab = tab.read(cx);
        let citext = tab
            .shown()
            .into_iter()
            .find(|e| e.name == "citext")
            .unwrap()
            .clone();
        (citext.versions[0].clone(), citext.default_version.unwrap())
    });
    app.on_manager(cx, &tab, |tab, window, cx| {
        tab.select("citext", window, cx);
        tab.choose_version(Some(oldest.clone()), cx);
    });
    app.click(cx, "extension-install");
    assert_eq!(
        app.previewed_sql(cx),
        format!("CREATE EXTENSION \"citext\" VERSION '{oldest}'")
    );
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the install", |app, cx| {
        app.state_of(cx, &tab, "citext") == Some(ExtensionState::UpdateAvailable)
    });
    app.on_manager(cx, &tab, |tab, _, cx| tab.set_filter(Filter::ToUpdate, cx));
    assert_eq!(cx.update(|cx| tab.read(cx).count(Filter::ToUpdate)), 1);
    app.click(cx, "extension-update");
    assert_eq!(app.previewed_sql(cx), "ALTER EXTENSION \"citext\" UPDATE");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the update", |_, cx| {
        cx.update(|cx| tab.read(cx).count(Filter::ToUpdate)) == 0
    });
    app.on_manager(cx, &tab, |tab, _, cx| tab.set_filter(Filter::All, cx));
    assert_eq!(
        on_database(
            Engine::Postgres,
            Some("kerno_ui_ext_ops"),
            "SELECT extversion FROM pg_extension WHERE extname = 'citext'"
        ),
        default
    );

    // Moved to another schema.
    app.on_manager(cx, &tab, |tab, window, cx| {
        tab.select("citext", window, cx);
        tab.ask(
            "citext",
            Op::Move {
                schema: "moved".into(),
            },
            window,
            cx,
        );
    });
    assert_eq!(
        app.previewed_sql(cx),
        "ALTER EXTENSION \"citext\" SET SCHEMA \"moved\""
    );
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the move", |_, cx| {
        cx.update(|cx| {
            tab.read(cx)
                .shown()
                .iter()
                .any(|e| e.name == "citext" && e.schema.as_deref() == Some("moved"))
        })
    });

    // A plain drop of cube: K14's list says earthdistance depends on it.
    app.on_manager(cx, &tab, |tab, window, cx| tab.select("cube", window, cx));
    app.click(cx, "extension-drop");
    assert_eq!(app.previewed_sql(cx), "DROP EXTENSION \"cube\"");
    app.wait_until(cx, "what the drop reaches", |app, cx| {
        app.has(cx, "sql-reach") && app.label_of(cx, "sql-reach").contains("earthdistance")
    });
    app.click(cx, "sql-cancel");
    // With cascade, both go, and the tree follows.
    app.click(cx, "extension-drop-cascade");
    assert_eq!(app.previewed_sql(cx), "DROP EXTENSION \"cube\" CASCADE");
    app.wait_until(cx, "the list of what the cascade drops", |app, cx| {
        app.has(cx, "sql-reach") && app.label_of(cx, "sql-reach").contains("earthdistance")
    });
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the drop", |app, cx| {
        app.state_of(cx, &tab, "earthdistance") == Some(ExtensionState::Available)
            && !app.shows(cx, &["kerno_ui_ext_ops", "Extensions", "cube"])
    });
}

/// K27: a read-only connection installs nothing (the buttons say why); a production one asks
/// twice.
#[gpui_kit::test]
fn read_only_refuses_and_production_asks_twice(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = scratch_database("kerno_ui_ext_rules");
    let installed = || {
        on_database(
            Engine::Postgres,
            Some("kerno_ui_ext_rules"),
            "SELECT count(*) FROM pg_extension WHERE extname = 'citext'",
        )
    };
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    let tab = app.open_manager(cx, "kerno_ui_ext_rules");
    app.on_manager(cx, &tab, |tab, window, cx| tab.select("citext", window, cx));
    assert!(app.has(cx, "extensions-read-only"));
    assert!(cx.update(|cx| tab.read(cx).why_not_write(cx)).is_some());
    app.click(cx, "extension-install");
    assert!(!app.has(cx, "sql-preview"));
    app.on_manager(cx, &tab, |tab, window, cx| {
        tab.ask_install("citext", window, cx)
    });
    assert!(!app.has(cx, "sql-preview"));
    // Its row menu says why, as the tree's do.
    app.on_manager(cx, &tab, |tab, window, cx| {
        tab.search_for("citext", window, cx)
    });
    app.right_click(cx, ("extension-row", 0usize), None);
    assert!(menu_entry(&app, cx, "Install… (read-only connection)").is_some());
    app.press(cx, &["escape"]);

    let mut config = testing::config(Engine::Postgres);
    config.environment = kerno_core::environment::PRODUCTION.into();
    let app = connected_as(cx, config);
    let tab = app.open_manager(cx, "kerno_ui_ext_rules");
    app.on_manager(cx, &tab, |tab, window, cx| {
        tab.ask_install("citext", window, cx)
    });
    assert!(app.has(cx, "sql-production"));
    app.click(cx, "sql-confirm");
    assert!(app.has(cx, "sql-preview") && installed() == "0");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the install", |app, cx| {
        app.state_of(cx, &tab, "citext") == Some(ExtensionState::Installed)
    });
    assert_eq!(installed(), "1");
}

/// K27: a user without the right reads why first: a trusted extension needs CREATE on the
/// database, an untrusted one a superuser; then the server's own words.
#[gpui_kit::test]
fn a_refusal_says_who_may_install_first(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = scratch_database("kerno_ui_ext_refused");
    let app = connected_as_user(cx, Engine::Postgres, "kerno_readonly", "kerno_ro_pw");
    let tab = app.open_manager(cx, "kerno_ui_ext_refused");
    let refused = |app: &Fixture, cx: &mut TestAppContext, name: &str| {
        app.on_manager(cx, &tab, |tab, window, cx| {
            tab.ask_install(name, window, cx)
        });
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the refusal", |_, cx| {
            cx.update(|cx| !tab.read(cx).is_running() && tab.read(cx).message().is_some())
        });
        let (failed, text) = cx.update(|cx| tab.read(cx).message().cloned()).unwrap();
        assert!(failed, "{text}");
        text
    };
    let text = refused(&app, cx, "citext");
    assert!(
        text.starts_with("This user may not install “citext”")
            && text.contains("“kerno_ui_ext_refused”")
            && text.contains("permission denied"),
        "{text}"
    );
    assert!(app.has(cx, "extensions-message"));
    let text = refused(&app, cx, "earthdistance");
    assert!(
        text.starts_with("Only a superuser can install “earthdistance”")
            && text.contains("permission denied"),
        "{text}"
    );
}

/// K27, K19: in a narrow group the bars wrap and the details go under the list, all inside it.
#[gpui_kit::test]
fn the_manager_stays_inside_a_narrow_group(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let tab = app.open_manager(cx, "kerno");
    app.on_manager(cx, &tab, |tab, window, cx| tab.select("hstore", window, cx));
    app.press(cx, &["secondary-t"]);
    app.dispatch(cx, actions::SplitRight);
    let ids: Vec<GroupId> = cx.update(|cx| app.workspace.read(cx).group_ids());
    assert_eq!(ids.len(), 2);
    let bounds = |app: &Fixture,
                  cx: &mut TestAppContext,
                  id: gpui_kit::ElementId|
     -> Bounds<Pixels> { app.step(cx, |window, _| window.find(id).bounds()) };
    let left = bounds(&app, cx, ("tab-group", ids[0]).into());
    let inside = |inner: Bounds<Pixels>| {
        inner.left() >= left.left() - px(1.)
            && inner.right() <= left.right() + px(1.)
            && inner.bottom() <= left.bottom() + px(1.)
    };
    for id in [
        gpui_kit::ElementId::from("extensions-refresh"),
        ("extensions-filter", 3usize).into(),
        "extension-details".into(),
        "extension-drop-cascade".into(),
    ] {
        let found = bounds(&app, cx, id.clone());
        assert!(
            inside(found),
            "{id:?} is drawn outside its group: {found:?} in {left:?}"
        );
    }
    // The details are under the list, not squeezed beside it.
    let details = bounds(&app, cx, "extension-details".into());
    let list = bounds(&app, cx, "extensions-list".into());
    assert!(
        details.top() >= list.bottom() - px(1.),
        "{details:?} {list:?}"
    );
}
