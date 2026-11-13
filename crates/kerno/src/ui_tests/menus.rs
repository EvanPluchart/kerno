//! A real right-click on the surfaces of CLAUDE.md §3 "Context menus
//! everywhere" that no other test right-clicks: the objects of the schema
//! tree and the rows, messages and summary items of the status bar (GOAL
//! criterion 2).

use super::*;

/// A navigator entry as its menu draws it: with the reason it cannot be
/// used, when it cannot.
fn entry(app: &Fixture, cx: &mut TestAppContext, action: Action, path: &[&str]) -> String {
    match app.why_not(cx, action, path) {
        Some(reason) => format!("{} ({reason})", action.label()),
        None => action.label().to_string(),
    }
}

#[gpui_kit::test]
fn each_kind_of_navigator_node_answers_a_right_click_with_its_menu(cx: &mut TestAppContext) {
    use crate::navigator::actions::Action::*;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_menu_nodes CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_menu_nodes",
            "CREATE TABLE kerno_ui_menu_nodes.t (id int PRIMARY KEY, note text)",
            "CREATE FUNCTION kerno_ui_menu_nodes.f() RETURNS int LANGUAGE sql AS 'SELECT 1'",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let schema = ["kerno", "kerno_ui_menu_nodes"];
    let tables = ["kerno", "kerno_ui_menu_nodes", "Tables"];
    let table = ["kerno", "kerno_ui_menu_nodes", "Tables", "t"];
    let column = [
        "kerno",
        "kerno_ui_menu_nodes",
        "Tables",
        "t",
        "Columns",
        "note",
    ];
    app.open_path(cx, &column);
    let functions = ["kerno", "kerno_ui_menu_nodes", "Functions"];
    app.open_path(cx, &functions);
    app.toggle(cx, &functions);
    // A function is listed with its arguments.
    let function_row = |app: &Fixture, cx: &mut TestAppContext| {
        app.tree(cx).into_iter().find_map(|line| {
            line.strip_prefix(&row(3, ""))
                .filter(|label| label.starts_with("f("))
                .map(str::to_string)
        })
    };
    app.wait_until(cx, "the function", |app, cx| {
        function_row(app, cx).is_some()
    });
    let function = function_row(&app, cx).unwrap();
    let function = [
        "kerno",
        "kerno_ui_menu_nodes",
        "Functions",
        function.as_str(),
    ];

    // What CLAUDE.md §4 B4 lists for each kind of node.
    let menus: [(&[&str], Vec<Action>); 6] = [
        (
            &["kerno"],
            vec![NewQueryHere, CreateSchema, CreateDatabase, CopyName, Drop],
        ),
        (
            &schema,
            vec![
                NewQueryHere,
                CreateTable,
                CreateView,
                CreateSchema,
                ShowDdl,
                CopyName,
                Rename,
                Drop,
            ],
        ),
        (&tables, vec![CreateTable, ImportCsv, Refresh]),
        (
            &table,
            vec![
                OpenData,
                OpenStructure,
                NewQueryHere,
                Generate(Template::Select),
                Generate(Template::Insert),
                Generate(Template::Update),
                Generate(Template::Delete),
                ShowDdl,
                CopyName,
                Rename,
                Truncate,
                Drop,
                Export,
            ],
        ),
        (
            &column,
            vec![
                EditColumn,
                OpenStructure,
                CopyName,
                RenameColumn,
                DropColumn,
            ],
        ),
        (
            &function,
            vec![NewQueryHere, ShowDdl, EditSource, CopyName, Rename, Drop],
        ),
    ];
    assert!(menu_entry(&app, cx, "Copy name").is_none());
    for (path, actions) in menus {
        let node = app.tree_row(cx, path);
        app.right_click(cx, ("node", node), None);
        for action in actions {
            let label = entry(&app, cx, action, path);
            assert!(
                menu_entry(&app, cx, &label).is_some(),
                "{path:?}: {label:?}"
            );
        }
        app.press(cx, &["escape"]);
        assert!(menu_entry(&app, cx, "Copy name").is_none(), "{path:?}");
    }

    // An entry runs from the menu: the table's SELECT, in a new tab.
    let node = app.tree_row(cx, &table);
    app.right_click(cx, ("node", node), None);
    click_menu_entry(&app, cx, "Generate SELECT");
    app.wait_until(cx, "the SELECT", |app, cx| app.titles(cx) == ["t"]);
    let (_, sql) = app.active_query(cx);
    assert!(
        sql.starts_with("SELECT") && sql.contains("FROM \"kerno_ui_menu_nodes\".\"t\""),
        "{sql}"
    );
}

#[gpui_kit::test]
fn the_rows_item_of_the_status_bar_answers_a_right_click(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g as kerno_ui_status_rows from generate_series(1, 3) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["3 rows"]);
    assert!(menu_entry(&app, cx, "Copy").is_none());
    app.right_click(cx, "status-rows", None);
    click_menu_entry(&app, cx, "Copy");
    // It copies what it says.
    let copied = app.clipboard(cx).unwrap_or_default();
    assert!(copied.starts_with('3'), "{copied:?}");
    assert!(app.status_bar(cx).contains(&copied), "{copied:?}");
}

#[gpui_kit::test]
fn the_messages_item_of_the_status_bar_answers_a_right_click(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "do $$ begin raise notice 'kerno_ui_status_messages'; end $$",
        "secondary-enter",
    );
    app.wait_until(cx, "the message", |app, cx| {
        app.status_bar(cx).contains(&"1 message".to_string())
    });
    let tab = app.tab(cx);
    // The result of the block is shown, not its messages.
    assert_eq!(cx.update(|cx| tab.read(cx).active_result()), 0);
    app.right_click(cx, "status-messages", None);
    click_menu_entry(&app, cx, "Show messages");
    // The messages come after the one result tab.
    assert_eq!(cx.update(|cx| tab.read(cx).active_result()), 1);
    app.right_click(cx, "status-messages", None);
    click_menu_entry(&app, cx, "Copy");
    assert_eq!(app.clipboard(cx).as_deref(), Some("1 message"));
}

#[gpui_kit::test]
fn the_summary_item_of_the_status_bar_answers_a_right_click(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    assert_eq!(app.status_bar(cx), ["Not connected"]);
    app.right_click(cx, "status-summary", None);
    assert!(menu_entry(&app, cx, "New connection…").is_some());
    click_menu_entry(&app, cx, "Copy");
    assert_eq!(app.clipboard(cx).as_deref(), Some("Not connected"));
}
