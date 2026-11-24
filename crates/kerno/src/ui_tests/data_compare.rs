//! K60 (docs/plans/M12.md, D-126): the data compare. The dialog picks two tables and says what
//! cannot be compared; the tab counts the differences, shows them one row per key with the
//! changed cells' source and target values, filters them by kind and by column, shows a row's
//! values whole and copies it; Cancel stops both reads on the server; "Open script" writes the
//! statements into a query tab on the target and runs nothing. Every test works in scratch
//! schemas `kerno_test_dc_ui_*`, dropped at the end; `analytics.big_table` is only read.

use gpui_kit::component::WindowExt as _;

use super::*;
use crate::data_compare_dialog::{self, DataCompareDialog};
use crate::data_compare_tab::{DataCompareTab, Filter, Spec};
use crate::query::grid::CopyFormat;
use kerno_core::data_compare::{Difference, Request};

/// The texts drawn in the last frame.
fn drawn(app: &Fixture, cx: &mut TestAppContext) -> Vec<String> {
    app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .into_iter()
            .map(|text| text.text().to_string())
            .collect()
    })
}

/// The same table `t` in two schemas of the test `tag`: `1` only in the source, `3` with
/// another name, `4` only in the target, `5` with another price (and so another `total`, which
/// the server computes); `2` the same. `extra_a` only in the source, `extra_b` only in the
/// target. Gives the two schemas.
fn scratch(tag: &str) -> (Scratch, String, String) {
    let (a, b) = (
        format!("kerno_test_dc_ui_{tag}_a"),
        format!("kerno_test_dc_ui_{tag}_b"),
    );
    let drop = |schema: &str| format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
    let table = |schema: &str, extra: &str| {
        format!(
            "CREATE TABLE {schema}.t (id int PRIMARY KEY, name text, price numeric(12,2), \
             total numeric GENERATED ALWAYS AS (price * 2) STORED, {extra} text)"
        )
    };
    let scratch = Scratch::new(
        Engine::Postgres,
        &[&drop(&a), &drop(&b)],
        &[
            &format!("CREATE SCHEMA {a}"),
            &table(&a, "extra_a"),
            &format!(
                "INSERT INTO {a}.t (id, name, price) VALUES (1, 'a', 1), (2, 'b', 2), \
                 (3, 'c', 3), (5, 'e', 5)"
            ),
            &format!("CREATE SCHEMA {b}"),
            &table(&b, "extra_b"),
            &format!(
                "INSERT INTO {b}.t (id, name, price) VALUES (2, 'b', 2), (3, 'C', 3), \
                 (4, 'd', 4), (5, 'e', 5.5)"
            ),
        ],
    );
    (scratch, a, b)
}

fn rows_of(schema: &str) -> String {
    on_server(
        Engine::Postgres,
        &format!("SELECT string_agg(id || ':' || name, ',' ORDER BY id) FROM {schema}.t"),
    )
}

impl Fixture {
    /// The compare of `t` in `source` with `t` in `target`, on the first connection, by `id`,
    /// every shared column compared.
    fn data_spec(&self, cx: &mut TestAppContext, source: &str, target: &str) -> Spec {
        let connection = cx.update(|cx| self.connections.read(cx).saved()[0].id.clone());
        let side = |schema: &str| Target {
            connection: connection.clone(),
            database: Some("kerno".into()),
            schema: Some(schema.into()),
        };
        Spec {
            source: side(source),
            target: side(target),
            request: Request {
                source: TableRef::new(source, "t"),
                target: TableRef::new(target, "t"),
                key: vec!["id".into()],
                columns: vec!["name".into(), "price".into(), "total".into()],
            },
        }
    }

    fn open_data_compare(&self, cx: &mut TestAppContext, spec: Spec) -> Entity<DataCompareTab> {
        self.step(cx, |window, cx| {
            self.workspace.update(cx, |workspace, cx| {
                workspace.open_data_compare(spec, window, cx)
            })
        })
    }

    /// Opens the data compare of `spec` and waits for its result.
    fn data_compare_tab(&self, cx: &mut TestAppContext, spec: Spec) -> Entity<DataCompareTab> {
        let tab = self.open_data_compare(cx, spec);
        self.wait_until(cx, "the data compare", |_, cx| {
            let state = cx.update(|cx| tab.read(cx).state_text());
            assert!(
                matches!(state.as_str(), "ready" | "loading" | "waiting"),
                "{state}"
            );
            state == "ready"
        });
        tab
    }

