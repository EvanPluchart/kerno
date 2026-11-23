//! K53 (D-122): the assistant asked from a right-click, against the stand-in endpoint on
//! 127.0.0.1 (`kerno_core::testing::FakeAi`), the fixture read only. Each entry opens the panel
//! with its subject as a chip and its message ready; the endpoint gets no connection before the
//! user sends; what is sent is the preview byte for byte; a result's values go only when ticked;
//! "Generate test data…" opens its SQL in a new tab and runs nothing; off, every entry says so.

use super::ai::{answer, enable, failure, panel, wait_for_details};
use super::*;
use crate::ai_view::ask::Entry as AiEntry;
use kerno_core::ai::{self, ask::Ask};
use kerno_core::testing::FakeAi;

/// The end of every fixture customer's email: a cell value, never a name.
const ROW_VALUE: &str = "@example.com";
const ANSWER: &str = "It holds the orders.\n```sql\nSELECT count(*) FROM shop.orders;\n```";
const TEST_DATA: &str = "Here:\n```sql\nINSERT INTO shop.orders (customer_id, placed_at) \
                         VALUES (1, now());\n```\nOne row.";

/// The question the active query tab's conversation is about, once asked.
fn asked(app: &Fixture, cx: &mut TestAppContext) -> Option<Ask> {
    let tab = cx.update(|cx| app.workspace.read(cx).active_query_tab().cloned())?;
    let panel = cx.update(|cx| tab.read(cx).ai_panel().cloned())?;
    cx.update(|cx| panel.read(cx).asked().cloned())
}

/// The bytes Send would send, once the catalog and the structures are read.
fn ready_preview(app: &Fixture, cx: &mut TestAppContext) -> String {
    wait_for_details(app, cx);
    app.wait_until(cx, "the message ready to send", |app, cx| {
        panel(app, cx).is_some_and(|panel| {
            cx.update(|cx| {
                panel.read(cx).payload(cx).is_some() && !panel.read(cx).reading_details()
            })
        })
    });
    let panel = panel(app, cx).unwrap();
    cx.update(|cx| ai::preview(&panel.read(cx).payload(cx).unwrap()))
}

fn sent_after_enter(app: &Fixture, cx: &mut TestAppContext, fake: &FakeAi) -> String {
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the answer", |app, cx| {
        if let Some(failure) = failure(app, cx) {
            panic!("{failure}");
        }
        answer(app, cx).is_some()
    });
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    String::from_utf8(requests[0].body.clone()).unwrap()
}

/// Opens AI ▸ of the tree row at `path` with a real right-click and picks `entry`.
fn tree_ai(app: &Fixture, cx: &mut TestAppContext, path: &[&str], entry: &str) {
    let row = app.tree_row(cx, path);
    app.right_click(cx, ("node", row), None);
    click_menu_entry(app, cx, "AI");
    click_menu_entry(app, cx, entry);
}

fn query_tabs(app: &Fixture, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| app.workspace.read(cx).tab_snapshots(cx))
        .iter()
        .filter(|tab| matches!(tab, TabSnapshot::Query { .. }))
        .count()
}

/// The first line of a table described in the context, and where it is.
fn place(preview: &str, table: &str) -> usize {
    preview
        .find(&format!("\\n- {table} (table)"))
        .unwrap_or_else(|| panic!("{table} not described:\n{preview}"))
}

