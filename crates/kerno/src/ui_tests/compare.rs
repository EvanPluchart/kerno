//! K30 (docs/plans/M12.md): the schema compare (DBeaver parity NAV-57, CM-21, CM-59,
//! TOOL-22). The dialog asks for two sides and refuses two engine families; the tab lists the
//! objects with their state, filters and counts them, puts each in the script or leaves it out,
//! and opens the script in a query tab on the target, where nothing runs by itself. Every test
//! works in scratch schemas `kerno_test_cmp_ui_*`, dropped at the end.

use super::*;
use crate::compare_dialog::{self, CompareDialog};
use crate::compare_tab::{CompareTab, Filter, Sides};

/// Two schemas of the test `tag` (tests run side by side): `lines` only in the source, `old`
/// only in the target, `orders` different, `same_t` the same. Gives the source's and the
/// target's names.
fn scratch(tag: &str) -> (Scratch, String, String) {
    let (a, b) = (
        format!("kerno_test_cmp_ui_{tag}_a"),
        format!("kerno_test_cmp_ui_{tag}_b"),
    );
    let drop = |schema: &str| format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
    let scratch = Scratch::new(
        Engine::Postgres,
        &[&drop(&a), &drop(&b)],
        &[
            &format!("CREATE SCHEMA {a}"),
            &format!(
                "CREATE TABLE {a}.orders (id int PRIMARY KEY, price numeric(12,2), note text)"
            ),
            &format!("CREATE TABLE {a}.lines (id int PRIMARY KEY)"),
            &format!("CREATE TABLE {a}.same_t (id int PRIMARY KEY)"),
            &format!("CREATE SCHEMA {b}"),
            &format!("CREATE TABLE {b}.orders (id int PRIMARY KEY, price numeric(10,2))"),
            &format!("CREATE TABLE {b}.old (id int PRIMARY KEY)"),
            &format!("CREATE TABLE {b}.same_t (id int PRIMARY KEY)"),
        ],
    );
    (scratch, a, b)
}

/// Whether `schema` has a table named `table`, on the server.
fn has_table(schema: &str, table: &str) -> bool {
    on_server(
        Engine::Postgres,
        &format!(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = '{schema}' AND table_name = '{table}'"
        ),
    ) == "1"
}

impl Fixture {
    /// `source` and `target`, two schemas of the fixture database of the first connection.
    fn sides(&self, cx: &mut TestAppContext, source: &str, target: &str) -> Sides {
        let connection = cx.update(|cx| self.connections.read(cx).saved()[0].id.clone());
        let side = |schema: &str| Target {
            connection: connection.clone(),
            database: Some("kerno".into()),
            schema: Some(schema.into()),
        };
        Sides {
            source: side(source),
            target: side(target),
        }
    }

    /// Opens the compare of `sides` as the dialog does, once both are read.
    fn compare_tab(&self, cx: &mut TestAppContext, sides: Sides) -> Entity<CompareTab> {
        let tab = self.step(cx, |window, cx| {
            self.workspace.update(cx, |workspace, cx| {
                workspace.open_compare(sides, window, cx)
            })
        });
        self.wait_until(cx, "the compare", |_, cx| {
            let state = cx.update(|cx| tab.read(cx).state_text());
            assert!(
                matches!(state.as_str(), "ready" | "loading" | "waiting"),
                "{state}"
            );
            state == "ready"
        });
        tab
    }

    fn on_compare<R>(
        &self,
        cx: &mut TestAppContext,
        tab: &Entity<CompareTab>,
        f: impl FnOnce(&mut CompareTab, &mut gpui_kit::Window, &mut gpui_kit::Context<CompareTab>) -> R,
    ) -> R {
        self.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| f(tab, window, cx))
        })
    }

    /// The texts drawn in the last frame.
    fn drawn(&self, cx: &mut TestAppContext) -> Vec<String> {
        self.step(cx, |window, _| {
            gpui_kit::base::test_support::texts(window)
                .into_iter()
                .map(|text| text.text().to_string())
                .collect()
        })
    }

    fn script_of(&self, cx: &mut TestAppContext, tab: &Entity<CompareTab>) -> String {
        cx.update(|cx| tab.read(cx).script(cx))
    }

    /// The place of `name` in the compare's list of objects.
    fn compare_row(&self, cx: &mut TestAppContext, tab: &Entity<CompareTab>, name: &str) -> usize {
        cx.update(|cx| {
            tab.read(cx)
                .items()
                .iter()
                .position(|item| item.name == name)
                .unwrap_or_else(|| panic!("no {name}"))
        })
    }
}

