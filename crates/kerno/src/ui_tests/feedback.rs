//! K21: the first outside user's feedback (2026-11-20). The connection
//! URL's example follows the engine and a pasted URL fills the form; a
//! result fetches its next page when scrolling or the keyboard reaches its
//! last rows, once, unless Settings turns it off.

use super::*;

/// The text painted in the window's last frame.
fn paints(app: &Fixture, cx: &mut TestAppContext, part: &str) -> bool {
    app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .iter()
            .any(|text| text.text().contains(part))
    })
}

/// The URL field's placeholder and text.
fn url_field(app: &Fixture, cx: &mut TestAppContext) -> (String, String) {
    app.step(cx, |window, _| {
        let node = window.find("conn-url");
        (
            node.placeholder().unwrap_or_default().to_string(),
            node.value().unwrap_or_default().to_string(),
        )
    })
}

const PG_EXAMPLE: &str = "postgres://user:password@host:5432/database";
const MYSQL_EXAMPLE: &str = "mysql://user:password@host:3306/database";
const MARIADB_EXAMPLE: &str = "mariadb://user:password@host:3306/database";

/// K21: "when you go from postgres to mysql the placeholder does not change".
#[gpui_kit::test]
fn the_url_example_follows_the_engine(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-n"]);
    assert_eq!(url_field(&app, cx).0, PG_EXAMPLE);
    app.click(cx, "engine-mysql");
    assert_eq!(url_field(&app, cx).0, MYSQL_EXAMPLE);
    app.click(cx, "engine-mariadb");
    assert_eq!(url_field(&app, cx).0, MARIADB_EXAMPLE);
    app.click(cx, "engine-postgres");
    assert_eq!(url_field(&app, cx).0, PG_EXAMPLE);
}

/// K21: a saved MySQL connection opens its dialog with MySQL's example.
#[gpui_kit::test]
fn editing_a_mysql_connection_shows_a_mysql_example(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let id = storage
        .save_connection(None, &testing::config(Engine::MySql))
        .unwrap();
    let app = open(cx, storage);
    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::Edit);
    app.wait_until(cx, "the connection dialog", |app, cx| {
        app.has(cx, "conn-name")
    });
    assert_eq!(url_field(&app, cx).0, MYSQL_EXAMPLE);
}

/// K21: "I had not understood that Fill filled the form below". The field
/// says so; a pasted URL fills the form at once (another engine's switches
/// it) and leaves alone what it does not name; a typed URL, partial or
/// whole, waits for Fill or Enter.
/// The owner's test of 2026-11-20: Fill with nothing typed takes the URL in the clipboard
/// (one click pastes and fills); with no URL there, it says so instead of doing nothing.
/// Fails with `apply_url` returning on an empty field.
#[gpui_kit::test]
fn fill_on_an_empty_field_takes_the_url_in_the_clipboard(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-n"]);
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("not a url".into()));
    app.click(cx, "conn-url-apply");
    assert!(app.has(cx, "conn-field-error"), "it says there is no URL");
    assert!(!paints(&app, cx, "Filled from the URL"));

    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
        "mysql://app:secret@db.example.com:3307/shop".into(),
    ));
    app.click(cx, "conn-url-apply");
    assert!(paints(&app, cx, "Filled from the URL"));
    assert!(!app.has(cx, "conn-field-error"));
    assert_eq!(url_field(&app, cx).1, "");
    app.click(cx, "conn-name");
    app.type_text(cx, "Clip");
    app.click(cx, "conn-save");
    app.wait_until(cx, "the connection to be stored", |app, _| {
        !app.storage.connections().unwrap().is_empty()
    });
    let stored = app.first_stored();
    assert_eq!(
        (stored.engine, stored.host.as_str(), stored.port),
        (Engine::MySql, "db.example.com", 3307)
    );
}

