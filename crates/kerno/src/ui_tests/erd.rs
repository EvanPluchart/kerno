//! The ER diagram tab (CLAUDE.md §4 J18; DBeaver #36058, Beekeeper #492,
//! #1160): opened from the schema tree, the structure tab and the palette;
//! its edges are the fixture's foreign keys on the three engines; a click
//! and Enter, or a double click, open a table; the three exports write
//! files that parse; the tab comes back after a restart.

use std::path::PathBuf;

use super::*;
use crate::erd_tab::{DiagramTab, ExportFormat};

/// The fixture's tables of the shop and their foreign keys, as
/// `(key, referencing table, referenced table)`, per engine.
fn fixture_keys(engine: Engine) -> Vec<(String, String, String)> {
    let keys: [(&str, &str, &str); 7] = if engine == Engine::Postgres {
        [
            ("categories_parent_id_fkey", "categories", "categories"),
            ("order_items_order_id_fkey", "order_items", "orders"),
            ("order_items_product_id_fkey", "order_items", "products"),
            ("orders_customer_id_fkey", "orders", "customers"),
            ("products_category_id_fkey", "products", "categories"),
            ("reviews_customer_id_fkey", "reviews", "customers"),
            ("reviews_product_id_fkey", "reviews", "products"),
        ]
    } else {
        [
            ("categories_parent_fk", "categories", "categories"),
            ("order_items_order_fk", "order_items", "orders"),
            ("order_items_product_fk", "order_items", "products"),
            ("orders_customer_fk", "orders", "customers"),
            ("products_category_fk", "products", "categories"),
            ("reviews_customer_fk", "reviews", "customers"),
            ("reviews_product_fk", "reviews", "products"),
        ]
    };
    keys.iter()
        .map(|(key, from, to)| (key.to_string(), from.to_string(), to.to_string()))
        .collect()
}

const SHOP_TABLES: [&str; 7] = [
    "audit_log",
    "categories",
    "customers",
    "order_items",
    "orders",
    "products",
    "reviews",
];

impl Fixture {
    /// The active tab, which shows a diagram.
    fn diagram(&self, cx: &mut TestAppContext) -> Entity<DiagramTab> {
        cx.update(|cx| {
            let workspace = self.workspace.read(cx);
            let index = workspace.active_tab_index().expect("an active tab");
            workspace.diagram_tab_at(index).expect("a diagram tab")
        })
    }

    /// Waits until the active diagram tab has drawn its tables.
    fn wait_for_diagram(&self, cx: &mut TestAppContext) -> Entity<DiagramTab> {
        let tab = self.diagram(cx);
        self.wait_until(cx, "the diagram", |_, cx| {
            let state = cx.update(|cx| tab.read(cx).state());
            assert!(!state.starts_with("failed"), "{state}");
            state.starts_with("ready")
        });
        tab
    }
}

/// The edges between the fixture's shop tables, sorted.
fn shop_edges(tab: &Entity<DiagramTab>, cx: &mut TestAppContext) -> Vec<(String, String, String)> {
    let mut edges: Vec<_> = cx
        .update(|cx| tab.read(cx).edges())
        .into_iter()
        .filter(|(_, from, to)| {
            SHOP_TABLES.contains(&from.as_str()) && SHOP_TABLES.contains(&to.as_str())
        })
        .collect();
    edges.sort();
    edges
}

#[gpui_kit::test]
fn a_schema_diagram_opens_from_the_tree_with_the_foreign_keys_as_edges(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        // PostgreSQL: the schema; MySQL and MariaDB: the database.
        let (path, title): (&[&str], _) = if engine == Engine::Postgres {
            (&["kerno", "shop"], "shop · diagram")
        } else {
            (&["kerno"], "kerno · diagram")
        };
        app.open_path(cx, path);
        let node = app.tree_row(cx, path);
        app.right_click(cx, ("node", node), None);
        assert!(menu_entry(&app, cx, "View diagram").is_some(), "{engine:?}");
        click_menu_entry(&app, cx, "View diagram");
        assert_eq!(app.titles(cx), [title], "{engine:?}");
        let tab = app.wait_for_diagram(cx);
        assert_eq!(shop_edges(&tab, cx), fixture_keys(engine), "{engine:?}");
        if engine == Engine::Postgres {
            // The schema holds the fixture's tables only: every edge and
            // table is one of them; `analytics.events → shop.customers`
            // leads out of the schema and is not drawn.
            let all = cx.update(|cx| tab.read(cx).edges()).len();
            assert_eq!(all, 7);
            for name in SHOP_TABLES {
                assert!(
                    cx.update(|cx| tab.read(cx).node_of(name)).is_some(),
                    "{name}"
                );
            }
        }
        // Each table is drawn, with its key marks.
        assert!(app.has(cx, ("erd-table", 0usize)), "{engine:?}");
    }
}

