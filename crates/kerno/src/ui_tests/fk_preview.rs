//! K18 (the owner's request of 2026-11-18): the row a foreign key points
//! to, previewed in the value panel under the cell's value. Fixtures read
//! only; scratch objects `kerno_ui_k18_*`.

use gpui_kit::App;
use kerno_core::sql::CellValue;

use super::critique::Locker;
use super::tool_wizards::paints;
use super::*;
use crate::query::value_panel::{ValuePanel, reads_started};

/// How long the selection rests before a read starts, and a little more
/// (`fk_preview::REST`).
const REST: Duration = Duration::from_millis(300);

/// The value panel of the active data tab.
fn panel(app: &Fixture, cx: &mut TestAppContext) -> Entity<ValuePanel> {
    let tab = app.data(cx);
    cx.update(|cx| tab.read(cx).value_panel())
        .expect("the value panel")
}

fn read<R>(
    cx: &mut TestAppContext,
    panel: &Entity<ValuePanel>,
    read: impl FnOnce(&ValuePanel, &App) -> R,
) -> R {
    cx.update(|cx| read(panel.read(cx), cx))
}

/// Lets the selection rest, then waits for the read to end.
fn rested(app: &Fixture, cx: &mut TestAppContext, panel: &Entity<ValuePanel>) -> String {
    cx.executor().advance_clock(REST);
    app.wait_until(cx, "the referenced row", |_, cx| {
        let state = read(cx, panel, |panel, _| panel.preview_state());
        state != "resting" && state != "reading"
    });
    read(cx, panel, |panel, _| panel.preview_state())
}

fn heading(cx: &mut TestAppContext, panel: &Entity<ValuePanel>) -> Option<String> {
    read(cx, panel, |panel, _| panel.preview_heading())
}

fn fields(cx: &mut TestAppContext, panel: &Entity<ValuePanel>) -> Vec<(String, Option<String>)> {
    read(cx, panel, |panel, cx| panel.preview_fields(cx))
}

fn field(name: &str, value: &str) -> (String, Option<String>) {
    (name.to_string(), Some(value.to_string()))
}

/// Opens a table of the first connection with the value panel shown, and
/// waits for its rows.
fn open_rows(app: &Fixture, cx: &mut TestAppContext, schema: &str, table: &str) {
    app.open_table(cx, schema, table, RelationKind::Table);
    app.wait_until(cx, "the rows", |app, cx| {
        app.data_state(cx).starts_with("Rows 1–")
    });
}

/// A scratch schema `name` with `parents (id, name)` holding 1 to 4 and
/// `kids (id, parent_id → parents)`, kid n pointing to parent n.
fn family(engine: Engine, name: &str) -> Scratch {
    let (cleanup, create) = scratch_schema(engine, name);
    Scratch::new(
        engine,
        &[&cleanup],
        &[
            &create,
            &format!("CREATE TABLE {name}.parents (id INT PRIMARY KEY, name VARCHAR(20))"),
            &format!(
                "CREATE TABLE {name}.kids (id INT PRIMARY KEY, parent_id INT, \
                 FOREIGN KEY (parent_id) REFERENCES {name}.parents (id))"
            ),
            &format!(
                "INSERT INTO {name}.parents VALUES (1, 'one'), (2, 'two'), (3, 'three'), \
                 (4, 'four')"
            ),
            &format!("INSERT INTO {name}.kids VALUES (1, 1), (2, 2), (3, 3), (4, 4)"),
        ],
    )
}

/// The customer of the first order, as the server has it.
fn first_customer(engine: Engine, column: &str) -> String {
    let shop = shop(engine);
    on_server(
        engine,
        &format!(
            "SELECT c.{column} FROM {shop}.customers c WHERE c.id = \
             (SELECT customer_id FROM {shop}.orders ORDER BY id LIMIT 1)"
        ),
    )
}