#[gpui_kit::test]
fn a_pasted_url_fills_the_form_and_a_typed_one_waits(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-n"]);
    assert!(paints(
        &app,
        cx,
        "Paste a URL, or click Fill to use the clipboard's"
    ));
    app.click(cx, "conn-name");
    app.type_text(cx, "Mine");

    // Typed: nothing is filled, the URL stays in its field.
    let typed = "mysql://root@db.example.com/shop";
    app.click(cx, "conn-url");
    app.type_text(cx, &typed[..10]);
    assert!(!paints(&app, cx, "Filled from the URL"));
    app.type_text(cx, &typed[10..]);
    assert!(!paints(&app, cx, "Filled from the URL"));
    assert_eq!(url_field(&app, cx).1, typed);
    app.press(cx, &["secondary-a", "backspace"]);

    // Pasted: the form is filled, the field emptied, and it says so.
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
        "mariadb://app:secret@db.example.com:3307/shop".into(),
    ));
    app.press(cx, &["secondary-v"]);
    assert!(paints(&app, cx, "Filled from the URL"));
    assert_eq!(url_field(&app, cx).1, "");
    app.click(cx, "conn-save");
    app.wait_until(cx, "the connection to be stored", |app, _| {
        !app.storage.connections().unwrap().is_empty()
    });
    let stored = app.first_stored();
    assert_eq!(
        (
            stored.engine,
            stored.name.as_str(),
            stored.host.as_str(),
            stored.port,
            stored.user.as_str(),
            stored.database.as_deref()
        ),
        (
            Engine::MariaDb,
            "Mine",
            "db.example.com",
            3307,
            "app",
            Some("shop")
        )
    );
}

/// Runs `sql` in a new tab and waits for its first page of 1,000 rows.
fn first_page(app: &Fixture, cx: &mut TestAppContext, sql: &str) -> Entity<TableState<Grid>> {
    app.run_in_new_tab(cx, sql, "secondary-enter");
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx) == ["1000 rows, more"]
    });
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid")
}

/// Lets the frames and the server run a while: what was going to arrive has.
fn settle(app: &Fixture, cx: &mut TestAppContext) {
    for _ in 0..5 {
        app.step(cx, |_, _| {});
        cx.run_until_parked();
    }
}

fn big_query(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "select id from analytics.big_table order by id limit 2500",
        _ => "select id from kerno_analytics.big_table order by id limit 2500",
    }
}

/// K21: "There is no load more". Scrolling to the end of the fetched rows
/// fetches the next page, once, and the footer says what is there; the
/// keyboard reaching the end does the same.
#[gpui_kit::test]
fn reaching_the_end_fetches_the_next_page_once(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        let grid = first_page(&app, cx, big_query(engine));
        assert_eq!(
            app.label_of(cx, "result-summary"),
            "1,000 rows fetched, more on the server",
            "{engine:?}"
        );
        app.step(cx, |_, cx| {
            grid.update(cx, |table, cx| table.scroll_to_row(999, cx))
        });
        app.wait_until(cx, "the second page", |app, cx| {
            app.outcomes(cx) == ["2000 rows, more"]
        });
        // One page per reach of the end: the next waits for the next one.
        settle(&app, cx);
        assert_eq!(app.outcomes(cx), ["2000 rows, more"], "{engine:?}");
        assert_eq!(
            app.label_of(cx, "result-summary"),
            "2,000 rows fetched, more on the server"
        );

        // The keyboard: Page Down from row 1,900 down to the end.
        app.step(cx, |window, cx| {
            grid.update(cx, |table, cx| table.set_selected_cell(1900, 1, cx));
            grid.read(cx).focus_handle(cx).focus(window, cx);
        });
        settle(&app, cx);
        assert_eq!(app.outcomes(cx), ["2000 rows, more"], "{engine:?}");
        for _ in 0..20 {
            if app.outcomes(cx) != ["2000 rows, more"] {
                break;
            }
            app.press(cx, &["pagedown"]);
            cx.run_until_parked();
        }
        app.wait_until(cx, "the last rows", |app, cx| {
            app.outcomes(cx) == ["2500 rows"]
        });
    }
}

/// Real-app pass: after the next page came, the status bar said "2000 rows · 34.45 s", the
/// time since the run started, while the statement took 28 ms. It keeps the statement's own
/// time through every page, as the footer does, and writes the rows as the footer does.
#[gpui_kit::test]
fn the_status_bar_keeps_the_statements_time_after_more_pages(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let grid = first_page(&app, cx, big_query(Engine::Postgres));
    // The rows item and the time after it.
    let rows_and_time = |app: &Fixture, cx: &mut TestAppContext| {
        let status = app.status_bar(cx);
        let at = (status.iter())
            .position(|item| item.ends_with(" rows"))
            .expect("a rows item");
        (status[at].clone(), status.get(at + 1).cloned())
    };
    let (rows, time) = rows_and_time(&app, cx);
    assert_eq!(rows, "1,000 rows");
    assert!(time.is_some());
    // The user reads a while before scrolling.
    std::thread::sleep(Duration::from_millis(1100));
    app.step(cx, |_, cx| {
        grid.update(cx, |table, cx| table.scroll_to_row(999, cx))
    });
    app.wait_until(cx, "the second page", |app, cx| {
        app.outcomes(cx) == ["2000 rows, more"]
    });
    assert_eq!(rows_and_time(&app, cx), ("2,000 rows".to_string(), time));
}

