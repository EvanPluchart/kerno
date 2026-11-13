//! Tests of v1 items that had none (docs/plans/M10.md Part 4, third list,
//! item 18): running the selection, the grid's header and row-number
//! selection, column resize and alignment, pasting on MySQL, the import
//! dialog's cancel and production confirmation, syntax highlighting.

use super::*;
use gpui_kit::component::table::TableSelection;

#[gpui_kit::test]
fn only_the_selected_text_runs(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 1 as a, 2 as b");
    // "select 1 as a", chosen from the keyboard: a part of the statement.
    app.press(cx, &["home"]);
    for _ in 0.."select 1 as a".len() {
        app.press(cx, &["shift-right"]);
    }
    let tab = app.tab(cx);
    let editor = cx.update(|cx| tab.read(cx).editor().clone());
    assert_eq!(cx.update(|cx| editor.read(cx).selected_range()), 0..13);
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the selection's row", |app, cx| {
        app.outcomes(cx) == ["1 rows"]
    });
    assert_eq!(
        app.result(cx, 0, 10),
        (vec!["a".to_string()], vec![vec![Some("1".to_string())]])
    );
    // Without a selection, the statement at the cursor runs whole.
    app.press(cx, &["end"]);
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the whole statement", |app, cx| {
        app.outcomes(cx) == ["1 rows"] && app.result(cx, 0, 10).0 == ["a", "b"]
    });
}

/// A query tab on PostgreSQL with three rows of a number and a text, and
/// its grid.
fn three_rows(cx: &mut TestAppContext) -> (Fixture, Entity<TableState<Grid>>) {
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "select g as n, 'v' || g as label from generate_series(1, 3) g",
        "secondary-enter",
    );
    app.wait_until(cx, "the rows", |app, cx| app.outcomes(cx) == ["3 rows"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    (app, grid)
}

#[gpui_kit::test]
fn a_column_is_selected_by_its_header_and_a_row_by_its_number(cx: &mut TestAppContext) {
    use crate::query::grid::Selected;
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (app, grid) = three_rows(cx);
    let selected = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let table = grid.read(cx);
            (
                table.selection(),
                table.delegate().selected(table.selection()),
            )
        })
    };

    // The header of "label" (column 0 of the table numbers the rows).
    app.click(cx, ("col-header", 2usize));
    assert_eq!(
        selected(cx),
        (
            TableSelection::Column(2),
            Some(Selected {
                rows: 0..=2,
                columns: vec![1]
            })
        )
    );
    // The number of the second row, the editor having the keyboard: the
    // grid takes it.
    app.click(cx, "query-editor");
    app.click(cx, ("row-number", 1usize));
    assert_eq!(
        selected(cx),
        (
            TableSelection::Row(1),
            Some(Selected {
                rows: 1..=1,
                columns: vec![0, 1]
            })
        )
    );
    // Shift and a click on another number stretches the rows to it.
    app.step(cx, |window, cx| {
        window.click_with_modifiers(("row-number", 2usize), gpui_kit::Modifiers::shift(), cx)
    });
    assert_eq!(
        selected(cx),
        (
            TableSelection::Row(1),
            Some(Selected {
                rows: 1..=2,
                columns: vec![0, 1]
            })
        )
    );
    // What is selected is what is copied.
    app.press(cx, &["secondary-c"]);
    assert_eq!(app.clipboard(cx).as_deref(), Some("2\tv2\n3\tv3\n"));
}

#[gpui_kit::test]
fn a_column_is_resized_by_dragging_the_edge_of_its_header(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (app, grid) = three_rows(cx);
    let width = |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).col_width(2).unwrap());
    let order = |cx: &mut TestAppContext| cx.update(|cx| grid.read(cx).delegate().shown_columns());
    let before = width(cx);
    // The handle straddles the right edge of the header: pressed just
    // inside it, then moved 60 px to the right.
    let header = app.step(cx, |window, _| window.find(("col-header", 2usize)).bounds());
    let from = gpui_kit::point(header.right() - px(2.), header.center().y);
    let to = gpui_kit::point(from.x + px(60.), from.y);
    app.step(cx, |window, cx| window.drag(from, to, cx));
    let after = width(cx);
    assert!(
        after > before + px(55.) && after < before + px(65.),
        "{before:?} → {after:?}"
    );
    // The column was resized, not moved.
    assert_eq!(order(cx), ["n", "label"]);
}