/// The tree's "Explain this table", with no query tab open: a new query tab on the table's
/// schema, the panel on it with the subject chip and the keyboard in the field; the message
/// says the tables it links to; the subject is described first, in full; nothing goes before
/// Enter; Enter sends exactly what the preview shows.
#[gpui_kit::test]
fn explain_this_table_from_the_tree_is_ready_and_sent_only_by_enter(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(ANSWER);
    let app = connected(cx, Engine::Postgres);
    enable(cx, &fake.base(), true);
    let orders = ["kerno", "shop", "Tables", "orders"];
    app.open_path(cx, &orders);
    assert_eq!(query_tabs(&app, cx), 0);
    let row = app.tree_row(cx, &orders);
    app.right_click(cx, ("node", row), None);
    click_menu_entry(&app, cx, "AI");
    for label in [
        "Ask about this table…",
        "Explain this table",
        "Write a query on this table…",
        "Generate test data…",
    ] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
    click_menu_entry(&app, cx, "Explain this table");
    app.wait_until(cx, "the conversation", |app, cx| asked(app, cx).is_some());
    assert_eq!(query_tabs(&app, cx), 1);
    let tab = app.tab(cx);
    let target = cx.update(|cx| tab.read(cx).target().cloned()).unwrap();
    assert_eq!(target.schema.as_deref(), Some("shop"));
    assert!(matches!(asked(&app, cx), Some(Ask::ExplainTable(_))));
    assert!(app.has(cx, "ai-panel") && app.has(cx, "ai-ask-subject"));
    assert!(
        app.has(cx, "ai-sample-rows"),
        "a table's own rows are the sample rows'"
    );
    let preview = ready_preview(&app, cx);
    for text in [
        "Explain what the table shop.orders holds",
        "It references: shop.customers.",
        "Tables that reference it: shop.order_items.",
        // K50's sketch of the subject, read in full.
        "\\n- shop.orders (table)\\n  id bigint not null\\n  customer_id bigint not null",
        "\\n  primary key (id)",
        "\\n  foreign key (customer_id) references shop.customers (id)",
    ] {
        assert!(preview.contains(text), "{text} missing:\n{preview}");
    }
    // The subject first, before the tables it links to and those the words name.
    let first = preview.find("\\n- ").unwrap();
    assert_eq!(first, place(&preview, "shop.orders"), "{preview}");
    assert!(place(&preview, "shop.orders") < place(&preview, "shop.customers"));
    assert!(place(&preview, "shop.customers") < place(&preview, "shop.order_items"));
    for never in ["nextval", "EUR", ROW_VALUE] {
        assert!(!preview.contains(never), "{never}:\n{preview}");
    }
    cx.run_until_parked();
    assert_eq!(fake.connections(), 0, "nothing goes before Enter");
    // Enter in the message field sends: the keyboard is there.
    let sent = sent_after_enter(&app, cx, &fake);
    assert_eq!(sent, preview);
    assert!(asked(&app, cx).is_none(), "the subject goes once");
}

/// "Generate test data…" on a read-only connection: the entry says it only writes text; the
/// number of rows is capped; the message names the tables the rows depend on; the answer's SQL
/// opens in a new query tab on the connection, and nothing runs.
#[gpui_kit::test]
fn generate_test_data_opens_its_sql_in_a_new_tab_and_runs_nothing(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(TEST_DATA);
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    enable(cx, &fake.base(), true);
    let orders = ["kerno", "shop", "Tables", "orders"];
    app.open_path(cx, &orders);
    let label = "Generate test data… (read-only connection: writes text, runs nothing)";
    tree_ai(&app, cx, &orders, label);
    app.wait_until(cx, "the conversation", |app, cx| asked(app, cx).is_some());
    assert!(app.has(cx, "ai-ask-caution"));
    // Typed past the cap: 100 rows are asked for.
    app.click(cx, "ai-ask-rows-input");
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "500");
    let preview = ready_preview(&app, cx);
    for text in [
        "Write PostgreSQL INSERT statements that add 100 rows of realistic test data to the \
         table shop.orders",
        "through its foreign keys: use key values that exist there, or insert rows into them \
         first, in the same block: shop.customers.",
    ] {
        assert!(preview.contains(text), "{text} missing:\n{preview}");
    }
    let history = app.storage.history(None, "", 1000).unwrap().len();
    app.click(cx, "ai-send");
    app.wait_until(cx, "the answer", |app, cx| answer(app, cx).is_some());
    assert_eq!(fake.requests()[0].body, preview.as_bytes());
    let tabs = query_tabs(&app, cx);
    app.click(cx, "ai-new-tab");
    assert_eq!(query_tabs(&app, cx), tabs + 1);
    let (_, sql) = app.active_query(cx);
    assert_eq!(
        sql,
        "INSERT INTO shop.orders (customer_id, placed_at) VALUES (1, now());"
    );
    assert!(!app.running(cx));
    assert!(app.outcomes(cx).is_empty());
    cx.run_until_parked();
    assert_eq!(app.storage.history(None, "", 1000).unwrap().len(), history);
}