/// CM-21: the dialog asks for both sides, says why it cannot compare yet, refuses a PostgreSQL
/// schema against a MySQL one in plain words before reading anything, and opens the tab once
/// the sides are named. The palette and Tools ▸ open it too.
#[gpui_kit::test]
fn the_dialog_asks_for_two_sides_and_refuses_two_engine_families(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    let (_scratch, a, b) = scratch("dialog");
    let storage = memory_storage();
    for engine in [Engine::Postgres, Engine::MySql] {
        storage
            .save_connection(None, &testing::config(engine))
            .unwrap();
    }
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let ids: Vec<String> = cx.update(|cx| {
        app.connections
            .read(cx)
            .saved()
            .iter()
            .map(|s| s.id.clone())
            .collect()
    });
    for id in &ids {
        kerno_core::secrets::save_secrets(secrets.as_ref(), id, &testing::secrets()).unwrap();
    }

    // The palette's command opens the dialog with nothing picked.
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::CompareSchemas), cx)
    });
    assert!(app.has(cx, "compare-dialog-start"));
    assert!(
        app.drawn(cx)
            .contains(&"Pick the connection of the source and of the target.".to_string())
    );
    app.click(cx, "compare-dialog-cancel");
    assert!(!app.has(cx, "compare-dialog-start"));

    let connections = app.connections.clone();
    let workspace = app.workspace.downgrade();
    let dialog: Entity<CompareDialog> = app.step(cx, |window, cx| {
        compare_dialog::open(connections, workspace, None, window, cx)
    });
    // A click picks a connection and connects it.
    app.click(cx, ("compare-connection", 0usize));
    app.click(cx, ("compare-connection", 1001usize));
    app.wait_until(cx, "both connections", |app, cx| {
        (0..2).all(|index| matches!(app.status(cx, index), Status::Connected { .. }))
    });
    let refusal = "Comparing a PostgreSQL schema with a MySQL one is not supported yet: the \
                   types do not map one to one.";
    assert_eq!(
        cx.update(|cx| dialog.read(cx).problem(cx)).as_deref(),
        Some(refusal)
    );
    assert!(app.drawn(cx).contains(&refusal.to_string()));
    app.click(cx, "compare-dialog-start");
    assert!(
        cx.update(|cx| app.workspace.read(cx).compare_tabs())
            .is_empty()
    );

    // The same connection twice, on the same schema: said too.
    app.click(cx, ("compare-connection", 1000usize));
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_fields(0, "kerno", &a, window, cx);
            dialog.set_fields(1, "kerno", &a, window, cx);
        })
    });
    assert_eq!(
        cx.update(|cx| dialog.read(cx).problem(cx)).as_deref(),
        Some("The source and the target are the same schema.")
    );
    // Swapped and named, it compares: the dialog closes on the tab.
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_fields(1, "kerno", &b, window, cx);
            dialog.swap(window, cx);
        })
    });
    assert_eq!(cx.update(|cx| dialog.read(cx).problem(cx)), None);
    app.click(cx, "compare-dialog-start");
    assert!(!app.has(cx, "compare-dialog-start"));
    let tabs = cx.update(|cx| app.workspace.read(cx).compare_tabs());
    assert_eq!(tabs.len(), 1);
    let sides = cx.update(|cx| tabs[0].read(cx).sides().clone());
    assert_eq!(
        (
            sides.source.schema.as_deref(),
            sides.target.schema.as_deref()
        ),
        (Some(b.as_str()), Some(a.as_str()))
    );
}