#[gpui_kit::test]
fn numbers_are_aligned_right_and_text_left(cx: &mut TestAppContext) {
    use gpui_kit::TextAlign;
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let app = connected(cx, engine);
        app.run_in_new_tab(
            cx,
            "select 1 as i, 2.5 as d, 'x' as t, null as z",
            "secondary-enter",
        );
        app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
        let tab = app.tab(cx);
        let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
        // The kit draws a cell's text with its column's alignment
        // (`TableState::render_cell`); column 0 numbers the rows.
        let aligns = cx.update(|cx| {
            let grid = grid.read(cx).delegate();
            (0..5)
                .map(|column| grid.column(column, cx).align)
                .collect::<Vec<_>>()
        });
        assert_eq!(
            aligns,
            [
                TextAlign::Right,
                TextAlign::Right,
                TextAlign::Right,
                TextAlign::Left,
                TextAlign::Left
            ],
            "{engine:?}"
        );
    }
}

#[gpui_kit::test]
fn pasting_fills_cells_on_mysql_and_mariadb(cx: &mut TestAppContext) {
    for engine in [Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let _scratch = Scratch::new(
            engine,
            &["DROP DATABASE IF EXISTS kerno_ui_paste_my"],
            &[
                "CREATE DATABASE kerno_ui_paste_my",
                "CREATE TABLE kerno_ui_paste_my.items (id INT PRIMARY KEY, \
                 name VARCHAR(20) NOT NULL, qty INT, \
                 twice INT AS (qty * 2) STORED, note VARCHAR(20))",
                "INSERT INTO kerno_ui_paste_my.items (id, name, qty, note) \
                 VALUES (1, 'a', 1, 'x'), (2, 'b', 2, 'y'), (3, 'c', 3, 'z')",
            ],
        );
        let app = connected(cx, engine);
        app.open_table(cx, "kerno_ui_paste_my", "items", RelationKind::Table);
        app.wait_for_rows(cx, "Rows 1–3 of 3");
        let grid = app.data_grid(cx);
        let select = |app: &Fixture, cx: &mut TestAppContext, cell: (usize, usize)| {
            app.step(cx, |window, cx| {
                grid.update(cx, |table, cx| {
                    table.delegate_mut().clear_selection();
                    table.set_selected_cell(cell.0, cell.1, cx);
                });
                grid.read(cx).focus_handle(cx).focus(window, cx);
            });
        };
        let paste = |app: &Fixture, cx: &mut TestAppContext, text: &str| {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.to_string()));
            app.press(cx, &["secondary-v"]);
        };
        let row = |cells: [&str; 5]| cells.map(str::to_string).to_vec();

        // A block from "qty" of the second row: the generated column is
        // skipped, an empty number is NULL, nothing goes past the end.
        select(&app, cx, (1, 3));
        paste(&app, cx, "10\t99\tn1\textra\n\t99\tn2\n30\t99\tn3\n");
        assert_eq!(
            app.data_rows(cx),
            [
                row(["1", "a", "1", "2", "x"]),
                row(["2", "b", "10", "4", "n1"]),
                row(["3", "c", "NULL", "6", "n2"]),
            ],
            "{engine:?}"
        );
        let tab = app.data(cx);
        let (_, statements) = cx.update(|cx| tab.read(cx).pending_sql(cx)).unwrap();
        assert_eq!(
            statements,
            [
                "UPDATE `kerno_ui_paste_my`.`items` SET `qty` = 10, `note` = 'n1' WHERE `id` = 2",
                "UPDATE `kerno_ui_paste_my`.`items` SET `qty` = NULL, `note` = 'n2' WHERE `id` = 3",
            ],
            "{engine:?}"
        );
        // Delete empties a nullable cell.
        select(&app, cx, (0, 5));
        app.press(cx, &["delete"]);
        assert_eq!(app.pending(cx), 3, "{engine:?}");

        app.click(cx, "data-apply");
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the applied changes", |app, cx| {
            app.pending(cx) == 0 && app.data_state(cx) == "Rows 1–3 of 3"
        });
        assert_eq!(
            on_server(
                engine,
                "SELECT GROUP_CONCAT(CONCAT_WS('|', id, IFNULL(qty, 'NULL'), \
                 IFNULL(twice, 'NULL'), IFNULL(note, 'NULL')) ORDER BY id SEPARATOR ',') \
                 FROM kerno_ui_paste_my.items"
            ),
            "1|1|2|NULL,2|10|20|n1,3|NULL|NULL|n2",
            "{engine:?}"
        );
    }
}

