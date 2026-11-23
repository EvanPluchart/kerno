//! K59 (docs/plans/M12.md): the field that filters a query's result on the
//! server proposes the result's columns and the words of a condition, as a
//! table's WHERE field does (K40); Show Completions opens the list in both.
//! Fixtures read only; scratch objects `kerno_test_k59_*`.

use super::*;

/// Whether the result filter's list is open, and its labels and details.
fn proposals(app: &Fixture, cx: &mut TestAppContext) -> (bool, Vec<(String, String)>) {
    let tab = app.tab(cx);
    cx.update(|cx| {
        let menu = tab
            .read(cx)
            .result_filter()
            .read(cx)
            .completion_menu_state();
        let items = menu
            .items
            .iter()
            .map(|item| (item.label.clone(), item.detail.clone().unwrap_or_default()))
            .collect();
        (menu.open, items)
    })
}

fn filter_text(app: &Fixture, cx: &mut TestAppContext) -> String {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).typed_result_filter(cx))
}

/// The result's filter proposes its columns with their types, an alias
/// and a name that needs quotes too (inserted quoted), then the words of a
/// condition, never Kerno's alias of the derived table; Tab and Enter take
/// a proposal, Escape closes the list, Enter with it closed applies the
/// filter; Ctrl+Space opens the list again.
#[gpui_kit::test]
fn the_result_filter_proposes_the_results_columns(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            continue;
        }
        let schema = "kerno_test_k59_filter";
        let quoted = match engine {
            Engine::Postgres => "\"full name\"",
            _ => "`full name`",
        };
        let (drop_schema, create_schema) = scratch_schema(engine, schema);
        let _scratch = Scratch::new(
            engine,
            &[&drop_schema],
            &[
                &create_schema,
                &format!(
                    "CREATE TABLE {schema}.account (id INT PRIMARY KEY, provider_id INT, \
                     {quoted} VARCHAR(20))"
                ),
                &format!(
                    "INSERT INTO {schema}.account VALUES (1, 1, 'x'), (2, 2, 'x'), (3, 3, 'y')"
                ),
            ],
        );
        let app = connected(cx, engine);
        app.run_in_new_tab(
            cx,
            &format!(
                "select a.id, a.provider_id, a.{quoted}, a.id * 2 doubled from {schema}.account a"
            ),
            "secondary-enter",
        );
        app.wait_until(cx, "the accounts", |app, cx| {
            app.outcomes(cx) == ["3 rows"] && !app.running(cx)
        });
        app.click(cx, "result-filter");
        app.type_text(cx, "pro");
        let (open, items) = proposals(&app, cx);
        assert!(open, "{engine:?}: {items:?}");
        let provider = items
            .iter()
            .find(|(label, _)| label == "provider_id")
            .unwrap_or_else(|| panic!("{engine:?}: {items:?}"));
        assert!(
            provider.1.to_lowercase().contains("int"),
            "{engine:?}: {provider:?}"
        );
        // Escape closes the list and leaves the text.
        app.press(cx, &["escape"]);
        assert!(!proposals(&app, cx).0, "{engine:?}");
        assert_eq!(filter_text(&app, cx), "pro");
        // Tab takes the first proposal of a longer word.
        app.type_text(cx, "v");
        assert_eq!(proposals(&app, cx).1[0].0, "provider_id", "{engine:?}");
        app.press(cx, &["tab"]);
        assert_eq!(filter_text(&app, cx), "provider_id", "{engine:?}");
        // What a condition is made of; not the rest of a statement, not the
        // derived table's alias.
        let words = |app: &Fixture, cx: &mut TestAppContext, typed: &str| {
            app.type_text(cx, typed);
            let (open, items) = proposals(app, cx);
            if open {
                app.press(cx, &["escape"]);
            }
            for _ in 0..typed.len() {
                app.press(cx, &["backspace"]);
            }
            items
                .into_iter()
                .map(|(label, _)| label)
                .collect::<Vec<_>>()
        };
        app.type_text(cx, " ");
        let i = words(&app, cx, "i");
        for word in ["IN", "IS NULL", "IS NOT NULL"] {
            assert!(i.iter().any(|label| label == word), "{engine:?}: {i:?}");
        }
        assert_eq!(
            i.iter().any(|label| label == "ILIKE"),
            engine == Engine::Postgres,
            "{engine:?}: {i:?}"
        );
        let o = words(&app, cx, "o");
        assert!(o.iter().any(|label| label == "OR"), "{engine:?}: {o:?}");
        assert!(
            !o.iter().any(|label| label == "ORDER BY"),
            "{engine:?}: {o:?}"
        );
        let k = words(&app, cx, "ker");
        assert!(
            !k.iter().any(|label| label == "kerno_q"),
            "{engine:?}: {k:?}"
        );
        assert_eq!(filter_text(&app, cx), "provider_id ");
        // With the list closed, Enter applies the filter.
        app.type_text(cx, "> 1");
        if proposals(&app, cx).0 {
            app.press(cx, &["escape"]);
        }
        app.press(cx, &["enter"]);
        app.wait_until(cx, "the filtered accounts", |app, cx| {
            app.outcomes(cx) == ["2 rows"] && !app.running(cx)
        });
        assert_eq!(filter_text(&app, cx), "provider_id > 1");
        // A name that needs quotes is inserted quoted; Enter takes it. A
        // column the statement named itself is proposed too.
        app.click(cx, "result-filter");
        app.press(cx, &["end"]);
        app.type_text(cx, " and full");
        let (open, items) = proposals(&app, cx);
        assert!(open, "{engine:?}");
        assert_eq!(items[0].0, "full name", "{engine:?}: {items:?}");
        app.press(cx, &["enter"]);
        assert_eq!(
            filter_text(&app, cx),
            format!("provider_id > 1 and {quoted}"),
            "{engine:?}"
        );
        // Only that: the half-typed condition is not run (it was on MySQL).
        assert!(!app.running(cx), "{engine:?}");
        assert_eq!(app.outcomes(cx), ["2 rows"], "{engine:?}");
        app.type_text(cx, " = 'y' and dou");
        let (open, items) = proposals(&app, cx);
        assert!(open, "{engine:?}: {items:?}");
        assert_eq!(items[0].0, "doubled", "{engine:?}: {items:?}");
        // Escape closes it, Show Completions opens it again.
        app.press(cx, &["escape"]);
        assert!(!proposals(&app, cx).0, "{engine:?}");
        app.press(cx, &["ctrl-space"]);
        let (open, items) = proposals(&app, cx);
        assert!(open, "{engine:?}: Ctrl+Space");
        assert_eq!(items[0].0, "doubled", "{engine:?}: {items:?}");
        app.press(cx, &["enter"]);
        app.type_text(cx, " = 6");
        app.press(cx, &["enter"]);
        app.wait_until(cx, "the one account", |app, cx| {
            app.outcomes(cx) == ["1 rows"] && !app.running(cx)
        });
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
    }
}