/// On the three engines: an order's customer shown by name and email under
/// its `customer_id`, read once for the same key shown twice, every column
/// on demand, "Open the row" as the cell menu's jump; nothing for a column
/// in no key, nor for a NULL key.
#[gpui_kit::test]
fn the_row_a_foreign_key_points_to_is_shown_under_its_value(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let shop = shop(engine);
        let app = connected(cx, engine);
        app.dispatch(cx, actions::ToggleValuePanel);
        open_rows(&app, cx, shop, "orders");
        let grid = app.data_grid(cx);
        let panel = panel(&app, cx);

        // customer_id: nothing read before the selection rests.
        let before = reads_started();
        select_cell(&app, cx, &grid, (0, 2));
        assert_eq!(
            read(cx, &panel, |panel, _| panel.preview_state()),
            "resting"
        );
        assert_eq!(reads_started(), before, "{engine:?}");
        assert_eq!(rested(&app, cx, &panel), "row", "{engine:?}");
        assert_eq!(reads_started(), before + 1);
        assert_eq!(
            heading(cx, &panel).as_deref(),
            Some("customers, by customer_id")
        );
        assert_eq!(
            fields(cx, &panel),
            [
                field("full_name", &first_customer(engine, "full_name")),
                field("email", &first_customer(engine, "email")),
            ],
            "{engine:?}"
        );
        assert!(app.has(cx, "fk-preview-row"));

        // A column in no key: no section. Back on the key: from the cache.
        select_cell(&app, cx, &grid, (0, 1));
        assert_eq!(heading(cx, &panel), None);
        assert!(!app.has(cx, "fk-preview-heading"));
        select_cell(&app, cx, &grid, (0, 2));
        assert_eq!(read(cx, &panel, |panel, _| panel.preview_state()), "row");
        cx.executor().advance_clock(REST);
        app.step(cx, |_, _| ());
        assert_eq!(reads_started(), before + 1, "{engine:?}");

        // Every column, then the chosen ones again.
        app.click(cx, "fk-preview-all");
        let all = fields(cx, &panel);
        let count = on_server(
            engine,
            &format!(
                "SELECT count(*) FROM information_schema.columns \
                 WHERE table_schema = '{shop}' AND table_name = 'customers'"
            ),
        );
        assert_eq!(all.len().to_string(), count, "{engine:?}");
        app.click(cx, "fk-preview-all");
        assert_eq!(fields(cx, &panel).len(), 2);

        // "Open the row": the cell menu's jump.
        app.click(cx, "fk-preview-open");
        app.wait_until(cx, "the customer's tab", |app, cx| {
            app.data_state(cx) == "Rows 1–1 of 1"
        });
        let tab = app.data(cx);
        assert_eq!(
            cx.update(|cx| tab.read(cx).table().name.clone()),
            "customers"
        );

        // A NULL key (a top category): nothing under the value.
        open_rows(&app, cx, shop, "categories");
        let grid = app.data_grid(cx);
        let panel = self::panel(&app, cx);
        select_cell(&app, cx, &grid, (0, 2));
        let header = read(cx, &panel, |panel, _| panel.header()).unwrap_or_default();
        assert!(header.starts_with("parent_id · "), "{header}");
        cx.executor().advance_clock(REST);
        app.step(cx, |_, _| ());
        assert_eq!(heading(cx, &panel), None, "{engine:?}");
        assert!(!app.has(cx, "fk-preview-heading"));
    }
}