#[gpui_kit::test]
fn a_click_and_enter_or_a_double_click_open_a_table_of_the_diagram(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.open_path(cx, &["kerno", "shop"]);
    app.act(cx, Action::ViewDiagram, &["kerno", "shop"]);
    let tab = app.wait_for_diagram(cx);
    // The diagram is fitted to the window: every table is in view.
    let zoom = cx.update(|cx| tab.read(cx).zoom());
    assert!(zoom > 0.1 && zoom <= 1., "{zoom}");

    let customers = cx.update(|cx| tab.read(cx).node_of("customers")).unwrap();
    app.click(cx, ("erd-table", customers));
    assert_eq!(cx.update(|cx| tab.read(cx).selected()), Some(customers));
    app.press(cx, &["enter"]);
    assert_eq!(app.titles(cx), ["shop · diagram", "customers"]);
    let data = app.data(cx);
    assert_eq!(
        cx.update(|cx| data.read(cx).table().clone()),
        TableRef::new("shop", "customers")
    );

    // Back to the diagram: a double click opens another table's data.
    app.click(cx, ("tab-title", 0usize));
    let orders = cx.update(|cx| tab.read(cx).node_of("orders")).unwrap();
    app.step(cx, |window, cx| {
        window.double_click(("erd-table", orders), cx)
    });
    assert_eq!(app.titles(cx), ["shop · diagram", "customers", "orders"]);

    // The right-click menu of a table opens its structure.
    app.click(cx, ("tab-title", 0usize));
    app.right_click(cx, ("erd-table", orders), None);
    for entry in ["Open data", "Open structure", "Copy name"] {
        assert!(menu_entry(&app, cx, entry).is_some(), "{entry}");
    }
    click_menu_entry(&app, cx, "Open structure");
    assert_eq!(
        app.titles(cx),
        [
            "shop · diagram",
            "customers",
            "orders",
            "orders · structure"
        ]
    );

    // Dragging a table moves it, for this session only: no write.
    app.click(cx, ("tab-title", 0usize));
    let before = cx.update(|cx| tab.read(cx).rect(orders));
    let start = app.step(cx, |window, _| {
        window.find(("erd-table", orders)).bounds().center()
    });
    let end = start + gpui_kit::point(px(40.), px(30.));
    app.step(cx, |window, cx| window.drag(start, end, cx));
    let after = cx.update(|cx| tab.read(cx).rect(orders));
    assert!(
        after.x > before.x && after.y > before.y,
        "{before:?} → {after:?}"
    );
}

#[gpui_kit::test]
fn the_three_exports_write_files_that_parse(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.open_path(cx, &["kerno", "shop"]);
    app.act(cx, Action::ViewDiagram, &["kerno", "shop"]);
    let tab = app.wait_for_diagram(cx);
    let dir = tempfile::tempdir().unwrap();
    let mut written = Vec::new();
    for (format, name) in [
        (ExportFormat::Svg, "shop.svg"),
        (ExportFormat::Png, "shop.png"),
        (ExportFormat::Mermaid, "shop.mmd"),
    ] {
        let path = dir.path().join(name);
        let target = path.clone();
        app.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| tab.export_to(format, target, window, cx))
        });
        app.wait_until(cx, name, |_, cx| {
            cx.update(|cx| tab.read(cx).last_export().cloned()) == Some(Ok(path.clone()))
        });
        written.push(path);
    }
    let svg = std::fs::read_to_string(&written[0]).unwrap();
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).unwrap();
    assert!(tree.size().width() > 100.);
    assert!(svg.contains(">orders</text>") && svg.contains(">PK</text>"));
    let png = image::load_from_memory(&std::fs::read(&written[1]).unwrap()).unwrap();
    // Twice the SVG's size, for sharp text.
    assert_eq!(png.width() as f32, (tree.size().width() * 2.).ceil());
    let mermaid = std::fs::read_to_string(&written[2]).unwrap();
    assert!(mermaid.starts_with("erDiagram\n"), "{mermaid}");
    assert!(
        mermaid.contains("\"shop.customers\" ||--o{ \"shop.orders\" : \"orders_customer_id_fkey\"")
    );
    // A copy for whoever wants to look at them (the main session's review).
    if let Some(samples) = std::env::var_os("KERNO_ERD_SAMPLES") {
        for path in &written {
            std::fs::copy(
                path,
                PathBuf::from(&samples).join(path.file_name().unwrap()),
            )
            .unwrap();
        }
    }
}