/// A query's result: "Explain this result" sends the statement, the column names and types and
/// the row count, no value of a row until its tick; "Ask about the selection…" says how many
/// cells go and sends them only once ticked. The palette's "AI: Explain This Result" asks the
/// same of the result on screen.
#[gpui_kit::test]
fn a_result_goes_without_its_values_until_ticked(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(ANSWER);
    let app = connected(cx, Engine::Postgres);
    enable(cx, &fake.base(), true);
    let statement = "select id, email from shop.customers order by id limit 3";
    app.run_in_new_tab(cx, statement, "secondary-enter");
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["3 rows"]);
    app.right_click(cx, "table-cell-0:1", None);
    click_menu_entry(&app, cx, "AI");
    click_menu_entry(&app, cx, "Explain this result");
    app.wait_until(cx, "the conversation", |app, cx| asked(app, cx).is_some());
    assert_eq!(
        query_tabs(&app, cx),
        1,
        "on the query tab's own conversation"
    );
    assert!(app.has(cx, "ai-ask-values") && !app.has(cx, "ai-sample-rows"));
    let preview = ready_preview(&app, cx);
    for text in [
        "Explain this result",
        statement,
        "Columns (name type):\\n- id int8\\n- email ",
        "Rows: 3 rows.",
        "No value of its rows is sent.",
    ] {
        assert!(preview.contains(text), "{text} missing:\n{preview}");
    }
    assert!(!preview.contains(ROW_VALUE), "{preview}");
    app.click(cx, "ai-ask-values");
    let ticked = ready_preview(&app, cx);
    assert!(
        ticked.contains("Its first 3 rows, which I chose to send:"),
        "{ticked}"
    );
    assert!(ticked.contains(ROW_VALUE), "{ticked}");
    // The palette asks the same, on the result on screen; the tick is off again.
    app.dispatch(cx, actions::AiExplainResult);
    assert!(!ready_preview(&app, cx).contains(ROW_VALUE));

    // Two rows by two columns selected; a right-click inside keeps them.
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    cx.update(|cx| {
        grid.update(cx, |table, cx| {
            table.set_selected_cell(0, 1, cx);
            table.delegate_mut().extend_to(1, 2);
        })
    });
    app.right_click(cx, "table-cell-1:2", None);
    click_menu_entry(&app, cx, "AI");
    click_menu_entry(&app, cx, "Ask about the selection…");
    app.wait_until(cx, "the selection", |app, cx| {
        matches!(asked(app, cx), Some(Ask::Selection { .. }))
    });
    assert!(app.shows_label(cx, "Include the selected values (4 cells)"));
    // It needs a question first.
    let panel = panel(&app, cx).unwrap();
    assert!(cx.update(|cx| panel.read(cx).payload(cx).is_none()));
    app.type_text(cx, "what do these have in common");
    let preview = ready_preview(&app, cx);
    assert!(
        preview.contains("I selected 4 cells; their values are not sent."),
        "{preview}"
    );
    assert!(!preview.contains(ROW_VALUE), "{preview}");
    app.click(cx, "ai-ask-values");
    let preview = ready_preview(&app, cx);
    assert!(
        preview.contains("The selected values, which I chose to send (4 of 4 cells):"),
        "{preview}"
    );
    assert_eq!(preview.matches(ROW_VALUE).count(), 2, "{preview}");
    cx.run_until_parked();
    assert_eq!(fake.connections(), 0);
    let sent = sent_after_enter(&app, cx, &fake);
    assert_eq!(sent, preview);
}