/// NAV-57: "Compare with…" on a schema of the tree (the same entry as its Tools ▸ "Compare
/// schemas…") opens the dialog with that schema as the source.
#[gpui_kit::test]
fn compare_with_on_a_schema_makes_it_the_source(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, a, _) = scratch("tree");
    let app = connected(cx, Engine::Postgres);
    app.open_path(cx, &["kerno", &a]);
    app.act(cx, Action::CompareWith, &["kerno", &a]);
    assert!(
        app.drawn(cx)
            .contains(&"Pick the target connection.".to_string())
    );
    // The target's schema is the connection's current one.
    app.click(cx, ("compare-connection", 1000usize));
    app.click(cx, "compare-dialog-start");
    let tabs = cx.update(|cx| app.workspace.read(cx).compare_tabs());
    let sides = cx.update(|cx| tabs[0].read(cx).sides().clone());
    assert_eq!(
        (
            sides.source.schema.as_deref(),
            sides.target.schema.as_deref()
        ),
        (Some(a.as_str()), Some("public"))
    );
    assert_eq!(sides.source.database.as_deref(), Some("kerno"));
}

/// NAV-57, TOOL-22: the tab counts the objects per state with "Same" hidden at first, searches
/// names, says what differs in plain words, and the script holds only the objects ticked, with
/// the drops of what only the target has as comments until they are asked for.
#[gpui_kit::test]
fn the_tab_counts_filters_and_puts_what_is_ticked_in_the_script(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, a, b) = scratch("tab");
    let app = connected(cx, Engine::Postgres);
    let sides = app.sides(cx, &a, &b);
    let tab = app.compare_tab(cx, sides);
    let count = |app: &Fixture, cx: &mut TestAppContext, filter| {
        app.on_compare(cx, &tab, |tab, _, _| tab.count(filter))
    };
    assert_eq!(
        Filter::ALL.map(|filter| count(&app, cx, filter)),
        [3, 1, 1, 1, 1]
    );
    let drawn = app.drawn(cx);
    for row in [
        "lines only in source",
        "old only in target",
        "orders different",
    ] {
        assert!(drawn.contains(&row.to_string()), "{row} in {drawn:?}");
    }
    assert!(!drawn.contains(&"same_t same".to_string()));
    app.click(cx, ("compare-filter", 4usize));
    assert!(app.drawn(cx).contains(&"same_t same".to_string()));
    app.click(cx, ("compare-filter", 0usize));
    app.on_compare(cx, &tab, |tab, window, cx| {
        tab.search_for("ORD", window, cx)
    });
    assert_eq!(count(&app, cx, Filter::Differences), 1);
    app.on_compare(cx, &tab, |tab, window, cx| tab.search_for("", window, cx));

    // The differences of the table clicked, both definitions beside them.
    let orders = app.compare_row(cx, &tab, "orders");
    app.click(cx, ("compare-row", orders));
    let drawn = app.drawn(cx);
    assert!(
        drawn.contains(
            &"column `note` is missing in the target\ncolumn `price`: numeric(10,2) → numeric(12,2)"
                .to_string()
        ),
        "{drawn:?}"
    );
    assert!(app.has(cx, "compare-source-ddl") && app.has(cx, "compare-target-ddl"));

    let script = app.script_of(cx, &tab);
    for line in [
        &format!(
            "-- Schema compare: brings test PostgreSQL · kerno.{b} to test PostgreSQL · kerno.{a}."
        ),
        &format!("ALTER TABLE \"{b}\".\"orders\" ALTER COLUMN \"price\" TYPE numeric(12,2);"),
        &format!("CREATE TABLE \"{b}\".\"lines\""),
        &format!("-- DROP TABLE \"{b}\".\"old\";"),
    ] {
        assert!(script.contains(line.as_str()), "{line} in\n{script}");
    }
    // Left out from its checkbox, put back from its menu.
    let lines = app.compare_row(cx, &tab, "lines");
    app.click(cx, ("compare-include", lines));
    assert!(!app.script_of(cx, &tab).contains("\"lines\""));
    app.right_click(cx, ("compare-row", lines), None);
    for label in [
        "Open in source",
        "Open in target",
        "Copy name",
        "Put in the script",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    app.click_label(cx, "Put in the script");
    assert!(app.script_of(cx, &tab).contains("\"lines\""));
    // None and All act on the objects shown.
    app.click(cx, "compare-none");
    assert!(app.script_of(cx, &tab).contains("-- Nothing to change"));
    app.click(cx, "compare-all");
    // The drops, once asked for.
    app.click(cx, "compare-drops");
    let script = app.script_of(cx, &tab);
    assert!(
        script.contains(&format!("\nDROP TABLE \"{b}\".\"old\";")),
        "{script}"
    );
    // Up and down move through the list; Space ticks.
    app.press(cx, &["down"]);
    let selected = cx.update(|cx| tab.read(cx).selected().map(|item| item.name.clone()));
    assert_eq!(selected.as_deref(), Some("old"));
    app.press(cx, &["space"]);
    assert!(!app.script_of(cx, &tab).contains("\"old\""));
}