    fn on_data_compare<R>(
        &self,
        cx: &mut TestAppContext,
        tab: &Entity<DataCompareTab>,
        f: impl FnOnce(
            &mut DataCompareTab,
            &mut gpui_kit::Window,
            &mut gpui_kit::Context<DataCompareTab>,
        ) -> R,
    ) -> R {
        self.step(cx, |window, cx| {
            tab.update(cx, |tab, cx| f(tab, window, cx))
        })
    }

    /// The grid's rows as it draws them, a cell per column.
    fn diff_rows(&self, cx: &mut TestAppContext, tab: &Entity<DataCompareTab>) -> Vec<Vec<String>> {
        cx.update(|cx| {
            let grid = tab.read(cx).grid().expect("the grid");
            let grid = grid.read(cx);
            let delegate = grid.delegate();
            (0..delegate.rows_count(cx))
                .map(|row| {
                    (0..delegate.columns_count(cx))
                        .map(|column| delegate.cell_text(row, column, cx))
                        .collect()
                })
                .collect()
        })
    }

    fn data_script(&self, cx: &mut TestAppContext, tab: &Entity<DataCompareTab>) -> String {
        cx.update(|cx| tab.read(cx).script(cx)).expect("a script")
    }
}

/// The dialog: nothing picked from the palette; a table of the tree on both sides, refused as
/// the same table; another schema for the target reads both tables, says which columns only
/// one side has and which the server computes, ticks the others, and opens the tab. Another
/// engine family and tables without a key are refused in plain words.
#[gpui_kit::test]
fn the_dialog_picks_two_tables_and_says_what_cannot_be_compared(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    let (_scratch, a, b) = scratch("dialog");
    let keyless = "kerno_test_dc_ui_keyless";
    let _keyless = Scratch::new(
        Engine::Postgres,
        &[&format!("DROP SCHEMA IF EXISTS {keyless} CASCADE")],
        &[
            &format!("CREATE SCHEMA {keyless}"),
            &format!("CREATE TABLE {keyless}.t (id int, name text)"),
            &format!("CREATE TABLE {keyless}.u (id int, name text)"),
        ],
    );
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

    // The palette's command, with no data tab: nothing picked.
    app.step(cx, |window, cx| {
        window.dispatch_action(Box::new(actions::CompareData), cx)
    });
    assert!(app.has(cx, "data-compare-dialog-start"));
    assert!(
        drawn(&app, cx).contains(&"Pick the connection of the source and of the target.".into())
    );
    app.click(cx, "data-compare-dialog-cancel");
    assert!(!app.has(cx, "data-compare-dialog-start"));

    let connections = app.connections.clone();
    let workspace = app.workspace.downgrade();
    let source = Target {
        connection: ids[0].clone(),
        database: Some("kerno".into()),
        schema: Some(a.clone()),
    };
    let dialog: Entity<DataCompareDialog> = app.step(cx, |window, cx| {
        data_compare_dialog::open(
            connections,
            workspace,
            Some((source, "t".into())),
            window,
            cx,
        )
    });
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    let problem =
        |_: &Fixture, cx: &mut TestAppContext| cx.update(|cx| dialog.read(cx).problem(cx));
    // Both sides on the table asked from: the same table, said.
    app.wait_until(cx, "the same table", |app, cx| {
        problem(app, cx)
            .is_some_and(|text| text.starts_with("The source and the target are the same table"))
    });
    // The target in the other schema: both tables read, their columns said.
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_fields(1, "kerno", &b, "t", window, cx)
        })
    });
    app.wait_until(cx, "both tables", |app, cx| problem(app, cx).is_none());
    let notes = drawn(&app, cx)
        .into_iter()
        .find(|text| text.contains("not compared"))
        .unwrap_or_default();
    for note in [
        "Only in the source, not compared: extra_a.",
        "Only in the target, not compared: extra_b.",
        "“total”: the target's server computes it: compared, never written.",
    ] {
        assert!(notes.contains(note), "{note} in {notes}");
    }
    assert!(drawn(&app, cx).contains(&"(id)".into()));
    let request = cx.update(|cx| dialog.read(cx).request(cx)).unwrap();
    assert_eq!(request.columns, ["name", "price", "total"]);
    // A column unticked is left out (`price`, the third column both have).
    app.click(cx, ("data-compare-column", 2usize));
    let request = cx.update(|cx| dialog.read(cx).request(cx)).unwrap();
    assert_eq!(request.columns, ["name", "total"]);
    app.click(cx, ("data-compare-column", 2usize));

    // A target without a key is matched by the source's key (its uniqueness is checked as the
    // rows are read); two tables without any key are refused before a row is read.
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_fields(1, "kerno", keyless, "t", window, cx)
        })
    });
    app.wait_until(cx, "a keyless target", |app, cx| problem(app, cx).is_none());
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_fields(0, "kerno", keyless, "u", window, cx)
        })
    });
    app.wait_until(cx, "the keyless tables", |app, cx| {
        problem(app, cx).is_some_and(|text| text.starts_with("Neither table has a primary key"))
    });
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_fields(0, "kerno", &a, "t", window, cx);
            dialog.set_fields(1, "kerno", &b, "t", window, cx)
        })
    });
    app.wait_until(cx, "both tables again", |app, cx| {
        problem(app, cx).is_none()
    });

    // Another engine family: refused in words.
    app.click(cx, ("data-compare-connection", 1001usize));
    app.wait_until(cx, "the MySQL connection", |app, cx| {
        matches!(app.status(cx, 1), Status::Connected { .. })
    });
    assert_eq!(
        problem(&app, cx).as_deref(),
        Some(
            "Comparing the rows of a PostgreSQL table with those of a MySQL one is not supported \
             yet: the types do not map one to one."
        )
    );
    app.click(cx, "data-compare-dialog-start");
    assert!(
        cx.update(|cx| app.workspace.read(cx).data_compare_tabs())
            .is_empty()
    );

    // Back on PostgreSQL, Compare opens the tab.
    app.click(cx, ("data-compare-connection", 1000usize));
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_fields(1, "kerno", &b, "t", window, cx)
        })
    });
    app.wait_until(cx, "both tables", |app, cx| problem(app, cx).is_none());
    app.click(cx, "data-compare-dialog-start");
    assert!(!app.has(cx, "data-compare-dialog-start"));
    let tabs = cx.update(|cx| app.workspace.read(cx).data_compare_tabs());
    assert_eq!(tabs.len(), 1);
    let spec = cx.update(|cx| tabs[0].read(cx).spec().clone());
    assert_eq!(spec.request.source, TableRef::new(&a, "t"));
    assert_eq!(spec.request.target, TableRef::new(&b, "t"));
}