/// MySQL and MariaDB: a table's data tab (its AI button, a column header) and the tree's column,
/// each in a query tab on the table's database; the palette's "AI: Explain This Table" on the data
/// tab.
#[gpui_kit::test]
fn columns_and_a_data_tab_ask_in_the_tables_database(cx: &mut TestAppContext) {
    for engine in [Engine::MySql, Engine::MariaDb] {
        if testing::available(engine) {
            columns_and_a_data_tab_on(engine, cx);
        }
    }
}

fn columns_and_a_data_tab_on(engine: Engine, cx: &mut TestAppContext) {
    let fake = FakeAi::start(ANSWER);
    let app = connected(cx, engine);
    enable(cx, &fake.base(), true);
    app.open_table(cx, "kerno", "orders", RelationKind::Table);
    app.wait_until(cx, "the rows", |app, cx| app.has(cx, "data-ai"));
    app.click(cx, "data-ai");
    click_menu_entry(&app, cx, "Explain this table");
    app.wait_until(cx, "the conversation", |app, cx| asked(app, cx).is_some());
    let tab = app.tab(cx);
    let target = cx.update(|cx| tab.read(cx).target().cloned()).unwrap();
    assert_eq!(target.database.as_deref(), Some("kerno"));
    let preview = ready_preview(&app, cx);
    assert!(
        preview.contains("Explain what the table kerno.orders holds"),
        "{preview}"
    );
    assert!(
        preview.contains("\\n- kerno.orders (table)\\n  id "),
        "{preview}"
    );
    assert!(
        preview.contains("It references: kerno.customers."),
        "{preview}"
    );

    // A column header of the data tab: its column, in its table's context.
    app.open_table(cx, "kerno", "orders", RelationKind::Table);
    let grid = app.data_grid(cx);
    let header = cx.update(|cx| {
        let grid = grid.read(cx).delegate();
        (1..=grid.layout().len())
            .find(|&column| {
                grid.source_column(column)
                    .is_some_and(|source| grid.store().columns()[source].name == "status")
            })
            .unwrap()
    });
    app.right_click(cx, ("column-header", header), None);
    click_menu_entry(&app, cx, "AI");
    click_menu_entry(&app, cx, "Explain these columns");
    app.wait_until(cx, "the columns", |app, cx| {
        matches!(asked(app, cx), Some(Ask::Columns { .. }))
    });
    let preview = ready_preview(&app, cx);
    assert!(
        preview.contains("Explain these columns of the table kerno.orders: ")
            && preview.contains("\\n- status "),
        "{preview}"
    );
    assert!(
        !preview.contains("\\n- customer_id "),
        "only the column picked"
    );

    // The tree's column.
    let column = ["kerno", "Tables", "orders", "Columns", "placed_at"];
    app.open_path(cx, &column);
    tree_ai(&app, cx, &column, "Explain these columns");
    app.wait_until(cx, "the tree's column", |app, cx| {
        matches!(asked(app, cx), Some(Ask::Columns { columns, .. }) if columns[0].0 == "placed_at")
    });

    // The palette on the data tab: its table.
    app.open_table(cx, "kerno", "orders", RelationKind::Table);
    let tabs = query_tabs(&app, cx);
    app.dispatch(cx, actions::AiExplainTable);
    app.wait_until(cx, "the palette's question", |app, cx| {
        query_tabs(app, cx) == tabs + 1 && matches!(asked(app, cx), Some(Ask::ExplainTable(_)))
    });
    cx.run_until_parked();
    assert_eq!(fake.connections(), 0);
}