/// A query's result that is one table's, editable, knows that table's
/// foreign keys: its panel previews them too; a result of no table does
/// not.
#[gpui_kit::test]
fn an_editable_result_previews_its_foreign_keys(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.dispatch(cx, actions::ToggleValuePanel);
    app.run_in_new_tab(
        cx,
        "select * from shop.orders order by id limit 5",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| {
        app.outcomes(cx)
            .first()
            .is_some_and(|outcome| outcome.contains(" rows"))
    });
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    let panel = cx
        .update(|cx| tab.read(cx).value_panel())
        .expect("the value panel");
    // Once the table of the result is known.
    app.wait_until(cx, "an editable result", |_, cx| {
        cx.update(|cx| grid.read(cx).delegate().pending().is_some())
    });
    select_cell(&app, cx, &grid, (0, 2));
    assert_eq!(rested(&app, cx, &panel), "row");
    assert_eq!(
        fields(cx, &panel),
        [
            field("full_name", &first_customer(Engine::Postgres, "full_name")),
            field("email", &first_customer(Engine::Postgres, "email")),
        ]
    );
    // A result of one table jumps as a table's data does (K49).
    assert!(app.has(cx, "fk-preview-open"));
    app.click(cx, "fk-preview-open");
    app.wait_until(cx, "the customer", |app, cx| {
        cx.update(|cx| app.workspace.read(cx).active_data_tab().is_some())
            && app.data_state(cx).starts_with("Rows 1–1 ")
    });

    app.run_in_new_tab(cx, "select 1 as customer_id", "secondary-enter");
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    let panel = cx
        .update(|cx| tab.read(cx).value_panel())
        .expect("the value panel");
    select_cell(&app, cx, &grid, (0, 1));
    cx.executor().advance_clock(REST);
    app.step(cx, |_, _| ());
    assert_eq!(heading(cx, &panel), None);
}

/// A pending key shows the pending value's row and says so; a key that
/// points to no row says it (MySQL with its checks off); a key the server
/// computes says it is shown once applied.
#[gpui_kit::test]
fn a_pending_or_dangling_key_says_so(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_k18_keys";
        let _scratch = family(engine, schema);
        if engine == Engine::MySql {
            on_server(
                engine,
                &format!("SET FOREIGN_KEY_CHECKS = 0; INSERT INTO {schema}.kids VALUES (5, 99)"),
            );
        }
        let app = connected(cx, engine);
        app.dispatch(cx, actions::ToggleValuePanel);
        open_rows(&app, cx, schema, "kids");
        let grid = app.data_grid(cx);
        let panel = panel(&app, cx);
        select_cell(&app, cx, &grid, (0, 2));
        assert_eq!(rested(&app, cx, &panel), "row");
        assert_eq!(fields(cx, &panel), [field("name", "one")]);
        assert!(!read(cx, &panel, |panel, _| panel.preview_pending()));

        // Pending: the row of the new value, said to be pending.
        app.step(cx, |_, cx| {
            Grid::set_value(&grid, 0, 1, CellValue::Text("2".into()), cx)
        });
        assert_eq!(rested(&app, cx, &panel), "row", "{engine:?}");
        assert_eq!(fields(cx, &panel), [field("name", "two")]);
        assert!(read(cx, &panel, |panel, _| panel.preview_pending()));
        assert!(app.has(cx, "fk-preview-pending"));

        // Computed by the server: not known until applied.
        app.step(cx, |_, cx| {
            Grid::set_value(&grid, 1, 1, CellValue::Expression("DEFAULT".into()), cx)
        });
        select_cell(&app, cx, &grid, (1, 2));
        assert!(app.has(cx, "fk-preview-later"));
        assert!(paints(
            &app,
            cx,
            "the row is shown once the change is applied"
        ));

        if engine == Engine::MySql {
            select_cell(&app, cx, &grid, (4, 2));
            assert_eq!(rested(&app, cx, &panel), "no row");
            assert!(paints(
                &app,
                cx,
                "No row with this key: parents has no row where id = 99."
            ));
        }
    }
}