#[gpui_kit::test]
fn the_structure_tab_and_the_palette_draw_a_table_and_its_neighbours(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let connection = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let target = Target {
        connection,
        database: None,
        schema: None,
    };
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_structure(
                target,
                TableRef::new("shop", "orders"),
                RelationKind::Table,
                window,
                cx,
            )
        })
    });
    app.wait_until(cx, "the structure", |app, cx| {
        app.has(cx, "structure-diagram")
    });
    app.click(cx, "structure-diagram");
    assert_eq!(app.titles(cx), ["orders · structure", "orders · diagram"]);
    let tab = app.wait_for_diagram(cx);
    // orders references customers; order_items references orders. Its key
    // to products leads out of the diagram.
    let mut edges = cx.update(|cx| tab.read(cx).edges());
    edges.sort();
    assert_eq!(
        edges,
        [
            (
                "order_items_order_id_fkey".into(),
                "order_items".into(),
                "orders".into()
            ),
            (
                "orders_customer_id_fkey".into(),
                "orders".into(),
                "customers".into()
            ),
        ]
    );
    assert_eq!(cx.update(|cx| tab.read(cx).state()), "ready: 3 tables");

    // The palette's command, from the structure tab, shows the same tab.
    app.click(cx, ("tab-title", 0usize));
    app.dispatch(cx, actions::ViewDiagram);
    assert_eq!(app.titles(cx), ["orders · structure", "orders · diagram"]);
    assert_eq!(app.active(cx), Some(1));
}

#[gpui_kit::test]
fn a_schema_without_tables_says_so(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_erd_empty CASCADE"],
        &["CREATE SCHEMA kerno_ui_erd_empty"],
    );
    let app = connected(cx, Engine::Postgres);
    let path = ["kerno", "kerno_ui_erd_empty"];
    app.open_path(cx, &path);
    app.act(cx, Action::ViewDiagram, &path);
    let tab = app.wait_for_diagram(cx);
    assert!(app.has(cx, "erd-empty"));
    assert_eq!(cx.update(|cx| tab.read(cx).state()), "ready: 0 tables");
}

#[gpui_kit::test]
async fn the_diagram_tab_comes_back_after_a_restart(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.open_path(cx, &["kerno", "shop"]);
    app.act(cx, Action::ViewDiagram, &["kerno", "shop"]);
    app.wait_for_diagram(cx);
    let stored = app
        .wait_for_stored_tabs(cx, |tabs| {
            tabs.iter()
                .any(|tab| matches!(tab, TabSnapshot::Diagram { .. }))
        })
        .await;
    assert!(
        matches!(&stored[..], [TabSnapshot::Diagram { schema, table: None, .. }] if schema == "shop"),
        "{stored:?}"
    );

    // The next launch shows the tab; it draws once connected.
    let restarted = open(cx, app.storage.clone());
    assert_eq!(restarted.titles(cx), ["shop · diagram"]);
    assert!(restarted.has(cx, "erd-waiting"));
    let id = cx.update(|cx| restarted.connections.read(cx).saved()[0].id.clone());
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    restarted.click(cx, "erd-connect");
    let tab = restarted.wait_for_diagram(cx);
    assert_eq!(shop_edges(&tab, cx), fixture_keys(Engine::Postgres));
}