/// The structure tab's AI button: the table's entries, and "Explain these columns" on the column
/// selected in Columns (greyed with why before one is).
#[gpui_kit::test]
fn the_structure_tab_asks_about_its_table_and_selected_column(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(ANSWER);
    let app = connected(cx, Engine::Postgres);
    enable(cx, &fake.base(), true);
    let orders = ["kerno", "shop", "Tables", "orders"];
    app.open_path(cx, &orders);
    app.act(cx, Action::OpenStructure, &orders);
    app.wait_until(cx, "the structure", |app, cx| app.has(cx, "structure-ai"));
    let tab = cx
        .update(|cx| app.workspace.read(cx).active_structure_tab().cloned())
        .unwrap();
    app.wait_until(cx, "the columns", |_, cx| {
        cx.update(|cx| tab.read(cx).grid(List::Columns).is_some())
    });
    app.click(cx, "structure-ai");
    assert!(app.shows_label(cx, "Explain these columns (select a column in Columns)"));
    app.press(cx, &["escape"]);
    let columns = cx.update(|cx| tab.read(cx).grid(List::Columns)).unwrap();
    cx.update(|cx| columns.update(cx, |table, cx| table.set_selected_row(2, cx)));
    app.click(cx, "structure-ai");
    click_menu_entry(&app, cx, "Explain these columns");
    app.wait_until(cx, "the column", |app, cx| asked(app, cx).is_some());
    let Some(Ask::Columns { table, columns, .. }) = asked(&app, cx) else {
        panic!("{:?}", asked(&app, cx));
    };
    assert_eq!(table, Some(TableRef::new("shop", "orders")));
    assert_eq!(
        columns,
        [("status".to_string(), "shop.order_status".to_string())]
    );
    cx.run_until_parked();
    assert_eq!(fake.connections(), 0);
}

/// Off: every AI entry (tree, grid, data tab, palette) says how to turn the assistant on and
/// opens nothing; the endpoint gets no connection.
#[gpui_kit::test]
fn off_every_entry_says_so_and_nothing_opens(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(ANSWER);
    let app = connected(cx, Engine::Postgres);
    enable(cx, &fake.base(), false);
    let off = "(turn on in Settings → AI)";
    let orders = ["kerno", "shop", "Tables", "orders"];
    app.open_path(cx, &orders);
    let row = app.tree_row(cx, &orders);
    app.right_click(cx, ("node", row), None);
    click_menu_entry(&app, cx, "AI");
    for entry in AiEntry::TABLE {
        let label = format!("{} {off}", entry.label());
        assert!(menu_entry(&app, cx, &label).is_some(), "{label}");
    }
    click_menu_entry(&app, cx, &format!("Explain this table {off}"));
    cx.run_until_parked();
    assert_eq!(query_tabs(&app, cx), 0, "a greyed entry opens nothing");
    app.press(cx, &["escape"]);

    app.run_in_new_tab(cx, "select 1 as n", "secondary-enter");
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    app.right_click(cx, "table-cell-0:1", None);
    click_menu_entry(&app, cx, "AI");
    for entry in [AiEntry::ExplainResult, AiEntry::AskSelection] {
        let label = format!("{} {off}", entry.label());
        assert!(menu_entry(&app, cx, &label).is_some(), "{label}");
    }
    app.press(cx, &["escape"]);
    for entry in AiEntry::ALL {
        let action = crate::actions::COMMANDS
            .iter()
            .find(|command| command.id == entry.command())
            .unwrap()
            .action;
        app.step(cx, |window, cx| window.dispatch_action(action(), cx));
        assert!(panel(&app, cx).is_none(), "{entry:?} opened the panel");
    }
    assert_eq!(query_tabs(&app, cx), 1);

    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_until(cx, "the rows", |app, cx| app.has(cx, "data-ai"));
    app.click(cx, "data-ai");
    let label = format!("Generate test data… {off}");
    assert!(menu_entry(&app, cx, &label).is_some(), "{label}");
    cx.run_until_parked();
    assert_eq!(fake.connections(), 0);
    assert!(fake.requests().is_empty());
}