/// "Compare data with…" on a table of the tree, and on a data tab's menu, opens the dialog
/// with that table on both sides.
#[gpui_kit::test]
fn compare_data_with_is_asked_from_a_table(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, a, _) = scratch("from");
    let app = connected(cx, Engine::Postgres);
    let path = ["kerno", a.as_str(), "Tables", "t"];
    app.open_path(cx, &path);
    app.act(cx, Action::CompareData, &path);
    app.wait_until(cx, "the dialog", |app, cx| {
        drawn(app, cx)
            .iter()
            .any(|text| text.starts_with("The source and the target are the same table"))
    });
    assert_eq!(
        cx.update(|cx| app.workspace.read(cx).data_compare_tabs())
            .len(),
        0
    );
    app.step(cx, |window, cx| window.close_dialog(cx));

    app.open_table(cx, &a, "t", RelationKind::Table);
    let index = app.active(cx).unwrap();
    app.right_click(cx, ("tab-title", index), None);
    click_menu_entry(&app, cx, "Compare data with…");
    app.wait_until(cx, "the dialog from the data tab", |app, cx| {
        drawn(app, cx)
            .iter()
            .any(|text| text.starts_with("The source and the target are the same table"))
    });
}

/// The tab counts every kind of difference, shows one row per key with the changed cells as
/// `source → target`, filters by kind and by column, shows the selected row's values on both
/// sides, and copies a row as the grid copies.
#[gpui_kit::test]
fn the_tab_counts_shows_and_filters_the_differences(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, a, b) = scratch("tab");
    let app = connected(cx, Engine::Postgres);
    let spec = app.data_spec(cx, &a, &b);
    let tab = app.data_compare_tab(cx, spec);
    let counts = cx.update(|cx| tab.read(cx).counts());
    assert_eq!(
        (
            counts.only_in_source,
            counts.only_in_target,
            counts.different,
            counts.same
        ),
        (1, 1, 2, 1)
    );
    for label in [
        "All differences 4",
        "Only in source 1",
        "Only in target 1",
        "Different 2",
    ] {
        assert!(app.shows_label(cx, label), "{label}");
    }
    assert!(drawn(&app, cx).contains(&"Same 1".to_string()));
    assert_eq!(
        app.diff_rows(cx, &tab),
        [
            ["only in source", "1", "a", "1.00", "2.00"],
            ["different", "3", "c → C", "3.00", "6.00"],
            ["only in target", "4", "d", "4.00", "8.00"],
            ["different", "5", "e", "5.00 → 5.50", "10.00 → 11.00"],
        ]
    );
    // By kind, by column.
    app.click(cx, ("data-compare-filter", 2usize));
    assert_eq!(app.diff_rows(cx, &tab).len(), 1);
    app.on_data_compare(cx, &tab, |tab, window, cx| {
        tab.set_column(Some(1), window, cx)
    });
    let rows = app.diff_rows(cx, &tab);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], "5");
    app.on_data_compare(cx, &tab, |tab, window, cx| {
        tab.set_column(None, window, cx);
        tab.set_filter(Filter::Differences, window, cx);
    });
    assert_eq!(app.diff_rows(cx, &tab).len(), 4);

    // A row's values, whole, on both sides.
    app.on_data_compare(cx, &tab, |tab, _, cx| tab.select(3, cx));
    let values = drawn(&app, cx)
        .into_iter()
        .find(|text| text.starts_with("id: "))
        .unwrap_or_default();
    assert!(values.contains("price: 5.00 | 5.50"), "{values}");
    assert!(values.contains("name: e | e"), "{values}");
    // Copied as the grid copies: the row as the source has it.
    let outcome = cx.update(|cx| tab.read(cx).outcome()).unwrap();
    let five = outcome
        .rows
        .iter()
        .position(|row| row.difference == Difference::Different && row.changed.len() == 2)
        .unwrap();
    let copied = |format| cx.update(|cx| tab.read(cx).row_text(five, 0, format));
    assert_eq!(
        copied(Some(CopyFormat::Tsv)).as_deref(),
        Some("5\te\t5.00\t10.00")
    );
    assert_eq!(copied(None).as_deref(), Some("5"));
    assert_eq!(
        copied(Some(CopyFormat::SqlInsert)).as_deref(),
        Some(
            format!(
                "INSERT INTO \"{a}\".\"t\" (\"id\", \"name\", \"price\", \"total\") VALUES (5, 'e', 5.00, 10.00);"
            )
            .as_str()
        )
    );
    // The row's menu offers the copies.
    app.right_click(cx, ("data-compare-cell", (3usize << 12) | 1), None);
    for label in [
        "Copy key",
        "Copy source row",
        "Copy target row",
        "Copy as CSV",
    ] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
}