impl Fixture {
    /// Goes from an import dialog's preview to its mapping, then asks to
    /// start: the confirmation with the SQL is shown.
    fn start_import(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<crate::import_dialog::ImportDialog>,
    ) {
        // "Map columns…" waits for the columns of an existing table.
        self.wait_until(cx, "the mapping", |app, cx| {
            app.click(cx, "import-next");
            cx.update(|cx| dialog.read(cx).state(cx)) == "mapping"
        });
        self.click(cx, "import-start");
        assert!(self.has(cx, "sql-preview"), "the SQL is shown first");
    }
}

#[gpui_kit::test]
fn an_import_on_production_is_confirmed_twice(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_import_prod CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_import_prod",
            "CREATE TABLE kerno_ui_import_prod.t (id int PRIMARY KEY, name text)",
        ],
    );
    let count = || {
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM kerno_ui_import_prod.t",
        )
    };
    let mut config = testing::config(Engine::Postgres);
    config.environment = kerno_core::environment::PRODUCTION.into();
    let app = connected_as(cx, config);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("t.csv");
    std::fs::write(&file, "id,name\n1,a\n2,b\n").unwrap();
    let dialog = app
        .import_dialog(
            cx,
            "kerno_ui_import_prod",
            Some(TableRef::new("kerno_ui_import_prod", "t")),
        )
        .expect("the import dialog");
    app.import_file(cx, &dialog, &file);
    app.start_import(cx, &dialog);

    // The first confirmation asks again; nothing has run.
    app.click(cx, "sql-confirm");
    assert!(app.has(cx, "sql-production"));
    assert_eq!(cx.update(|cx| dialog.read(cx).state(cx)), "mapping");
    assert_eq!(count(), "0");
    // Cancel there imports nothing.
    app.click(cx, "sql-cancel");
    assert!(!app.has(cx, "sql-production"));
    assert_eq!(cx.update(|cx| dialog.read(cx).state(cx)), "mapping");

    // Asked twice, it runs.
    app.click(cx, "import-start");
    app.click(cx, "sql-confirm");
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the import", |_, cx| {
        cx.update(|cx| dialog.read(cx).state(cx)) == "done: 2 rows"
    });
    assert_eq!(count(), "2");
}

#[gpui_kit::test]
fn an_import_is_cancelled_from_its_dialog_and_keeps_nothing(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        // A failed run may leave the insert sleeping: it would hold the drop.
        &[
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE query LIKE '%kerno_ui_import_cancel%' AND pid <> pg_backend_pid()",
            "DROP SCHEMA IF EXISTS kerno_ui_import_cancel CASCADE",
        ],
        &[
            "CREATE SCHEMA kerno_ui_import_cancel",
            "CREATE TABLE kerno_ui_import_cancel.t (id int PRIMARY KEY, name text)",
            // Each row waits: the import is still running when it is cancelled.
            "CREATE FUNCTION kerno_ui_import_cancel.slow() RETURNS trigger LANGUAGE plpgsql \
             AS $$ BEGIN PERFORM pg_sleep(60); RETURN NEW; END $$",
            "CREATE TRIGGER slow BEFORE INSERT ON kerno_ui_import_cancel.t \
             FOR EACH ROW EXECUTE FUNCTION kerno_ui_import_cancel.slow()",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("t.csv");
    std::fs::write(&file, "id,name\n1,a\n2,b\n").unwrap();
    let dialog = app
        .import_dialog(
            cx,
            "kerno_ui_import_cancel",
            Some(TableRef::new("kerno_ui_import_cancel", "t")),
        )
        .expect("the import dialog");
    app.import_file(cx, &dialog, &file);
    app.start_import(cx, &dialog);
    app.click(cx, "sql-confirm");
    // A cancel that reaches the server before the insert is ignored there.
    app.wait_until(cx, "the insert on the server", |_, cx| {
        cx.update(|cx| dialog.read(cx).state(cx)) == "running"
            && running_on_server(Engine::Postgres, "kerno_ui_import_cancel")
    });
    app.click(cx, "import-cancel");
    app.wait_until(cx, "the cancellation", |_, cx| {
        cx.update(|cx| dialog.read(cx).state(cx)) == "cancelled"
    });
    assert!(app.has(cx, "import-cancelled"));
    assert!(!running_on_server(
        Engine::Postgres,
        "kerno_ui_import_cancel"
    ));
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM kerno_ui_import_cancel.t"
        ),
        "0"
    );
}