/// A composite key uses each of its columns, and either column shows the
/// same row, read once; read again once the rows are read again.
#[gpui_kit::test]
fn a_composite_key_uses_every_column(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let name = "kerno_ui_k18_pairs";
        let (cleanup, create) = scratch_schema(engine, name);
        let _scratch = Scratch::new(
            engine,
            &[&cleanup],
            &[
                &create,
                &format!(
                    "CREATE TABLE {name}.pairs (a INT, b INT, label VARCHAR(20), PRIMARY KEY (a, b))"
                ),
                &format!(
                    "CREATE TABLE {name}.uses (id INT PRIMARY KEY, a INT, b INT, \
                     FOREIGN KEY (a, b) REFERENCES {name}.pairs (a, b))"
                ),
                &format!("INSERT INTO {name}.pairs VALUES (1, 1, 'one one'), (1, 2, 'one two')"),
                &format!("INSERT INTO {name}.uses VALUES (1, 1, 2)"),
            ],
        );
        let app = connected(cx, engine);
        app.dispatch(cx, actions::ToggleValuePanel);
        open_rows(&app, cx, name, "uses");
        let grid = app.data_grid(cx);
        let panel = panel(&app, cx);
        let before = reads_started();
        select_cell(&app, cx, &grid, (0, 2));
        assert_eq!(rested(&app, cx, &panel), "row", "{engine:?}");
        assert_eq!(heading(cx, &panel).as_deref(), Some("pairs, by a, b"));
        assert_eq!(fields(cx, &panel), [field("label", "one two")]);
        select_cell(&app, cx, &grid, (0, 3));
        assert_eq!(rested(&app, cx, &panel), "row");
        assert_eq!(fields(cx, &panel), [field("label", "one two")]);
        assert_eq!(reads_started(), before + 1, "{engine:?}");
        // Read again once the rows are: they may have changed.
        on_server(
            engine,
            &format!("UPDATE {name}.pairs SET label = 'changed' WHERE b = 2"),
        );
        app.on_data(cx, |tab, window, cx| tab.reload(window, cx));
        app.wait_until(cx, "the rows again", |app, cx| {
            app.data_state(cx).starts_with("Rows 1–") && app.data_grid(cx) != grid
        });
        let grid = app.data_grid(cx);
        select_cell(&app, cx, &grid, (0, 3));
        assert_eq!(rested(&app, cx, &panel), "row");
        assert_eq!(fields(cx, &panel), [field("label", "changed")]);
        assert_eq!(reads_started(), before + 2, "{engine:?}");
    }
}

/// The columns picked for a table come back after a restart, for every
/// key of that table, and "Kerno's choice" goes back to the default.
#[gpui_kit::test]
fn the_columns_picked_for_a_table_come_back_after_a_restart(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.dispatch(cx, actions::ToggleValuePanel);
    open_rows(&app, cx, "shop", "orders");
    let grid = app.data_grid(cx);
    let panel = panel(&app, cx);
    select_cell(&app, cx, &grid, (0, 2));
    assert_eq!(rested(&app, cx, &panel), "row");
    app.step(cx, |_, cx| {
        panel.update(cx, |panel, cx| panel.pick_preview_column("phone", cx))
    });
    let picked: Vec<String> = fields(cx, &panel)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(picked, ["email", "full_name", "phone"]);
    let stored = app.storage.clone();
    app.wait_until(cx, "the stored choice", |_, _| {
        let key = r#"["kerno","shop","customers"]"#;
        let connection = stored.connections().unwrap()[0].id.clone();
        stored.preview_columns(&connection, key).unwrap().is_some()
            && Settings::load(&stored).value_panel
    });

    let restarted = open(cx, app.storage.clone());
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let id = cx.update(|cx| restarted.connections.read(cx).saved()[0].id.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    restarted.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    restarted.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    open_rows(&restarted, cx, "shop", "orders");
    let grid = restarted.data_grid(cx);
    let panel = self::panel(&restarted, cx);
    // Another order, another customer: the same columns.
    select_cell(&restarted, cx, &grid, (1, 2));
    assert_eq!(rested(&restarted, cx, &panel), "row");
    let names: Vec<String> = fields(cx, &panel)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["email", "full_name", "phone"]);

    restarted.step(cx, |_, cx| {
        panel.update(cx, |panel, cx| panel.automatic_preview_columns(cx))
    });
    let names: Vec<String> = fields(cx, &panel)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(names, ["full_name", "email"]);
}