/// Cancel stops both reads, on the servers too: a million rows against ten, cancelled once
/// the first rows came.
#[gpui_kit::test]
fn cancel_stops_both_reads_on_the_servers(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let copy = "kerno_test_dc_ui_cancel";
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[&format!("DROP SCHEMA IF EXISTS {copy} CASCADE")],
        &[
            &format!("CREATE SCHEMA {copy}"),
            &format!(
                "CREATE TABLE {copy}.big_table AS SELECT * FROM analytics.big_table WHERE id <= 10"
            ),
            &format!("ALTER TABLE {copy}.big_table ADD PRIMARY KEY (id)"),
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let connection = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let side = |schema: &str| Target {
        connection: connection.clone(),
        database: Some("kerno".into()),
        schema: Some(schema.into()),
    };
    let spec = Spec {
        source: side("analytics"),
        target: side(copy),
        request: Request {
            source: TableRef::new("analytics", "big_table"),
            target: TableRef::new(copy, "big_table"),
            key: vec!["id".into()],
            columns: [
                "bucket", "small", "hash", "username", "amount", "score", "ratio", "is_even",
                "day", "at_time", "created", "seen_at", "note", "uid", "doc", "arr", "ip", "mood",
                "padding",
            ]
            .map(String::from)
            .to_vec(),
        },
    };
    // The compare's own statement: a data tab on the table reads `SELECT *`.
    let marker = "\"padding\" FROM \"analytics\".\"big_table\" ORDER BY \"id\"";
    let tab = app.open_data_compare(cx, spec);
    app.wait_until(cx, "the read on the server", |_, _| {
        running_on_server(Engine::Postgres, marker)
    });
    app.click(cx, "data-compare-cancel");
    assert_eq!(cx.update(|cx| tab.read(cx).state_text()), "cancelled");
    let read = cx.update(|cx| tab.read(cx).counts().read[0]);
    assert!(read < 1_000_000, "{read}");
    assert!(
        drawn(&app, cx)
            .iter()
            .any(|text| text.starts_with("Cancelled after")),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while running_on_server(Engine::Postgres, marker) {
        assert!(
            std::time::Instant::now() < deadline,
            "the source's read still runs on the server"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // Refresh compares again.
    app.click(cx, "data-compare-refresh");
    assert_eq!(cx.update(|cx| tab.read(cx).state_text()), "loading");
    app.on_data_compare(cx, &tab, |tab, _, cx| tab.cancel(cx));
}

/// "Open script" opens the statements in a new query tab bound to the target, exactly the
/// tab's text, and runs nothing; the deletes are comments until ticked.
#[gpui_kit::test]
fn open_script_opens_a_query_tab_on_the_target_and_runs_nothing(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (_scratch, a, b) = scratch("open");
    let app = connected(cx, Engine::Postgres);
    let spec = app.data_spec(cx, &a, &b);
    let tab = app.data_compare_tab(cx, spec.clone());
    let script = app.data_script(cx, &tab);
    for line in [
        "-- Data compare: makes test PostgreSQL · kerno.",
        &format!("INSERT INTO \"{b}\".\"t\" (\"id\", \"name\", \"price\") VALUES (1, 'a', 1.00);"),
        &format!("UPDATE \"{b}\".\"t\" SET \"name\" = 'c' WHERE \"id\" = 3;"),
        &format!("UPDATE \"{b}\".\"t\" SET \"price\" = 5.00 WHERE \"id\" = 5;"),
        "-- Rows only in the target, kept: 1.",
    ] {
        assert!(script.contains(line), "{line} in\n{script}");
    }
    app.click(cx, "data-compare-deletes");
    assert!(
        app.data_script(cx, &tab)
            .contains(&format!("DELETE FROM \"{b}\".\"t\" WHERE \"id\" = 4;"))
    );
    let script = app.data_script(cx, &tab);
    app.click(cx, "data-compare-open-script");
    let (title, text) = app.active_query(cx);
    assert_eq!(title, "Make t like t");
    assert_eq!(text, script);
    let query = app.tab(cx);
    let target = cx.update(|cx| query.read(cx).target().cloned());
    assert_eq!(target, Some(spec.target.clone()));
    assert!(app.outcomes(cx).is_empty());
    assert_eq!(rows_of(&b), "2:b,3:C,4:d,5:e");
    // The same compare again shows the tab already open.
    app.open_data_compare(cx, spec);
    assert_eq!(
        cx.update(|cx| app.workspace.read(cx).data_compare_tabs())
            .len(),
        1
    );
}

/// Two tables with the same rows say so, and there is no script to open.
#[gpui_kit::test]
fn tables_with_the_same_rows_say_so(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (c, d) = ("kerno_test_dc_ui_same_a", "kerno_test_dc_ui_same_b");
    let drop = |schema: &str| format!("DROP SCHEMA IF EXISTS {schema} CASCADE");
    let create = |schema: &str| {
        [
            format!("CREATE SCHEMA {schema}"),
            format!(
                "CREATE TABLE {schema}.t (id int PRIMARY KEY, name text, price numeric(12,2), \
                 total numeric)"
            ),
            format!("INSERT INTO {schema}.t VALUES (1, 'a', 1, 2), (2, NULL, NULL, NULL)"),
        ]
    };
    let setup: Vec<String> = create(c).into_iter().chain(create(d)).collect();
    let setup: Vec<&str> = setup.iter().map(String::as_str).collect();
    let _scratch = Scratch::new(Engine::Postgres, &[&drop(c), &drop(d)], &setup);
    let app = connected(cx, Engine::Postgres);
    let spec = app.data_spec(cx, c, d);
    let tab = app.data_compare_tab(cx, spec);
    assert!(
        drawn(&app, cx).contains(&"The two tables hold the same rows: 2 rows compared.".into())
    );
    let titles = app.titles(cx).len();
    app.click(cx, "data-compare-open-script");
    // Its command too, whatever the button says.
    app.on_data_compare(cx, &tab, |tab, _, cx| tab.open_script(cx));
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
    let spec = app.data_spec(cx, &a, &b);
    let _tab = app.data_compare_tab(cx, spec);
    assert!(app.has(cx, "data-compare-read-only"));
    app.click(cx, "data-compare-open-script");
    app.press(cx, &["secondary-shift-enter"]);
    app.wait_until(cx, "the refusal", |app, cx| !app.outcomes(cx).is_empty());
    assert_eq!(app.outcomes(cx)[0], "failed []");
    assert_eq!(rows_of(&b), "2:b,3:C,4:d,5:e");
}