/// SQLite: the tree's "Explain this table" on a file opens a query tab on its `main` database,
/// the table described first with its keys; nothing goes before Enter.
#[gpui_kit::test]
fn explain_this_table_on_a_sqlite_file(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = super::sqlite::fixture_file(dir.path());
    let fake = FakeAi::start(ANSWER);
    let app = connected_as(cx, super::sqlite::file_config(&file));
    enable(cx, &fake.base(), true);
    let orders = ["main", "Tables", "orders"];
    // The file's one database opens on its folders by itself: the tree is read first.
    app.wait_until(cx, "the folders", |app, cx| {
        app.tree(cx).iter().any(|row| row.trim() == "Triggers")
    });
    app.open_path(cx, &orders);
    tree_ai(&app, cx, &orders, "Explain this table");
    app.wait_until(cx, "the conversation", |app, cx| asked(app, cx).is_some());
    assert!(matches!(asked(&app, cx), Some(Ask::ExplainTable(_))));
    let preview = ready_preview(&app, cx);
    for text in [
        "Explain what the table main.orders holds",
        "It references: main.customers.",
        "\\n- main.orders (table)\\n  id ",
        "\\n  primary key (id)",
    ] {
        assert!(preview.contains(text), "{text} missing:\n{preview}");
    }
    assert!(
        preview.contains("customer_id INTEGER not null"),
        "{preview}"
    );
    cx.run_until_parked();
    assert_eq!(fake.connections(), 0);
    let sent = sent_after_enter(&app, cx, &fake);
    assert_eq!(sent, preview);
}

/// A table tab's statement goes without its literals (a filter's values are often a row's, from
/// "Filter by this value" or a foreign key followed): `?` in place of each, whether the values'
/// box is ticked or not; ticked, the rows go, the statement still without them.
#[gpui_kit::test]
fn a_table_tabs_statement_goes_without_its_literals(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let fake = FakeAi::start(ANSWER);
    let app = connected(cx, Engine::Postgres);
    enable(cx, &fake.base(), true);
    let (value, number) = ("customer3@example.com", "9000");
    let filter = Filter {
        conditions: Vec::new(),
        raw: format!("email = '{value}' AND id < {number}"),
    };
    let ask = |app: &Fixture, cx: &mut TestAppContext| {
        app.open_table(cx, "shop", "customers", RelationKind::Table);
        app.wait_until(cx, "the filtered row", |app, cx| {
            app.has(cx, "data-ai") && app.data_rows(cx).len() == 1
        });
        app.right_click(cx, "table-cell-0:1", None);
        click_menu_entry(app, cx, "AI");
        click_menu_entry(app, cx, "Explain this result");
        app.wait_until(cx, "the question", |app, cx| {
            matches!(asked(app, cx), Some(Ask::Result { .. }))
        });
    };
    app.open_table(cx, "shop", "customers", RelationKind::Table);
    app.wait_until(cx, "the rows", |app, cx| app.has(cx, "data-ai"));
    app.on_data(cx, |tab, window, cx| {
        tab.set_filter(filter.clone(), window, cx)
    });

    // Unticked: the statement and what is sent hold `?`, never the literals.
    ask(&app, cx);
    let Some(Ask::Result { shown, .. }) = asked(&app, cx) else {
        unreachable!()
    };
    assert!(
        shown.statement.contains("email = ? AND id < ?"),
        "{}",
        shown.statement
    );
    let preview = ready_preview(&app, cx);
    let sent = sent_after_enter(&app, cx, &fake);
    assert_eq!(sent, preview);
    assert!(sent.contains("email = ? AND id < ?"), "{sent}");
    for literal in [value, number] {
        assert!(!sent.contains(literal), "{literal} sent:\n{sent}");
    }

    // Ticked: the rows go, the statement keeps its `?`.
    ask(&app, cx);
    app.click(cx, "ai-ask-values");
    let preview = ready_preview(&app, cx);
    assert!(
        preview.contains(value),
        "the row is sent once ticked:\n{preview}"
    );
    assert!(preview.contains("email = ? AND id < ?"), "{preview}");
    assert!(!preview.contains(&format!("'{value}'")), "{preview}");
    assert!(!preview.contains(number), "{preview}");
}