/// K21: a complete result fetches nothing at its end; a page asked for and
/// not yet here is not asked again.
#[gpui_kit::test]
fn a_complete_result_or_a_page_on_its_way_is_not_asked_again(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g from generate_series(1, 500) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["500 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    let asks = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            gpui_kit::component::table::TableDelegate::has_more(grid.read(cx).delegate(), cx)
        })
    };
    assert!(!asks(cx), "nothing more to ask for");
    app.step(cx, |_, cx| {
        grid.update(cx, |table, cx| table.scroll_to_row(499, cx))
    });
    settle(&app, cx);
    assert_eq!(app.outcomes(cx), ["500 rows"]);

    // A page on its way: the end reached twice before it is here asks once.
    let grid = first_page(&app, cx, "select g from generate_series(1, 2500) g");
    app.step(cx, |window, cx| {
        grid.update(cx, |table, cx| {
            table.delegate_mut().load_more(window, cx);
            table.delegate_mut().load_more(window, cx);
        })
    });
    app.wait_until(cx, "the second page", |app, cx| {
        app.outcomes(cx) == ["2000 rows, more"]
    });
    settle(&app, cx);
    assert_eq!(app.outcomes(cx), ["2000 rows, more"]);
}

/// K21: Settings ▸ Grid turns the fetch at the end off (DBeaver's "auto
/// fetch next segment"); Fetch more still works. Rows sorted here (a SHOW
/// cannot be read again in another order) are not added to by themselves.
#[gpui_kit::test]
fn the_setting_off_or_rows_sorted_here_fetch_nothing_by_themselves(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.fetch_on_scroll = false));
    let grid = first_page(&app, cx, "select g from generate_series(1, 2500) g");
    app.step(cx, |_, cx| {
        grid.update(cx, |table, cx| table.scroll_to_row(999, cx))
    });
    settle(&app, cx);
    assert_eq!(app.outcomes(cx), ["1000 rows, more"]);
    app.click(cx, "fetch-more");
    app.wait_until(cx, "the page asked for", |app, cx| {
        app.outcomes(cx) == ["2000 rows, more"]
    });

    // On again, a SHOW of 100-row pages sorted here.
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            settings.fetch_on_scroll = true;
            settings.page_size = 100;
        })
    });
    app.run_in_new_tab(cx, "show all", "secondary-enter");
    app.wait_until(cx, "the first page", |app, cx| {
        app.outcomes(cx) == ["100 rows, more"]
    });
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    app.step(cx, |window, cx| {
        grid.update(cx, |table, cx| {
            table
                .delegate_mut()
                .perform_sort(1, ColumnSort::Descending, window, cx)
        })
    });
    app.step(cx, |_, cx| {
        grid.update(cx, |table, cx| table.scroll_to_row(99, cx))
    });
    settle(&app, cx);
    assert_eq!(app.outcomes(cx), ["100 rows, more"]);
}

/// K21, K36: a table's footer says what is fetched, its fetch size, and
/// what the way on reads; a load adds its rows under those there.
#[gpui_kit::test]
fn a_tables_footer_says_what_is_fetched_and_what_comes_next(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 100));
    app.open_table(cx, "shop", "orders", RelationKind::Table);
    app.wait_until(cx, "the first load", |app, cx| {
        app.data_state(cx) == "Rows 1–100 of 5,000"
    });
    assert_eq!(app.label_of(cx, "data-fetch-size"), "Fetch size: 100");
    assert_eq!(app.label_of(cx, "data-fetch-more"), "Fetch 100 more");
    assert_eq!(app.label_of(cx, "data-fetch-all"), "Fetch all");
    app.click(cx, "data-fetch-more");
    app.wait_until(cx, "the second load", |app, cx| {
        app.data_state(cx) == "Rows 1–200 of 5,000"
    });
}