/// TOOL-22, CM-59: "Open script" opens the script in a new query tab bound to the target
/// schema, with exactly the compare's text, and runs nothing; Open in target shows a table's
/// structure.
#[gpui_kit::test]
fn open_script_opens_a_query_tab_on_the_target_and_runs_nothing(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, a, b) = scratch("open");
    let app = connected(cx, Engine::Postgres);
    let sides = app.sides(cx, &a, &b);
    let tab = app.compare_tab(cx, sides.clone());
    let script = app.script_of(cx, &tab);
    app.click(cx, "compare-open-script");
    let (title, text) = app.active_query(cx);
    assert_eq!(title, format!("Bring {b} to {a}"));
    assert_eq!(text, script);
    let query = app.tab(cx);
    let target = cx.update(|cx| query.read(cx).target().cloned());
    assert_eq!(target, Some(sides.target.clone()));
    assert!(app.outcomes(cx).is_empty());
    assert!(!has_table(&b, "lines") && has_table(&b, "old"));

    // Open in target: the table's structure, on the target.
    let titles = app.titles(cx).len();
    app.on_compare(cx, &tab, |tab, _, cx| tab.open_object("orders", 1, cx));
    assert_eq!(app.titles(cx).len(), titles + 1);
    // The same sides again show the tab already open.
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_compare(sides, window, cx)
        })
    });
    assert_eq!(
        cx.update(|cx| app.workspace.read(cx).compare_tabs()).len(),
        1
    );
}

/// NAV-57: two identical schemas say so, and there is no script to open.
#[gpui_kit::test]
fn identical_schemas_say_so(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (c, d) = ("kerno_test_cmp_ui_c", "kerno_test_cmp_ui_d");
    let drop = |schema: &str| format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[&drop(c), &drop(d)],
        &[
            &format!("CREATE SCHEMA {c}"),
            &format!("CREATE TABLE {c}.t (id int PRIMARY KEY, name text NOT NULL)"),
            &format!("CREATE SCHEMA {d}"),
            &format!("CREATE TABLE {d}.t (id int PRIMARY KEY, name text NOT NULL)"),
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let sides = app.sides(cx, c, d);
    let _tab = app.compare_tab(cx, sides);
    assert!(
        app.drawn(cx)
            .contains(&"The two schemas are identical.".to_string())
    );
    let titles = app.titles(cx).len();
    app.click(cx, "compare-open-script");
    assert_eq!(app.titles(cx).len(), titles);
}

/// The read-only rule holds where the script runs: the compare says the target is read-only,
/// the script still opens there, and running it is refused.
#[gpui_kit::test]
fn a_read_only_target_opens_the_script_and_refuses_it_there(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, a, b) = scratch("read_only");
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    let sides = app.sides(cx, &a, &b);
    let _tab = app.compare_tab(cx, sides);
    assert!(app.has(cx, "compare-read-only"));
    app.click(cx, "compare-open-script");
    app.press(cx, &["secondary-shift-enter"]);
    app.wait_until(cx, "the refusal", |app, cx| !app.outcomes(cx).is_empty());
    assert_eq!(app.outcomes(cx)[0], "failed []");
    assert!(!has_table(&b, "lines"));
}