/// Show Completions (Ctrl+Space) opens the list of a table's WHERE field,
/// empty or not: it was the query editor's only.
#[gpui_kit::test]
fn show_completions_opens_the_list_of_a_tables_where_field(cx: &mut TestAppContext) {
    for (engine, schema) in [(Engine::Postgres, "shop"), (Engine::MySql, "kerno")] {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        app.open_table(cx, schema, "customers", RelationKind::Table);
        app.wait_until(cx, "the customers", |app, cx| {
            app.data_state(cx).starts_with("Rows 1–")
        });
        app.click(cx, "filter-raw");
        let menu = |app: &Fixture, cx: &mut TestAppContext| {
            let tab = app.data(cx);
            cx.update(|cx| {
                let menu = tab.read(cx).raw_filter().read(cx).completion_menu_state();
                let labels: Vec<String> = menu.items.iter().map(|i| i.label.clone()).collect();
                (menu.open, labels)
            })
        };
        assert!(!menu(&app, cx).0, "{engine:?}");
        app.press(cx, &["ctrl-space"]);
        let (open, labels) = menu(&app, cx);
        assert!(open, "{engine:?}: {labels:?}");
        assert!(
            labels.iter().any(|label| label == "email"),
            "{engine:?}: {labels:?}"
        );
        app.press(cx, &["escape"]);
        app.type_text(cx, "em");
        app.press(cx, &["escape"]);
        assert!(!menu(&app, cx).0, "{engine:?}");
        app.press(cx, &["ctrl-space"]);
        let (open, labels) = menu(&app, cx);
        assert!(open, "{engine:?}");
        assert_eq!(labels[0], "email", "{engine:?}: {labels:?}");
        // Enter takes it and does not apply "em".
        app.press(cx, &["enter"]);
        let tab = app.data(cx);
        assert_eq!(
            cx.update(|cx| tab.read(cx).raw_filter().read(cx).value().to_string()),
            "email"
        );
        assert_eq!(
            cx.update(|cx| tab.read(cx).applied_filter().clone()),
            kerno_core::browse::Filter::default(),
            "{engine:?}"
        );
        app.step(cx, |window, cx| {
            window.dispatch_action(Box::new(actions::CloseTab), cx)
        });
    }
}