#[gpui_kit::test]
fn an_import_into_a_new_table_says_when_the_table_is_created_first(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_imp_new";
        let (cleanup, create) = scratch_schema(engine, schema);
        let _scratch = Scratch::new(engine, &[&cleanup], &[&create]);
        let tables = || {
            on_server(
                engine,
                &format!(
                    "SELECT count(*) FROM information_schema.tables \
                     WHERE table_schema = '{schema}'"
                ),
            )
        };
        let app = connected(cx, engine);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fresh.csv");
        std::fs::write(&file, "id,name\n1,a\n2,b\n").unwrap();
        let dialog = app
            .import_dialog(cx, schema, None)
            .expect("the import dialog");
        app.import_file(cx, &dialog, &file);
        app.start_import(cx, &dialog);
        assert_eq!(
            app.label_of(cx, "sql-title"),
            format!("Import 2 rows into {schema}.fresh"),
            "{engine:?}"
        );
        let first = "The table is created first, on its own: MySQL and MariaDB commit a \
                     CREATE TABLE at once. Then the rows are inserted in one transaction. \
                     If a row fails, none is kept and the new table is dropped.";
        let together = "The table is created and the rows inserted in one transaction: \
                        all of them, or none.";
        let mysql = engine != Engine::Postgres;
        assert_eq!(
            super::tool_wizards::paints(&app, cx, first),
            mysql,
            "{engine:?}"
        );
        assert_eq!(
            super::tool_wizards::paints(&app, cx, together),
            !mysql,
            "{engine:?}"
        );
        // Cancelled there, nothing ran: no table is left behind.
        app.click(cx, "sql-cancel");
        assert!(!app.has(cx, "sql-preview"), "{engine:?}");
        assert_eq!(cx.update(|cx| dialog.read(cx).state(cx)), "mapping");
        assert_eq!(tables(), "0", "{engine:?}");
    }
}

#[gpui_kit::test]
fn sql_keywords_strings_and_comments_are_highlighted(cx: &mut TestAppContext) {
    use gpui_kit::component::Rope;
    use gpui_kit::component::highlighter::SyntaxHighlighter;

    let app = open(cx, memory_storage());
    // The editor's own highlighter is private to the kit (`pub(super) fn
    // highlighter` in gpui-base's `input/base/mode.rs`); it is this one,
    // built for "sql" by the kit's factory, styled by the theme's
    // `highlight_theme` (gpui-component `input/input.rs`).
    let text = "select 'kerno' as name -- a note\nfrom t";
    let mut highlighter = SyntaxHighlighter::new("sql");
    highlighter.update(None, &Rope::from(text), None);
    for dark in [false, true] {
        if dark {
            app.dispatch(cx, actions::UseDarkTheme);
        }
        let theme = cx.update(|cx| {
            assert_eq!(cx.theme().is_dark(), dark);
            cx.theme().highlight_theme.clone()
        });
        let styles = highlighter.styles(&(0..text.len()), &*theme);
        let style_at = |offset: usize| {
            styles
                .iter()
                .find(|(range, _)| range.contains(&offset))
                .map(|(_, style)| *style)
                .unwrap_or_default()
        };
        let keyword = style_at(0);
        let string = style_at(text.find("'kerno'").unwrap() + 1);
        let comment = style_at(text.find("-- a note").unwrap() + 3);
        for (what, style) in [
            ("keyword", keyword),
            ("string", string),
            ("comment", comment),
        ] {
            assert!(style.color.is_some(), "dark {dark}: {what} {styles:?}");
        }
        assert_ne!(keyword.color, string.color, "dark {dark}");
        assert_ne!(keyword.color, comment.color, "dark {dark}");
        assert_ne!(string.color, comment.color, "dark {dark}");
        // "from", on the next line, is a keyword too.
        assert_eq!(style_at(text.find("from").unwrap()), keyword, "dark {dark}");
    }
}
