//! Reordering the columns of an existing table from its form (CLAUDE.md
//! §4 J8, Beekeeper #1341): ↑ / ↓ and the row's right-click menu on MySQL
//! and MariaDB, applied with `FIRST` / `AFTER`; on PostgreSQL the controls
//! are off and say why.

use super::tool_wizards::paints;
use super::*;

/// The columns of `schema.items` on the server, in order.
fn server_order(engine: Engine, schema: &str) -> String {
    on_server(
        engine,
        &format!(
            "SELECT GROUP_CONCAT(COLUMN_NAME ORDER BY ORDINAL_POSITION) \
             FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = '{schema}' AND TABLE_NAME = 'items'"
        ),
    )
}

impl Fixture {
    /// The column names of the active table form, in order.
    fn form_columns(&self, cx: &mut TestAppContext) -> Vec<String> {
        let editor = self.editor(cx);
        self.step(cx, |window, cx| editor.read(cx).column_focus(window, cx).0)
    }
}

#[gpui_kit::test]
fn columns_move_with_the_arrows_and_the_row_menu_on_mysql_and_mariadb(cx: &mut TestAppContext) {
    for engine in [Engine::MySql, Engine::MariaDb] {
        if !testing::available(engine) {
            return;
        }
        let schema = "kerno_ui_reorder";
        let (drop_schema, create_schema) = scratch_schema(engine, schema);
        let items = scratch_table(
            engine,
            &format!("{schema}.items"),
            "name varchar(20) NOT NULL DEFAULT 'n', qty int, note text COMMENT 'free'",
        );
        let _scratch = Scratch::new(engine, &[&drop_schema], &[&create_schema, &items]);
        let app = connected(cx, engine);
        app.open_structure(cx, schema, "items", RelationKind::Table);
        app.wait_for_structure(cx);
        app.click(cx, "structure-edit");
        app.wait_for_editor(cx);
        assert!(paints(
            &app,
            cx,
            "↑ and ↓ move a column; the server rewrites the table to put it there."
        ));

        app.click(cx, ("column-up", 3usize));
        assert_eq!(app.form_columns(cx), ["id", "name", "note", "qty"]);
        app.click(cx, ("column-down", 0usize));
        assert_eq!(app.form_columns(cx), ["name", "id", "note", "qty"]);
        // The row's menu, from a right-click beside its remove button.
        app.right_click(
            cx,
            ("column-row", 3usize),
            Some(gpui_kit::point(gpui_kit::px(1.), gpui_kit::px(1.))),
        );
        app.click_label(cx, "Move up");
        assert_eq!(app.form_columns(cx), ["name", "id", "qty", "note"]);

        // One move is enough: `id` after `name`, the others in their order.
        // MariaDB prints a display width.
        let int = if engine == Engine::MariaDb {
            "int(11)"
        } else {
            "int"
        };
        let sql = app.editor_sql(cx).expect("statements");
        assert_eq!(
            sql,
            format!(
                "ALTER TABLE `{schema}`.`items`\n    \
                 MODIFY COLUMN `id` {int} NOT NULL AUTO_INCREMENT AFTER `name`"
            ),
            "{engine:?}"
        );
        app.click(cx, "editor-apply");
        assert!(app.has(cx, "sql-preview"));
        app.click(cx, "sql-confirm");
        app.wait_until(cx, "the alter", |app, cx| app.has(cx, "editor-no-change"));
        assert_eq!(server_order(engine, schema), "name,id,qty,note");
        // The moved column kept its definition, and is still the key.
        assert_eq!(
            on_server(
                engine,
                &format!(
                    "SELECT CONCAT_WS(' ', IS_NULLABLE, EXTRA, COLUMN_KEY) FROM information_schema.COLUMNS \
                     WHERE TABLE_SCHEMA = '{schema}' AND TABLE_NAME = 'items' \
                     AND COLUMN_NAME = 'id'"
                )
            ),
            "NO auto_increment PRI"
        );
        app.press(cx, &["secondary-w"]);
    }
}

#[gpui_kit::test]
fn postgres_says_why_its_columns_cannot_move(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let schema = "kerno_ui_reorder_pg";
    let (drop_schema, create_schema) = scratch_schema(engine, schema);
    let items = scratch_table(engine, &format!("{schema}.items"), "name text, qty int");
    let _scratch = Scratch::new(engine, &[&drop_schema], &[&create_schema, &items]);
    let app = connected(cx, engine);
    app.open_structure(cx, schema, "items", RelationKind::Table);
    app.wait_for_structure(cx);
    app.click(cx, "structure-edit");
    app.wait_for_editor(cx);

    let editor = app.editor(cx);
    assert_eq!(
        cx.update(|cx| editor.read(cx).reorder_refused(cx)),
        Some(kerno_core::table_draft::CANNOT_MOVE_COLUMNS)
    );
    assert!(paints(
        &app,
        cx,
        kerno_core::table_draft::CANNOT_MOVE_COLUMNS
    ));
    // Neither the arrows nor the menu move anything.
    app.click(cx, ("column-up", 2usize));
    app.right_click(
        cx,
        ("column-row", 2usize),
        Some(gpui_kit::point(gpui_kit::px(1.), gpui_kit::px(1.))),
    );
    assert!(app.shows_label(cx, "Move up"));
    app.click_label(cx, "Move up");
    app.step(cx, |_, cx| {
        editor.update(cx, |editor, cx| editor.move_column(2, -1, cx))
    });
    assert_eq!(app.form_columns(cx), ["id", "name", "qty"]);
    assert!(app.has(cx, "editor-no-change"));
}