/// Hidden, the panel reads nothing; shown, the selection moving quickly
/// through keys reads only the one it rests on.
#[gpui_kit::test]
fn nothing_is_read_while_hidden_or_moving(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let schema = "kerno_ui_k18_moves";
    let _scratch = family(Engine::Postgres, schema);
    let app = connected(cx, Engine::Postgres);
    open_rows(&app, cx, schema, "kids");
    let grid = app.data_grid(cx);
    let before = reads_started();
    select_cell(&app, cx, &grid, (0, 2));
    cx.executor().advance_clock(REST);
    app.step(cx, |_, _| ());
    assert_eq!(reads_started(), before);

    app.dispatch(cx, actions::ToggleValuePanel);
    let panel = panel(&app, cx);
    assert_eq!(rested(&app, cx, &panel), "row");
    assert_eq!(reads_started(), before + 1);
    for row in 1..4 {
        select_cell(&app, cx, &grid, (row, 2));
        cx.executor().advance_clock(Duration::from_millis(100));
        app.step(cx, |_, _| ());
    }
    assert_eq!(rested(&app, cx, &panel), "row");
    assert_eq!(fields(cx, &panel), [field("name", "four")]);
    assert_eq!(reads_started(), before + 2);
}

/// Whether the preview's statement waits for the lock on the server.
/// Not `Locker::blocks`: another session of the app can wait for the lock
/// too (the table's size no longer does, K71).
fn preview_waits() -> bool {
    on_server(
        Engine::Postgres,
        "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' \
         AND query LIKE 'SELECT * FROM \"kerno_ui_k18_lock\".%'",
    ) != "0"
}

/// A read waiting behind a lock is stopped on the server when the
/// selection moves on, and after 10 s; "Try again" reads it once the lock
/// is gone.
#[gpui_kit::test]
fn a_read_behind_a_lock_is_stopped(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let schema = "kerno_ui_k18_lock";
    let _scratch = family(Engine::Postgres, schema);
    let app = connected(cx, Engine::Postgres);
    app.dispatch(cx, actions::ToggleValuePanel);
    open_rows(&app, cx, schema, "kids");
    let grid = app.data_grid(cx);
    let panel = panel(&app, cx);
    let lock = Locker::start(Engine::Postgres, &format!("{schema}.parents"));

    select_cell(&app, cx, &grid, (0, 2));
    cx.executor().advance_clock(REST);
    app.wait_until(cx, "the read waiting", |_, _| preview_waits());
    assert_eq!(
        read(cx, &panel, |panel, _| panel.preview_state()),
        "reading"
    );
    // The selection moves on: the read is stopped on the server.
    select_cell(&app, cx, &grid, (0, 1));
    app.wait_until(cx, "the read stopped", |_, _| !preview_waits());

    // Too long: stopped after 10 s, and said.
    select_cell(&app, cx, &grid, (1, 2));
    cx.executor().advance_clock(REST);
    app.wait_until(cx, "the read waiting", |_, _| preview_waits());
    // The clock moves on at each try: the server shows the read waiting
    // before the client has set its 10 s timer (K65: on the Linux runner, one
    // advance came too early and the timer never fired).
    app.wait_until(cx, "the too long read stopped", |_, cx| {
        cx.executor().advance_clock(Duration::from_secs(10));
        !preview_waits()
    });
    assert_eq!(
        read(cx, &panel, |panel, _| panel.preview_state()),
        "Not read: the server took longer than 10 s."
    );
    assert!(app.has(cx, "fk-preview-stopped"));

    drop(lock);
    app.click(cx, "fk-preview-retry");
    app.wait_until(cx, "the row", |_, cx| {
        read(cx, &panel, |panel, _| panel.preview_state()) == "row"
    });
    assert_eq!(fields(cx, &panel), [field("name", "two")]);
}
